//! `tlp-desk-relay`: the relay server (TLP-relay). Run it on a machine both devices can
//! reach (a small VPS, or a PC at home with one UDP port forwarded), give both devices its
//! address and the token, and a device that is not on the local network becomes reachable
//! by its ID. The relay only copies end-to-end-encrypted packets; see
//! `docs/src/user-guide/en/relay.md` and `tlp_desk_net::relay`.
//!
//! ```text
//! tlp-desk-relay --token 'a long secret' [--listen 0.0.0.0:47402] [--ports 47410-47509]
//! ```
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::ops::RangeInclusive;

use anyhow::{bail, Context, Result};
use clap::Parser;
use tlp_desk_net::relay::server::{RelayServer, RelayServerConfig};
use tlp_desk_net::relay::{Token, DEFAULT_RELAY_PORT};

/// TLP-Desk relay server.
#[derive(Debug, Parser)]
#[command(name = "tlp-desk-relay", version, about)]
struct Args {
    /// Shared secret; every device using this relay must be given the same one.
    #[arg(long, env = "TLP_RELAY_TOKEN", hide_env_values = true)]
    token: String,
    /// Control socket (UDP).
    #[arg(long, default_value_t = SocketAddr::from(([0, 0, 0, 0], DEFAULT_RELAY_PORT)))]
    listen: SocketAddr,
    /// Session port range (UDP), one port per live session, e.g. `47410-47509`.
    #[arg(long, default_value = "47410-47509", value_parser = parse_ports)]
    ports: RangeInclusive<u16>,
    /// Log filter (`RUST_LOG` syntax).
    #[arg(long, default_value = "info")]
    log: String,
}

fn parse_ports(s: &str) -> Result<RangeInclusive<u16>, String> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| "expected LOW-HIGH, e.g. 47410-47509".to_owned())?;
    let lo: u16 = a.trim().parse().map_err(|_| format!("bad port {a}"))?;
    let hi: u16 = b.trim().parse().map_err(|_| format!("bad port {b}"))?;
    if lo == 0 || hi < lo {
        return Err("the range must be LOW-HIGH with LOW >= 1".into());
    }
    Ok(lo..=hi)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| args.log.clone().into()),
        )
        .init();
    if args.token.trim().len() < 12 {
        bail!("the token must be at least 12 characters");
    }
    if args.ports.contains(&args.listen.port()) {
        bail!("the session port range must not contain the control port");
    }
    let server = RelayServer::bind(RelayServerConfig {
        listen: args.listen,
        ports: args.ports.clone(),
        token: Token::from_secret(&args.token)?,
    })
    .await
    .with_context(|| format!("cannot listen on {}", args.listen))?;
    tracing::info!(
        control = %server.control_addr()?,
        sessions = %format!("{}-{}", args.ports.start(), args.ports.end()),
        "tlp-desk-relay listening (open these UDP ports in the firewall)"
    );
    tokio::select! {
        () = server.run() => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_ranges_parse() {
        assert_eq!(
            parse_ports("47410-47509").unwrap(),
            tlp_desk_net::relay::DEFAULT_SESSION_PORTS
        );
        assert_eq!(parse_ports(" 5 - 5 ").unwrap(), 5..=5);
        assert!(parse_ports("5").is_err());
        assert!(parse_ports("9-5").is_err());
        assert!(parse_ports("0-5").is_err());
        assert!(parse_ports("a-5").is_err());
    }
}
