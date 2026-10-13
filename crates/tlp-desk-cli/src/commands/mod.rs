//! Command-line structure: global options ([`Cli`]) and one module per subcommand.
//!
//! Adding a command (other lanes): create `commands/<name>.rs` with a `<Name>Args` clap
//! `Args` struct and a `run(ctx, args)` function, then add one line in each FEATURE HOOK below
//! (`mod`, the [`Cmd`] variant, the [`run`] arm). Dial/auth helpers live in [`common`].

pub mod common;
pub mod connect;
pub mod id;
pub mod list;
pub mod serve;
// FEATURE HOOK: `pub mod <command>;` for new subcommands.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::text;
use common::Ctx;

/// The whole command line.
#[derive(Debug, Parser)]
#[command(name = "TLP-Desk-cli", version, about = text::ABOUT)]
pub struct Cli {
    /// Directory holding the identity key and known hosts (default: the app's data dir).
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    /// More logging (repeat for even more). RUST_LOG overrides.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print this device's ID and key fingerprint.
    Id(id::IdArgs),
    /// Discover TLP-Desk devices on the LAN.
    List(list::ListArgs),
    /// Host: let LAN viewers see and control this PC.
    Serve(serve::ServeArgs),
    /// Viewer: connect to a host and report fps / bitrate / RTT every second.
    Connect(connect::ConnectArgs),
    // FEATURE HOOK: one variant per new subcommand.
}

/// Runs the selected subcommand.
pub async fn run(cli: &Cli) -> Result<()> {
    let ctx = Ctx {
        data_dir: cli
            .data_dir
            .clone()
            .unwrap_or_else(tlp_desk_crypto::default_dir),
    };
    match &cli.cmd {
        Cmd::Id(a) => id::run(&ctx, a),
        Cmd::List(a) => list::run(&ctx, a).await,
        Cmd::Serve(a) => serve::run(&ctx, a).await,
        Cmd::Connect(a) => connect::run(&ctx, a).await,
        // FEATURE HOOK: one arm per new subcommand.
    }
}
