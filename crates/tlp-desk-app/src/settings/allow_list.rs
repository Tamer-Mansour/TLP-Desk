//! Validation behind the allow-list (CIDR) editor in Settings.
//!
//! Every entry is checked with [`AllowList::parse`] (the same parser the endpoint uses), so
//! what the editor accepts is exactly what the host enforces. Ranges that reach beyond the
//! local network are allowed but flagged, so the UI can warn before saving them.

use std::net::IpAddr;

use tlp_desk_net::{AllowList, Cidr};

use crate::i18n::format::to_western_digits;

/// A checked, normalized allow-list entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CidrEntry {
    /// Normalized text (`192.168.1.0/24`; a bare address becomes `/32` or `/128`).
    pub text: String,
    /// The range contains addresses outside RFC 1918 / link-local / loopback.
    pub public: bool,
}

/// Why the editor refuses an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidrError {
    /// Nothing typed.
    Empty,
    /// Not an address or `address/prefix`.
    Invalid,
    /// The same range is already in the list.
    Duplicate,
}

/// Check one typed entry against the current list. Arabic-Indic digits are accepted.
pub fn check_entry(input: &str, existing: &[String]) -> Result<CidrEntry, CidrError> {
    let typed = to_western_digits(input.trim());
    if typed.is_empty() {
        return Err(CidrError::Empty);
    }
    let parsed = AllowList::parse(&[typed.as_str()]).map_err(|_| CidrError::Invalid)?;
    let Some(cidr) = parsed.ranges().first().copied() else {
        return Err(CidrError::Invalid);
    };
    let text = cidr.to_string();
    if existing.iter().any(|e| same_range(e, &cidr)) {
        return Err(CidrError::Duplicate);
    }
    Ok(CidrEntry {
        public: !is_local_range(&text),
        text,
    })
}

fn same_range(entry: &str, cidr: &Cidr) -> bool {
    AllowList::parse(&[entry])
        .ok()
        .and_then(|l| l.ranges().first().copied())
        .is_some_and(|c| c == *cidr)
}

/// Split normalized `addr/prefix` text (as produced by [`Cidr`]'s `Display`).
fn split(text: &str) -> Option<(IpAddr, u8)> {
    let (a, p) = text.split_once('/')?;
    Some((a.parse().ok()?, p.parse().ok()?))
}

/// Whether the whole range lies inside one of the LAN default ranges.
pub fn is_local_range(text: &str) -> bool {
    let Some((addr, prefix)) = split(text) else {
        return false;
    };
    AllowList::lan_default().ranges().iter().any(|lan| {
        let Some((_, lan_prefix)) = split(&lan.to_string()) else {
            return false;
        };
        lan_prefix <= prefix && lan.contains(addr)
    })
}

/// Whether any entry of the list reaches beyond the local network.
pub fn has_public_ranges(list: &[String]) -> bool {
    list.iter().any(|e| !is_local_range(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Vec<String> {
        crate::settings::host::default_allow_list()
    }

    #[test]
    fn cidr_editor_accepts_and_normalizes() {
        let e = check_entry(" 192.168.5.0/24 ", &[]).unwrap();
        assert_eq!(e.text, "192.168.5.0/24");
        assert!(!e.public);
        let host = check_entry("10.1.2.3", &[]).unwrap();
        assert_eq!(host.text, "10.1.2.3/32");
        let v6 = check_entry("fd00:1::/64", &[]).unwrap();
        assert_eq!(v6.text, "fd00:1::/64");
        assert!(!v6.public);
        // Typed with Arabic-Indic digits.
        let ar = check_entry("١٩٢.١٦٨.٥.٠/٢٤", &[]).unwrap();
        assert_eq!(ar.text, "192.168.5.0/24");
    }

    #[test]
    fn cidr_editor_rejects_bad_input() {
        assert_eq!(check_entry("   ", &[]), Err(CidrError::Empty));
        for bad in [
            "nonsense",
            "10.0.0.0/33",
            "192.168.1/24",
            "fe80::/200",
            "شبكة المكتب",
        ] {
            assert_eq!(check_entry(bad, &[]), Err(CidrError::Invalid), "{bad}");
        }
        assert_eq!(
            check_entry("10.0.0.0/8", &defaults()),
            Err(CidrError::Duplicate)
        );
        assert_eq!(
            check_entry("192.168.1.9", &["192.168.1.9/32".into()]),
            Err(CidrError::Duplicate)
        );
    }

    #[test]
    fn cidr_editor_flags_public_ranges() {
        for public in [
            "8.8.8.0/24",
            "0.0.0.0/0",
            "100.64.0.0/10",
            "2001:db8::/32",
            "10.0.0.0/7",
        ] {
            assert!(check_entry(public, &[]).unwrap().public, "{public}");
        }
        for local in ["172.20.0.0/16", "169.254.1.0/24", "127.0.0.1", "fe80::/64"] {
            assert!(!check_entry(local, &[]).unwrap().public, "{local}");
        }
        assert!(!has_public_ranges(&defaults()));
        assert!(has_public_ranges(&["8.8.8.8/32".into()]));
    }
}
