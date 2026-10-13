use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// An IPv4 or IPv6 CIDR range, e.g. `192.168.0.0/16` or `fe80::/10`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `None` if `prefix` is longer than the address.
    pub fn new(addr: IpAddr, prefix: u8) -> Option<Self> {
        let max = if addr.is_ipv4() { 32 } else { 128 };
        (prefix <= max).then_some(Self { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, normalize(ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
                (u32::from(net) & mask) == (u32::from(ip) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix as u32).unwrap_or(0);
                (u128::from(net) & mask) == (u128::from(ip) & mask)
            }
            _ => false,
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Error parsing a [`Cidr`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid CIDR: {0}")]
pub struct BadCidr(pub String);

impl FromStr for Cidr {
    type Err = BadCidr;

    /// `a.b.c.d/n`, `x::y/n`, or a bare address (host route).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadCidr(s.to_owned());
        let (a, p) = match s.trim().split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s.trim(), None),
        };
        let addr: IpAddr = a.parse().map_err(|_| bad())?;
        let prefix = match p {
            Some(p) => p.parse().map_err(|_| bad())?,
            None if addr.is_ipv4() => 32,
            None => 128,
        };
        Self::new(addr, prefix).ok_or_else(bad)
    }
}

/// Maps IPv4-mapped IPv6 (`::ffff:a.b.c.d`, seen on dual-stack sockets) back to IPv4.
pub fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// Which peer addresses may connect to (or be discovered by) this device.
///
/// Default ([`AllowList::lan_default`]): RFC 1918 private IPv4, IPv4 link-local, loopback,
/// IPv6 unique-local (`fc00::/7`), IPv6 link-local (`fe80::/10`) and `::1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowList {
    ranges: Vec<Cidr>,
}

impl Default for AllowList {
    fn default() -> Self {
        Self::lan_default()
    }
}

impl AllowList {
    pub fn lan_default() -> Self {
        let v4 = |a, b, c, d, p| Cidr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), p).unwrap();
        let v6 = |s: &str, p| Cidr::new(IpAddr::V6(s.parse::<Ipv6Addr>().unwrap()), p).unwrap();
        Self {
            ranges: vec![
                v4(10, 0, 0, 0, 8),
                v4(172, 16, 0, 0, 12),
                v4(192, 168, 0, 0, 16),
                v4(169, 254, 0, 0, 16),
                v4(127, 0, 0, 0, 8),
                v6("fc00::", 7),
                v6("fe80::", 10),
                v6("::1", 128),
            ],
        }
    }

    /// Custom list (replaces the default). An empty list allows nobody.
    pub fn from_ranges(ranges: Vec<Cidr>) -> Self {
        Self { ranges }
    }

    /// Parses a list like `["192.168.1.0/24", "10.0.0.0/8"]`.
    pub fn parse<S: AsRef<str>>(items: &[S]) -> Result<Self, BadCidr> {
        items
            .iter()
            .map(|s| s.as_ref().parse())
            .collect::<Result<_, _>>()
            .map(Self::from_ranges)
    }

    pub fn ranges(&self) -> &[Cidr] {
        &self.ranges
    }

    pub fn is_allowed(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        self.ranges.iter().any(|r| r.contains(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn allow_list_default_accepts_lan_only() {
        let a = AllowList::default();
        for ok in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.50",
            "169.254.10.10",
            "127.0.0.1",
            "::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:192.168.1.5",
        ] {
            assert!(a.is_allowed(ip(ok)), "{ok} should be allowed");
        }
        for bad in [
            "8.8.8.8",
            "172.32.0.1",
            "172.15.255.255",
            "100.64.0.1",
            "192.169.0.1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
            "0.0.0.0",
        ] {
            assert!(!a.is_allowed(ip(bad)), "{bad} should be rejected");
        }
    }

    #[test]
    fn allow_list_custom_cidrs() {
        let a = AllowList::parse(&["192.168.5.0/24", "10.9.9.9"]).unwrap();
        assert!(a.is_allowed(ip("192.168.5.77")));
        assert!(!a.is_allowed(ip("192.168.6.1")));
        assert!(a.is_allowed(ip("10.9.9.9")));
        assert!(!a.is_allowed(ip("10.9.9.8")));
        assert!(!AllowList::from_ranges(vec![]).is_allowed(ip("127.0.0.1")));
        assert!(AllowList::parse(&["0.0.0.0/0"])
            .unwrap()
            .is_allowed(ip("1.1.1.1")));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("nonsense".parse::<Cidr>().is_err());
    }
}
