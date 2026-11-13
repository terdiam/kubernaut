//! Local audit trail for destructive actions.
//!
//! A separate concern from `AppState::ensure_writable`: that is a pre-check
//! refusing an action outright, while this records what actually happened
//! (including failures) after the call returns. Rolling JSON-Lines files,
//! pruned the same way as `logging.rs`'s crash log, but in their own
//! directory — this is a trail of what the app did, not diagnostic output.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Days of entries kept. Matches the crash-log retention in `logging.rs`.
const KEEP_FILES: usize = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// RFC3339, e.g. `2026-09-22T10:35:00Z`.
    pub timestamp: String,
    pub cluster: String,
    /// "scale" | "restart" | "cordon" | "uncordon" | "drain" | "delete" |
    /// "evict" | "helmRollback" | "helmUninstall" ...
    pub action: String,
    /// The object acted on, e.g. `deployments/default/api` or a node name.
    pub target: String,
    /// "ok", or the error message when the action failed.
    pub outcome: String,
}

fn audit_directory() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "kubernaut", "Kubernaut")
        .map(|directories| directories.data_local_dir().join("audit"))
}

/// `YYYY-MM-DD`, taken off the front of the RFC3339 `now()` string rather
/// than a dedicated date-formatting call, to avoid depending on more of
/// jiff's API than this one format.
fn today() -> String {
    k8s_openapi::jiff::Timestamp::now()
        .to_string()
        .chars()
        .take(10)
        .collect()
}

/// Keep the audit directory from growing without bound.
fn prune(directory: &PathBuf) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("audit-"))
        .collect();

    files.sort_by_key(|entry| entry.file_name());
    while files.len() > KEEP_FILES {
        if let Some(oldest) = files.first() {
            let _ = std::fs::remove_file(oldest.path());
        }
        files.remove(0);
    }
}

pub struct AuditLog {
    directory: Option<PathBuf>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            directory: audit_directory(),
        }
    }

    /// Record an entry. Best-effort: a write failure here must not fail the
    /// action it is recording.
    pub fn record(&self, entry: &AuditEntry) {
        let Some(directory) = &self.directory else {
            return;
        };
        if std::fs::create_dir_all(directory).is_err() {
            return;
        }
        prune(directory);

        let file = directory.join(format!("audit-{}.jsonl", today()));
        let Ok(line) = serde_json::to_string(entry) else {
            return;
        };
        use std::io::Write;
        if let Ok(mut handle) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = writeln!(handle, "{line}");
        }
    }

    /// Most recent entries, newest first, across as many rolled files as
    /// needed to reach `limit`.
    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        let Some(directory) = &self.directory else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("audit-"))
            .collect();
        files.sort_by_key(|entry| entry.file_name());

        let mut out = Vec::new();
        for file in files.iter().rev() {
            let Ok(text) = std::fs::read_to_string(file.path()) else {
                continue;
            };
            let mut parsed: Vec<AuditEntry> = text
                .lines()
                .rev()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            out.append(&mut parsed);
            if out.len() >= limit {
                break;
            }
        }
        out.truncate(limit);
        out
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}
