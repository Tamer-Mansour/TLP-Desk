//! `chat`: connect to a host, send one chat message, print replies for a while.

use std::time::Duration;

use anyhow::{bail, Result};
use clap::Args;
use serde_json::json;
use tlp_desk_net::{ChatEvent, ChatFeature, FeatureSet, CHAT_FEATURE};
use tlp_desk_viewer::ViewerEvent;

use super::common::{dial, peer_json, Ctx, DialArgs};
use crate::text;

/// Arguments of `chat`.
#[derive(Debug, Clone, Args)]
pub struct ChatArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// The message (UTF-8; Arabic and emoji welcome). Quote it.
    pub message: String,
    /// Keep the session open this many seconds and print the host user's replies.
    #[arg(long, default_value_t = 2)]
    pub wait: u64,
}

/// Runs `chat`.
pub async fn run(ctx: &Ctx, args: &ChatArgs) -> Result<()> {
    let out = ctx.out;
    let chat = ChatFeature::new();
    let mut dialed = dial(ctx, &args.dial, FeatureSet::new().with(chat.clone())).await?;
    let key = dialed.session.feature_key();
    let peer = dialed.session.peer().clone();

    // The chat task reports whether chat is allowed as soon as the session starts.
    let allowed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(ev) = dialed.events.recv().await {
            if let ViewerEvent::Feature(e) = ev {
                if let Some(ChatEvent::Availability(a)) = e.payload::<ChatEvent>() {
                    return *a;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !allowed {
        dialed.close().await;
        bail!(text::chat_not_allowed());
    }
    if let Err(e) = chat.send(key, &args.message) {
        dialed.close().await;
        bail!(text::chat_rejected(&e.to_string()));
    }
    out.result(
        text::chat_sent(),
        &json!({ "type": "chat_sent", "peer": peer_json(&peer) }),
    );

    // At least a moment, so the message leaves before the goodbye.
    let wait = Duration::from_secs(args.wait).max(Duration::from_millis(500));
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => break,
            ev = dialed.events.recv() => match ev {
                Some(ViewerEvent::Feature(e)) if e.feature == CHAT_FEATURE => {
                    if let Some(ChatEvent::Received(m)) = e.payload::<ChatEvent>() {
                        out.event(
                            text::chat_received(&peer.name, &m.text),
                            &json!({ "type": "chat", "peer": peer_json(&peer), "text": m.text }),
                        );
                    }
                }
                Some(ViewerEvent::Ended(_)) | None => break,
                Some(_) => {}
            }
        }
    }
    dialed.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Cli, Cmd};
    use clap::Parser;

    #[test]
    fn chat_parses_target_message_and_wait() {
        let cli = Cli::try_parse_from([
            "TLP-Desk-cli",
            "chat",
            "192.168.1.5",
            "مرحباً، هل ترى الشاشة؟ OK 👍",
            "--code",
            "123456",
            "--wait",
            "7",
        ])
        .unwrap();
        let Cmd::Chat(c) = cli.cmd else {
            panic!("not chat")
        };
        assert_eq!(c.dial.target, "192.168.1.5");
        assert_eq!(c.message, "مرحباً، هل ترى الشاشة؟ OK 👍");
        assert_eq!(c.wait, 7);
        assert_eq!(c.dial.auth.code.as_deref(), Some("123456"));
        assert!(Cli::try_parse_from(["TLP-Desk-cli", "chat", "192.168.1.5"]).is_err());
    }
}
