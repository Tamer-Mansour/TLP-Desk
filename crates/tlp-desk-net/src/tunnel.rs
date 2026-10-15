//! Building blocks for TCP port forwarding (`StreamKind::Tunnel`), shared by the host and
//! viewer tunnel features:
//!
//! * [`pipe_tcp`]: copies bytes both ways between a TCP connection and one bidirectional
//!   QUIC side stream, with half-close, an idle timeout, cancellation and byte counters.
//!   Backpressure comes from QUIC flow control: a slow reader on either side stalls the
//!   writer instead of buffering without bound (one fixed buffer per direction).
//! * [`resolve_target`]: the host's check of where a tunnel may lead. Only IP literals (and
//!   `localhost`) inside the [`AllowList`] are accepted, so a tunnel can never reach the
//!   internet and no DNS query ever leaves the LAN.
//! * [`close_reason`]: stable, machine-readable `TunnelClose::error` strings that the viewer
//!   translates for its user.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use quinn::{RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::session::STREAM_REFUSED;
use crate::AllowList;

/// Copy buffer per direction and connection.
pub const TUNNEL_BUF: usize = 64 * 1024;

/// Longest `target_host` accepted from a peer (a DNS name is at most 253 bytes).
pub const MAX_TARGET_HOST_LEN: usize = 253;

/// Longest `TunnelClose::error` text we keep from a peer (longer text is cut).
pub const MAX_CLOSE_TEXT: usize = 200;

/// Machine-readable `TunnelClose::error` values. The viewer maps them to translated text;
/// anything else is shown as sanitized free text.
pub mod close_reason {
    /// The target host is outside the host's allow-list (e.g. a public internet address).
    pub const TARGET_NOT_ALLOWED: &str = "target-not-allowed";
    /// The target is malformed (port 0, not an IP address, too long...).
    pub const BAD_TARGET: &str = "bad-target";
    /// The session already has the maximum number of tunnels.
    pub const TOO_MANY_TUNNELS: &str = "too-many-tunnels";
    /// A tunnel with this id is already open.
    pub const DUPLICATE_ID: &str = "duplicate-id";
    /// The session lacks the `TUNNEL` permission.
    pub const PERMISSION_DENIED: &str = "permission-denied";
    /// The host user revoked the `TUNNEL` permission; every tunnel was closed.
    pub const PERMISSION_REVOKED: &str = "permission-revoked";
}

/// Why a target was refused by [`resolve_target`]; [`TargetError::reason`] is the wire text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetError {
    /// Malformed: port 0, empty, too long, or a host name instead of an IP address.
    Bad,
    /// Well-formed but outside the allow-list (or unspecified/multicast/broadcast).
    NotAllowed,
}

impl TargetError {
    /// The [`close_reason`] string for `TunnelClose::error`.
    pub fn reason(self) -> &'static str {
        match self {
            TargetError::Bad => close_reason::BAD_TARGET,
            TargetError::NotAllowed => close_reason::TARGET_NOT_ALLOWED,
        }
    }
}

/// Checks a `TunnelOpen` target on the host. `None`, empty or `localhost` mean the host
/// itself (`127.0.0.1`). Host names other than `localhost` are refused on purpose: resolving
/// them could send a DNS query beyond the LAN, and the resolved address could change between
/// the check and the connect.
pub fn resolve_target(
    host: Option<&str>,
    port: u16,
    allow: &AllowList,
) -> Result<SocketAddr, TargetError> {
    if port == 0 {
        return Err(TargetError::Bad);
    }
    let host = host.map(str::trim).unwrap_or("");
    if host.len() > MAX_TARGET_HOST_LEN {
        return Err(TargetError::Bad);
    }
    let ip = if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        let literal = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        literal
            .parse::<IpAddr>()
            .map_err(|_| TargetError::Bad)
            .map(crate::normalize)?
    };
    let forbidden = ip.is_unspecified()
        || ip.is_multicast()
        || matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
    if forbidden || !allow.is_allowed(ip) {
        return Err(TargetError::NotAllowed);
    }
    Ok(SocketAddr::new(ip, port))
}

/// Keeps a peer-supplied close text printable and short (it ends up in logs and the UI).
pub fn sanitize_close_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLOSE_TEXT)
        .collect()
}

/// Byte counters of one connection or one whole tunnel (lock-free, shared with the UI).
#[derive(Debug, Default)]
pub struct TunnelCounters {
    /// Bytes read from the local TCP side and sent into the stream.
    pub tcp_to_stream: AtomicU64,
    /// Bytes received from the stream and written to the local TCP side.
    pub stream_to_tcp: AtomicU64,
}

impl TunnelCounters {
    /// `(tcp_to_stream, stream_to_tcp)`.
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.tcp_to_stream.load(Ordering::Relaxed),
            self.stream_to_tcp.load(Ordering::Relaxed),
        )
    }
}

/// How a [`pipe_tcp`] run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeEnd {
    /// Both directions finished cleanly (each side half-closed its sending direction).
    Done,
    /// No byte moved in either direction for the idle timeout.
    Idle,
    /// The `cancel` future resolved (tunnel closed, permission revoked, session over).
    Cancelled,
    /// A read or write failed (peer reset, TCP error...).
    Error(String),
}

/// Copies bytes between `tcp` and a tunnel stream until both directions are finished.
///
/// * **Half-close:** TCP EOF finishes the QUIC send direction; a finished QUIC stream shuts
///   down the TCP write half. The other direction keeps flowing until it ends too.
/// * **Idle:** after `idle` without traffic the connection is aborted ([`PipeEnd::Idle`]).
/// * **Abort:** on cancellation, idle or error the QUIC stream is reset / stopped with
///   [`STREAM_REFUSED`] and the TCP connection is dropped, so neither peer waits forever.
/// * Each byte moved is added to every counter in `counters` (e.g. the connection's and
///   the tunnel's).
pub async fn pipe_tcp(
    tcp: TcpStream,
    mut send: SendStream,
    mut recv: RecvStream,
    idle: Duration,
    cancel: impl Future<Output = ()>,
    counters: &[&TunnelCounters],
) -> PipeEnd {
    let _ = tcp.set_nodelay(true);
    let (mut tcp_rd, mut tcp_wr) = tcp.into_split();
    let base = Instant::now();
    let last_ms = AtomicU64::new(0);
    let touch = || {
        let ms = u64::try_from(base.elapsed().as_millis()).unwrap_or(u64::MAX);
        last_ms.store(ms, Ordering::Relaxed);
    };

    let up = async {
        let mut buf = vec![0u8; TUNNEL_BUF];
        loop {
            let n = tcp_rd.read(&mut buf).await?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                // TCP half-close: finish our QUIC direction, keep receiving.
                send.finish().map_err(io::Error::other)?;
                return Ok::<(), io::Error>(());
            };
            send.write_all(chunk).await.map_err(io::Error::from)?;
            for c in counters {
                c.tcp_to_stream
                    .fetch_add(u64::try_from(n).unwrap_or(0), Ordering::Relaxed);
            }
            touch();
        }
    };
    let down = async {
        let mut buf = vec![0u8; TUNNEL_BUF];
        loop {
            let n = match recv.read(&mut buf).await.map_err(io::Error::from)? {
                Some(n) => n,
                None => {
                    // The peer finished its direction: half-close the TCP side.
                    tcp_wr.shutdown().await?;
                    return Ok::<(), io::Error>(());
                }
            };
            let Some(chunk) = buf.get(..n) else {
                return Err(io::Error::other("read past the buffer"));
            };
            tcp_wr.write_all(chunk).await?;
            for c in counters {
                c.stream_to_tcp
                    .fetch_add(u64::try_from(n).unwrap_or(0), Ordering::Relaxed);
            }
            touch();
        }
    };
    let idle_watch = async {
        loop {
            let last = Duration::from_millis(last_ms.load(Ordering::Relaxed));
            let due = last.saturating_add(idle);
            let now = base.elapsed();
            if now >= due {
                return;
            }
            tokio::time::sleep(due - now).await;
        }
    };

    let end = tokio::select! {
        r = async { tokio::try_join!(up, down) } => match r {
            Ok(_) => PipeEnd::Done,
            Err(e) => PipeEnd::Error(e.to_string()),
        },
        () = cancel => PipeEnd::Cancelled,
        () = idle_watch => PipeEnd::Idle,
    };
    if end != PipeEnd::Done {
        let _ = send.reset(STREAM_REFUSED.into());
        let _ = recv.stop(STREAM_REFUSED.into());
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow() -> AllowList {
        AllowList::lan_default()
    }

    #[test]
    fn local_and_lan_targets_are_allowed() {
        let a = allow();
        for (host, want) in [
            (None, "127.0.0.1:80"),
            (Some(""), "127.0.0.1:80"),
            (Some("localhost"), "127.0.0.1:80"),
            (Some("LocalHost"), "127.0.0.1:80"),
            (Some("127.0.0.1"), "127.0.0.1:80"),
            (Some("192.168.1.20"), "192.168.1.20:80"),
            (Some(" 10.0.0.5 "), "10.0.0.5:80"),
            (Some("::1"), "[::1]:80"),
            (Some("[::1]"), "[::1]:80"),
            (Some("::ffff:192.168.1.5"), "192.168.1.5:80"),
            (Some("fd00::7"), "[fd00::7]:80"),
        ] {
            assert_eq!(
                resolve_target(host, 80, &a),
                Ok(want.parse().unwrap()),
                "{host:?}"
            );
        }
    }

    #[test]
    fn internet_and_special_targets_are_refused() {
        let a = allow();
        for host in [
            "8.8.8.8",
            "1.1.1.1",
            "100.64.0.1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
            "0.0.0.0",
            "::",
            "255.255.255.255",
            "224.0.0.251",
            "ff02::1",
        ] {
            assert_eq!(
                resolve_target(Some(host), 80, &a),
                Err(TargetError::NotAllowed),
                "{host}"
            );
        }
    }

    #[test]
    fn malformed_targets_are_refused() {
        let a = allow();
        assert_eq!(resolve_target(None, 0, &a), Err(TargetError::Bad));
        let long = "1".repeat(MAX_TARGET_HOST_LEN + 1);
        for host in [
            "example.com",
            "nas.local",
            "مكتب",
            "192.168.1.1:80",
            long.as_str(),
        ] {
            assert_eq!(
                resolve_target(Some(host), 80, &a),
                Err(TargetError::Bad),
                "{host}"
            );
        }
        // A custom allow-list is honoured (an empty one allows nothing, not even loopback).
        assert_eq!(
            resolve_target(None, 80, &AllowList::from_ranges(vec![])),
            Err(TargetError::NotAllowed)
        );
    }

    #[test]
    fn close_text_is_sanitized() {
        assert_eq!(sanitize_close_text("ok\r\n\u{1b}[31m"), "ok[31m");
        assert_eq!(sanitize_close_text("خطأ في الاتصال"), "خطأ في الاتصال");
        assert_eq!(
            sanitize_close_text(&"x".repeat(1000)).chars().count(),
            MAX_CLOSE_TEXT
        );
    }
}
