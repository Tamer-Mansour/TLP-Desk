//! Linux IPv4 neighbour cache from `/proc/net/arp` (documented in `proc(5)`). Read-only.
//!
//! IPv6 neighbours would need a netlink query and are not reported; Wake-on-LAN targets are
//! IPv4 in practice.

use super::{RawRow, RowState};
use crate::NetError;

/// Upper bound on rows read, so a pathological file cannot make us allocate without end.
const MAX_ROWS: usize = 65_536;

/// ARP flag `ATF_COM`: the entry is complete (the hardware address is valid).
const ATF_COM: u32 = 0x2;

/// Reads and parses `/proc/net/arp`. A missing file (no IPv4 stack, restricted sandbox) is an
/// empty table, not an error.
pub(super) fn table() -> Result<Vec<RawRow>, NetError> {
    match std::fs::read_to_string("/proc/net/arp") {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(NetError::Io(e)),
    }
}

/// Parses the text of `/proc/net/arp`; malformed lines are skipped.
pub(super) fn parse(text: &str) -> Vec<RawRow> {
    text.lines()
        .skip(1) // header
        .filter_map(parse_line)
        .take(MAX_ROWS)
        .collect()
}

fn parse_line(line: &str) -> Option<RawRow> {
    let mut cols = line.split_whitespace();
    let ip = cols.next()?.parse().ok()?;
    let _hw_type = cols.next()?;
    let flags = u32::from_str_radix(cols.next()?.trim_start_matches("0x"), 16).ok()?;
    let hw = parse_mac(cols.next()?)?;
    Some(RawRow {
        ip,
        hw,
        state: if flags & ATF_COM != 0 {
            RowState::Resolved
        } else {
            RowState::Unresolved
        },
    })
}

fn parse_mac(s: &str) -> Option<Vec<u8>> {
    s.split(':')
        .map(|p| u8::from_str_radix(p, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.1.1      0x1         0x2         aa:bb:cc:dd:ee:ff     *        eth0
192.168.1.77     0x1         0x0         00:00:00:00:00:00     *        eth0
garbage line
192.168.1.9      0x1         0x2         zz:bb:cc:dd:ee:ff     *        eth0
";

    #[test]
    fn parses_complete_and_incomplete_rows_and_skips_garbage() {
        let rows = parse(SAMPLE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ip.to_string(), "192.168.1.1");
        assert_eq!(rows[0].hw, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(rows[0].state, RowState::Resolved);
        assert_eq!(rows[1].state, RowState::Unresolved);
    }

    #[test]
    fn empty_and_header_only_inputs_are_empty() {
        assert!(parse("").is_empty());
        assert!(parse("IP address HW type\n").is_empty());
    }
}
