//! `get-file`: download files and folders from a host (viewer side), with progress.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use super::common::{Ctx, DialArgs};
use super::send_file::{connect, watch};

/// Arguments of `get-file`.
#[derive(Debug, Clone, Args)]
pub struct GetFileArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Absolute paths on the host (files and folders in one folder; folders recursively).
    #[arg(required = true, num_args = 1..)]
    pub remote_paths: Vec<String>,
    /// Local destination folder (default: the current folder).
    #[arg(long, default_value = ".")]
    pub to: PathBuf,
}

/// Runs `get-file`.
pub async fn run(ctx: &Ctx, args: &GetFileArgs) -> Result<()> {
    let (ft, mut dialed) = connect(ctx, &args.dial).await?;
    let mut sub = ft.subscribe();
    let key = dialed.session.feature_key();
    let res = match ft
        .start_download(key, args.remote_paths.clone(), args.to.clone())
        .await
    {
        Ok(id) => watch(ctx, &ft, &mut dialed, id, &mut sub).await,
        Err(e) => Err(e.into()),
    };
    dialed.close().await;
    res
}
