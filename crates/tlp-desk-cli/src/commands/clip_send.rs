//! `clip-send`: put text on a host's clipboard through a normal viewer session, using the
//! clipboard feature with an in-memory clipboard (this PC's own clipboard is never read or
//! written). The host must grant `CLIPBOARD` (`serve --clipboard`, or the app's default).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::json;
use tlp_desk_files::{
    ClipContent, ClipboardConfig, ClipboardEvent, ClipboardFeature, Direction, MemoryClipboard,
    RefusedReason, FEATURE_NAME,
};
use tlp_desk_net::{FeatureEvent, FeatureSet};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::ViewerEvent;

use super::common::{dial, peer_json, Ctx, DialArgs};
use crate::text;

/// How long to wait for the clipboard to go out after connecting.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Arguments of `clip-send`.
#[derive(Debug, Clone, Args)]
pub struct ClipSendArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// The text to send (Arabic and every other character are kept exactly).
    pub text: Option<String>,
    /// Send the contents of this UTF-8 text file instead (read byte-exact).
    #[arg(long, conflicts_with = "text")]
    pub file: Option<PathBuf>,
}

/// The text to send: the argument, or a UTF-8 file read byte-exact (CRLF, RTL marks, joiners
/// and everything else kept as is).
pub fn load_text(text: Option<&str>, file: Option<&Path>) -> Result<String> {
    match (text, file) {
        (Some(t), None) => Ok(t.to_owned()),
        (None, Some(p)) => {
            let bytes = std::fs::read(p).with_context(|| format!("cannot read {}", p.display()))?;
            String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8 text", p.display()))
        }
        (Some(_), Some(_)) | (None, None) => bail!("{}", text::clip_nothing()),
    }
}

/// A line for a clipboard feature event (kinds and sizes only), if `event` is one. Used by
/// `serve --clipboard` too.
pub fn event_line(event: &FeatureEvent) -> Option<(String, serde_json::Value)> {
    if event.feature != FEATURE_NAME {
        return None;
    }
    event.payload::<ClipboardEvent>().map(|e| match e {
        ClipboardEvent::Synced {
            direction,
            kind,
            bytes,
        } => (
            match direction {
                Direction::Sent => text::clip_sent(*bytes),
                Direction::Received => text::clip_received(*bytes),
            },
            json!({
                "type": "clipboard",
                "direction": format!("{direction:?}").to_lowercase(),
                "kind": format!("{kind:?}").to_lowercase(),
                "bytes": bytes,
            }),
        ),
        ClipboardEvent::Refused {
            direction,
            kind,
            reason,
        } => (
            match reason {
                RefusedReason::TooLarge => text::clip_too_large(),
                _ => text::clip_not_synced(),
            }
            .to_owned(),
            json!({
                "type": "clipboard_refused",
                "direction": format!("{direction:?}").to_lowercase(),
                "kind": format!("{kind:?}").to_lowercase(),
                "reason": format!("{reason:?}"),
            }),
        ),
    })
}

/// Connects, sends the text as the clipboard and disconnects.
pub async fn run(ctx: &Ctx, args: &ClipSendArgs) -> Result<()> {
    let content = load_text(args.text.as_deref(), args.file.as_deref())?;
    let feature = ClipboardFeature::new(
        Arc::new(MemoryClipboard::with(ClipContent::Text(content))),
        ClipboardConfig {
            push_on_start: true,
            enabled_by_default: true,
            ..ClipboardConfig::default()
        },
    );
    let mut dialed = dial(ctx, &args.dial, FeatureSet::new().with(feature)).await?;
    let peer = peer_json(dialed.session.peer());
    if !dialed
        .session
        .permissions()
        .contains(Permissions::CLIPBOARD)
    {
        dialed.session.disconnect();
        dialed.close().await;
        bail!("{}", text::clip_not_allowed());
    }
    let events = &mut dialed.events;
    let outcome = tokio::time::timeout(SEND_TIMEOUT, async {
        while let Some(ev) = events.recv().await {
            match ev {
                ViewerEvent::Feature(e) => match e.payload::<ClipboardEvent>() {
                    Some(ClipboardEvent::Synced {
                        direction: Direction::Sent,
                        bytes,
                        ..
                    }) if e.feature == FEATURE_NAME => return Ok(*bytes),
                    Some(ClipboardEvent::Refused { reason, .. }) if e.feature == FEATURE_NAME => {
                        return Err(match reason {
                            RefusedReason::TooLarge => text::clip_too_large(),
                            _ => text::clip_not_synced(),
                        });
                    }
                    _ => {}
                },
                ViewerEvent::Ended(_) => return Err(text::clip_ended_early()),
                _ => {}
            }
        }
        Err(text::clip_ended_early())
    })
    .await;
    // Give a side-stream payload time to drain before saying goodbye.
    tokio::time::sleep(Duration::from_millis(300)).await;
    dialed.session.disconnect();
    let _ = tokio::time::timeout(Duration::from_secs(3), dialed.session.ended()).await;
    dialed.close().await;
    match outcome {
        Ok(Ok(bytes)) => {
            ctx.out.result(
                text::clip_sent(bytes),
                &json!({ "type": "clipboard_sent", "bytes": bytes, "peer": peer }),
            );
            Ok(())
        }
        Ok(Err(why)) => bail!("{why}"),
        Err(_) => bail!("{}", text::clip_timeout()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_text_is_read_byte_exact() {
        let dir = std::env::temp_dir().join(format!("tlp-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("نص.txt");
        let content = "مرحبا\u{200F}\r\nلا\u{200C}مي\u{200D}\n";
        std::fs::write(&p, content).unwrap();
        assert_eq!(load_text(None, Some(&p)).unwrap(), content);
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(load_text(None, Some(&p)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_text(Some("أ"), None).unwrap(), "أ");
        assert!(load_text(None, None).is_err());
        assert!(load_text(Some("x"), Some(&p)).is_err());
    }

    #[test]
    fn events_become_lines_without_contents() {
        let ev = FeatureEvent::new(
            FEATURE_NAME,
            1,
            ClipboardEvent::Synced {
                direction: Direction::Received,
                kind: tlp_desk_files::ClipKind::Text,
                bytes: 12,
            },
        );
        let (line, json) = event_line(&ev).unwrap();
        assert!(line.contains("12"));
        assert_eq!(json["direction"], "received");
        assert_eq!(json["kind"], "text");
        assert!(event_line(&FeatureEvent::new("chat", 1, 5u8)).is_none());
    }
}
