//! Viewer side: asks for audio, reads the host's audio streams and plays them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tlp_desk_net::{Feature, FeatureCtx, RecvStream, Session, StreamKind, STREAM_REFUSED};
use tlp_desk_proto::{Message, Permissions};
use tokio::sync::mpsc;

use super::wire::read_packet;
use super::{control, AudioEvent, Player, ViewerAudioState, FEATURE_NAME};
use crate::device::{AudioPlayback, DeviceHandle};
use crate::jitter::JitterConfig;

/// A user request for one session.
#[derive(Debug, Clone, Copy)]
enum Command {
    Enable(bool),
    Volume(f32),
}

#[derive(Debug, Clone, Copy)]
struct Defaults {
    enabled: bool,
    volume: f32,
}

type Sessions = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Command>>>>;

/// Plays the host's system audio on this viewer. Register one instance in
/// `ViewerConfig::features`; control each session through its `feature_key`.
pub struct AudioViewer {
    playback: Arc<dyn AudioPlayback>,
    jitter: JitterConfig,
    defaults: Mutex<Defaults>,
    sessions: Sessions,
}

impl std::fmt::Debug for AudioViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioViewer")
            .field("jitter", &self.jitter)
            .finish_non_exhaustive()
    }
}

impl AudioViewer {
    /// A viewer feature playing through `playback` (use [`crate::SystemPlayback`] for the
    /// real speakers). New sessions request audio (`enabled_by_default`) at full volume.
    pub fn new(playback: Arc<dyn AudioPlayback>, enabled_by_default: bool) -> Self {
        Self {
            playback,
            jitter: JitterConfig::default(),
            defaults: Mutex::new(Defaults {
                enabled: enabled_by_default,
                volume: 1.0,
            }),
            sessions: Arc::default(),
        }
    }

    /// Uses a different jitter buffer tuning for new sessions.
    #[must_use]
    pub fn with_jitter(mut self, jitter: JitterConfig) -> Self {
        self.jitter = jitter;
        self
    }

    fn defaults(&self) -> std::sync::MutexGuard<'_, Defaults> {
        self.defaults.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether sessions started from now on ask for audio right away (the app setting).
    pub fn set_enabled_by_default(&self, enabled: bool) {
        self.defaults().enabled = enabled;
    }

    /// Volume for sessions started from now on, `0.0..=1.0`.
    pub fn set_default_volume(&self, volume: f32) {
        if volume.is_finite() {
            self.defaults().volume = volume.clamp(0.0, 1.0);
        }
    }

    fn command(&self, session_key: u64, cmd: Command) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session_key)
            .is_some_and(|tx| tx.send(cmd).is_ok())
    }

    /// Turns audio on or off for a running session (toolbar toggle). `false` if the session
    /// is unknown or already ended.
    pub fn set_enabled(&self, session_key: u64, enabled: bool) -> bool {
        self.command(session_key, Command::Enable(enabled))
    }

    /// Sets a running session's playback volume, `0.0..=1.0`.
    pub fn set_volume(&self, session_key: u64, volume: f32) -> bool {
        self.command(session_key, Command::Volume(volume))
    }
}

impl Feature for AudioViewer {
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
        let (tx, rx) = mpsc::unbounded_channel();
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ctx.session_key, tx);
        let task = ViewerTask {
            playback: self.playback.clone(),
            jitter: self.jitter,
            defaults: *self.defaults(),
            sessions: self.sessions.clone(),
        };
        rt.spawn(task.run(ctx, inbox, rx));
    }
}

struct ViewerTask {
    playback: Arc<dyn AudioPlayback>,
    jitter: JitterConfig,
    defaults: Defaults,
    sessions: Sessions,
}

impl ViewerTask {
    async fn run(
        self,
        ctx: FeatureCtx,
        mut inbox: mpsc::UnboundedReceiver<Message>,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        let key = ctx.session_key;
        match Player::new(self.jitter) {
            Ok(player) => self.serve(&ctx, &mut inbox, &mut commands, player).await,
            Err(e) => tracing::warn!(error = %e, "audio decoder unavailable; audio disabled"),
        }
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
    }

    async fn serve(
        &self,
        ctx: &FeatureCtx,
        inbox: &mut mpsc::UnboundedReceiver<Message>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        player: Player,
    ) {
        player.set_volume(self.defaults.volume);
        let accepting = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(accept_streams(
            ctx.session.clone(),
            player.clone(),
            accepting.clone(),
        ));
        let mut perms = ctx.permissions.clone();
        let mut permitted = ctx.allowed(Permissions::AUDIO);
        let mut wanted = self.defaults.enabled;
        let mut host_on = false;
        let mut output: Option<DeviceHandle> = None;
        let mut last: Option<ViewerAudioState> = None;
        if wanted && permitted {
            send(&ctx.session, true).await;
        }
        loop {
            let state = ViewerAudioState {
                permitted,
                enabled: wanted,
                playing: output.is_some(),
                volume: player.volume(),
            };
            if last != Some(state) {
                ctx.emit(FEATURE_NAME, AudioEvent::Viewer(state));
                last = Some(state);
            }
            tokio::select! {
                msg = inbox.recv() => match msg {
                    None => break,
                    Some(Message::AudioControl { enabled, .. }) => host_on = enabled,
                    Some(_) => continue,
                },
                cmd = commands.recv() => match cmd {
                    None => break,
                    Some(Command::Enable(on)) => {
                        if on != wanted {
                            wanted = on;
                            if permitted {
                                send(&ctx.session, on).await;
                            }
                        }
                    }
                    Some(Command::Volume(v)) => player.set_volume(v),
                },
                changed = perms.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let now = ctx.allowed(Permissions::AUDIO);
                    if now && !permitted && wanted {
                        // Our earlier request was blocked without AUDIO: ask again.
                        send(&ctx.session, true).await;
                    }
                    if !now {
                        host_on = false;
                    }
                    permitted = now;
                }
            }
            let receive = wanted && permitted;
            accepting.store(receive, Ordering::Release);
            let play = receive && host_on;
            if play && output.is_none() {
                player.clear();
                match self.playback.start(player.source()) {
                    Ok(h) => output = Some(h),
                    Err(e) => {
                        tracing::warn!(error = %e, "audio output failed to start");
                        ctx.emit(FEATURE_NAME, AudioEvent::DeviceError(e.to_string()));
                        // Do not retry on every message; the user can toggle audio again.
                        wanted = false;
                        send(&ctx.session, false).await;
                    }
                }
            } else if !play && output.is_some() {
                output = None;
                player.clear();
            }
        }
        accepting.store(false, Ordering::Release);
        reader.abort();
    }
}

async fn send(session: &Session, enabled: bool) {
    if let Err(e) = session.send(&control(enabled)).await {
        tracing::debug!(error = %e, "could not send AudioControl");
    }
}

/// Accepts the host's audio streams; each one is read by its own task.
async fn accept_streams(session: Arc<Session>, player: Player, accepting: Arc<AtomicBool>) {
    while let Ok(mut s) = session.accept_side_stream(StreamKind::Audio).await {
        if s.send.is_some() || !accepting.load(Ordering::Acquire) {
            // Not wanted, not permitted, or not the uni stream the protocol defines.
            let _ = s.recv.stop(STREAM_REFUSED.into());
            continue;
        }
        tokio::spawn(read_stream(s.id, s.recv, player.clone(), accepting.clone()));
    }
}

async fn read_stream(
    generation: u64,
    mut recv: RecvStream,
    player: Player,
    accepting: Arc<AtomicBool>,
) {
    let mut seq: u64 = 0;
    let mut buf = Vec::new();
    loop {
        match read_packet(&mut recv, &mut buf).await {
            Ok(true) => {
                if !accepting.load(Ordering::Acquire) {
                    let _ = recv.stop(STREAM_REFUSED.into());
                    return;
                }
                player.push(generation, seq, std::mem::take(&mut buf));
                seq = seq.saturating_add(1);
            }
            Ok(false) => return,
            Err(e) => {
                tracing::debug!(error = %e, generation, "bad audio stream; dropped");
                let _ = recv.stop(STREAM_REFUSED.into());
                return;
            }
        }
    }
}
