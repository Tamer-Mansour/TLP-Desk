//! `mkdir`: create a folder (and missing parents) on the remote PC.

use anyhow::Result;
use clap::Args;

use super::common::{Ctx, DialArgs};
use super::remote_fs::{done_json, with_client};
use crate::text;

/// Arguments of `mkdir`.
#[derive(Debug, Clone, Args)]
pub struct MkdirArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Absolute path of the new folder on the remote PC.
    pub path: String,
}

/// Runs `mkdir`.
pub async fn run(ctx: &Ctx, args: &MkdirArgs) -> Result<()> {
    let out = ctx.out;
    let path = args.path.clone();
    with_client(ctx, &args.dial, |fs| async move {
        fs.make_dir(&path).await?;
        out.result(text::fs_created(&path), &done_json("mkdir", &path));
        Ok(())
    })
    .await
}
