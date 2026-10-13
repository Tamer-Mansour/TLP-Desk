//! `list`: discover TLP-Desk devices on the LAN.

use std::time::Duration;

use anyhow::Result;
use clap::Args;
use tlp_desk_net::{Discovery, DiscoveryConfig};

use super::common::Ctx;
use crate::text;

/// Arguments of `list`.
#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// How long to listen for announcements.
    #[arg(long, default_value_t = 5)]
    pub seconds: u64,
}

/// Runs `list`.
pub async fn run(_ctx: &Ctx, args: &ListArgs) -> Result<()> {
    println!("{}", text::searching(args.seconds));
    let disc = Discovery::start(DiscoveryConfig::default()).await?;
    disc.refresh().await;
    tokio::time::sleep(Duration::from_secs(args.seconds)).await;
    let devices = disc.devices();
    if devices.is_empty() {
        println!("{}", text::NO_DEVICES);
    }
    for d in devices {
        println!("{}", text::device_row(&d));
    }
    Ok(())
}
