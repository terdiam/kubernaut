//! Local cache of Trivy image scan results.
//!
//! The local-binary scanner is genuinely slow — up to ten minutes per image,
//! plus a ~1.2GB vulnerability database download on first use — and every
//! image was re-scanned on every Security Center open. Caching by image
//! reference avoids that: a scan result is reused until it goes stale.

use std::time::Duration;

use k8s_security::vulnerabilities::Vulnerability;
use parking_lot::Mutex;
use rusqlite::{Connection, params};

fn cache_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("dev", "kubernaut", "Kubernaut")
        .map(|directories| directories.data_local_dir().join("scan-cache.sqlite"))
}

fn open() -> Option<Connection> {
    let path = cache_path()?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let conn = Connection::open(&path).ok()?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS image_scans (
            image TEXT PRIMARY KEY,
            scanned_at INTEGER NOT NULL,
            vulnerabilities_json TEXT NOT NULL
        )",
        [],
    )
    .ok()?;
    Some(conn)
}

/// Cache of Trivy scan results. Any open/migrate failure degrades to a no-op
/// cache — a corrupt or unwritable cache file must not stop scanning from
/// working, only stop it from being fast.
pub struct ScanCache {
    conn: Option<Mutex<Connection>>,
}

impl ScanCache {
    pub fn new() -> Self {
        Self {
            conn: open().map(Mutex::new),
        }
    }

    /// `None` on a miss or a result older than `max_age`.
    pub fn get(&self, image: &str, max_age: Duration) -> Option<Vec<Vulnerability>> {
        let conn = self.conn.as_ref()?.lock();
        let (scanned_at, json): (i64, String) = conn
            .query_row(
                "SELECT scanned_at, vulnerabilities_json FROM image_scans WHERE image = ?1",
                params![image],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()?;

        let now = k8s_openapi::jiff::Timestamp::now().as_second();
        if now - scanned_at > max_age.as_secs() as i64 {
            return None;
        }
        serde_json::from_str(&json).ok()
    }

    pub fn put(&self, image: &str, vulnerabilities: &[Vulnerability]) {
        let Some(conn) = &self.conn else { return };
        let Ok(json) = serde_json::to_string(vulnerabilities) else {
            return;
        };
        let now = k8s_openapi::jiff::Timestamp::now().as_second();
        let _ = conn.lock().execute(
            "INSERT INTO image_scans (image, scanned_at, vulnerabilities_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(image) DO UPDATE SET scanned_at = excluded.scanned_at, vulnerabilities_json = excluded.vulnerabilities_json",
            params![image, now, json],
        );
    }
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new()
    }
}
