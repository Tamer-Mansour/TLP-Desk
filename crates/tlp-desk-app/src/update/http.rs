//! Blocking HTTPS client for the update check (runs on the update worker thread, never the UI
//! thread). Only `https://` URLs are fetched; plain `http://` is accepted for loopback alone so
//! tests can use a local fake server.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use super::UpdateError;

/// HTTP client with the app's user agent, timeouts and TLS settings.
pub struct Client {
    agent: ureq::Agent,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// Client that trusts the operating system's certificate store.
    pub fn new() -> Self {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(format!("TLP-Desk/{}", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_body(Some(Duration::from_secs(60)))
            .max_redirects(5)
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }

    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, UpdateError> {
        check_url(url)?;
        let resp = self
            .agent
            .get(url)
            .header(
                "Accept",
                "application/vnd.github+json, application/octet-stream",
            )
            .call()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(UpdateError::Status(status));
        }
        Ok(resp)
    }

    /// The whole body, refusing anything larger than `max` bytes.
    pub fn get_bytes(&self, url: &str, max: u64) -> Result<Vec<u8>, UpdateError> {
        let resp = self.get(url)?;
        if resp.body().content_length().is_some_and(|n| n > max) {
            return Err(UpdateError::TooLarge);
        }
        let mut out = Vec::new();
        resp.into_body()
            .into_reader()
            .take(max.saturating_add(1))
            .read_to_end(&mut out)
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        if out.len() as u64 > max {
            return Err(UpdateError::TooLarge);
        }
        Ok(out)
    }

    /// Streams `url` into `dest` (created/truncated), at most `max` bytes. `progress` gets
    /// `(done, total)` (`total` 0 when unknown); `cancel` aborts and deletes the file.
    pub fn download(
        &self,
        url: &str,
        dest: &Path,
        max: u64,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<(), UpdateError> {
        let result = self.download_inner(url, dest, max, progress, cancel);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }

    fn download_inner(
        &self,
        url: &str,
        dest: &Path,
        max: u64,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<(), UpdateError> {
        let resp = self.get(url)?;
        let total = resp.body().content_length().unwrap_or(0);
        if total > max {
            return Err(UpdateError::TooLarge);
        }
        let mut reader = resp.into_body().into_reader();
        let mut file = std::fs::File::create(dest)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut done: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpdateError::Cancelled);
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| UpdateError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            done = done.saturating_add(n as u64);
            if done > max {
                return Err(UpdateError::TooLarge);
            }
            file.write_all(buf.get(..n).unwrap_or_default())?;
            progress(done, total);
        }
        file.flush()?;
        Ok(())
    }
}

/// `https://` anywhere, `http://` only to loopback (tests).
pub fn check_url(url: &str) -> Result<(), UpdateError> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
        if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(());
        }
    }
    Err(UpdateError::InsecureUrl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_loopback_http() {
        assert!(check_url("https://api.github.com/repos/x").is_ok());
        assert!(check_url("http://127.0.0.1:5555/a").is_ok());
        assert!(check_url("http://localhost/a").is_ok());
        assert!(check_url("http://[::1]:80/a").is_ok());
        assert!(check_url("http://github.com/a").is_err());
        assert!(check_url("http://127.0.0.1.evil.example/a").is_err());
        assert!(check_url("ftp://x").is_err());
        assert!(check_url("file:///C:/x.exe").is_err());
    }
}
