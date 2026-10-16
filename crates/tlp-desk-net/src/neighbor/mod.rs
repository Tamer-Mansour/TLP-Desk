//! Hardware (MAC) addresses of LAN neighbours, read from the OS neighbour (ARP / NDP) cache.
//!
//! Wake-on-LAN needs the MAC address of a device that is switched off. The address book learns
//! it while the device is on: after a connection (or a discovery hit) the OS has the peer's
//! IP -> MAC mapping in its neighbour cache, and [`neighbor_mac`] reads it from there.
//!
//! This module only **reads** the local cache. It never sends ARP requests or any other
//! packet, so looking up an address that is not cached simply returns `None`.

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
// The parser is plain text handling, so its tests also run on other hosts.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;

use std::net::IpAddr;

use crate::NetError;

/// One usable entry of the neighbour cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Neighbor {
    /// The neighbour's IP address.
    pub ip: IpAddr,
    /// Its 48-bit hardware (MAC) address.
    pub mac: [u8; 6],
}

/// Reachability of a raw cache row, as far as MAC learning cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowState {
    /// The OS has (or had) a resolved hardware address for the IP.
    Resolved,
    /// Resolution failed or is still in progress: the hardware address is not valid.
    Unresolved,
}

/// A raw cache row as the platform reports it, before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawRow {
    pub ip: IpAddr,
    pub hw: Vec<u8>,
    pub state: RowState,
}

/// Keeps only rows whose hardware address is a real unicast Ethernet MAC.
///
/// Skipped: unresolved rows, addresses that are not 6 bytes (tunnels, loopback), the all-zero
/// address, the broadcast address and multicast addresses (bit 0 of the first octet).
pub(crate) fn usable(rows: impl IntoIterator<Item = RawRow>) -> Vec<Neighbor> {
    rows.into_iter()
        .filter(|r| r.state == RowState::Resolved)
        .filter_map(|r| {
            let mac: [u8; 6] = r.hw.as_slice().try_into().ok()?;
            let first = mac.first().copied().unwrap_or(0);
            let unicast = first & 0x01 == 0;
            let zero = mac.iter().all(|b| *b == 0);
            (unicast && !zero).then_some(Neighbor { ip: r.ip, mac })
        })
        .collect()
}

/// The usable entries of the OS neighbour cache (IPv4 ARP and IPv6 neighbour discovery).
///
/// # Errors
///
/// [`NetError::Io`] when the OS refuses to return the table. On platforms without an
/// implementation the table is empty.
pub fn neighbor_table() -> Result<Vec<Neighbor>, NetError> {
    #[cfg(windows)]
    {
        windows::table().map(usable)
    }
    #[cfg(target_os = "linux")]
    {
        linux::table().map(usable)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Ok(Vec::new())
    }
}

/// The MAC address the OS has cached for `ip`, if any. IPv4-mapped IPv6 addresses are looked
/// up as IPv4. Errors are logged and reported as `None`: callers treat a missing MAC as
/// "not learned yet".
pub fn neighbor_mac(ip: IpAddr) -> Option<[u8; 6]> {
    let ip = canonical(ip);
    match neighbor_table() {
        Ok(table) => find(&table, ip),
        Err(e) => {
            tracing::debug!(%e, "cannot read the neighbour cache");
            None
        }
    }
}

/// `::ffff:a.b.c.d` -> `a.b.c.d`; every other address unchanged.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

fn find(table: &[Neighbor], ip: IpAddr) -> Option<[u8; 6]> {
    table.iter().find(|n| canonical(n.ip) == ip).map(|n| n.mac)
}

/// Formats a MAC as `aa:bb:cc:dd:ee:ff` (lowercase), the form [`crate::parse_mac`] accepts.
pub fn format_mac(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ip: &str, hw: &[u8], state: RowState) -> RawRow {
        RawRow {
            ip: ip.parse().unwrap(),
            hw: hw.to_vec(),
            state,
        }
    }

    #[test]
    fn usable_keeps_only_resolved_unicast_ethernet() {
        let good = [0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e];
        let rows = vec![
            row("192.168.1.20", &good, RowState::Resolved),
            row("192.168.1.21", &good, RowState::Unresolved),
            row("192.168.1.22", &[0; 6], RowState::Resolved),
            row("192.168.1.255", &[0xff; 6], RowState::Resolved),
            row(
                "224.0.0.251",
                &[0x01, 0x00, 0x5e, 0, 0, 0xfb],
                RowState::Resolved,
            ),
            row("10.0.0.1", &[1, 2, 3], RowState::Resolved),
            row("10.0.0.2", &[], RowState::Resolved),
        ];
        let got = usable(rows);
        assert_eq!(
            got,
            vec![Neighbor {
                ip: "192.168.1.20".parse().unwrap(),
                mac: good
            }]
        );
    }

    #[test]
    fn lookup_matches_ipv4_mapped_addresses() {
        let mac = [0x02, 0, 0, 0, 0, 1];
        let table = vec![Neighbor {
            ip: "10.1.2.3".parse().unwrap(),
            mac,
        }];
        let mapped: IpAddr = "::ffff:10.1.2.3".parse().unwrap();
        assert_eq!(find(&table, canonical(mapped)), Some(mac));
        assert_eq!(find(&table, "10.1.2.4".parse().unwrap()), None);
    }

    #[test]
    fn mac_formats_round_trip_through_parse() {
        let mac = [0x00, 0x1a, 0x2b, 0xcc, 0xdd, 0xef];
        let s = format_mac(mac);
        assert_eq!(s, "00:1a:2b:cc:dd:ef");
        assert_eq!(crate::parse_mac(&s), Some(mac));
    }

    /// Reads the real cache (read-only, sends nothing). Must not fail on a normal machine.
    #[test]
    fn reading_the_real_cache_does_not_fail() {
        let table = neighbor_table().unwrap();
        for n in table {
            assert_eq!(n.mac.first().map(|b| b & 1), Some(0));
        }
    }
}
