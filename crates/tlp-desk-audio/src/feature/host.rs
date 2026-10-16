//! Host side: capture -> resample -> silence gate -> Opus -> `StreamKind::Audio` streams.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tlp_desk_net::{Feature, FeatureCtx, SendStream, Session, StreamKind, STREAM_REFUSED};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::wire::write_packet;
use super::{control, AudioEvent, FEATURE_NAME};
use crate::codec::{OpusEncoder, DEFAULT_BITRATE};
use crate::device::{AudioCapture, DeviceHandle, PcmChunk, PcmSink};
use crate::dsp::{Resampler, SilenceGate};
use crate::format::{CHANNELS, FRAME_LEN, SAMPLE_RATE};

/// Streams the host's system audio to viewers that ask for it and hold `AUDIO`.
///
/// Register one instance in `HostConfig::features`; it serves every session.
pub struct AudioHost {
    capture: Arc<dyn AudioCapture>,
    bitrate: u32,
}

impl std::fmt::Debug for AudioHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioHost")
            .field("bitrate", &self.bitrate)
            .finish_non_exhaustive()
    }
}

impl AudioHost {
    /// A host feature capturing from `capture` (use [`crate::SystemCapture`] for the real
    /// loopback device) at the default Opus bitrate.
    pub fn new(capture: Arc<dyn AudioCapture>) -> Self {
        Self {
            capture,
            bitrate: DEFAULT_BITRATE,
        }
    }

    /// Changes the Opus bitrate (bits per second).
    #[must_use]
    pub fn with_bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = bitrate;
        self
    }
}

impl Feature for AudioHost {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::AudioControl { .. })
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::error!("audio feature started outside a tokio runtime; audio disabled");
            return;
        };
        rt.spawn(host_task(ctx, inbox, self.capture.clone(), self.bitrate));
    }
}

/// A running capture + encoder for one session.
struct Streaming {
    /// Dropping it stops the device, which ends the encoder task after its last chunk.
    _device: DeviceHandle,
    encoder: JoinHandle<()>,
}

async fn host_task(
    ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    capture: Arc<dyn AudioCapture>,
    bitrate: u32,
) {
    // Only the host sends audio: refuse every audio stream the viewer opens.
    let refuse = tokio::spawn(refuse_streams(ctx.session.clone()));
    let generation = Arc::new(AtomicU64::new(0));
    let mut perms = ctx.permissions.clone();
    let mut wanted = false;
    let mut active: Option<Streaming> = None;
    loop {
        let asked = tokio::select! {
            msg = inbox.recv() => match msg {
                None => break,
                Some(Message::AudioControl { enabled, .. }) => {
                    wanted = enabled;
                    true
                }
                Some(_) => continue,
            },
            changed = perms.changed() => {
                if changed.is_err() {
                    break;
                }
                false
            }
        };
        let should = wanted && ctx.allowed(Permissions::AUDIO);
        let was = active.is_some();
        if should && !was {
            active = start(&ctx, &capture, bitrate, &generation);
        } else if !should && was {
            stop(&mut active);
            tracing::info!(session = ctx.session_key, "audio capture stopped");
        }
        let now = active.is_some();
        if now != was {
            ctx.emit(FEATURE_NAME, AudioEvent::HostStreaming(now));
        }
        // Answer every request, and tell the viewer when we stop on our own (revoked).
        if asked || now != was {
            if let Err(e) = ctx.session.send(&control(now)).await {
                tracing::debug!(error = %e, "could not send AudioControl");
            }
        }
    }
    stop(&mut active);
    refuse.abort();
}

fn start(
    ctx: &FeatureCtx,
    capture: &Arc<dyn AudioCapture>,
    bitrate: u32,
    generation: &Arc<AtomicU64>,
) -> Option<Streaming> {
    let (sink, rx) = PcmSink::channel();
    match capture.start(sink) {
        Ok(device) => {
            tracing::info!(session = ctx.session_key, "audio capture started");
            let encoder = tokio::spawn(encode_loop(
                ctx.session.clone(),
                rx,
                bitrate,
                generation.clone(),
            ));
            Some(Streaming {
                _device: device,
                encoder,
            })
        }
        Err(e) => {
            tracing::warn!(session = ctx.session_key, error = %e, "audio capture failed to start");
            ctx.emit(FEATURE_NAME, AudioEvent::DeviceError(e.to_string()));
            None
        }
    }
}

fn stop(active: &mut Option<Streaming>) {
    if let Some(s) = active.take() {
        // Stop at once: abort the encoder (even if a write is stuck on a congested
        // connection; its stream ends with it) and drop the device.
        s.encoder.abort();
        drop(s);
    }
}

async fn refuse_streams(session: Arc<Session>) {
    while let Ok(mut s) = session.accept_side_stream(StreamKind::Audio).await {
        tracing::debug!("refusing an audio stream opened by the viewer");
        let _ = s.recv.stop(STREAM_REFUSED.into());
    }
}

/// Turns captured chunks into Opus packets on audio streams, one stream per burst of sound.
async fn encode_loop(
    session: Arc<Session>,
    mut rx: mpsc::Receiver<PcmChunk>,
    bitrate: u32,
    generation: Arc<AtomicU64>,
) {
    let mut enc = match OpusEncoder::new(bitrate) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "audio encoder failed");
            return;
        }
    };
    let mut gate = SilenceGate::default();
    let mut resampler: Option<Resampler> = None;
    let mut pcm: Vec<f32> = Vec::with_capacity(FRAME_LEN * 4);
    let mut converted = Vec::new();
    let mut scratch = Vec::new();
    let mut stream: Option<SendStream> = None;
    while let Some(chunk) = rx.recv().await {
        let fits = resampler
            .as_ref()
            .is_some_and(|r| r.in_rate() == chunk.rate && r.in_channels() == chunk.channels);
        if !fits {
            match Resampler::new(chunk.rate, chunk.channels, SAMPLE_RATE, CHANNELS) {
                Ok(r) => resampler = Some(r),
                Err(e) => {
                    tracing::warn!(error = %e, "unusable capture format; chunk dropped");
                    continue;
                }
            }
        }
        let Some(r) = resampler.as_mut() else {
            continue;
        };
        converted.clear();
        r.process(&chunk.samples, &mut converted);
        pcm.extend_from_slice(&converted);

        let mut offset = 0;
        while let Some(frame) = pcm.get(offset..offset + FRAME_LEN) {
            offset += FRAME_LEN;
            if !gate.should_send(frame) {
                if let Some(mut s) = stream.take() {
                    tracing::debug!("audio paused: silence");
                    let _ = s.finish();
                }
                continue;
            }
            if stream.is_none() {
                let id = generation.fetch_add(1, Ordering::AcqRel);
                match session.open_uni(StreamKind::Audio, id).await {
                    Ok(s) => {
                        // A fresh stream is decoded by a fresh decoder state.
                        enc.reset();
                        stream = Some(s);
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "cannot open audio stream; stopping");
                        return;
                    }
                }
            }
            let packet = match enc.encode(frame) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "audio encode failed; frame dropped");
                    continue;
                }
            };
            if let Some(s) = stream.as_mut() {
                if let Err(e) = write_packet(s, &packet, &mut scratch).await {
                    tracing::debug!(error = %e, "audio stream write failed");
                    stream = None;
                }
            }
        }
        pcm.drain(..offset.min(pcm.len()));
    }
    if let Some(mut s) = stream {
        let _ = s.finish();
    }
}
