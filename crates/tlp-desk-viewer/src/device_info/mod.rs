//! Device-info feature (viewer side): sends `DeviceInfoRequest` and reports each
//! `DeviceInfo` answer to the UI.
//!
//! Register [`DeviceInfoClient`] in `ViewerConfig::features`, call [`request_device_info`],
//! and match `ViewerEvent::Feature(ev)` with `ev.feature == DEVICE_INFO_FEATURE`;
//! `ev.payload::<DeviceInfo>()` is the answer. The host needs `VIEW` to answer.
//!
//! The answer comes from the host and is treated as hostile: [`sanitize`] caps every string
//! and the disk list and fixes `free > total` before the UI sees it.

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{DeviceInfo, Message};
use tokio::sync::mpsc;

use crate::ViewerSession;

/// [`Feature::name`] of the device-info feature (same on the host).
pub const DEVICE_INFO_FEATURE: &str = "device_info";

/// Longest string accepted in any field, in characters.
const MAX_TEXT_CHARS: usize = 256;
/// Most disks accepted.
const MAX_DISKS: usize = 64;

/// The viewer-side feature: forwards `DeviceInfo` answers to the UI.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceInfoClient;

impl Feature for DeviceInfoClient {
    fn name(&self) -> &'static str {
        DEVICE_INFO_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::DeviceInfo(_))
    }

    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(async move {
            while let Some(msg) = inbox.recv().await {
                if let Message::DeviceInfo(info) = msg {
                    ctx.emit(DEVICE_INFO_FEATURE, sanitize(info));
                }
            }
        });
    }
}

/// Asks the host for its device info. `false` if the session already ended.
pub fn request_device_info(session: &ViewerSession) -> bool {
    session.send_message(Message::DeviceInfoRequest)
}

/// Caps strings at 256 characters and the disk list at 64, and clamps each disk's free space
/// to its total, so a misbehaving host cannot flood or confuse the UI.
pub fn sanitize(mut info: DeviceInfo) -> DeviceInfo {
    for s in [
        &mut info.os,
        &mut info.version,
        &mut info.cpu,
        &mut info.hostname,
    ] {
        truncate_chars(s, MAX_TEXT_CHARS);
    }
    info.disks.truncate(MAX_DISKS);
    for d in &mut info.disks {
        truncate_chars(&mut d.mount, MAX_TEXT_CHARS);
        truncate_chars(&mut d.label, MAX_TEXT_CHARS);
        d.free_bytes = d.free_bytes.min(d.total_bytes);
    }
    info
}

fn truncate_chars(s: &mut String, max: usize) {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_proto::DiskInfo;

    #[test]
    fn sanitize_caps_hostile_answers() {
        let info = DeviceInfo {
            os: "و".repeat(10_000),
            version: String::new(),
            cpu: "x".into(),
            cores: 0,
            ram_bytes: u64::MAX,
            disks: vec![
                DiskInfo {
                    mount: "د:\\".into(),
                    label: "قرص".into(),
                    total_bytes: 10,
                    free_bytes: 99,
                };
                1000
            ],
            uptime_s: u64::MAX,
            hostname: "مكتب".into(),
        };
        let s = sanitize(info);
        assert_eq!(s.os.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(s.disks.len(), MAX_DISKS);
        assert_eq!(s.disks[0].free_bytes, 10);
        assert_eq!(s.disks[0].label, "قرص");
        assert_eq!(s.hostname, "مكتب");
    }

    #[test]
    fn handles_only_answers() {
        assert!(!DeviceInfoClient.handles(&Message::DeviceInfoRequest));
    }
}
