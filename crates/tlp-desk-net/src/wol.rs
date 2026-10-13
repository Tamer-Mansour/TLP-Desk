//! Wake-on-LAN magic packets (LAN broadcast only).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use crate::NetError;

/// Standard WoL UDP port.
pub const WOL_PORT: u16 = 9;

/// Parses `aa:bb:cc:dd:ee:ff`, `aa-bb-cc-dd-ee-ff` or `aabbccddeeff`.
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let hex: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.' | ' '))
        .collect();
    if hex.len() != 12 || !hex.is_ascii() {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(mac)
}

/// 6 x `0xFF` followed by the MAC repeated 16 times (102 bytes).
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut p = [0xFFu8; 102];
    for chunk in p[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&mac);
    }
    p
}

/// Sends a magic packet for `mac`. With `target = None` it goes to every IPv4 interface's
/// subnet broadcast and `255.255.255.255`, port 9.
pub fn send_wake_on_lan(mac: [u8; 6], target: Option<SocketAddr>) -> Result<(), NetError> {
    let sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))?;
    sock.set_broadcast(true)?;
    let packet = magic_packet(mac);
    let targets = match target {
        Some(t) => vec![t],
        None => crate::discovery::broadcast_targets(WOL_PORT),
    };
    let mut sent = false;
    let mut last_err = None;
    for t in targets {
        match sock.send_to(&packet, t) {
            Ok(_) => sent = true,
            Err(e) => last_err = Some(e),
        }
    }
    match (sent, last_err) {
        (true, _) => Ok(()),
        (false, Some(e)) => Err(e.into()),
        (false, None) => Err(NetError::Discovery("no broadcast target".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wol_magic_packet_layout() {
        let mac = parse_mac("01:23:45:67:89:ab").unwrap();
        assert_eq!(mac, [0x01, 0x23, 0x45, 0x67, 0x89, 0xab]);
        assert_eq!(parse_mac("01-23-45-67-89-AB"), Some(mac));
        assert_eq!(parse_mac("0123456789ab"), Some(mac));
        assert_eq!(parse_mac("01:23:45"), None);
        assert_eq!(parse_mac("zz:23:45:67:89:ab"), None);
        let p = magic_packet(mac);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..12 + i * 6], &mac);
        }
    }

    #[test]
    fn wol_sends_to_explicit_target() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mac = [1, 2, 3, 4, 5, 6];
        send_wake_on_lan(mac, Some(rx.local_addr().unwrap())).unwrap();
        let mut buf = [0u8; 200];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &magic_packet(mac)[..]);
    }
}
