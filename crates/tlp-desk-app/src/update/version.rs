//! Version parsing and comparison (semver, `1.0.0-rc.3` style; release tags may start with `v`).

use semver::Version;

/// The version of this build (the workspace version).
pub fn current() -> Version {
    // The workspace version is a literal in Cargo.toml and always valid semver; fall back to
    // 0.0.0 (which makes every release look newer) rather than crash if it ever is not.
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// Parses a release tag or announced version: `v1.0.0-rc.3`, `1.0.0`, ` V2.1.0 `.
pub fn parse(tag: &str) -> Option<Version> {
    let t = tag.trim();
    let t = t
        .strip_prefix('v')
        .or_else(|| t.strip_prefix('V'))
        .unwrap_or(t);
    if t.len() > 64 {
        return None;
    }
    Version::parse(t).ok()
}

/// `true` when `candidate` is strictly newer than `current` by semver precedence
/// (`1.0.0-rc.3 < 1.0.0-rc.10 < 1.0.0`; build metadata is ignored).
pub fn is_newer(candidate: &Version, current: &Version) -> bool {
    candidate.cmp_precedence(current) == std::cmp::Ordering::Greater
}

/// The newest version among peer announcements that is newer than `current`, with the name
/// of the device that announced it. Unparseable announcements are skipped.
pub fn newest_peer<'a, I>(peers: I, current: &Version) -> Option<(Version, String)>
where
    I: IntoIterator<Item = (&'a str, Option<&'a str>)>,
{
    let mut best: Option<(Version, String)> = None;
    for (name, ver) in peers {
        let Some(v) = ver.and_then(parse) else {
            continue;
        };
        if !is_newer(&v, current) {
            continue;
        }
        let better = best
            .as_ref()
            .is_none_or(|(b, _)| v.cmp_precedence(b) == std::cmp::Ordering::Greater);
        if better {
            best = Some((v, name.to_owned()));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse(s).unwrap()
    }

    #[test]
    fn rc_ordering_follows_semver() {
        assert!(is_newer(&v("1.0.0-rc.4"), &v("1.0.0-rc.3")));
        assert!(is_newer(&v("1.0.0-rc.10"), &v("1.0.0-rc.9")));
        assert!(is_newer(&v("1.0.0"), &v("1.0.0-rc.10")));
        assert!(is_newer(&v("1.0.1"), &v("1.0.0")));
        assert!(!is_newer(&v("1.0.0-rc.3"), &v("1.0.0-rc.3")));
        assert!(!is_newer(&v("1.0.0-rc.3"), &v("1.0.0")));
        assert!(!is_newer(&v("0.9.9"), &v("1.0.0-rc.1")));
        // Build metadata never makes a version "newer".
        assert!(!is_newer(&v("1.0.0+win"), &v("1.0.0")));
    }

    #[test]
    fn tags_with_and_without_v_parse() {
        assert_eq!(v("v1.0.0-rc.3"), v("1.0.0-rc.3"));
        assert_eq!(v(" V2.1.0 "), Version::new(2, 1, 0));
        assert!(parse("latest").is_none());
        assert!(parse("").is_none());
        assert!(parse("١.٠.٠").is_none());
        assert!(parse(&"1".repeat(100)).is_none());
    }

    #[test]
    fn current_matches_the_workspace_version() {
        assert_eq!(current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn newest_peer_picks_the_highest_newer_version() {
        let cur = v("1.0.0-rc.3");
        let peers = [
            ("مكتب المحاسبة", Some("1.0.0-rc.5")),
            ("Old-PC", Some("1.0.0-rc.2")),
            ("Bad", Some("not a version")),
            ("Unknown", None),
            ("Lab", Some("1.0.0-rc.4")),
        ];
        let (ver, name) = newest_peer(peers, &cur).unwrap();
        assert_eq!(ver, v("1.0.0-rc.5"));
        assert_eq!(name, "مكتب المحاسبة");
        assert!(newest_peer([("Same", Some("1.0.0-rc.3"))], &cur).is_none());
    }
}
