//! The always-visible host banner: "<name> is connected to this device" with an End session
//! button, in its own small topmost window, whenever any viewer is connected — also while the
//! main window is minimized or in the tray (security model in CLAUDE.md).
//!
//! * [`BannerModel`]: what the banner shows (pure, tested): one row per session, peer names
//!   sanitized (no control or bidi-override characters, length-capped) and always paired with
//!   the authenticated device ID, so a viewer cannot disguise itself with a crafted name.
//! * [`view`]: the egui viewport (topmost, undecorated, not focused when it appears, no
//!   taskbar button).
//!
//! **Capture:** the banner is deliberately *not* excluded from screen capture. The viewer sees
//! the same notice the host user sees (so it is clear the session is visible to the host), and
//! excluding it would need `SetWindowDisplayAffinity` (unsafe Win32) in this pure crate. It
//! sits at the top edge and is small, so it hides very little of the remote screen.

pub mod view;

use crate::backend::{PeerInfo, SessionId};

/// Longest peer name shown (the rest is cut with "…").
pub const MAX_NAME_CHARS: usize = 40;
/// At most this many sessions get their own row; more are summarized.
pub const MAX_ROWS: usize = 4;

/// One "who is connected" row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerRow {
    pub session: SessionId,
    /// Sanitized display name (may be empty when the peer sent none).
    pub name: String,
    /// Authenticated 9-digit ID, grouped (`482 913 057`), Western digits.
    pub device_id: String,
}

/// What the banner shows; `None` from [`BannerModel::from_sessions`] means "no banner".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerModel {
    pub rows: Vec<BannerRow>,
    /// Sessions beyond [`MAX_ROWS`].
    pub more: usize,
}

/// Remove characters that could hide or reorder text (controls, bidi embeddings/overrides/
/// isolates, zero-width marks) and cap the length.
pub fn sanitize_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| {
            !c.is_control()
                // ZWNJ/ZWJ (U+200C/D) stay: they only affect joining, and Arabic-script
                // names legitimately use them.
                && !matches!(*c as u32,
                    0x200B | 0x200E | 0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF)
        })
        .collect();
    let clean = clean.trim();
    if clean.chars().count() > MAX_NAME_CHARS {
        let mut cut: String = clean.chars().take(MAX_NAME_CHARS - 1).collect();
        cut.push('…');
        cut
    } else {
        clean.to_owned()
    }
}

impl BannerModel {
    /// The banner for the current host sessions; `None` when nobody is connected.
    pub fn from_sessions(sessions: &[(SessionId, PeerInfo)]) -> Option<Self> {
        if sessions.is_empty() {
            return None;
        }
        let rows = sessions
            .iter()
            .take(MAX_ROWS)
            .map(|(session, peer)| BannerRow {
                session: *session,
                name: sanitize_name(&peer.name),
                device_id: peer.device_id.to_string(),
            })
            .collect();
        Some(Self {
            rows,
            more: sessions.len().saturating_sub(MAX_ROWS),
        })
    }

    /// Number of text lines the window needs.
    pub fn lines(&self) -> usize {
        self.rows.len() + usize::from(self.more > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DeviceId;

    fn peer(id: u32, name: &str) -> PeerInfo {
        PeerInfo {
            device_id: DeviceId(id),
            name: name.into(),
            address: "192.168.1.9".parse().unwrap(),
        }
    }

    #[test]
    fn banner_visibility_follows_host_sessions() {
        let mut sessions: Vec<(SessionId, PeerInfo)> = Vec::new();
        assert_eq!(
            BannerModel::from_sessions(&sessions),
            None,
            "nobody: no banner"
        );

        sessions.push((SessionId(1), peer(482_913_057, "حاسوب سارة")));
        let b = BannerModel::from_sessions(&sessions).expect("one viewer: banner");
        assert_eq!(b.rows.len(), 1);
        assert_eq!(b.rows[0].name, "حاسوب سارة");
        assert_eq!(b.rows[0].device_id, "482 913 057");

        sessions.push((SessionId(2), peer(7, "Laptop")));
        let b = BannerModel::from_sessions(&sessions).unwrap();
        assert_eq!(
            b.rows.iter().map(|r| r.session).collect::<Vec<_>>(),
            [SessionId(1), SessionId(2)]
        );

        sessions.retain(|(s, _)| *s != SessionId(1));
        let b = BannerModel::from_sessions(&sessions).unwrap();
        assert_eq!(b.rows.len(), 1);
        assert_eq!(b.rows[0].device_id, "000 000 007");

        sessions.clear();
        assert_eq!(
            BannerModel::from_sessions(&sessions),
            None,
            "last one left: hidden"
        );
    }

    #[test]
    fn many_sessions_are_summarized() {
        let sessions: Vec<_> = (0..7)
            .map(|i| (SessionId(i), peer(i as u32, "pc")))
            .collect();
        let b = BannerModel::from_sessions(&sessions).unwrap();
        assert_eq!(b.rows.len(), MAX_ROWS);
        assert_eq!(b.more, 3);
        assert_eq!(b.lines(), MAX_ROWS + 1);
    }

    #[test]
    fn hostile_names_cannot_disguise_the_viewer() {
        // Right-to-left override would show "gpj.exe" reversed; isolates and zero-width
        // characters could hide text; newlines could push the ID out of view.
        assert_eq!(sanitize_name("abc\u{202E}gpj.exe"), "abcgpj.exe");
        assert_eq!(sanitize_name("سارة\u{2067}\u{200F}\n\tPC"), "سارةPC");
        assert_eq!(sanitize_name("   "), "");
        let long = sanitize_name(&"م".repeat(100));
        assert_eq!(long.chars().count(), MAX_NAME_CHARS);
        assert!(long.ends_with('…'));
    }
}
