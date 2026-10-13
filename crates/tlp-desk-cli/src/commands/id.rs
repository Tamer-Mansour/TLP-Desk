//! `id`: print this device's ID and key fingerprint.

use anyhow::Result;
use clap::Args;

use super::common::{load_identity, Ctx};
use crate::text;

/// Arguments of `id` (none yet).
#[derive(Debug, Clone, Default, Args)]
pub struct IdArgs {}

/// Runs `id`.
pub fn run(ctx: &Ctx, _args: &IdArgs) -> Result<()> {
    let id = load_identity(&ctx.data_dir)?;
    println!(
        "{}",
        text::identity(
            id.device_id(),
            &id.fingerprint(),
            &ctx.data_dir.display().to_string()
        )
    );
    Ok(())
}
