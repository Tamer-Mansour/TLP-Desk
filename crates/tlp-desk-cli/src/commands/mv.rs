//! `mv`: rename or move a file or folder within one drive of the remote PC (never overwrites).

use anyhow::Result;
use clap::Args;
use serde_json::json;

use super::common::{Ctx, DialArgs};
use super::remote_fs::with_client;
use crate::text;

/// Arguments of `mv`.
#[derive(Debug, Clone, Args)]
pub struct MvArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Absolute path of the existing entry on the remote PC.
    pub from: String,
    /// Absolute new path (same drive; must not exist yet).
    pub to: String,
}

/// Runs `mv`.
pub async fn run(ctx: &Ctx, args: &MvArgs) -> Result<()> {
    let out = ctx.out;
    let (from, to) = (args.from.clone(), args.to.clone());
    with_client(ctx, &args.dial, |fs| async move {
        fs.rename(&from, &to).await?;
        out.result(
            text::fs_moved(&from, &to),
            &json!({ "type": "done", "op": "mv", "path": from, "to": to }),
        );
        Ok(())
    })
    .await
}
