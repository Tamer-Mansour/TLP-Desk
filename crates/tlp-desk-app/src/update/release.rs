//! GitHub Releases API responses: parse, pick the newest release, find its assets.
//!
//! Everything here is untrusted input from the network. It only decides *what to offer*;
//! nothing runs until [`super::verify`] has checked the signature and the hash.

use semver::Version;
use serde::Deserialize;

use super::version;

/// Most release-note characters shown in the app (the rest is on the release page).
pub const MAX_NOTES_CHARS: usize = 1200;
/// Most release-note lines shown in the app.
pub const MAX_NOTES_LINES: usize = 12;

/// One release as the GitHub REST API returns it (only the fields we use).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

/// One release asset.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

/// A downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Size the API reports (0 if unknown); used for progress only, never trusted.
    pub size: u64,
}

/// A release newer than this build, ready to offer to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// First lines of the release notes, plain text (Markdown markers left as typed).
    pub notes: String,
    /// The release page (opened in the browser for "Download" from the LAN notice).
    pub html_url: String,
    /// `TLP-Desk-<ver>-setup.exe`, `SHA256SUMS.txt`, `SHA256SUMS.txt.minisig`; `None` when
    /// the release lacks any of them (then only the release page is offered).
    pub files: Option<ReleaseFiles>,
}

/// The three files an in-app install needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFiles {
    pub setup: Asset,
    pub sums: Asset,
    pub signature: Asset,
}

/// Name of the setup asset for `v` (what `installer/build-setup.ps1` produces).
pub fn setup_name(v: &Version) -> String {
    format!("TLP-Desk-{v}-setup.exe")
}

/// Checksums manifest asset name.
pub const SUMS_NAME: &str = "SHA256SUMS.txt";
/// Signature of the manifest (minisign format).
pub const SIG_NAME: &str = "SHA256SUMS.txt.minisig";

/// Parses `/releases/latest` (one object) or `/releases` (an array).
pub(crate) fn parse_releases(json: &[u8]) -> Result<Vec<GhRelease>, serde_json::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<GhRelease>),
        One(Box<GhRelease>),
    }
    Ok(match serde_json::from_slice::<OneOrMany>(json)? {
        OneOrMany::Many(v) => v,
        OneOrMany::One(r) => vec![*r],
    })
}

/// The newest non-draft release newer than `current` (pre-releases only when allowed).
pub(crate) fn pick_newest(
    releases: Vec<GhRelease>,
    current: &Version,
    include_prereleases: bool,
) -> Option<Release> {
    releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| version::parse(&r.tag_name).map(|v| (v, r)))
        .filter(|(v, r)| include_prereleases || (!r.prerelease && v.pre.is_empty()))
        .filter(|(v, _)| version::is_newer(v, current))
        .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
        .map(|(v, r)| to_release(v, r))
}

fn to_release(version: Version, r: GhRelease) -> Release {
    let find = |name: &str| {
        r.assets.iter().find(|a| a.name == name).map(|a| Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    };
    let files = match (find(&setup_name(&version)), find(SUMS_NAME), find(SIG_NAME)) {
        (Some(setup), Some(sums), Some(signature)) => Some(ReleaseFiles {
            setup,
            sums,
            signature,
        }),
        _ => None,
    };
    Release {
        notes: notes_excerpt(r.body.as_deref().unwrap_or_default()),
        tag: r.tag_name,
        html_url: r.html_url,
        version,
        files,
    }
}

/// First [`MAX_NOTES_LINES`] non-empty lines, at most [`MAX_NOTES_CHARS`] characters, with
/// control characters (bidi overrides included) removed so the notes render as plain text.
pub fn notes_excerpt(body: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    for line in body
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
    {
        if out.lines().count() >= MAX_NOTES_LINES {
            out.push('…');
            break;
        }
        let clean: String = line
            .chars()
            .filter(|c| !c.is_control() && !is_bidi_control(*c))
            .collect();
        let n = clean.chars().count();
        if chars.saturating_add(n) > MAX_NOTES_CHARS {
            let room = MAX_NOTES_CHARS.saturating_sub(chars);
            out.extend(clean.chars().take(room));
            out.push('…');
            break;
        }
        chars = chars.saturating_add(n);
        out.push_str(&clean);
        out.push('\n');
    }
    out.trim_end().to_owned()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool, draft: bool) -> String {
        let v = tag.trim_start_matches('v');
        format!(
            r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{pre},"html_url":"https://github.com/Tamer-Mansour/TLP-Desk/releases/tag/{tag}",
               "body":"Notes for {tag}","assets":[
               {{"name":"TLP-Desk-{v}-setup.exe","browser_download_url":"https://example.invalid/{v}/setup.exe","size":10}},
               {{"name":"SHA256SUMS.txt","browser_download_url":"https://example.invalid/{v}/sums","size":1}},
               {{"name":"SHA256SUMS.txt.minisig","browser_download_url":"https://example.invalid/{v}/sig","size":1}}]}}"#
        )
    }

    #[test]
    fn picks_newest_respecting_prerelease_setting() {
        let json = format!(
            "[{},{},{},{}]",
            rel("v1.0.0-rc.4", true, false),
            rel("v1.0.0-rc.10", true, false),
            rel("v2.0.0", false, true), // draft: never offered
            rel("v0.9.0", false, false),
        );
        let list = parse_releases(json.as_bytes()).unwrap();
        let cur = version::parse("1.0.0-rc.3").unwrap();
        let r = pick_newest(list.clone(), &cur, true).unwrap();
        assert_eq!(r.version.to_string(), "1.0.0-rc.10");
        let files = r.files.unwrap();
        assert_eq!(files.setup.name, "TLP-Desk-1.0.0-rc.10-setup.exe");
        assert_eq!(files.signature.name, SIG_NAME);
        // Stable only: nothing newer than rc.3 that is not a pre-release.
        assert!(pick_newest(list, &cur, false).is_none());
    }

    #[test]
    fn latest_endpoint_single_object_parses() {
        let json = rel("v1.0.0", false, false);
        let list = parse_releases(json.as_bytes()).unwrap();
        let cur = version::parse("1.0.0-rc.3").unwrap();
        let r = pick_newest(list, &cur, false).unwrap();
        assert_eq!(r.tag, "v1.0.0");
        assert_eq!(r.notes, "Notes for v1.0.0");
    }

    #[test]
    fn missing_assets_still_offer_the_release_page() {
        let json = r#"{"tag_name":"v9.0.0","html_url":"https://github.com/x","assets":[]}"#;
        let list = parse_releases(json.as_bytes()).unwrap();
        let r = pick_newest(list, &version::current(), false).unwrap();
        assert!(r.files.is_none());
        assert_eq!(r.html_url, "https://github.com/x");
    }

    #[test]
    fn garbage_json_is_an_error_not_a_crash() {
        assert!(parse_releases(b"<html>rate limited</html>").is_err());
        assert!(parse_releases(b"{\"message\":\"Not Found\"}").is_err());
    }

    #[test]
    fn notes_are_trimmed_and_cleaned() {
        let body =
            "## ما الجديد\r\n\r\n- إصلاح \u{202E}شيء\n- Fix B\n".to_owned() + &"line\n".repeat(50);
        let n = notes_excerpt(&body);
        assert!(n.starts_with("## ما الجديد\n- إصلاح شيء\n- Fix B"));
        assert!(!n.contains('\u{202E}'));
        assert!(n.lines().count() <= MAX_NOTES_LINES + 1);
        let long = "x".repeat(5000);
        assert!(notes_excerpt(&long).chars().count() <= MAX_NOTES_CHARS + 1);
    }
}
