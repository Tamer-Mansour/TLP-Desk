//! `rm`: delete a file or folder on the remote PC (permanently).

use anyhow::Result;
use clap::Args;

use super::common::{Ctx, DialArgs};
use super::remote_fs::{done_json, with_client};
use crate::text;

/// Arguments of `rm`.
#[derive(Debug, Clone, Args)]
pub struct RmArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Absolute path on the remote PC.
    pub path: String,
    /// Also delete a folder that is not empty, with everything in it.
    #[arg(short, long)]
    pub recursive: bool,
}

/// Runs `rm`.
pub async fn run(ctx: &Ctx, args: &RmArgs) -> Result<()> {
    let out = ctx.out;
    let path = args.path.clone();
    let recursive = args.recursive;
    with_client(ctx, &args.dial, |fs| async move {
        fs.delete(&path, recursive).await?;
        out.result(text::fs_deleted(&path), &done_json("rm", &path));
        Ok(())
    })
    .await
}
