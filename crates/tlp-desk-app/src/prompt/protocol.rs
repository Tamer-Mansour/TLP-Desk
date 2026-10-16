//! The JSON lines spoken between the GUI (or the service launcher) and the
//! `TLP-Desk.exe --accept-prompt` process.
//!
//! * stdin, line 1: one [`PromptRequest`];
//! * stdin, later lines: [`CANCEL_LINE`] closes the window without an answer (the viewer gave
//!   up). End of stdin means the same: the parent is gone, so nobody could use an answer;
//! * stdout: exactly one [`PromptDecision`] line, then the process exits.
//!
//! The peer name comes from the network, so everything read here is treated as hostile: lines
//! are size-capped before parsing and display strings are length-capped.

use serde::{Deserialize, Serialize};
use tlp_desk_proto::Permissions;

use crate::i18n::{DigitStyle, Lang};
use crate::settings::host::PermissionSet;

/// Longest line accepted on stdin or stdout (a request is a few hundred bytes).
pub const MAX_LINE: usize = 16 * 1024;
/// Peer names longer than this (in characters) are cut with an ellipsis for display.
pub const MAX_NAME_CHARS: usize = 64;
/// The line the parent writes to close the window early.
pub const CANCEL_LINE: &str = "cancel";

/// How the peer authenticated (the chip under the name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptAuth {
    /// No secret: the user at this PC decides (the normal case for a popup).
    #[default]
    AcceptPrompt,
    /// The peer typed the current one-time code.
    OneTimeCode,
    /// The peer used the unattended-access password.
    Password,
}

/// Window colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTheme {
    /// Follow the Windows app theme.
    #[default]
    System,
    Dark,
    Light,
}

/// Everything the popup shows about one incoming request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptRequest {
    /// Self-reported device name, UTF-8 (often Arabic).
    pub peer_name: String,
    /// Nine-digit device ID.
    pub device_id: u32,
    /// Textual IP address of the peer.
    pub address: String,
    pub auth: PromptAuth,
    /// Permissions the session gets if the user just clicks Accept (the host defaults).
    pub permissions: PermissionSet,
    /// After this many seconds without an answer the popup closes with `timeout`.
    pub timeout_secs: u32,
    pub lang: Lang,
    pub digits: DigitStyle,
    pub theme: PromptTheme,
    /// Play the short Windows notification sound when the window appears.
    pub sound: bool,
    /// The permissions are fixed (shown, but cannot be switched for this request).
    pub permissions_locked: bool,
}

impl Default for PromptRequest {
    fn default() -> Self {
        Self {
            peer_name: String::new(),
            device_id: 0,
            address: String::new(),
            auth: PromptAuth::AcceptPrompt,
            permissions: PermissionSet::default(),
            timeout_secs: 30,
            lang: Lang::default(),
            digits: DigitStyle::default(),
            theme: PromptTheme::System,
            sound: true,
            permissions_locked: false,
        }
    }
}

impl PromptRequest {
    /// Clamp hostile or nonsensical values: a bounded timeout, VIEW always present, a peer
    /// name cut to [`MAX_NAME_CHARS`] without control characters.
    pub fn sanitized(mut self) -> Self {
        self.timeout_secs = self.timeout_secs.clamp(5, 600);
        self.permissions.0 = self.permissions.0.union(Permissions::VIEW);
        self.peer_name = display_name(&self.peer_name);
        self.address = self
            .address
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        self
    }
}

/// What the user decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Accept,
    Decline,
    /// Nobody answered within the timeout (treated as a decline).
    Timeout,
    /// The parent cancelled (the viewer gave up) or stdin closed.
    Cancelled,
}

/// The popup's single answer, written as one line on stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptDecision {
    pub outcome: Outcome,
    /// The permissions the user left switched on (meaningful for `accept`).
    pub permissions: PermissionSet,
    /// "Trust this device" was ticked (stored for a future auto-accept; grants nothing now).
    #[serde(default)]
    pub trust: bool,
}

impl PromptDecision {
    /// Whether the session should be accepted.
    pub fn accepted(&self) -> bool {
        self.outcome == Outcome::Accept
    }
}

/// Protocol errors (bad JSON, oversized line, missing input).
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("prompt line is longer than {MAX_LINE} bytes")]
    TooLong,
    #[error("no prompt request on stdin")]
    Missing,
    #[error("invalid prompt json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("prompt i/o failed: {0}")]
    Io(#[from] std::io::Error),
}

/// One JSON line (no trailing newline).
pub fn encode<T: Serialize>(msg: &T) -> Result<String, PromptError> {
    Ok(serde_json::to_string(msg)?)
}

/// Parse one line, refusing oversized input before touching the JSON parser.
pub fn decode<T: serde::de::DeserializeOwned>(line: &str) -> Result<T, PromptError> {
    if line.len() > MAX_LINE {
        return Err(PromptError::TooLong);
    }
    Ok(serde_json::from_str(line.trim())?)
}

/// Read one `\n`-terminated line of at most [`MAX_LINE`] bytes. `Ok(None)` at end of input.
pub fn read_line(r: &mut impl std::io::BufRead) -> Result<Option<String>, PromptError> {
    use std::io::BufRead as _;
    let mut buf = Vec::new();
    let limit = u64::try_from(MAX_LINE)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let n = std::io::Read::take(&mut *r, limit).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > MAX_LINE && buf.last() != Some(&b'\n') {
        return Err(PromptError::TooLong);
    }
    Ok(Some(String::from_utf8_lossy(&buf).trim_end().to_owned()))
}

/// A peer name safe to show: control characters removed, at most [`MAX_NAME_CHARS`].
pub fn display_name(name: &str) -> String {
    let clean: Vec<char> = name.chars().filter(|c| !c.is_control()).collect();
    let trimmed: String = clean.iter().collect::<String>().trim().to_owned();
    if trimmed.chars().count() <= MAX_NAME_CHARS {
        return trimmed;
    }
    let mut cut: String = trimmed
        .chars()
        .take(MAX_NAME_CHARS.saturating_sub(1))
        .collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arabic_request() -> PromptRequest {
        PromptRequest {
            peer_name: "حاسوب-المختبر".into(),
            device_id: 950_442_118,
            address: "192.168.1.81".into(),
            lang: Lang::Ar,
            ..PromptRequest::default()
        }
    }

    #[test]
    fn request_round_trips_with_arabic_name() {
        let req = arabic_request();
        let line = encode(&req).unwrap();
        assert!(!line.contains('\n'));
        let back: PromptRequest = decode(&line).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn decision_round_trips() {
        let mut perms = PermissionSet(Permissions::VIEW);
        perms.set(Permissions::FILES, true);
        let d = PromptDecision {
            outcome: Outcome::Accept,
            permissions: perms,
            trust: true,
        };
        let line = encode(&d).unwrap();
        assert!(line.contains("\"accept\""), "{line}");
        let back: PromptDecision = decode(&line).unwrap();
        assert_eq!(back, d);
        assert!(back.accepted());
    }

    #[test]
    fn missing_fields_take_defaults() {
        let req: PromptRequest = decode(r#"{"peer_name":"x"}"#).unwrap();
        assert_eq!(req.timeout_secs, 30);
        assert_eq!(req.auth, PromptAuth::AcceptPrompt);
    }

    #[test]
    fn oversized_and_garbage_lines_are_refused() {
        let long = "x".repeat(MAX_LINE + 1);
        assert!(matches!(
            decode::<PromptRequest>(&long),
            Err(PromptError::TooLong)
        ));
        assert!(decode::<PromptRequest>("{not json").is_err());
        let mut r = std::io::Cursor::new(long.into_bytes());
        assert!(matches!(read_line(&mut r), Err(PromptError::TooLong)));
    }

    #[test]
    fn read_line_splits_lines_and_reports_end() {
        let mut r = std::io::Cursor::new(b"{\"a\":1}\r\ncancel\n".to_vec());
        assert_eq!(read_line(&mut r).unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(read_line(&mut r).unwrap().as_deref(), Some(CANCEL_LINE));
        assert_eq!(read_line(&mut r).unwrap(), None);
    }

    #[test]
    fn sanitize_bounds_hostile_values() {
        let req = PromptRequest {
            peer_name: format!("\u{7}{}", "ب".repeat(200)),
            timeout_secs: 0,
            permissions: PermissionSet(Permissions::NONE),
            address: "1.2.3.4\n".into(),
            ..PromptRequest::default()
        }
        .sanitized();
        assert_eq!(req.timeout_secs, 5);
        assert!(req.permissions.0.contains(Permissions::VIEW));
        assert_eq!(req.peer_name.chars().count(), MAX_NAME_CHARS);
        assert!(req.peer_name.ends_with('…'));
        assert!(!req.peer_name.contains('\u{7}'));
        assert_eq!(req.address, "1.2.3.4");
    }
}
