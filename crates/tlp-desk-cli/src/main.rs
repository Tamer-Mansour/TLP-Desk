//! Headless TLP-Desk CLI for scripts and agents: print identity, list LAN devices, host a
//! session (real capture/encode/input), or connect as a viewer and report statistics.
//!
//! The command structure lives in [`commands`] (one module per subcommand).
#![forbid(unsafe_code)]

mod commands;
mod text;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = commands::Cli::parse();
    text::set_lang(cli.lang);
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
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| rt.block_on(commands::run(&cli)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", text::error_line(&e));
            ExitCode::FAILURE
        }
    }
}
