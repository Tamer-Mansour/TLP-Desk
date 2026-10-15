//! `ls`: list a folder on the remote PC (or its drives and known folders).

use anyhow::Result;
use clap::Args;

use super::common::{Ctx, DialArgs};
use super::remote_fs::{entry_json, root_json, with_client};
use crate::text;

/// Arguments of `ls`.
#[derive(Debug, Clone, Args)]
pub struct LsArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Absolute folder on the remote PC (e.g. `C:\Users`). Omit it to list the drives and
    /// known folders.
    pub path: Option<String>,
}

/// Runs `ls`.
pub async fn run(ctx: &Ctx, args: &LsArgs) -> Result<()> {
    let out = ctx.out;
    let path = args.path.clone();
    with_client(ctx, &args.dial, |fs| async move {
        match path {
            None => {
                for r in fs.roots().await? {
                    out.result(text::fs_root_row(&r), &root_json(&r));
                }
            }
            Some(p) => {
                let list = fs.list_dir(&p).await?;
                if list.is_empty() {
                    out.progress(text::fs_empty_folder());
                }
                for e in &list {
                    out.result(text::fs_entry_row(e), &entry_json(e));
                }
            }
        }
        Ok(())
    })
    .await
}
