//! A scripted, timer-free [`Backend`] plus the fixture data the media scenes show.
//!
//! The in-app fake backend (`backend::fake`) runs on tokio timers (discovery every 5 s, an
//! incoming request after 6 s), which would make screenshots depend on wall-clock timing. Here
//! every command is a no-op and the scenes set the app state directly, so the same inputs
//! always render the same pixels. Visual data (monitors, the test pattern) still comes from
//! `backend::fake`, so the README shows what `TLP-Desk --fake` shows.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use tlp_desk_app::backend::{
    AttemptId, Backend, ConnectTarget, DeviceId, DiscoveredDevice, EventReceiver, EventSender,
    LocalInfo, PeerInfo, RequestId, SessionId,
};
use tlp_desk_app::i18n::Lang;
use tlp_desk_proto::InputEvent;

/// Backend whose commands do nothing; the scene scripts drive the UI state instead.
pub struct MediaBackend {
    local: LocalInfo,
    /// Kept so the app's event channel stays open (it is never written to).
    _events: EventSender,
}

impl MediaBackend {
    /// The backend for one language (this device's name is localized) and its event stream.
    pub fn start(lang: Lang) -> (Arc<dyn Backend>, EventReceiver) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let backend = Self {
            local: local_info(lang),
            _events: tx,
        };
        (Arc::new(backend), rx)
    }
}

impl Backend for MediaBackend {
    fn local_info(&self) -> LocalInfo {
        self.local.clone()
    }
    fn set_device_name(&self, _name: String) {}
    fn refresh_one_time_code(&self) {}
    fn connect(&self, _target: ConnectTarget, _code: Option<String>) -> AttemptId {
        AttemptId(1)
    }
    fn cancel_connect(&self, _attempt: AttemptId) {}
    fn forget_host_key(&self, _attempt: AttemptId) -> bool {
        false
    }
    fn respond_to_request(&self, _request: RequestId, _accept: bool) {}
    fn send_input(&self, _session: SessionId, _event: InputEvent) {}
    fn set_view_only(&self, _session: SessionId, _view_only: bool) {}
    fn disconnect(&self, _session: SessionId) {}
    fn select_monitor(&self, _session: SessionId, _index: u32) {}
}

/// `en` or `ar` text for fixture data, so Arabic shots show Arabic device names.
pub fn pick(lang: Lang, en: &'static str, ar: &'static str) -> String {
    match lang {
        Lang::Ar => ar.to_owned(),
        _ => en.to_owned(),
    }
}

fn ip(a: [u8; 4]) -> IpAddr {
    IpAddr::V4(Ipv4Addr::from(a))
}

/// This device as shown on the home screen.
pub fn local_info(lang: Lang) -> LocalInfo {
    LocalInfo {
        device_id: DeviceId(482_913_057),
        device_name: pick(lang, "DESKTOP-TLP01", "حاسوب-المنزل"),
        addresses: vec![ip([192, 168, 1, 23]), ip([10, 0, 0, 8])],
        one_time_code: "275931".into(),
    }
}

/// Devices "found" on the LAN: mixed Latin/Arabic names, one offline.
pub fn devices(lang: Lang) -> Vec<DiscoveredDevice> {
    let dev = |id, name: String, a, online| DiscoveredDevice {
        device_id: DeviceId(id),
        name,
        address: ip(a),
        online,
    };
    vec![
        dev(
            715_204_386,
            pick(lang, "Office-PC", "حاسوب-المكتب"),
            [192, 168, 1, 40],
            true,
        ),
        dev(306_117_942, "حاسوب-المكتبة".into(), [192, 168, 1, 57], true),
        dev(
            528_660_413,
            pick(lang, "Reception", "الاستقبال"),
            [192, 168, 1, 62],
            true,
        ),
        dev(950_442_118, "Lab-Laptop".into(), [192, 168, 1, 81], false),
    ]
}

/// The peer of the viewer session (the first device in [`devices`]).
pub fn session_peer(lang: Lang) -> PeerInfo {
    PeerInfo {
        device_id: DeviceId(715_204_386),
        name: pick(lang, "Office-PC", "حاسوب-المكتب"),
        address: ip([192, 168, 1, 40]),
    }
}

/// The peer that asks to connect to this device (incoming request, host banner).
pub fn incoming_peer(lang: Lang) -> PeerInfo {
    PeerInfo {
        device_id: DeviceId(950_442_118),
        name: pick(lang, "Lab-Laptop", "حاسوب-المختبر"),
        address: ip([192, 168, 1, 81]),
    }
}
