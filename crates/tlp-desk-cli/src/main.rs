//! Headless TLP-Desk CLI for scripts and agents: print identity, list LAN devices, host a
//! session (real capture/encode/input), or connect as a viewer and report statistics.
//!
//! The command structure lives in [`commands`] (one module per subcommand).
#![forbid(unsafe_code)]

mod commands;
mod text;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = commands::Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(commands::run(&cli))
}
