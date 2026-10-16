//! Device-info feature (host side): answers `DeviceInfoRequest` with `DeviceInfo` (OS, CPU,
//! RAM, disks, uptime, host name).
//!
//! **Permission:** `VIEW`. Anyone allowed to see the screen can already read the OS, the
//! host name and most of this from the desktop, so a separate permission would add a toggle
//! without adding protection. The host core checks `VIEW` before routing the request here and
//! [`DeviceInfoFeature`] checks it again when answering, so a live revoke wins.
//!
//! **Cost:** collecting is done on a blocking thread and the result is cached for
//! [`DeviceInfoFeature::with_min_interval`] (default 2 s), so a viewer spamming requests
//! cannot make the host burn CPU; every request still gets an answer.
//!
//! The data comes from a [`DeviceInfoSource`]; [`SystemInfoSource`] reads the real machine
//! (through the `sysinfo` crate, no `unsafe` here), tests plug in a fake.

mod system;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{DeviceInfo, DiskInfo, Message, Permissions};
use tokio::sync::mpsc;

pub use system::SystemInfoSource;

/// [`Feature::name`] of the device-info feature (host and viewer side).
pub const DEVICE_INFO_FEATURE: &str = "device_info";

/// Longest string sent in any field, in characters.
pub const MAX_TEXT_CHARS: usize = 256;
/// Most disks listed.
pub const MAX_DISKS: usize = 64;

/// Where system information comes from.
pub trait DeviceInfoSource: Send + Sync + 'static {
    /// Collects a fresh snapshot. May block for a few milliseconds (called off the runtime).
    fn collect(&self) -> DeviceInfo;
}

/// Reported to the host UI each time device info was sent to a viewer
/// (payload of a [`tlp_desk_net::FeatureEvent`] named [`DEVICE_INFO_FEATURE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfoServed;

/// The host-side feature. Register it in `HostConfig::features`.
pub struct DeviceInfoFeature {
    source: Arc<dyn DeviceInfoSource>,
    min_interval: Duration,
    cache: Arc<Mutex<Option<(Instant, DeviceInfo)>>>,
}

impl DeviceInfoFeature {
    /// A feature answering from `source`.
    pub fn new(source: Arc<dyn DeviceInfoSource>) -> Self {
        Self {
            source,
            min_interval: Duration::from_secs(2),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// The feature reading the real machine.
    pub fn system() -> Self {
        Self::new(Arc::new(SystemInfoSource))
    }

    /// Re-collect at most once per `interval`; answers in between come from the cache.
    #[must_use]
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }
}

impl std::fmt::Debug for DeviceInfoFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceInfoFeature")
            .field("min_interval", &self.min_interval)
            .finish_non_exhaustive()
    }
}

impl Feature for DeviceInfoFeature {
    fn name(&self) -> &'static str {
        DEVICE_INFO_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::DeviceInfoRequest)
    }

    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        let source = self.source.clone();
        let cache = self.cache.clone();
        let min_interval = self.min_interval;
        tokio::spawn(async move {
            while let Some(msg) = inbox.recv().await {
                if !matches!(msg, Message::DeviceInfoRequest) {
                    continue;
                }
                if !ctx.allowed(Permissions::VIEW) {
                    tracing::debug!(session = ctx.session_key, "device info refused: no VIEW");
                    continue;
                }
                let info = match cached(&cache, min_interval) {
                    Some(info) => info,
                    None => {
                        let src = source.clone();
                        let Ok(fresh) = tokio::task::spawn_blocking(move || src.collect()).await
                        else {
                            tracing::warn!("device info collection failed");
                            continue;
                        };
                        let fresh = limit(fresh);
                        *cache.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some((Instant::now(), fresh.clone()));
                        fresh
                    }
                };
                // Re-check: the permission may have been revoked while collecting.
                if !ctx.allowed(Permissions::VIEW) {
                    continue;
                }
                if ctx.session.send(&Message::DeviceInfo(info)).await.is_err() {
                    break;
                }
                tracing::info!(session = ctx.session_key, "device info sent");
                ctx.emit(DEVICE_INFO_FEATURE, DeviceInfoServed);
            }
        });
    }
}

fn cached(
    cache: &Mutex<Option<(Instant, DeviceInfo)>>,
    min_interval: Duration,
) -> Option<DeviceInfo> {
    let guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
    match &*guard {
        Some((at, info)) if at.elapsed() < min_interval => Some(info.clone()),
        _ => None,
    }
}

/// Caps every string at [`MAX_TEXT_CHARS`] characters and the disk list at [`MAX_DISKS`], so
/// the message always fits a control frame whatever the machine reports.
pub fn limit(mut info: DeviceInfo) -> DeviceInfo {
    for s in [
        &mut info.os,
        &mut info.version,
        &mut info.cpu,
        &mut info.hostname,
    ] {
        truncate_chars(s, MAX_TEXT_CHARS);
    }
    info.disks.truncate(MAX_DISKS);
    for DiskInfo { mount, label, .. } in &mut info.disks {
        truncate_chars(mount, MAX_TEXT_CHARS);
        truncate_chars(label, MAX_TEXT_CHARS);
    }
    info
}

/// Truncates `s` to at most `max` characters, on a character boundary.
fn truncate_chars(s: &mut String, max: usize) {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> DeviceInfo {
        DeviceInfo {
            os: "Windows 11 Pro".into(),
            version: "10.0.26100".into(),
            cpu: "Test CPU".into(),
            cores: 8,
            ram_bytes: 16 << 30,
            disks: vec![DiskInfo {
                mount: "C:\\".into(),
                label: "النظام".into(),
                total_bytes: 512 << 30,
                free_bytes: 100 << 30,
            }],
            uptime_s: 3600,
            hostname: "حاسوب-المكتب".into(),
        }
    }

    #[test]
    fn limit_keeps_normal_info_unchanged() {
        assert_eq!(limit(info()), info());
    }

    #[test]
    fn limit_caps_arabic_text_on_char_boundaries_and_disk_count() {
        let mut i = info();
        i.hostname = "ب".repeat(1000);
        i.disks = vec![i.disks[0].clone(); 500];
        i.disks[0].label = "م".repeat(300);
        let l = limit(i);
        assert_eq!(l.hostname.chars().count(), MAX_TEXT_CHARS);
        assert!(l.hostname.chars().all(|c| c == 'ب'));
        assert_eq!(l.disks.len(), MAX_DISKS);
        assert_eq!(l.disks[0].label.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn handles_only_the_request() {
        let f = DeviceInfoFeature::new(Arc::new(SystemInfoSource));
        assert!(f.handles(&Message::DeviceInfoRequest));
        assert!(!f.handles(&Message::DeviceInfo(info())));
        assert_eq!(f.name(), DEVICE_INFO_FEATURE);
    }
}
