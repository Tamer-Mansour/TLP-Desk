//! Every user-facing CLI string, in one place (English for now; the i18n table comes with the
//! CLI `--lang` work in a later milestone).

use std::time::Duration;

use tlp_desk_crypto::{DeviceId, Fingerprint};
use tlp_desk_host::{CancelReason, ConnectedViewer, EndReason as HostEnd, IncomingRequest};
use tlp_desk_net::{AuthKind, DiscoveredDevice, PeerInfo};
use tlp_desk_proto::{Permissions, RejectReason};
use tlp_desk_viewer::{EndReason as ViewerEnd, MonitorDesc, ViewerStats};

pub const ABOUT: &str = "Headless TLP-Desk client for scripts and testing (LAN only)";
pub const WAITING_FOR_ACCEPT: &str = "Waiting for the host user to accept...";
pub const PROMPT_HINT: &str = "Type y + Enter to accept, anything else to reject.";
pub const STOPPING: &str = "Stopping.";
pub const NO_DEVICES: &str = "No devices found.";
pub const NO_FRAME: &str = "No frame was decoded; snapshot not written.";
pub const PASSWORD_REMOVED: &str = "Permanent password removed: password sign-in is off.";

pub fn password_set(path: &str) -> String {
    format!("Permanent password set (only its Argon2id verifier is stored, in {path}).")
}

pub fn password_too_short() -> String {
    format!(
        "The password is too short: use at least {} characters.",
        tlp_desk_crypto::MIN_PASSWORD_CHARS
    )
}

pub fn password_access(on: bool) -> &'static str {
    if on {
        "Password access: on (permanent password set)."
    } else {
        "Password access: off (no permanent password; see set-password)."
    }
}
pub fn identity(id: DeviceId, fp: &Fingerprint, dir: &str) -> String {
    format!(
        "Device ID:   {id}\nFingerprint: {}\nKey store:   {dir}",
        fp.to_hex()
    )
}

pub fn listening(id: DeviceId, addr: std::net::SocketAddr, name: &str) -> String {
    format!("Hosting as \"{name}\" - device ID {id}, listening on UDP {addr}")
}

pub fn code(code: &str) -> String {
    format!("One-time code: {code}")
}

pub fn code_disabled() -> &'static str {
    "One-time code access is off (use --code to enable). Viewers must be accepted."
}

pub fn auto_accept() -> &'static str {
    "WARNING: --auto-accept is on: every LAN viewer that asks is accepted without a prompt."
}

pub fn incoming(r: &IncomingRequest) -> String {
    format!(
        "\nIncoming connection: \"{}\" (ID {}) from {}",
        r.peer.name, r.peer.device_id, r.peer.addr
    )
}

pub fn accept_question() -> &'static str {
    "Accept? [y/N] "
}

pub fn auto_accepted(r: &IncomingRequest) -> String {
    format!("Auto-accepting {} ({})", r.peer.device_id, r.peer.addr)
}

pub fn cancelled(reason: CancelReason) -> String {
    match reason {
        CancelReason::ViewerLeft => "The viewer gave up before an answer.".into(),
        CancelReason::TimedOut => "No answer in time: rejected.".into(),
        CancelReason::HostShutdown => "Host stopping: request rejected.".into(),
    }
}

pub fn auth_failed(peer: &PeerInfo, kind: AuthKind, reason: RejectReason) -> String {
    format!(
        "Refused {} ({}) using {kind:?}: {reason:?}",
        peer.device_id, peer.addr
    )
}

pub fn connected_viewer(v: &ConnectedViewer) -> String {
    format!(
        "CONNECTED: \"{}\" (ID {}) from {} via {:?}, permissions {}",
        v.peer.name,
        v.peer.device_id,
        v.peer.addr,
        v.auth,
        permissions(v.permissions)
    )
}

pub fn viewer_left(peer: &PeerInfo, reason: &HostEnd) -> String {
    format!(
        "DISCONNECTED: {} ({}) - {reason:?}",
        peer.device_id, peer.addr
    )
}

pub fn code_changed(code: &str) -> String {
    format!("New one-time code: {code}")
}

pub fn permissions(p: Permissions) -> String {
    let names = [
        (Permissions::VIEW, "view"),
        (Permissions::CONTROL, "mouse"),
        (Permissions::KEYBOARD, "keyboard"),
        (Permissions::CLIPBOARD, "clipboard"),
        (Permissions::FILES, "files"),
        (Permissions::AUDIO, "audio"),
        (Permissions::RESTART, "restart"),
        (Permissions::PRIVACY_MODE, "privacy"),
        (Permissions::CHAT, "chat"),
        (Permissions::TUNNEL, "tunnel"),
        (Permissions::WHITEBOARD, "whiteboard"),
        (Permissions::SYSTEM, "system"),
    ];
    let v: Vec<&str> = names
        .iter()
        .filter(|(f, _)| p.contains(*f))
        .map(|(_, n)| *n)
        .collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join("+")
    }
}

pub fn device_row(d: &DiscoveredDevice) -> String {
    let addrs: Vec<String> = d.addrs.iter().map(|a| a.to_string()).collect();
    let via = match (d.via_mdns, d.via_broadcast) {
        (true, true) => "mdns+broadcast",
        (true, false) => "mdns",
        _ => "broadcast",
    };
    format!(
        "{}  {:<24}  {}  (v{}, {via})",
        d.device_id,
        d.name,
        addrs.join(", "),
        d.protocol_version
    )
}

pub fn searching(secs: u64) -> String {
    format!("Searching the LAN for {secs} s...")
}

pub fn connecting(target: &str) -> String {
    format!("Connecting to {target}...")
}

pub fn connected_host(peer: &PeerInfo, perms: Permissions) -> String {
    format!(
        "Connected to \"{}\" (ID {}) at {}, permissions {}",
        peer.name,
        peer.device_id,
        peer.addr,
        permissions(perms)
    )
}

pub fn stats_line(t: Duration, s: &ViewerStats) -> String {
    format!(
        "[{:>3}s] {:>5.1} fps  {:>8.0} kbps  rtt {:>5.1} ms  latency ~{:>5.1} ms  decode {:>5.1} ms  {}x{}  frames {}/{}",
        t.as_secs(),
        s.fps,
        s.bitrate_kbps,
        s.rtt_ms,
        s.latency_ms,
        s.decode_ms,
        s.width,
        s.height,
        s.frames_decoded,
        s.frames_received
    )
}

pub fn session_ended(r: &ViewerEnd) -> String {
    format!("Session ended: {r:?}")
}

pub fn snapshot_saved(path: &str, w: u32, h: u32) -> String {
    format!("Snapshot {w}x{h} saved to {path}")
}

pub fn key_changed_help() -> &'static str {
    "\n!!! THE HOST KEY HAS CHANGED !!!\n\
     Someone may be impersonating this host (man-in-the-middle), or it was reinstalled.\n\
     If you are sure the host was reinstalled, run again with --forget-key."
}

pub fn summary(s: &ViewerStats, secs: f32) -> String {
    format!(
        "Summary: {} frames decoded in {:.1} s ({:.1} fps avg)",
        s.frames_decoded,
        secs,
        s.frames_decoded as f32 / secs.max(0.001)
    )
}

pub fn host_priority(local_active: bool) -> &'static str {
    if local_active {
        "The host user is using the mouse/keyboard; your input is paused."
    } else {
        "The host user is idle again; your input is active."
    }
}

pub fn monitors(list: &[MonitorDesc], active: u32) -> String {
    let mut out = String::from("Host monitors:");
    for m in list {
        out.push_str(&format!(
            "\n  {} {}: {}x{} at ({}, {}){}{}",
            if m.index == active { "*" } else { " " },
            m.index,
            m.width,
            m.height,
            m.x,
            m.y,
            if m.primary { " primary" } else { "" },
            if m.name.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", m.name)
            }
        ));
    }
    out
}

pub fn selecting_monitor(index: u32) -> String {
    format!("Switching to monitor {index}...")
}
