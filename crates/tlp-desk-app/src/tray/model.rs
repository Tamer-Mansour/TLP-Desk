//! What the tray shows, independent of the OS tray: menu entries, labels and tooltip. Tested
//! without a real tray icon.

use crate::i18n::{tr_raw, Lang, S};

/// Logical-order template text with `{name}` placeholders filled in (for text the OS renders
/// itself, so no egui shaping).
fn tr_fmt_raw(lang: Lang, key: S, args: &[(&str, &str)]) -> String {
    let mut s = tr_raw(lang, key).to_owned();
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

/// Something the user picked in the tray menu (or by clicking the icon).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrayAction {
    /// Show and focus the main window.
    Open,
    /// Copy this device's ID (Western digits) to the clipboard.
    CopyId,
    /// Rotate the one-time code.
    NewCode,
    /// Incoming connections on/off ("pause hosting").
    ToggleIncoming,
    /// Open the main window on the Settings screen.
    Settings,
    /// Open the main window on the connection log.
    ConnectionLog,
    /// End every session and exit.
    Quit,
}

/// One menu row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEntry {
    /// A clickable item; `checked` is `Some` for check items.
    Item {
        action: TrayAction,
        /// Logical-order text (the OS shapes and lays out Arabic itself).
        label: String,
        checked: Option<bool>,
    },
    Separator,
}

/// The menu in display order.
pub fn menu(lang: Lang, incoming_enabled: bool) -> Vec<TrayEntry> {
    let item = |action, key| TrayEntry::Item {
        action,
        label: tr_raw(lang, key).to_owned(),
        checked: None,
    };
    vec![
        item(TrayAction::Open, S::TrayOpen),
        TrayEntry::Separator,
        item(TrayAction::CopyId, S::TrayCopyId),
        item(TrayAction::NewCode, S::TrayNewCode),
        TrayEntry::Item {
            action: TrayAction::ToggleIncoming,
            label: tr_raw(lang, S::IncomingEnabled).to_owned(),
            checked: Some(incoming_enabled),
        },
        TrayEntry::Separator,
        item(TrayAction::Settings, S::Settings),
        item(TrayAction::ConnectionLog, S::ConnectionLog),
        TrayEntry::Separator,
        item(TrayAction::Quit, S::TrayQuit),
    ]
}

/// Windows keeps at most 127 UTF-16 units of a tray tooltip (`szTip` is 128 with the NUL).
pub const MAX_TOOLTIP_UTF16: usize = 127;

/// Cut `s` so its UTF-16 length fits `max` units, ending with "…" if it was cut. Never splits
/// a character (or a surrogate pair).
pub fn fit_utf16(s: &str, max: usize) -> String {
    if s.encode_utf16().count() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 1; // room for the ellipsis
    for c in s.chars() {
        let n = c.len_utf16();
        if used + n > max {
            break;
        }
        used += n;
        out.push(c);
    }
    out.push('…');
    out
}

/// Tooltip text: the device ID (grouped, Western digits), paused state, and a pending
/// incoming request if there is one.
pub fn tooltip(
    lang: Lang,
    device_id: &str,
    incoming_enabled: bool,
    request: Option<&str>,
) -> String {
    let key = if incoming_enabled {
        S::TrayTooltip
    } else {
        S::TrayTooltipPaused
    };
    let mut text = tr_fmt_raw(lang, key, &[("id", device_id)]);
    if let Some(name) = request {
        text.push('\n');
        text.push_str(&tr_fmt_raw(lang, S::TrayTooltipRequest, &[("name", name)]));
    }
    fit_utf16(&text, MAX_TOOLTIP_UTF16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(entries: &[TrayEntry]) -> Vec<TrayAction> {
        entries
            .iter()
            .filter_map(|e| match e {
                TrayEntry::Item { action, .. } => Some(*action),
                TrayEntry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn tray_menu_has_every_action_in_both_languages() {
        for lang in Lang::ALL {
            let m = menu(lang, true);
            assert_eq!(
                actions(&m),
                [
                    TrayAction::Open,
                    TrayAction::CopyId,
                    TrayAction::NewCode,
                    TrayAction::ToggleIncoming,
                    TrayAction::Settings,
                    TrayAction::ConnectionLog,
                    TrayAction::Quit
                ]
            );
            for e in &m {
                if let TrayEntry::Item { label, .. } = e {
                    assert!(!label.trim().is_empty());
                }
            }
        }
        let ar = menu(Lang::Ar, false);
        let TrayEntry::Item { label, .. } = &ar[0] else {
            panic!("first entry is Open");
        };
        assert_eq!(
            label, "فتح TLP-Desk",
            "Arabic menu text, logical order for the OS"
        );
        assert!(ar.iter().any(|e| matches!(
            e,
            TrayEntry::Item {
                action: TrayAction::ToggleIncoming,
                checked: Some(false),
                ..
            }
        )));
    }

    #[test]
    fn tray_tooltip_shows_id_pause_and_requests() {
        let t = tooltip(Lang::En, "482 913 057", true, None);
        assert_eq!(t, "TLP-Desk — ID 482 913 057");
        let paused = tooltip(Lang::Ar, "482 913 057", false, None);
        assert!(paused.contains("482 913 057") && paused.contains("متوقفة"));
        let req = tooltip(Lang::Ar, "482 913 057", true, Some("حاسوب سارة"));
        assert!(req.contains("حاسوب سارة"));
        let long = tooltip(
            Lang::Ar,
            "482 913 057",
            true,
            Some(&"اسم طويل جداً ".repeat(30)),
        );
        assert!(long.encode_utf16().count() <= MAX_TOOLTIP_UTF16);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn fit_utf16_never_splits_surrogate_pairs() {
        let s = "😀".repeat(100); // 2 UTF-16 units each
        let cut = fit_utf16(&s, 11);
        assert!(cut.encode_utf16().count() <= 11);
        assert_eq!(cut.chars().filter(|c| *c == '😀').count(), 5);
        assert_eq!(fit_utf16("short", 127), "short");
    }
}
