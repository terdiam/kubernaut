//! Presentation rules shared by every front end: what colour a status word
//! gets, and how an object's age reads. Pure functions over `&str`; nothing
//! here knows about a window system.

use std::fmt::Write as _;

use k8s_openapi::jiff::Timestamp;

/// How a status reads at a glance. Ordered by severity, so the worst part of a
/// compound value (`Ready,SchedulingDisabled`) is simply the `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Ok,
    Pending,
    Warn,
    Error,
}

/// Column names whose values are a status rather than free text.
const STATUS_COLUMNS: &[&str] = &[
    "status",
    "phase",
    "ready",
    "health",
    "health status",
    "sync status",
    "state",
    "conditions",
    "available",
    "reason",
    "type",
];

const OK: &[&str] = &[
    "ready",
    "running",
    "succeeded",
    "completed",
    "bound",
    "active",
    "available",
    "healthy",
    "deployed",
    "synced",
    "normal",
    "true",
    "valid",
    "established",
    "namesaccepted",
    "approved",
    "attached",
];

const PENDING: &[&str] = &[
    "pending",
    "containercreating",
    "podinitializing",
    "progressing",
    "creating",
    "initializing",
    "waiting",
    "provisioning",
    "reconciling",
    "installing",
    "upgrading",
    "pending-install",
    "pending-upgrade",
    "pending-rollback",
];

const WARN: &[&str] = &[
    "terminating",
    "schedulingdisabled",
    "warning",
    "suspended",
    "released",
    "outofsync",
    "superseded",
    "degraded",
    "notfound",
    "missing",
    "unknown",
    "uninstalling",
    "paused",
];

const ERROR: &[&str] = &[
    "notready",
    "failed",
    "error",
    "crashloopbackoff",
    "imagepullbackoff",
    "errimagepull",
    "invalidimagename",
    "createcontainerconfigerror",
    "createcontainererror",
    "startererror",
    "starterror",
    "evicted",
    "oomkilled",
    "lost",
    "unhealthy",
    "backoff",
    "false",
    "unschedulable",
    "deadlineexceeded",
    "uninstalled",
    "rejected",
    "forbidden",
];

/// Anything ending in these reads as trouble even when the exact word is new,
/// which happens constantly with controller-invented reasons.
const ERROR_SUFFIXES: &[&str] = &["backoff", "error", "failed"];

#[inline]
fn contains_ignore_case(set: &[&str], word: &str) -> bool {
    set.iter().any(|entry| entry.eq_ignore_ascii_case(word))
}

#[inline]
fn ends_with_ignore_case(word: &str, suffix: &str) -> bool {
    // Compare bytes: slicing the `str` could land inside a multi-byte char.
    let (word, suffix) = (word.as_bytes(), suffix.as_bytes());
    word.len() >= suffix.len() && word[word.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

fn word_tone(word: &str) -> Option<Tone> {
    let word = word.trim();
    if word.is_empty() {
        None
    } else if contains_ignore_case(ERROR, word)
        || ERROR_SUFFIXES
            .iter()
            .any(|suffix| ends_with_ignore_case(word, suffix))
    {
        Some(Tone::Error)
    } else if contains_ignore_case(WARN, word) {
        Some(Tone::Warn)
    } else if contains_ignore_case(PENDING, word) {
        Some(Tone::Pending)
    } else if contains_ignore_case(OK, word) {
        Some(Tone::Ok)
    } else {
        None
    }
}

/// Tone for a bare status value, for places that already know it is a status.
///
/// Compound values such as `Ready,SchedulingDisabled` take the most severe tone
/// of their parts: the cordon is the part that matters.
pub fn tone_of_value(value: &str) -> Option<Tone> {
    value.split([',', '/']).filter_map(word_tone).max()
}

/// Tone for a cell, or `None` when the column is not a status.
///
/// Kubernetes spreads status across dozens of vocabularies, so rather than a
/// rule per kind this maps the words themselves, which also covers CRD printer
/// columns nobody anticipated.
pub fn tone_for(column: &str, value: &str) -> Option<Tone> {
    let column = column.trim();
    if !contains_ignore_case(STATUS_COLUMNS, column) {
        return None;
    }
    tone_of_value(value)
}

/// Seconds since the Unix epoch, now.
#[inline]
pub fn now() -> i64 {
    Timestamp::now().as_second()
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp.
pub fn parse_timestamp(rfc3339: &str) -> Option<i64> {
    rfc3339.parse::<Timestamp>().ok().map(Timestamp::as_second)
}

/// Append a kubectl-style age (`5m3s`, `2h10m`, `3d4h`, `120d`) to `out`.
///
/// Writes into a caller-owned buffer so a table can refresh every row's age on
/// a timer without allocating a string per row.
pub fn write_age(out: &mut String, seconds: i64) {
    let s = seconds.max(0);
    // Writing to a `String` cannot fail.
    let _ = if s < 60 {
        write!(out, "{s}s")
    } else if s < 3_600 {
        write!(out, "{}m{}s", s / 60, s % 60)
    } else if s < 86_400 {
        write!(out, "{}h{}m", s / 3_600, (s % 3_600) / 60)
    } else if s < 86_400 * 100 {
        write!(out, "{}d{}h", s / 86_400, (s % 86_400) / 3_600)
    } else {
        write!(out, "{}d", s / 86_400)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_words_map_to_their_tone() {
        assert_eq!(tone_for("Status", "Running"), Some(Tone::Ok));
        assert_eq!(tone_for("Status", "Pending"), Some(Tone::Pending));
        assert_eq!(tone_for("Status", "Terminating"), Some(Tone::Warn));
        assert_eq!(tone_for("Status", "CrashLoopBackOff"), Some(Tone::Error));
        assert_eq!(
            tone_for("Status", "running"),
            Some(Tone::Ok),
            "case-insensitive"
        );
    }

    #[test]
    fn only_status_columns_are_coloured() {
        assert_eq!(tone_for("Name", "Running"), None);
        assert_eq!(tone_for("Image", "failed"), None);
        assert_eq!(
            tone_for("  status ", "Failed"),
            Some(Tone::Error),
            "name is trimmed"
        );
    }

    #[test]
    fn compound_values_take_the_worst_part() {
        assert_eq!(
            tone_for("Status", "Ready,SchedulingDisabled"),
            Some(Tone::Warn)
        );
        assert_eq!(tone_for("Ready", "Ready/NotReady"), Some(Tone::Error));
        assert_eq!(tone_for("Status", "Unrecognised,Running"), Some(Tone::Ok));
    }

    #[test]
    fn new_reasons_ending_in_trouble_words_read_as_errors() {
        assert_eq!(tone_for("Reason", "SomethingNewBackOff"), Some(Tone::Error));
        assert_eq!(tone_for("Reason", "ProvisioningFailed"), Some(Tone::Error));
        assert_eq!(tone_for("Reason", "whatever"), None);
    }

    #[test]
    fn suffix_check_does_not_split_a_multibyte_character() {
        // `é` is two bytes; a naive `&s[len - 5..]` would panic here.
        assert_eq!(tone_of_value("éééé"), None);
        assert_eq!(tone_of_value(""), None);
    }

    fn age(seconds: i64) -> String {
        let mut out = String::new();
        write_age(&mut out, seconds);
        out
    }

    #[test]
    fn ages_read_like_kubectl() {
        assert_eq!(age(0), "0s");
        assert_eq!(age(59), "59s");
        assert_eq!(age(60), "1m0s");
        assert_eq!(age(3_599), "59m59s");
        assert_eq!(age(3_600), "1h0m");
        assert_eq!(age(86_399), "23h59m");
        assert_eq!(age(86_400), "1d0h");
        assert_eq!(age(86_400 * 99 + 3_600), "99d1h");
        assert_eq!(age(86_400 * 100), "100d");
        assert_eq!(age(-5), "0s", "a clock skew never shows a negative age");
    }

    #[test]
    fn timestamps_parse_to_epoch_seconds() {
        assert_eq!(parse_timestamp("1970-01-01T00:01:00Z"), Some(60));
        assert_eq!(parse_timestamp("not a time"), None);
    }
}
