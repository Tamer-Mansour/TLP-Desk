//! The real sound card through `cpal`: WASAPI on Windows.
//!
//! Loopback capture is WASAPI's documented way to record "what you hear": an *input* stream
//! opened on a *render* (output) endpoint, which cpal maps to `AUDCLNT_STREAMFLAGS_LOOPBACK`
//! (see Microsoft Learn, "Loopback Recording"). While nothing plays, WASAPI delivers no
//! packets, so an idle capture costs no CPU.
//!
//! Each device runs on its own thread that owns the cpal stream and parks until the
//! [`DeviceHandle`] is dropped; that keeps the stream on one thread on every platform.

use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use super::{AudioCapture, AudioPlayback, DeviceHandle, PcmChunk, PcmSink, PcmSource};
use crate::error::AudioError;

/// How long the OS may take to open a device.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

fn dev_err(e: impl std::fmt::Display) -> AudioError {
    AudioError::Device(e.to_string())
}

/// Runs `open` on a new thread, keeps what it returns alive there until the handle drops,
/// and reports whether opening worked.
fn run_device<F>(name: &str, open: F) -> Result<DeviceHandle, AudioError>
where
    F: FnOnce() -> Result<cpal::Stream, AudioError> + Send + 'static,
{
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), AudioError>>(1);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let stream = match open() {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(dev_err(e)));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            // Parks until the handle (the only sender) is dropped.
            let _ = stop_rx.recv();
            drop(stream);
        })
        .map_err(dev_err)?;
    match ready_rx.recv_timeout(OPEN_TIMEOUT + Duration::from_secs(1)) {
        Ok(Ok(())) => Ok(DeviceHandle::new(stop_tx)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(AudioError::Device(
            "the audio device did not open in time".into(),
        )),
    }
}

/// Loopback capture of the default render device (what the host's speakers play).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCapture;

impl AudioCapture for SystemCapture {
    fn start(&self, sink: PcmSink) -> Result<DeviceHandle, AudioError> {
        run_device("audio-loopback", move || {
            let host = cpal::default_host();
            let device = host
                .default_output_device()
                .ok_or(AudioError::NoDevice("output"))?;
            let supported = device.default_output_config().map_err(dev_err)?;
            let config = supported.config();
            tracing::info!(
                rate = config.sample_rate,
                channels = config.channels,
                format = ?supported.sample_format(),
                "starting loopback capture"
            );
            match supported.sample_format() {
                SampleFormat::F32 => open_capture::<f32>(&device, config, sink),
                SampleFormat::I16 => open_capture::<i16>(&device, config, sink),
                SampleFormat::I32 => open_capture::<i32>(&device, config, sink),
                other => Err(AudioError::Device(format!(
                    "unsupported loopback sample format {other:?}"
                ))),
            }
        })
    }
}

fn open_capture<T>(
    device: &cpal::Device,
    config: StreamConfig,
    sink: PcmSink,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let (rate, channels) = (config.sample_rate, config.channels);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let samples = data.iter().map(|s| s.to_sample::<f32>()).collect();
                // A full queue drops the chunk: the encoder is behind, never queue up delay.
                sink.send(PcmChunk {
                    rate,
                    channels,
                    samples,
                });
            },
            |e| tracing::warn!(error = %e, "loopback capture error"),
            Some(OPEN_TIMEOUT),
        )
        .map_err(dev_err)
}

/// The default output device (the viewer's speakers or headphones).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPlayback;

impl AudioPlayback for SystemPlayback {
    fn start(&self, source: Box<dyn PcmSource>) -> Result<DeviceHandle, AudioError> {
        run_device("audio-playback", move || {
            let host = cpal::default_host();
            let device = host
                .default_output_device()
                .ok_or(AudioError::NoDevice("output"))?;
            let supported = device.default_output_config().map_err(dev_err)?;
            let config = supported.config();
            tracing::info!(
                rate = config.sample_rate,
                channels = config.channels,
                format = ?supported.sample_format(),
                "starting audio playback"
            );
            match supported.sample_format() {
                SampleFormat::F32 => open_playback::<f32>(&device, config, source),
                SampleFormat::I16 => open_playback::<i16>(&device, config, source),
                SampleFormat::I32 => open_playback::<i32>(&device, config, source),
                other => Err(AudioError::Device(format!(
                    "unsupported output sample format {other:?}"
                ))),
            }
        })
    }
}

fn open_playback<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut source: Box<dyn PcmSource>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let (rate, channels) = (config.sample_rate, config.channels);
    let mut buf: Vec<f32> = Vec::new();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                buf.resize(data.len(), 0.0);
                source.fill(&mut buf, rate, channels);
                for (d, s) in data.iter_mut().zip(&buf) {
                    *d = T::from_sample(*s);
                }
            },
            |e| tracing::warn!(error = %e, "audio playback error"),
            Some(OPEN_TIMEOUT),
        )
        .map_err(dev_err)
}
