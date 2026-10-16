//! Device-info glue for the backends: which features to register, how a viewer
//! `FeatureEvent` becomes [`BackendEvent::DeviceInfo`], and the fake backend's sample answer.

use std::sync::Arc;

use tlp_desk_host::{DeviceInfoFeature, FeatureSet as HostFeatureSet};
use tlp_desk_viewer::{DeviceInfoClient, FeatureEvent, FeatureSet as ViewerFeatureSet};

use super::{BackendEvent, DeviceInfo, DiskInfo, SessionId};

/// `set` plus the host-side feature that answers device-info requests from the real machine.
pub fn with_host_feature(set: HostFeatureSet) -> HostFeatureSet {
    set.with(Arc::new(DeviceInfoFeature::system()))
}

/// `set` plus the viewer-side feature that receives device-info answers.
pub fn with_viewer_feature(set: ViewerFeatureSet) -> ViewerFeatureSet {
    set.with(Arc::new(DeviceInfoClient))
}

/// The UI event for a viewer feature event, if it is a device-info answer.
pub fn map_viewer_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    if event.feature != tlp_desk_viewer::DEVICE_INFO_FEATURE {
        return None;
    }
    let info = event.payload::<DeviceInfo>()?.clone();
    Some(BackendEvent::DeviceInfo { session, info })
}

/// What the fake backend's simulated remote PC reports (Arabic host name and disk label).
pub fn fake_info() -> DeviceInfo {
    DeviceInfo {
        os: "Windows 11 Pro".into(),
        version: "10.0.26100".into(),
        cpu: "Intel(R) Core(TM) i7-12700 @ 2.10GHz".into(),
        cores: 20,
        ram_bytes: 32 * 1024 * 1024 * 1024,
        disks: vec![
            DiskInfo {
                mount: "C:\\".into(),
                label: "النظام".into(),
                total_bytes: 511 * 1024 * 1024 * 1024,
                free_bytes: 187 * 1024 * 1024 * 1024,
            },
            DiskInfo {
                mount: "D:\\".into(),
                label: "Data".into(),
                total_bytes: 2 * 1024 * 1024 * 1024 * 1024,
                free_bytes: 1_400 * 1024 * 1024 * 1024,
            },
        ],
        uptime_s: 3 * 86_400 + 5 * 3600 + 42 * 60,
        hostname: "حاسوب-المحاسبة".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_device_info_events_are_mapped() {
        let s = SessionId(7);
        let ev = FeatureEvent::new(tlp_desk_viewer::DEVICE_INFO_FEATURE, 7, fake_info());
        match map_viewer_event(s, &ev) {
            Some(BackendEvent::DeviceInfo { session, info }) => {
                assert_eq!(session, s);
                assert_eq!(info.hostname, "حاسوب-المحاسبة");
            }
            other => panic!("unexpected {other:?}"),
        }
        let chat = FeatureEvent::new("chat", 7, String::from("مرحبا"));
        assert!(map_viewer_event(s, &chat).is_none());
        // Right name, wrong payload type: ignored, never a panic.
        let bad = FeatureEvent::new(tlp_desk_viewer::DEVICE_INFO_FEATURE, 7, 5u32);
        assert!(map_viewer_event(s, &bad).is_none());
    }

    #[test]
    fn registers_one_feature_each() {
        assert_eq!(
            with_host_feature(HostFeatureSet::new()).names(),
            [tlp_desk_host::DEVICE_INFO_FEATURE]
        );
        assert_eq!(
            with_viewer_feature(ViewerFeatureSet::new()).names(),
            [tlp_desk_viewer::DEVICE_INFO_FEATURE]
        );
    }
}
