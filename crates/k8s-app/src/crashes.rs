//! What went wrong in earlier runs, read back from files on this machine.
//!
//! Two sources. A panic is written into the app's own log by the panic hook
//! (see [`crate::logging`]). A crash the app could not catch — a segfault, or
//! being killed for a bad code signature — leaves nothing in that log, but the
//! operating system keeps its own report, which macOS puts in
//! `~/Library/Logs/DiagnosticReports`. Both are read here and shown together,
//! newest first. Nothing is sent anywhere.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{logging, present};

/// The most of one log file that is read, from its end. A day's log of a busy
/// session is far smaller.
const MAX_READ: u64 = 8 * 1024 * 1024;
/// Entries returned.
const MAX_ENTRIES: usize = 50;
/// Lines kept of one panic, which is a backtrace and not worth more.
const MAX_PANIC_LINES: usize = 120;
/// Stack frames kept of one system report.
const MAX_FRAMES: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashKind {
    /// A Rust panic, recorded in the app's log.
    Panic,
    /// A report the operating system wrote for a process it ended.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashEntry {
    pub kind: CrashKind,
    /// Seconds since the Unix epoch; 0 when the report gave no usable time.
    pub at: i64,
    /// One line: what happened.
    pub title: String,
    /// Everything worth reading, as plain text, ready to copy.
    pub detail: String,
    /// The file it came from, by name.
    pub source: String,
}

/// Where the operating system keeps reports for crashed apps, if this one does.
pub fn reports_directory() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join("Library/Logs/DiagnosticReports"))
    } else {
        None
    }
}

/// Every crash on record, newest first.
pub fn find() -> Vec<CrashEntry> {
    let mut entries = Vec::new();
    if let Some(directory) = logging::log_directory() {
        for path in files_in(&directory, |name| name.starts_with("kubernaut.log")) {
            if let Some(text) = read_tail(&path) {
                entries.extend(parse_panics(&text, &file_name(&path)));
            }
        }
    }
    if let Some(directory) = reports_directory() {
        for path in files_in(&directory, is_own_report) {
            if let Some(text) = read_tail(&path)
                && let Some(entry) = parse_report(&text, &file_name(&path))
            {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.at));
    entries.truncate(MAX_ENTRIES);
    entries
}

/// A report for this app: the files are named for the executable.
fn is_own_report(name: &str) -> bool {
    name.starts_with("kubernaut-ui") && (name.ends_with(".ips") || name.ends_with(".crash"))
}

fn files_in(directory: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| keep(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The end of a file, as text. A cut that lands mid-line or mid-character is
/// harmless: the parsers skip what they cannot place.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length > MAX_READ {
        file.seek(SeekFrom::Start(length - MAX_READ)).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(MAX_READ).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether a log line starts a record: tracing writes an RFC 3339 time first,
/// and a backtrace's own lines never look like that.
fn starts_record(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > 20 && bytes.get(4) == Some(&b'-') && bytes.get(10) == Some(&b'T')
}

/// The message of an `ERROR` record that reports a panic.
fn panic_message(line: &str) -> Option<&str> {
    let (_, after_level) = line.split_once(" ERROR ")?;
    let message = after_level
        .split_once(": ")
        .map_or(after_level, |(_, message)| message);
    message.starts_with("panic").then_some(message)
}

/// Panics recorded in log text.
pub fn parse_panics(text: &str, source: &str) -> Vec<CrashEntry> {
    struct Open<'a> {
        at: i64,
        message: &'a str,
        lines: Vec<&'a str>,
    }
    let close = |open: Open<'_>, out: &mut Vec<CrashEntry>| {
        let title = open
            .message
            .strip_prefix("panic at ")
            .and_then(|rest| rest.lines().next())
            .map_or_else(|| "panic (no message recorded)".to_string(), str::to_string);
        out.push(CrashEntry {
            kind: CrashKind::Panic,
            at: open.at,
            title,
            detail: open.lines.join("\n"),
            source: source.to_string(),
        });
    };

    let mut out = Vec::new();
    let mut current: Option<Open<'_>> = None;
    for line in text.lines() {
        if starts_record(line) {
            if let Some(open) = current.take() {
                close(open, &mut out);
            }
            if let Some(message) = panic_message(line) {
                let stamp = line.split_whitespace().next().unwrap_or_default();
                current = Some(Open {
                    at: present::parse_timestamp(stamp).unwrap_or(0),
                    message,
                    lines: vec![line],
                });
            }
        } else if let Some(open) = &mut current
            && open.lines.len() < MAX_PANIC_LINES
        {
            open.lines.push(line);
        }
    }
    if let Some(open) = current {
        close(open, &mut out);
    }
    out
}

/// `2026-11-14 14:37:37.00 +0700` as RFC 3339, which the rest of the app reads.
fn report_time(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once(' ')?;
    let (time, offset) = rest.split_once(' ')?;
    let time = time.split('.').next()?;
    let (sign, digits) = offset.split_at_checked(1)?;
    let (hours, minutes) = digits.split_at_checked(2)?;
    present::parse_timestamp(&format!("{date}T{time}{sign}{hours}:{minutes}"))
}

/// One macOS `.ips` report: a JSON header line, then a JSON body.
pub fn parse_report(text: &str, source: &str) -> Option<CrashEntry> {
    let (header, body) = text.split_once('\n')?;
    let header: Value = serde_json::from_str(header).ok()?;
    let body: Value = serde_json::from_str(body).ok()?;

    let when = header
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("");
    let text_of = |pointer: &str| body.pointer(pointer).and_then(Value::as_str);

    let exception = text_of("/exception/type").unwrap_or("unknown exception");
    let signal = text_of("/exception/signal");
    let indicator = text_of("/termination/indicator");
    let namespace = text_of("/termination/namespace");

    let mut title = exception.to_string();
    if let Some(signal) = signal {
        title.push_str(" — ");
        title.push_str(signal);
    }

    let mut detail = vec![
        format!("Time: {when}"),
        format!("App: {}", text_of("/procName").unwrap_or("kubernaut-ui")),
        format!("Exception: {title}"),
    ];
    if let Some(indicator) = indicator {
        detail.push(format!(
            "Terminated by: {}{indicator}",
            namespace.map(|n| format!("{n}: ")).unwrap_or_default()
        ));
    }

    let images = body.get("usedImages").and_then(Value::as_array);
    let faulting = body
        .get("faultingThread")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| body.get("threads")?.as_array()?.get(index));
    if let Some(frames) = faulting
        .and_then(|thread| thread.get("frames")?.as_array())
        .filter(|frames| !frames.is_empty())
    {
        detail.push(String::new());
        detail.push("Crashed thread:".into());
        for (number, frame) in frames.iter().take(MAX_FRAMES).enumerate() {
            let image = frame
                .get("imageIndex")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| images?.get(index)?.get("name")?.as_str())
                .unwrap_or("?");
            let symbol = frame.get("symbol").and_then(Value::as_str).unwrap_or("?");
            let place = match (
                frame.get("sourceFile").and_then(Value::as_str),
                frame.get("sourceLine").and_then(Value::as_u64),
            ) {
                (Some(file), Some(line)) => format!(" ({file}:{line})"),
                _ => String::new(),
            };
            detail.push(format!("{number:>3}  {image}  {symbol}{place}"));
        }
    }

    Some(CrashEntry {
        kind: CrashKind::System,
        at: report_time(when).unwrap_or(0),
        title,
        detail: detail.join("\n"),
        source: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{CrashKind, is_own_report, parse_panics, parse_report, report_time};

    const LOG: &str = "\
2026-11-14T07:00:00.000000Z  INFO k8s_app::boot: started
2026-11-14T07:49:43.430302Z ERROR k8s_app::logging: panic at crates/x/src/lib.rs:12:5: index out of bounds
stack backtrace:
   0: rust_begin_unwind
   1: kubernaut_ui::main
2026-11-14T07:50:00.000000Z  INFO k8s_app::boot: after
2026-11-14T08:00:00.000000Z ERROR k8s_app::logging: panic
   0: old style
";

    #[test]
    fn a_panic_is_found_with_its_backtrace_and_stops_at_the_next_record() {
        let found = parse_panics(LOG, "kubernaut.log.2026-11-14");
        assert_eq!(found.len(), 2);

        let first = &found[0];
        assert_eq!(first.kind, CrashKind::Panic);
        assert_eq!(first.title, "crates/x/src/lib.rs:12:5: index out of bounds");
        assert!(first.detail.contains("kubernaut_ui::main"));
        assert!(
            !first.detail.contains("after"),
            "the next record is not part of it"
        );
        assert_eq!(first.at, 1_794_642_583);

        // The older log format had no message in the header.
        assert_eq!(found[1].title, "panic (no message recorded)");
        assert!(found[1].detail.contains("old style"));
    }

    #[test]
    fn a_log_without_panics_yields_nothing_and_a_cut_start_is_skipped() {
        assert!(parse_panics("2026-11-14T07:00:00.000000Z  INFO a: b\n", "x").is_empty());
        assert!(parse_panics("  3: half a backtrace\n", "x").is_empty());
        assert!(parse_panics("", "x").is_empty());
    }

    const REPORT: &str = r#"{"app_name":"kubernaut-ui","timestamp":"2026-11-14 14:37:37.00 +0700","bundleID":"dev.kubernaut.gpui-dev"}
{"procName":"kubernaut-ui","exception":{"type":"EXC_CRASH","signal":"SIGKILL (Code Signature Invalid)"},"termination":{"namespace":"CODESIGNING","indicator":"Taskgated Invalid Signature"},"faultingThread":0,"threads":[{"frames":[{"imageIndex":0,"symbol":"main","sourceFile":"main.rs","sourceLine":7},{"imageIndex":1,"symbol":"start"}]}],"usedImages":[{"name":"kubernaut-ui"},{"name":"dyld"}]}"#;

    #[test]
    fn a_system_report_gives_the_exception_the_reason_and_the_crashed_stack() {
        let entry = parse_report(REPORT, "kubernaut-ui.ips").unwrap();
        assert_eq!(entry.kind, CrashKind::System);
        assert_eq!(entry.title, "EXC_CRASH — SIGKILL (Code Signature Invalid)");
        assert!(
            entry
                .detail
                .contains("Terminated by: CODESIGNING: Taskgated Invalid Signature")
        );
        assert!(entry.detail.contains("kubernaut-ui  main (main.rs:7)"));
        assert!(entry.detail.contains("dyld  start"));
        // 14:37:37 at +07:00 is 07:37:37 UTC.
        assert_eq!(entry.at, 1_794_641_857);
    }

    #[test]
    fn a_report_with_no_frames_has_no_empty_stack_heading() {
        let report = r#"{"timestamp":"2026-11-14 14:37:37.00 +0700"}
{"exception":{"type":"EXC_CRASH"},"faultingThread":0,"threads":[{"frames":[]}]}"#;
        let entry = parse_report(report, "x.ips").unwrap();
        assert!(!entry.detail.contains("Crashed thread"));
    }

    #[test]
    fn something_that_is_not_a_report_is_ignored() {
        assert!(parse_report("not json\nnot json", "x").is_none());
        assert!(parse_report("{}", "x").is_none());
    }

    #[test]
    fn report_times_and_names_are_read_strictly() {
        assert_eq!(
            report_time("2026-11-14 00:00:00.00 +0000"),
            Some(1_794_614_400)
        );
        assert!(report_time("yesterday").is_none());
        assert!(is_own_report("kubernaut-ui-2026-11-14-143737.ips"));
        assert!(!is_own_report("Safari-2026.ips"));
        assert!(!is_own_report("kubernaut-ui-notes.txt"));
    }
}
