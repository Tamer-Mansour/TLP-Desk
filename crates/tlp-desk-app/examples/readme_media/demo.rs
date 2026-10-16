//! The scripted demo video: home → type an ID and code → connecting → session with a moving
//! remote pointer and live stats → switch to the second monitor → disconnect.
//!
//! Every frame is a pure function of the frame number (simulated clock, no randomness), so
//! re-running produces the same PNG sequence. `scripts/update-readme-media.ps1` encodes it
//! with ffmpeg.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use tlp_desk_app::app::{App, Pending};
use tlp_desk_app::backend::fake::{fake_monitors, test_pattern_frame};
use tlp_desk_app::backend::{
    AttemptId, ConnectTarget, DeviceId, FrameSlot, SessionId, SessionStats,
};
use tlp_desk_app::i18n::Lang;
use tlp_desk_app::ui::session::ViewerSession;
use tlp_desk_proto::Permissions;

use crate::backend::{devices, session_peer};
use crate::render::{self, Look};

/// Frames per second of the demo.
pub const FPS: u32 = 30;

/// Timeline marks, seconds.
const DEVICES_FOUND: f32 = 0.6;
const TYPE_ID: f32 = 1.2;
const TYPE_CODE: f32 = 2.6;
const CONNECT: f32 = 3.5;
const SESSION: f32 = 4.4;
const SWITCH_MONITOR: f32 = 7.6;
const DISCONNECT: f32 = 10.0;
const END: f32 = 11.0;

const ID: &str = "715 204 386";
const CODE: &str = "275 931";

/// The first `n` characters of `s`, `n` growing at `rate` characters per second from `start`.
fn typed(s: &str, t: f32, start: f32, rate: f32) -> String {
    let n = ((t - start).max(0.0) * rate) as usize;
    s.chars().take(n).collect()
}

/// Remote pointer on a smooth figure-eight, normalized to `0..=u16::MAX`.
fn pointer(t: f32) -> (u16, u16) {
    let x = 0.5 + 0.32 * (t * 1.3).sin();
    let y = 0.5 + 0.22 * (t * 2.6).sin();
    let n = |v: f32| (v.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
    (n(x), n(y))
}

/// Stats as the backend would report them once a second; varied but fixed.
fn stats(second: u32) -> SessionStats {
    SessionStats {
        fps: if second == 0 { 58.0 } else { 60.0 },
        latency_ms: 14.0 + (second * 7 % 9) as f32,
        latency_synced: true,
        bitrate_kbps: 11_000 + second * 1_337 % 3_000,
    }
}

/// The session view and its frame mailbox while the demo is "connected".
struct Live {
    frames: std::sync::Arc<FrameSlot>,
    monitor: u32,
}

/// Apply the timeline at time `t` to the app.
fn apply(app: &mut App, lang: Lang, t: f32, live: &mut Option<Live>) {
    app.devices = (t >= DEVICES_FOUND).then(|| devices(lang));
    if t < CONNECT {
        app.address = typed(ID, t, TYPE_ID, 9.0);
        app.code = typed(CODE, t, TYPE_CODE, 9.0);
        return;
    }
    if t < SESSION {
        app.pending.get_or_insert_with(|| Pending {
            attempt: AttemptId(1),
            target: ID.into(),
            awaiting_accept: false,
            dial: ConnectTarget::Id(DeviceId(715_204_386)),
            code: Some(CODE.replace(' ', "")),
        });
        return;
    }
    if t >= DISCONNECT {
        app.session = None;
        *live = None;
        app.address.clear();
        app.code.clear();
        return;
    }
    app.pending = None;
    let l = live.get_or_insert_with(|| {
        let frames = FrameSlot::new();
        let mut view = ViewerSession::new(
            SessionId(1),
            session_peer(lang),
            Permissions::ALL,
            frames.clone(),
        );
        view.set_monitors(fake_monitors(), 0);
        app.session = Some(view);
        Live { frames, monitor: 0 }
    });
    let monitor = u32::from(t >= SWITCH_MONITOR);
    let since = t - SESSION;
    if let Some(view) = app.session.as_mut() {
        if monitor != l.monitor {
            view.set_monitors(fake_monitors(), monitor);
            l.monitor = monitor;
        }
        let second = since as u32;
        view.stats = (second >= 1).then(|| stats(second));
        view.set_fixed_elapsed(Some(Duration::from_secs(u64::from(second))));
    }
    l.frames
        .publish(test_pattern_frame(monitor, since, pointer(since)));
}

/// Render the demo for `lang` into `dir/f00000.png`, `f00001.png`, ... Returns the frame count.
pub fn render(lang: Lang, dir: &Path) -> anyhow::Result<u32> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let look = Look {
        lang,
        dark: true,
        pixels_per_point: 1.0,
        step_dt: 1.0 / FPS as f32,
    };
    let mut h = render::harness(look);
    let mut live = None;
    let total = (END * FPS as f32) as u32;
    for i in 0..total {
        let t = i as f32 / FPS as f32;
        apply(h.state_mut(), lang, t, &mut live);
        h.step();
        let img = render::capture(&mut h)?;
        let png = render::encode_png(&img, false)?;
        render::write_if_changed(&dir.join(format!("f{i:05}.png")), &png)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_grows_and_stops_at_full_text() {
        assert_eq!(typed(ID, 0.0, TYPE_ID, 9.0), "");
        assert_eq!(typed(ID, TYPE_ID + 0.25, TYPE_ID, 9.0), "71");
        assert_eq!(typed(ID, 99.0, TYPE_ID, 9.0), ID);
    }
}
