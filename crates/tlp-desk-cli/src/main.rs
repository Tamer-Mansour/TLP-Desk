//! Headless TLP-Desk CLI for scripts and agents: print identity, list LAN devices, host a
//! session (real capture/encode/input), or connect as a viewer and report statistics.
//!
//! The command structure lives in [`commands`] (one module per subcommand).
// The only `unsafe` is the console code-page call in `console` (documented there).
#![deny(unsafe_code)]

mod commands;
mod console;
mod exit;
mod output;
mod text;

use std::process::ExitCode;

use clap::FromArgMatches;

fn main() -> ExitCode {
    console::enable_utf8_output();
    // Arabic `--help` needs the language before clap parses, so look for it in argv first.
    let args: Vec<String> = std::env::args().collect();
    let lang =
        commands::help::requested_lang(&args, std::env::var("TLP_DESK_LANG").ok().as_deref());
    let matches = commands::help::command(lang).get_matches();
    let cli = match commands::Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
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
        Err(e) => ExitCode::from(cli.output().error(&e, &text::error_line(&e)).code()),
    }
}
