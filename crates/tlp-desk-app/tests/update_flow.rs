//! Update check + download + verification against a fake GitHub on 127.0.0.1 (no real
//! network). One `#[ignore]`d test talks to the real GitHub API.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tlp_desk_app::update::{self, http::Client, CheckConfig, UpdateError};

#[path = "../src/update/sign.rs"]
#[allow(dead_code)]
mod sign;

type Routes = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Minimal HTTP/1.1 server: GET only, one request per connection, 404 for unknown paths.
fn serve() -> (String, Routes) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: Routes = Arc::default();
    let r = routes.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let body = r.lock().unwrap().get(&path).cloned();
            let (status, body) = match body {
                Some(b) => ("200 OK", b),
                None => ("404 Not Found", b"{\"message\":\"Not Found\"}".to_vec()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    (base, routes)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Fixture {
    base: String,
    routes: Routes,
    key: sign::SecretKey,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let (base, routes) = serve();
        Self {
            base,
            routes,
            key: sign::SecretKey::from_parts([4; 8], [2; 32]),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn cfg(&self, pre: bool) -> CheckConfig {
        CheckConfig {
            api_base: self.base.clone(),
            repo: "Tamer-Mansour/TLP-Desk".into(),
            include_prereleases: pre,
            public_key: self.key.public_key_base64(),
            download_dir: self.dir.path().join("TLP-Desk").join("update"),
        }
    }

    fn route(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), body.into());
    }

    /// Publishes release `ver` with a setup.exe, its signed manifest and the API listing.
    fn publish(&self, ver: &str, setup: &[u8], prerelease: bool) {
        let b = &self.base;
        let name = format!("TLP-Desk-{ver}-setup.exe");
        let manifest = format!(
            "{}  {name}\n{}  TLP-Desk-{ver}-win-x64-portable.zip\n",
            hex(&Sha256::digest(setup)),
            hex(&[0u8; 32])
        );
        self.route(&format!("/dl/{ver}/{name}"), setup);
        self.route(&format!("/dl/{ver}/SHA256SUMS.txt"), manifest.clone());
        self.route(
            &format!("/dl/{ver}/SHA256SUMS.txt.minisig"),
            self.key
                .sign(manifest.as_bytes(), &format!("TLP-Desk {ver}")),
        );
        let rel = format!(
            r###"{{"tag_name":"v{ver}","draft":false,"prerelease":{prerelease},
              "html_url":"https://github.com/Tamer-Mansour/TLP-Desk/releases/tag/v{ver}",
              "body":"## الجديد في {ver}\n- Faster file transfer\n","assets":[
              {{"name":"{name}","browser_download_url":"{b}/dl/{ver}/{name}","size":{}}},
              {{"name":"SHA256SUMS.txt","browser_download_url":"{b}/dl/{ver}/SHA256SUMS.txt","size":1}},
              {{"name":"SHA256SUMS.txt.minisig","browser_download_url":"{b}/dl/{ver}/SHA256SUMS.txt.minisig","size":1}}]}}"###,
            setup.len()
        );
        self.route(
            "/repos/Tamer-Mansour/TLP-Desk/releases?per_page=30",
            format!("[{rel}]"),
        );
        if !prerelease {
            self.route("/repos/Tamer-Mansour/TLP-Desk/releases/latest", rel);
        }
    }
}

fn current() -> semver::Version {
    semver::Version::parse("1.0.0-rc.3").unwrap()
}

#[test]
fn check_download_and_verify_happy_path() {
    let f = Fixture::new();
    let setup = b"MZ fake installer bytes".repeat(5000);
    f.publish("1.0.0-rc.4", &setup, true);
    let client = Client::new();
    let cfg = f.cfg(true);
    let rel = update::check(&client, &cfg, &current()).unwrap().unwrap();
    assert_eq!(rel.version.to_string(), "1.0.0-rc.4");
    assert!(rel.notes.starts_with("## الجديد في 1.0.0-rc.4"));
    let mut seen = Vec::new();
    let path = update::download_and_verify(
        &client,
        &cfg,
        &rel,
        &mut |d, t| seen.push((d, t)),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(path, cfg.download_dir.join("TLP-Desk-1.0.0-rc.4-setup.exe"));
    assert_eq!(std::fs::read(&path).unwrap(), setup);
    assert_eq!(seen.last().unwrap().0, setup.len() as u64);
    // Re-verification right before launch passes, and fails after tampering.
    update::reverify(&cfg, &path).unwrap();
    std::fs::write(&path, b"swapped").unwrap();
    assert!(matches!(
        update::reverify(&cfg, &path),
        Err(UpdateError::HashMismatch)
    ));
    assert!(!path.exists());
}

#[test]
fn prerelease_setting_and_up_to_date() {
    let f = Fixture::new();
    f.publish("1.0.0-rc.4", b"x", true);
    let client = Client::new();
    // Stable channel: /releases/latest is 404 while only pre-releases exist -> up to date.
    assert!(update::check(&client, &f.cfg(false), &current())
        .unwrap()
        .is_none());
    // Already on the newest version.
    let newest = semver::Version::parse("1.0.0-rc.4").unwrap();
    assert!(update::check(&client, &f.cfg(true), &newest)
        .unwrap()
        .is_none());
}

#[test]
fn tampered_installer_is_refused_and_deleted() {
    let f = Fixture::new();
    f.publish("1.0.0", b"good installer", false);
    // The mirror now serves different bytes than the signed manifest lists.
    f.route(
        "/dl/1.0.0/TLP-Desk-1.0.0-setup.exe",
        b"evil installer".to_vec(),
    );
    let client = Client::new();
    let cfg = f.cfg(false);
    let rel = update::check(&client, &cfg, &current()).unwrap().unwrap();
    let err =
        update::download_and_verify(&client, &cfg, &rel, &mut |_, _| {}, &AtomicBool::new(false))
            .unwrap_err();
    assert!(matches!(err, UpdateError::HashMismatch), "{err:?}");
    let left: Vec<_> = std::fs::read_dir(&cfg.download_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("setup.exe"))
        .collect();
    assert!(left.is_empty(), "no installer may remain: {left:?}");
}

#[test]
fn bad_signature_is_refused_before_downloading() {
    let f = Fixture::new();
    f.publish("1.0.0", b"good installer", false);
    // Signed by somebody else's key.
    let other = sign::SecretKey::from_parts([4; 8], [9; 32]);
    let manifest = f.routes.lock().unwrap()["/dl/1.0.0/SHA256SUMS.txt"].clone();
    f.route(
        "/dl/1.0.0/SHA256SUMS.txt.minisig",
        other.sign(&manifest, "TLP-Desk 1.0.0"),
    );
    let client = Client::new();
    let cfg = f.cfg(false);
    let rel = update::check(&client, &cfg, &current()).unwrap().unwrap();
    let err = update::download_and_verify(
        &client,
        &cfg,
        &rel,
        &mut |_, _| panic!("must not download the installer"),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(err, UpdateError::BadSignature), "{err:?}");
}

#[test]
fn garbage_api_response_is_an_error() {
    let f = Fixture::new();
    f.route(
        "/repos/Tamer-Mansour/TLP-Desk/releases?per_page=30",
        b"<html>rate limited</html>".to_vec(),
    );
    let err = update::check(&Client::new(), &f.cfg(true), &current()).unwrap_err();
    assert!(matches!(err, UpdateError::BadResponse), "{err:?}");
}

/// Real GitHub API (internet). Run once by hand:
/// `cargo test -p tlp-desk-app --test update_flow -- --ignored --exact real_github_api_check`
#[test]
#[ignore = "contacts api.github.com"]
fn real_github_api_check() {
    let cfg = CheckConfig::github(true);
    let old = semver::Version::parse("0.0.1").unwrap();
    let r = update::check(&Client::new(), &cfg, &old).unwrap();
    let r = r.expect("the repository has releases");
    println!("newest: {} {}", r.version, r.html_url);
}
