//! Wake-on-LAN for saved devices: where to send the magic packet.
//!
//! A switched-off PC has no IP address, so the packet is broadcast. The limited broadcast
//! `255.255.255.255` leaves through one interface only (Windows picks it), which misses the
//! device on a PC with several networks (Wi-Fi + Ethernet, VPN, Hyper-V switch). So when we
//! know an address the device used, we send to the *directed* broadcast of each local
//! interface whose subnet contains that address. Without a match we fall back to every
//! interface's broadcast ([`tlp_desk_net::send_wake_on_lan`] with no target).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use super::MacAddr;

/// A local IPv4 interface address with its netmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Iface {
    /// This PC's address on the interface.
    pub ip: Ipv4Addr,
    /// The interface's netmask.
    pub netmask: Ipv4Addr,
}

impl Iface {
    fn contains(self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask);
        u32::from(self.ip) & mask == u32::from(ip) & mask
    }

    /// The directed broadcast address, `None` for /31 and /32 (no broadcast address).
    fn broadcast(self) -> Option<Ipv4Addr> {
        let mask = u32::from(self.netmask);
        if mask.leading_ones() >= 31 || self.ip.is_loopback() {
            return None;
        }
        Some(Ipv4Addr::from(u32::from(self.ip) | !mask))
    }
}

/// This PC's IPv4 interfaces (local query, no network I/O).
pub fn local_ifaces() -> Vec<Iface> {
    if_addrs::get_if_addrs()
        .map(|ifs| {
            ifs.into_iter()
                .filter_map(|i| match i.addr {
                    if_addrs::IfAddr::V4(v4) => Some(Iface {
                        ip: v4.ip,
                        netmask: v4.netmask,
                    }),
                    if_addrs::IfAddr::V6(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Directed-broadcast targets (UDP port 9) for a device last seen at `last_ips`.
/// Empty when no local subnet contains any of them: the caller then broadcasts on every
/// interface.
pub fn targets(last_ips: &[IpAddr], ifaces: &[Iface]) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    for ip in last_ips {
        let v4 = match ip {
            IpAddr::V4(v4) => *v4,
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => v4,
                None => continue,
            },
        };
        for iface in ifaces.iter().filter(|i| i.contains(v4)) {
            if let Some(b) = iface.broadcast() {
                let t = SocketAddr::new(IpAddr::V4(b), tlp_desk_net::WOL_PORT);
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
    }
    out
}

/// Send a magic packet for `mac` to `targets`, or to every interface's broadcast when
/// `targets` is empty. Succeeds if at least one packet left this PC.
///
/// # Errors
///
/// The last send error when no packet could be sent.
pub fn send(mac: MacAddr, targets: &[SocketAddr]) -> Result<(), tlp_desk_net::NetError> {
    if targets.is_empty() {
        return tlp_desk_net::send_wake_on_lan(mac.0, None);
    }
    let mut last_err = None;
    let mut sent = false;
    for t in targets {
        match tlp_desk_net::send_wake_on_lan(mac.0, Some(*t)) {
            Ok(()) => sent = true,
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) if !sent => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(ip: [u8; 4], mask: [u8; 4]) -> Iface {
        Iface {
            ip: Ipv4Addr::from(ip),
            netmask: Ipv4Addr::from(mask),
        }
    }

    #[test]
    fn picks_the_directed_broadcast_of_the_matching_subnet() {
        let ifaces = [
            iface([192, 168, 1, 10], [255, 255, 255, 0]),
            iface([10, 20, 0, 5], [255, 255, 0, 0]),
            iface([172, 16, 5, 1], [255, 255, 255, 252]),
        ];
        let ips: Vec<IpAddr> = vec!["10.20.33.44".parse().unwrap()];
        assert_eq!(
            targets(&ips, &ifaces),
            vec!["10.20.255.255:9".parse::<SocketAddr>().unwrap()]
        );
        let ips: Vec<IpAddr> = vec![
            "192.168.1.77".parse().unwrap(),
            "::ffff:192.168.1.78".parse().unwrap(),
            "172.16.5.2".parse().unwrap(),
        ];
        assert_eq!(
            targets(&ips, &ifaces),
            vec![
                "192.168.1.255:9".parse::<SocketAddr>().unwrap(),
                "172.16.5.3:9".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn unknown_subnet_ipv6_and_point_to_point_fall_back_to_all_interfaces() {
        let ifaces = [
            iface([192, 168, 1, 10], [255, 255, 255, 0]),
            iface([10, 9, 9, 1], [255, 255, 255, 255]),
            iface([127, 0, 0, 1], [255, 0, 0, 0]),
        ];
        let ips: Vec<IpAddr> = vec![
            "192.168.2.5".parse().unwrap(),
            "fd00::5".parse().unwrap(),
            "10.9.9.1".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ];
        assert!(targets(&ips, &ifaces).is_empty());
        assert!(targets(&[], &ifaces).is_empty());
    }

    /// Sends to an explicit loopback receiver only: no real WOL broadcast leaves the PC.
    #[test]
    fn send_reaches_explicit_loopback_targets() {
        use std::net::UdpSocket;
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mac = MacAddr([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]);
        send(mac, &[rx.local_addr().unwrap()]).unwrap();
        let mut buf = [0u8; 256];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &tlp_desk_net::magic_packet(mac.0)[..]);
    }
}
