//! Display helpers for the connection log: local date/time, search/filter and CSV export.

use std::fmt::Write as _;

use time::{OffsetDateTime, UtcOffset};

use super::record::{Direction, LogAuth, LogEnd, LogEntry};
use crate::i18n::format::to_western_digits;
use crate::i18n::{tr_raw, Lang, S};

/// The machine's current UTC offset (UTC if it cannot be determined).
pub fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

/// `2026-10-13 18:42:05` in the given offset, Western digits, logical order. The UI applies
/// the digit style; CSV keeps it as is.
pub fn format_time(unix_ms: u64, offset: UtcOffset) -> String {
    let nanos = i128::from(unix_ms).saturating_mul(1_000_000);
    let Ok(t) = OffsetDateTime::from_unix_timestamp_nanos(nanos) else {
        return String::new();
    };
    let t = t.to_offset(offset);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Which entries the log screen shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFilter {
    #[default]
    All,
    Incoming,
    Outgoing,
    /// Refused attempts only (declined, wrong code, ...).
    Refused,
}

impl LogFilter {
    /// All filters in display order.
    pub const ALL: [LogFilter; 4] = [Self::All, Self::Incoming, Self::Outgoing, Self::Refused];
}

/// Whether `e` passes the filter and the search text. The search matches the name
/// (case-insensitive), the device ID (spaces ignored), the address and the fingerprint;
/// Arabic-Indic digits in the query are accepted.
pub fn matches(e: &LogEntry, filter: LogFilter, query: &str) -> bool {
    let kind_ok = match filter {
        LogFilter::All => true,
        LogFilter::Incoming => e.direction == Direction::Incoming,
        LogFilter::Outgoing => e.direction == Direction::Outgoing,
        LogFilter::Refused => e.end.is_refusal(),
    };
    if !kind_ok {
        return false;
    }
    let q = to_western_digits(query.trim()).to_lowercase();
    if q.is_empty() {
        return true;
    }
    let id_query: String = q.chars().filter(|c| !c.is_whitespace()).collect();
    e.name.to_lowercase().contains(&q)
        || (!id_query.is_empty() && format!("{:09}", e.device_id).contains(&id_query))
        || e.address.contains(&q)
        || e.fingerprint.contains(&q)
}

/// i18n key for a direction.
pub fn direction_key(d: Direction) -> S {
    match d {
        Direction::Incoming => S::LogIncoming,
        Direction::Outgoing => S::LogOutgoing,
    }
}

/// i18n key for an auth method.
pub fn auth_key(a: LogAuth) -> S {
    match a {
        LogAuth::AcceptPrompt => S::LogAuthAccept,
        LogAuth::OneTimeCode => S::OneTimeCode,
        LogAuth::Password => S::LogAuthPassword,
        LogAuth::Unknown => S::LogUnknown,
    }
}

/// i18n key for an end reason.
pub fn end_key(e: LogEnd) -> S {
    match e {
        LogEnd::ClosedHere => S::LogEndClosedHere,
        LogEnd::ClosedRemotely => S::LogEndClosedRemotely,
        LogEnd::PermissionRevoked => S::LogEndRevoked,
        LogEnd::Error => S::LogEndError,
        LogEnd::AppExit => S::LogEndAppExit,
        LogEnd::Declined => S::LogEndDeclined,
        LogEnd::TimedOut => S::LogEndTimedOut,
        LogEnd::WrongCredentials => S::LogEndWrongCredentials,
        LogEnd::RateLimited => S::LogEndRateLimited,
        LogEnd::NotAllowed => S::LogEndNotAllowed,
        LogEnd::Unknown => S::LogUnknown,
    }
}

/// One CSV field: quoted, quotes doubled, and a leading `= + - @` (or tab/CR) neutralized so
/// a spreadsheet never runs a peer-chosen device name as a formula.
fn field(out: &mut String, value: &str) {
    out.push('"');
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        out.push('\'');
    }
    out.push_str(&value.replace('"', "\"\""));
    out.push('"');
}

/// CSV of `entries` with a localized header and labels (UTF-8 with BOM so spreadsheet apps
/// detect Arabic correctly). Times use `offset`; digits stay Western.
pub fn to_csv(entries: &[LogEntry], lang: Lang, offset: UtcOffset) -> String {
    let mut out = String::from('\u{feff}');
    let header = [
        S::LogColTime,
        S::LogColDirection,
        S::DeviceName,
        S::IdLabel,
        S::AddressLabel,
        S::LogColFingerprint,
        S::LogColAuth,
        S::StatDuration,
        S::LogColPermissions,
        S::LogColEnd,
    ];
    for (i, k) in header.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        field(&mut out, tr_raw(lang, *k));
    }
    out.push_str("\r\n");
    for e in entries {
        let duration_secs = e.duration_ms / 1000;
        let cols = [
            format_time(e.started_ms, offset),
            tr_raw(lang, direction_key(e.direction)).to_owned(),
            e.name.clone(),
            format!("{:09}", e.device_id),
            e.address.clone(),
            e.fingerprint.clone(),
            tr_raw(lang, auth_key(e.auth)).to_owned(),
            duration_secs.to_string(),
            e.permissions.names().join(" "),
            tr_raw(lang, end_key(e.end)).to_owned(),
        ];
        for (i, c) in cols.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            field(&mut out, c);
        }
        let _ = write!(out, "\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PermissionSet;
    use tlp_desk_proto::Permissions;

    fn sample() -> LogEntry {
        LogEntry {
            // 2026-10-13 15:42:05 UTC
            started_ms: 1_791_906_125_000,
            duration_ms: 125_400,
            direction: Direction::Incoming,
            device_id: 12_345_678,
            name: "=حاسوب \"سارة\"".into(),
            fingerprint: "ab12".repeat(16),
            address: "192.168.1.7".into(),
            auth: LogAuth::OneTimeCode,
            permissions: PermissionSet(Permissions::VIEW | Permissions::CONTROL),
            end: LogEnd::ClosedRemotely,
            detail: None,
        }
    }

    #[test]
    fn times_format_in_the_given_offset() {
        let e = sample();
        assert_eq!(
            format_time(e.started_ms, UtcOffset::UTC),
            "2026-10-13 15:42:05"
        );
        let riyadh = UtcOffset::from_hms(3, 0, 0).unwrap();
        assert_eq!(format_time(e.started_ms, riyadh), "2026-10-13 18:42:05");
        assert_eq!(format_time(0, UtcOffset::UTC), "1970-01-01 00:00:00");
        // Absurd values do not panic.
        let _ = format_time(u64::MAX, UtcOffset::UTC);
    }

    #[test]
    fn search_and_filters() {
        let e = sample();
        assert!(matches(&e, LogFilter::All, ""));
        assert!(matches(&e, LogFilter::Incoming, "سارة"));
        assert!(matches(&e, LogFilter::All, "012 345"));
        assert!(
            matches(&e, LogFilter::All, "٠١٢٣٤٥٦٧٨"),
            "Arabic-Indic digits"
        );
        assert!(matches(&e, LogFilter::All, "192.168.1"));
        assert!(
            matches(&e, LogFilter::All, "AB12AB"),
            "fingerprint, case-insensitive"
        );
        assert!(!matches(&e, LogFilter::Outgoing, ""));
        assert!(!matches(&e, LogFilter::Refused, ""));
        assert!(!matches(&e, LogFilter::All, "laptop"));
        let refused = LogEntry {
            end: LogEnd::WrongCredentials,
            ..sample()
        };
        assert!(matches(&refused, LogFilter::Refused, ""));
    }

    #[test]
    fn csv_export_is_escaped_and_localized() {
        let csv = to_csv(&[sample()], Lang::Ar, UtcOffset::UTC);
        assert!(csv.starts_with('\u{feff}'));
        let mut lines = csv.lines();
        let header = lines.next().unwrap();
        assert!(header.contains(tr_raw(Lang::Ar, S::LogColTime)));
        let row = lines.next().unwrap();
        assert!(row.starts_with("\"2026-10-13 15:42:05\""));
        // Formula prefix neutralized, quotes doubled, Arabic kept exactly.
        assert!(row.contains("\"'=حاسوب \"\"سارة\"\"\""), "{row}");
        assert!(row.contains("\"012345678\""));
        assert!(row.contains("\"125\""));
        assert!(row.contains("\"view control\""));
        assert!(row.contains(tr_raw(Lang::Ar, S::LogEndClosedRemotely)));
        assert_eq!(lines.next(), None);
        let en = to_csv(&[], Lang::En, UtcOffset::UTC);
        assert_eq!(en.lines().count(), 1, "header only");
    }
}
