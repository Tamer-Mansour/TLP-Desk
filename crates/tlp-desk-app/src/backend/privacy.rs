//! Privacy mode glue for the real backend: the text of the host's privacy screen and the
//! mapping of the privacy features' events to [`BackendEvent`]s.

use std::sync::Arc;

use tlp_desk_host::{PrivacyStatus, PrivacyTextFn, PrivacyViewer, PRIVACY_FEATURE};
use tlp_desk_input::{PrivacyScreenText, ScreenLine};
use tlp_desk_net::FeatureEvent;
use tlp_desk_viewer::PrivacyState;

use super::{BackendEvent, SessionId};
use crate::i18n::{tr_raw, Lang, S};

/// The host's privacy screen: title, every connected viewer and the local way out, in Arabic
/// and English. Whoever sits at the host may not read the app's UI language, and the OS
/// renders (shapes, orders) this text itself, so raw logical-order strings are used.
pub fn screen_text(viewers: &[PrivacyViewer]) -> PrivacyScreenText {
    let mut lines = Vec::new();
    for lang in [Lang::Ar, Lang::En] {
        lines.push(ScreenLine::title(tr_raw(lang, S::PrivacyScreenTitle)));
    }
    for v in viewers {
        for lang in [Lang::Ar, Lang::En] {
            let line = tr_raw(lang, S::PrivacyScreenViewer)
                .replace("{name}", &v.name)
                .replace("{id}", &v.device_id);
            lines.push(ScreenLine::body(line));
        }
    }
    for lang in [Lang::Ar, Lang::En] {
        lines.push(ScreenLine::body(tr_raw(lang, S::PrivacyScreenExit)));
    }
    PrivacyScreenText { lines }
}

/// [`screen_text`] as the host feature's text callback.
pub fn text_fn() -> PrivacyTextFn {
    Arc::new(|viewers: &[PrivacyViewer]| screen_text(viewers))
}

/// Host side: a privacy feature event as a backend event.
pub fn host_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    if event.feature != PRIVACY_FEATURE {
        return None;
    }
    let status = event.payload::<PrivacyStatus>()?;
    Some(BackendEvent::HostPrivacyChanged {
        session,
        active: status.active,
    })
}

/// Viewer side: a privacy feature event as a backend event.
pub fn viewer_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    if event.feature != PRIVACY_FEATURE {
        return None;
    }
    let state = event.payload::<PrivacyState>()?;
    Some(BackendEvent::PrivacyChanged {
        session,
        enabled: state.enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_host::PrivacyCause;

    #[test]
    fn screen_text_is_bilingual_and_lists_every_viewer() {
        let viewers = [
            PrivacyViewer {
                name: "حاسوب-المكتبة".into(),
                device_id: "306 117 942".into(),
            },
            PrivacyViewer {
                name: "Office-PC".into(),
                device_id: "715 204 386".into(),
            },
        ];
        let t = screen_text(&viewers);
        let titles: Vec<_> = t.lines.iter().filter(|l| l.large).collect();
        assert_eq!(titles.len(), 2);
        assert!(titles[0].is_rtl() && !titles[1].is_rtl());
        let all: Vec<&str> = t.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            all.contains(&"متصل: حاسوب-المكتبة (306 117 942)"),
            "{all:?}"
        );
        assert!(
            all.contains(&"Connected: Office-PC (715 204 386)"),
            "{all:?}"
        );
        assert!(all.iter().any(|l| l.contains("Ctrl+Alt+Shift+P")));
        assert!(all.iter().all(|l| !l.contains('{')));
        // Everything fits the shield's limits, so nothing is cut off.
        assert_eq!(t.sanitized(), t);
    }

    #[test]
    fn feature_events_map_to_backend_events() {
        let s = SessionId(4);
        let host = FeatureEvent::new(
            PRIVACY_FEATURE,
            1,
            PrivacyStatus {
                active: true,
                cause: PrivacyCause::Requested,
            },
        );
        assert!(matches!(
            host_event(s, &host),
            Some(BackendEvent::HostPrivacyChanged { session, active: true }) if session == s
        ));
        let viewer = FeatureEvent::new(PRIVACY_FEATURE, 1, PrivacyState { enabled: false });
        assert!(matches!(
            viewer_event(s, &viewer),
            Some(BackendEvent::PrivacyChanged { enabled: false, .. })
        ));
        // Other features and wrong payloads are not ours.
        assert!(viewer_event(s, &FeatureEvent::new("chat", 1, 5u32)).is_none());
        assert!(host_event(s, &FeatureEvent::new(PRIVACY_FEATURE, 1, 5u32)).is_none());
    }
}
