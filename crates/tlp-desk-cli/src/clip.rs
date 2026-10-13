//! `clip-send`: put text on a host's clipboard through a normal viewer session, using the
//! clipboard feature with an in-memory clipboard (this PC's own clipboard is never read or
//! written). The host must grant `CLIPBOARD` (`serve --clipboard`, or the app's default).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tlp_desk_files::{
    ClipContent, ClipboardConfig, ClipboardEvent, ClipboardFeature, Direction, MemoryClipboard,
    RefusedReason, FEATURE_NAME,
};
use tlp_desk_net::{Discovery, DiscoveryConfig, Endpoint, EndpointConfig, FeatureSet};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, KnownHostsStore, Viewer, ViewerConfig, ViewerEvent,
};

/// How long to wait for the clipboard to go out after connecting.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// The text to send: the argument, or a UTF-8 file read byte-exact (CRLF, RTL marks, joiners
/// and everything else kept as is).
pub fn load_text(text: Option<String>, file: Option<&Path>) -> Result<String> {
    match (text, file) {
        (Some(t), None) => Ok(t),
        (None, Some(p)) => {
            let bytes = std::fs::read(p).with_context(|| format!("cannot read {}", p.display()))?;
            String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8 text", p.display()))
        }
        (Some(_), Some(_)) => bail!("give either the text or --file, not both"),
        (None, None) => bail!("nothing to send: give the text or --file"),
    }
}

/// Connects to `target`, sends `text` as the clipboard and disconnects.
pub async fn clip_send(
    dir: &Path,
    target: &str,
    auth: AuthMethod,
    text: String,
    port: u16,
) -> Result<()> {
    let identity = crate::load_identity(dir)?;
    let endpoint = Arc::new(Endpoint::bind(
        identity,
        EndpointConfig {
            bind: crate::any_addr(port),
            listen: false,
            ..EndpointConfig::default()
        },
    )?);
    let target: ConnectTarget = target.parse()?;
    let discovery = match target {
        ConnectTarget::Id(_) | ConnectTarget::Name(_) => Some(Arc::new(
            Discovery::start(DiscoveryConfig::default()).await?,
        )),
        _ => None,
    };
    let clipboard = Arc::new(MemoryClipboard::with(ClipContent::Text(text)));
    let feature = ClipboardFeature::new(
        clipboard,
        ClipboardConfig {
            push_on_start: true,
            enabled_by_default: true,
            ..ClipboardConfig::default()
        },
    );
    let viewer = Viewer::new(
        endpoint.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(dir.join(tlp_desk_crypto::KNOWN_HOSTS_FILE)),
            discovery,
            features: FeatureSet::new().with(feature),
            ..ViewerConfig::default()
        },
    )?;
    println!("{}", crate::text::connecting(&target.to_string()));
    if auth == AuthMethod::AcceptPrompt {
        println!("{}", crate::text::WAITING_FOR_ACCEPT);
    }
    let (session, mut events) = tokio::select! {
        r = viewer.connect(target, auth) => r?,
        _ = tokio::signal::ctrl_c() => bail!("cancelled"),
    };
    if !session.permissions().contains(Permissions::CLIPBOARD) {
        session.disconnect();
        bail!("the host does not allow clipboard sync for this session");
    }
    let outcome = tokio::time::timeout(SEND_TIMEOUT, async {
        while let Some(ev) = events.recv().await {
            match ev {
                ViewerEvent::Feature(e) if e.feature == FEATURE_NAME => {
                    match e.payload::<ClipboardEvent>() {
                        Some(ClipboardEvent::Synced {
                            direction: Direction::Sent,
                            bytes,
                            ..
                        }) => return Ok(*bytes),
                        Some(ClipboardEvent::Refused { reason, .. }) => {
                            return Err(match reason {
                                RefusedReason::TooLarge => "the text is too large to send",
                                _ => "the clipboard could not be sent",
                            })
                        }
                        _ => {}
                    }
                }
                ViewerEvent::Ended(_) => return Err("the session ended before sending"),
                _ => {}
            }
        }
        Err("the session ended before sending")
    })
    .await;
    // Give a side-stream payload time to drain before saying goodbye.
    tokio::time::sleep(Duration::from_millis(300)).await;
    session.disconnect();
    let _ = tokio::time::timeout(Duration::from_secs(3), session.ended()).await;
    drop(session);
    endpoint.close();
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    match outcome {
        Ok(Ok(bytes)) => {
            println!("Clipboard sent ({bytes} bytes).");
            Ok(())
        }
        Ok(Err(why)) => bail!("{why}"),
        Err(_) => bail!("timed out waiting for the clipboard to be sent"),
    }
}

/// A log line for a clipboard event (kinds and sizes only), if event is one.
pub fn event_line(event: &tlp_desk_net::FeatureEvent) -> Option<String> {
    if event.feature != FEATURE_NAME {
        return None;
    }
    event.payload::<ClipboardEvent>().map(|e| match e {
        ClipboardEvent::Synced {
            direction,
            kind,
            bytes,
        } => format!("Clipboard {direction:?} ({kind:?}, {bytes} bytes)").to_lowercase(),
        ClipboardEvent::Refused {
            direction,
            kind,
            reason,
        } => format!("Clipboard not synced ({direction:?} {kind:?}: {reason:?})"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_text_is_read_byte_exact() {
        let dir = std::env::temp_dir().join(format!("tlp-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("نص.txt");
        let text = "مرحبا\u{200F}\r\nلا\u{200C}مي\u{200D}\n";
        std::fs::write(&p, text).unwrap();
        assert_eq!(load_text(None, Some(&p)).unwrap(), text);
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(load_text(None, Some(&p)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_text(Some("أ".into()), None).unwrap(), "أ");
        assert!(load_text(None, None).is_err());
        assert!(load_text(Some("x".into()), Some(&p)).is_err());
    }
}
