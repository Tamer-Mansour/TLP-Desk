//! Forwarding rules: what to listen on locally and where the host should connect.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;

use crate::ViewerError;

/// One port forwarding rule: listen on `bind` on the viewer, connect to
/// `target_host:target_port` as seen from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardSpec {
    /// Local listening address. Default `127.0.0.1:<port>`; port 0 picks a free port (the
    /// real one is in [`crate::TunnelStatus::local`]).
    pub bind: SocketAddr,
    /// Must be `true` to listen on anything but loopback: other devices on the network could
    /// then use the tunnel too. The UI asks for an explicit confirmation.
    pub allow_lan_bind: bool,
    /// Target as seen from the host: an IP address (`None` = the host itself). The host only
    /// accepts loopback and LAN addresses.
    pub target_host: Option<String>,
    pub target_port: u16,
}

impl ForwardSpec {
    /// `127.0.0.1:local_port` -> `target_host:target_port` on the host.
    pub fn local(local_port: u16, target_host: Option<String>, target_port: u16) -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), local_port),
            allow_lan_bind: false,
            target_host: target_host.filter(|h| !h.trim().is_empty()),
            target_port,
        }
    }

    /// Whether listening on [`ForwardSpec::bind`] exposes the tunnel to other devices.
    pub fn binds_beyond_loopback(&self) -> bool {
        !self.bind.ip().is_loopback()
    }

    /// The target as `host:port` for display (`localhost` when `target_host` is `None`).
    pub fn target_display(&self) -> String {
        target_display(self.target_host.as_deref(), self.target_port)
    }
}

/// `host:port`, with IPv6 in brackets and `localhost` for the host itself.
pub(crate) fn target_display(host: Option<&str>, port: u16) -> String {
    match host {
        None => format!("localhost:{port}"),
        Some(h) if h.contains(':') && !h.starts_with('[') => format!("[{h}]:{port}"),
        Some(h) => format!("{h}:{port}"),
    }
}

impl fmt::Display for ForwardSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.bind, self.target_display())
    }
}

/// Arabic-Indic (`٠-٩`) and Extended Arabic-Indic (`۰-۹`) digits become ASCII, so rules
/// typed with an Arabic keyboard work.
fn western_digits(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{0660}'..='\u{0669}' => char::from_digit(u32::from(c) - 0x0660, 10).unwrap_or(c),
            '\u{06F0}'..='\u{06F9}' => char::from_digit(u32::from(c) - 0x06F0, 10).unwrap_or(c),
            c => c,
        })
        .collect()
}

/// Splits on `:` outside `[...]`.
fn split_fields(s: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let mut depth = 0u8;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '[' if depth == 0 => depth = 1,
            ']' if depth == 1 => depth = 0,
            '[' | ']' => return None,
            ':' if depth == 0 => {
                out.push(s.get(start..i)?);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    out.push(s.get(start..)?);
    Some(out)
}

fn port(s: &str) -> Option<u16> {
    s.trim().parse().ok()
}

fn unbracket(s: &str) -> &str {
    s.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(s)
}

impl FromStr for ForwardSpec {
    type Err = ViewerError;

    /// `LOCAL_PORT:REMOTE_PORT`, `LOCAL_PORT:REMOTE_HOST:REMOTE_PORT` or
    /// `BIND_IP:LOCAL_PORT:REMOTE_HOST:REMOTE_PORT` (IPv6 in brackets, e.g.
    /// `8080:[::1]:80`). A non-loopback `BIND_IP` sets [`ForwardSpec::allow_lan_bind`]
    /// because the user typed it explicitly; callers should still warn.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let bad = || ViewerError::TunnelSpec(input.to_owned());
        let s = western_digits(input.trim());
        let fields = split_fields(&s).ok_or_else(bad)?;
        let (bind_ip, local, host, remote) = match fields.as_slice() {
            [l, r] => (None, *l, None, *r),
            [l, h, r] => (None, *l, Some(*h), *r),
            [b, l, h, r] => (Some(*b), *l, Some(*h), *r),
            _ => return Err(bad()),
        };
        let local_port = port(local).ok_or_else(bad)?;
        let target_port = port(remote).filter(|p| *p != 0).ok_or_else(bad)?;
        let target_host = match host.map(|h| unbracket(h.trim())) {
            None | Some("") => None,
            Some(h) if h.eq_ignore_ascii_case("localhost") => None,
            Some(h) => Some(h.parse::<IpAddr>().map_err(|_| bad())?.to_string()),
        };
        let mut spec = ForwardSpec::local(local_port, target_host, target_port);
        if let Some(b) = bind_ip {
            let ip: IpAddr = unbracket(b.trim()).parse().map_err(|_| bad())?;
            spec.bind = SocketAddr::new(ip, local_port);
            spec.allow_lan_bind = !ip.is_loopback();
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ForwardSpec {
        s.parse().unwrap()
    }

    #[test]
    fn short_forms_listen_on_loopback() {
        let s = p("8080:80");
        assert_eq!(s.bind, "127.0.0.1:8080".parse().unwrap());
        assert!(!s.allow_lan_bind && !s.binds_beyond_loopback());
        assert_eq!((s.target_host.as_deref(), s.target_port), (None, 80));
        assert_eq!(s.target_display(), "localhost:80");

        let s = p("8080:192.168.1.5:3389");
        assert_eq!(s.target_host.as_deref(), Some("192.168.1.5"));
        assert_eq!(s.target_port, 3389);
        assert_eq!(s.to_string(), "127.0.0.1:8080 -> 192.168.1.5:3389");

        assert_eq!(p("8080:localhost:80").target_host, None);
        assert_eq!(p("0:80").bind.port(), 0);
    }

    #[test]
    fn ipv6_and_bind_address() {
        let s = p("8080:[::1]:80");
        assert_eq!(s.target_host.as_deref(), Some("::1"));
        assert_eq!(s.target_display(), "[::1]:80");

        let s = p("0.0.0.0:8080:10.0.0.2:22");
        assert_eq!(s.bind, "0.0.0.0:8080".parse().unwrap());
        assert!(s.allow_lan_bind && s.binds_beyond_loopback());

        let s = p("[::1]:8080:127.0.0.1:443");
        assert!(s.bind.ip().is_loopback() && !s.allow_lan_bind);
    }

    #[test]
    fn arabic_digits_are_accepted() {
        let s = p("٨٠٨٠:١٩٢.١٦٨.١.٥:٨٠");
        assert_eq!(s.bind.port(), 8080);
        assert_eq!(s.target_host.as_deref(), Some("192.168.1.5"));
        assert_eq!(s.target_port, 80);
        assert_eq!(p("۸۰۸۰:۸۰").target_port, 80);
    }

    #[test]
    fn bad_rules_are_rejected() {
        for bad in [
            "",
            "8080",
            "8080:0",
            "70000:80",
            "8080:example.com:80",
            "8080:مكتب:80",
            "8080:::1:80",
            "8080:[::1:80",
            "a:b:c:d:e",
            "x:8080:1.2.3.4:80",
        ] {
            assert!(
                matches!(bad.parse::<ForwardSpec>(), Err(ViewerError::TunnelSpec(_))),
                "{bad:?} should be rejected"
            );
        }
    }
}
