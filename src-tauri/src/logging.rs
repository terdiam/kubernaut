//! Local logging and crash capture.
//!
//! Nothing here leaves the machine. This app holds credentials for production
//! clusters, and the only privacy guarantee worth trusting is that there is no
//! code to send anything anywhere. What it does instead is keep a rolling local
//! log and record panics into it, so a bug report can be a file the user reads
//! first and chooses to share.

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
};

use tracing_subscriber::EnvFilter;

/// Days of logs kept. Enough to cover "it broke on Friday" reported on Monday.
const KEEP_FILES: usize = 7;

pub fn log_directory() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "kubernaut", "Kubernaut")
        .map(|directories| directories.data_local_dir().join("logs"))
}

/// Start logging to the terminal and to a rolling file.
///
/// Returns the guard that flushes the file writer; dropping it early loses
/// buffered lines, including the last ones before a crash.
pub fn init() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_env("KUBERNAUT_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,kube=warn,tower=warn"));

    let Some(directory) = log_directory() else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
        return None;
    };
    if std::fs::create_dir_all(&directory).is_err() {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
        return None;
    }

    prune(&directory);

    let appender = tracing_appender::rolling::daily(&directory, "kubernaut.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    // The file gets timestamps and targets; the terminal stays readable.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();

    install_panic_hook();
    Some(guard)
}

/// Record panics in the log rather than losing them to a closed window.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown location".into());

        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with no message".into());

        // `Backtrace::force_capture` rather than `capture`, because the useful
        // case is a user's machine where `RUST_BACKTRACE` is not set.
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            location = %location,
            message = %message,
            "panic\n{backtrace}"
        );

        previous(info);
    }));
}

/// Keep the log directory from growing without bound.
fn prune(directory: &PathBuf) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("kubernaut.log")
        })
        .collect();

    files.sort_by_key(|entry| entry.file_name());
    while files.len() > KEEP_FILES {
        if let Some(oldest) = files.first() {
            let _ = std::fs::remove_file(oldest.path());
        }
        files.remove(0);
    }
}

/// A crash recorded in an earlier run, if there is one.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashReport {
    /// Log file the panic was found in.
    pub file: String,
    /// The panic line and the lines around it.
    pub excerpt: String,
}

/// Look for a panic in the most recent log file.
///
/// Deliberately shallow: this is a prompt to look at the log, not a crash
/// analytics pipeline. Nothing is uploaded and nothing is parsed beyond finding
/// the marker.
pub fn last_crash() -> Option<CrashReport> {
    let directory = log_directory()?;
    let mut files: Vec<_> = std::fs::read_dir(&directory)
        .ok()?
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("kubernaut.log")
        })
        .collect();
    files.sort_by_key(|entry| entry.file_name());

    let newest = files.last()?;
    let reader = BufReader::new(File::open(newest.path()).ok()?);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let index = lines.iter().rposition(|line| line.contains("panic"))?;
    let start = index.saturating_sub(2);
    let end = (index + 12).min(lines.len());

    Some(CrashReport {
        file: newest.path().display().to_string(),
        excerpt: lines[start..end].join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_directory_is_under_the_app_data_path() {
        let directory = log_directory().expect("a data directory on this platform");
        assert!(directory.ends_with("logs"));
        assert!(
            directory.to_string_lossy().contains("Kubernaut"),
            "{}",
            directory.display()
        );
    }
}
