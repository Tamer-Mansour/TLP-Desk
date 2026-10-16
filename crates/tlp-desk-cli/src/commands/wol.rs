//! `wol <mac|alias>` (alias `exec-wol`): wake a sleeping PC on the LAN with a Wake-on-LAN
//! magic packet.
//!
//! `<alias>` is looked up in the GUI address book (alias or name of a saved device that has a
//! MAC), then in `<data dir>/wol-aliases.txt`, one `name = MAC` per line (UTF-8,
//! Arabic names welcome, `#` starts a comment). The packet goes to every IPv4 subnet
//! broadcast, or with `--to` to one LAN address; anything outside the LAN allow-list is
//! refused (LAN-only rule).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::json;
use tlp_desk_net::{parse_mac, send_wake_on_lan, AllowList, WOL_PORT};

use super::common::Ctx;
use crate::exit::CliError;
use crate::text;

/// Alias file name inside the data directory.
pub const ALIAS_FILE: &str = "wol-aliases.txt";

/// Arguments of `wol`.
#[derive(Debug, Clone, Args)]
pub struct WolArgs {
    /// MAC address (`01:23:45:67:89:ab`, `01-23-...` or 12 hex digits) or an alias from
    /// wol-aliases.txt in the data dir.
    pub target: String,
    /// Send to this LAN address (IP or IP:port, e.g. a subnet broadcast) instead of every
    /// local subnet broadcast. Port defaults to 9.
    #[arg(long)]
    pub to: Option<String>,
}

/// Runs `wol`.
pub fn run(ctx: &Ctx, args: &WolArgs) -> Result<()> {
    let mac = resolve(&ctx.data_dir, &args.target)?;
    let to = args.to.as_deref().map(parse_to).transpose()?;
    send_wake_on_lan(mac, to).context("cannot send the Wake-on-LAN packet")?;
    let mac_s = format_mac(mac);
    let dest = to.map_or_else(|| "broadcast".to_owned(), |a| a.to_string());
    ctx.out.result(
        text::wol_sent(&mac_s, &dest),
        &json!({ "type": "wol_sent", "mac": mac_s, "to": dest }),
    );
    Ok(())
}

/// A MAC address as given, or the MAC of an alias.
pub fn resolve(data_dir: &Path, target: &str) -> Result<[u8; 6]> {
    if let Some(mac) = parse_mac(target) {
        return Ok(mac);
    }
    let path = data_dir.join(ALIAS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    if let Some(mac) = lookup_alias(&text, target) {
        return Ok(mac);
    }
    // The GUI's address book: saved devices with a MAC, found by alias or device name.
    for dir in address_book_dirs(data_dir) {
        let Ok(json) = std::fs::read_to_string(dir.join(ADDRESS_BOOK_FILE)) else {
            continue;
        };
        if let Some(mac) = lookup_address_book(&json, target) {
            return Ok(mac);
        }
    }
    Err(CliError::NotFound(target.to_owned()).into())
}

/// The GUI's address book file name (`%APPDATA%\TLP-Desk\address_book.json`).
pub const ADDRESS_BOOK_FILE: &str = "address_book.json";

/// Folders that may hold the address book: the per-user app folder, then the data dir.
fn address_book_dirs(data_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(std::path::PathBuf::from(appdata).join("TLP-Desk"));
    }
    dirs.push(data_dir.to_path_buf());
    dirs
}

/// Finds a saved device whose alias or name equals `name` (case-insensitive, trimmed) and has
/// a MAC. Reads only the fields it needs, so newer files keep working.
pub fn lookup_address_book(json: &str, name: &str) -> Option<[u8; 6]> {
    let want = name.trim().to_lowercase();
    let doc: serde_json::Value = serde_json::from_str(json).ok()?;
    doc.get("entries")?.as_array()?.iter().find_map(|e| {
        let field = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        let hit = [field("alias"), field("name")]
            .iter()
            .any(|n| !n.trim().is_empty() && n.trim().to_lowercase() == want);
        if hit {
            parse_mac(field("mac"))
        } else {
            None
        }
    })
}

/// Finds `name` (case-insensitive, trimmed) in `name = MAC` lines.
pub fn lookup_alias(text: &str, name: &str) -> Option<[u8; 6]> {
    let want = name.trim().to_lowercase();
    text.lines()
        .map(|l| l.split('#').next().unwrap_or_default())
        .filter_map(|l| l.split_once('='))
        .find(|(n, _)| n.trim().to_lowercase() == want)
        .and_then(|(_, mac)| parse_mac(mac.trim()))
}

/// `--to` value: IP or IP:port, which must be a LAN address or the IPv4 broadcast.
pub fn parse_to(s: &str) -> Result<SocketAddr> {
    let addr = s
        .parse::<SocketAddr>()
        .or_else(|_| s.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, WOL_PORT)))
        .with_context(|| format!("--to {s}: not an IP address"))?;
    let ip = addr.ip();
    if ip != IpAddr::V4(Ipv4Addr::BROADCAST) && !AllowList::lan_default().is_allowed(ip) {
        anyhow::bail!("--to {ip} is not a local-network address");
    }
    Ok(addr)
}

/// `01:23:45:67:89:ab`.
pub fn format_mac(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::Exit;

    const FILE: &str = "# my PCs\nمكتب المحاسبة = 01:23:45:67:89:AB\nNAS=0a-0b-0c-0d-0e-0f # basement\nbroken = zz\n";

    #[test]
    fn aliases_resolve_with_arabic_names() {
        assert_eq!(
            lookup_alias(FILE, "مكتب المحاسبة"),
            Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab])
        );
        assert_eq!(lookup_alias(FILE, " nas "), Some([10, 11, 12, 13, 14, 15]));
        assert_eq!(lookup_alias(FILE, "broken"), None);
        assert_eq!(lookup_alias(FILE, "missing"), None);
    }

    #[test]
    fn address_book_aliases_resolve_with_arabic() {
        let json = r#"{"version":1,"entries":[
            {"device_id":"123456789","alias":"مكتب المحاسبة","name":"PC-7","mac":"01:23:45:67:89:ab"},
            {"device_id":"222222222","alias":"","name":"Lab","mac":null},
            {"device_id":"333333333","name":"NAS","mac":"0a:0b:0c:0d:0e:0f"}]}"#;
        let mac = Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab]);
        assert_eq!(lookup_address_book(json, "مكتب المحاسبة"), mac);
        assert_eq!(lookup_address_book(json, " pc-7 "), mac);
        assert_eq!(
            lookup_address_book(json, "nas"),
            Some([10, 11, 12, 13, 14, 15])
        );
        assert_eq!(lookup_address_book(json, "Lab"), None);
        assert_eq!(lookup_address_book(json, ""), None);
        assert_eq!(lookup_address_book("not json", "nas"), None);
    }

    #[test]
    fn resolve_prefers_mac_and_reports_unknown_alias() {
        let dir = std::env::temp_dir().join(format!("tlp-cli-wol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ALIAS_FILE), FILE).unwrap();
        assert_eq!(resolve(&dir, "0123456789ab").unwrap()[5], 0xab);
        assert_eq!(resolve(&dir, "NAS").unwrap(), [10, 11, 12, 13, 14, 15]);
        let err = resolve(&dir, "غير موجود").unwrap_err();
        assert_eq!(Exit::of(&err), Exit::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
        // No alias file at all: still a clean not-found.
        let err = resolve(&dir, "x").unwrap_err();
        assert_eq!(Exit::of(&err), Exit::NotFound);
    }

    #[test]
    fn to_must_stay_on_the_lan() {
        assert_eq!(
            parse_to("192.168.1.255").unwrap(),
            "192.168.1.255:9".parse().unwrap()
        );
        assert_eq!(
            parse_to("10.0.0.7:7").unwrap(),
            "10.0.0.7:7".parse().unwrap()
        );
        assert!(parse_to("255.255.255.255").is_ok());
        assert!(parse_to("8.8.8.8").is_err());
        assert!(parse_to("not-an-ip").is_err());
    }

    #[test]
    fn mac_formatting() {
        assert_eq!(
            format_mac([1, 0x23, 0x45, 0x67, 0x89, 0xab]),
            "01:23:45:67:89:ab"
        );
    }
}
