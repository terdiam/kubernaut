//! Image vulnerabilities.
//!
//! Two sources, preferred in this order:
//!
//! * **Trivy Operator**, when it runs in the cluster — it has already scanned
//!   every image and stores the results as `VulnerabilityReport` objects.
//!   Reading them is one API call and costs nothing.
//! * **The `trivy` binary**, otherwise — scanning locally pulls image metadata
//!   and a vulnerability database, which is slow and needs network access, so
//!   it runs on demand rather than in the background.
//!
//! When neither is available the app says so plainly instead of showing an
//! empty list that reads like "no vulnerabilities".

use std::{path::Path, process::Stdio, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use kube::{
    Api,
    api::{DynamicObject, ListParams},
    discovery::ApiResource,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Finding, Result, ScanReport, SecurityError, Severity, Source};

/// How long a single image scan may take, once the database is in place.
///
/// Pulling and unpacking a large image is the slow part; a minute is typical
/// and ten is a stuck registry.
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long the first-run database download may take.
///
/// The download is around 110 MiB (about 1.2 GB once unpacked). Folding it into
/// the first scan's timeout is how the first scan a user ever runs fails with a
/// confusing "timed out".
const DATABASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// Repositories to try for the vulnerability database, in order.
///
/// Trivy defaults to a Google mirror. On the machine this was developed
/// against, that mirror accepted the connection and then transferred nothing —
/// zero bytes in five minutes — while the upstream repository completed in
/// eight seconds. A stalled mirror is indistinguishable from a slow link
/// unless you try somewhere else, so the fallback is automatic and the UI is
/// told which one worked.
const DATABASE_REPOSITORIES: &[&str] = &[
    // Trivy's own default, kept first so a working mirror is still preferred.
    "mirror.gcr.io/aquasec/trivy-db:2",
    "ghcr.io/aquasecurity/trivy-db:2",
];

/// A stalled registry looks identical to a slow one; give each candidate its
/// own budget so a dead mirror cannot consume the whole allowance.
const REPOSITORY_ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);

/// Where vulnerability data came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Scanner {
    /// Reports already in the cluster.
    TrivyOperator { reports: usize },
    /// A local binary.
    TrivyBinary {
        path: String,
        version: String,
        /// False until the vulnerability database has been downloaded. The
        /// first download is large, so the UI offers it as its own step rather
        /// than hiding it inside a scan.
        database_ready: bool,
    },
    /// Nothing available; `reason` explains what to install.
    None { reason: String },
}

/// One CVE affecting one image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vulnerability {
    pub id: String,
    pub severity: Severity,
    pub package: String,
    pub installed_version: String,
    pub fixed_version: Option<String>,
    pub title: String,
    pub image: String,
    /// Where the image runs, for triage.
    pub namespace: Option<String>,
    pub workload: Option<String>,
}

/// Detect what can scan.
pub async fn detect(cluster: &Arc<ClusterHandle>, sidecar_dir: Option<&Path>) -> Scanner {
    // Ask discovery first. Listing a CRD that does not exist returns 404, which
    // the client logs as a warning on every detection — alarming noise for a
    // condition that is simply "the operator is not installed".
    if operator_installed(cluster).await
        && let Ok(count) = operator_report_count(cluster).await
    {
        return Scanner::TrivyOperator { reports: count };
    }

    let binary = sidecar_dir
        .map(|dir| dir.join(if cfg!(windows) { "trivy.exe" } else { "trivy" }))
        .filter(|path| path.is_file())
        .or_else(|| k8s_core::paths::which("trivy"));

    match binary {
        Some(path) => match version_of(&path).await {
            Ok(version) => Scanner::TrivyBinary {
                database_ready: database_present(&path).await,
                path: path.display().to_string(),
                version,
            },
            Err(err) => Scanner::None {
                reason: format!(
                    "trivy was found at {} but did not run: {err}",
                    path.display()
                ),
            },
        },
        None => Scanner::None {
            reason: "Install Trivy Operator in the cluster for continuous scanning, or the \
                     `trivy` CLI on this machine to scan on demand."
                .into(),
        },
    }
}

/// The `VulnerabilityReport` CRD, addressed dynamically because it only exists
/// when the operator is installed.
fn report_resource() -> ApiResource {
    ApiResource {
        group: "aquasecurity.github.io".into(),
        version: "v1alpha1".into(),
        api_version: "aquasecurity.github.io/v1alpha1".into(),
        kind: "VulnerabilityReport".into(),
        plural: "vulnerabilityreports".into(),
    }
}

/// Does this cluster define the operator's report CRD?
async fn operator_installed(cluster: &Arc<ClusterHandle>) -> bool {
    let discovery = match cluster.discovery() {
        Some(discovery) => discovery,
        None => match cluster.refresh_discovery().await {
            Ok(discovery) => discovery,
            Err(_) => return false,
        },
    };
    discovery
        .get("aquasecurity.github.io/v1alpha1/vulnerabilityreports")
        .is_some()
}

async fn operator_report_count(cluster: &Arc<ClusterHandle>) -> Result<usize> {
    let api: Api<DynamicObject> = Api::all_with(cluster.client.clone(), &report_resource());
    let list = api.list(&ListParams::default().limit(1)).await?;
    Ok(list.items.len())
}

fn severity_of(entry: &Value) -> Severity {
    Severity::parse(
        entry
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN"),
    )
}

/// Read everything the operator has already found.
pub async fn from_operator(
    cluster: &Arc<ClusterHandle>,
    namespace: Option<&str>,
) -> Result<Vec<Vulnerability>> {
    let resource = report_resource();
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(cluster.client.clone(), ns, &resource),
        None => Api::all_with(cluster.client.clone(), &resource),
    };

    let list = api.list(&ListParams::default()).await?;
    let mut out = Vec::new();

    for report in list.items {
        let namespace = report.metadata.namespace.clone();
        // The operator records which workload the image belongs to in labels.
        let labels = report.metadata.labels.clone().unwrap_or_default();
        let workload = labels
            .get("trivy-operator.resource.name")
            .cloned()
            .or_else(|| report.metadata.name.clone());

        let data = &report.data;
        let image = format!(
            "{}{}:{}",
            data.pointer("/report/registry/server")
                .and_then(Value::as_str)
                .map(|server| format!("{server}/"))
                .unwrap_or_default(),
            data.pointer("/report/artifact/repository")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            data.pointer("/report/artifact/tag")
                .and_then(Value::as_str)
                .unwrap_or("latest")
        );

        for entry in data
            .pointer("/report/vulnerabilities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push(Vulnerability {
                id: entry
                    .get("vulnerabilityID")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                severity: severity_of(entry),
                package: entry
                    .get("resource")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                installed_version: entry
                    .get("installedVersion")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                fixed_version: entry
                    .get("fixedVersion")
                    .and_then(Value::as_str)
                    .filter(|version| !version.is_empty())
                    .map(String::from),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                image: image.clone(),
                namespace: namespace.clone(),
                workload: workload.clone(),
            });
        }
    }

    Ok(out)
}

/// Is the vulnerability database already downloaded?
///
/// Read from `trivy version --format json`, which reports the database's
/// `DownloadedAt` when one is present. Asking trivy beats guessing a cache
/// path, which differs by platform and configuration — and beats probing with
/// `--download-db-only --skip-db-update`, which trivy rejects as a contradictory
/// flag combination, so that probe reported "no database" every single time.
async fn database_present(binary: &Path) -> bool {
    let run = tokio::process::Command::new(binary)
        .args(["version", "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let Ok(Ok(output)) = tokio::time::timeout(std::time::Duration::from_secs(30), run).await else {
        return false;
    };

    serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/VulnerabilityDB/DownloadedAt")
                .and_then(Value::as_str)
                .map(|stamp| !stamp.is_empty())
        })
        .unwrap_or(false)
}

/// Download the vulnerability database, trying each known repository in turn.
///
/// Separate from scanning on purpose: this is the slow, large step, and a user
/// deserves to be told that is what is happening. Returns the repository that
/// worked.
pub async fn download_database(binary: &Path) -> Result<String> {
    let deadline = tokio::time::Instant::now() + DATABASE_TIMEOUT;
    let mut failures: Vec<String> = Vec::new();

    for repository in DATABASE_REPOSITORIES {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let budget = REPOSITORY_ATTEMPT_TIMEOUT.min(remaining);

        let run = tokio::process::Command::new(binary)
            .args(["image", "--download-db-only", "--db-repository", repository])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match tokio::time::timeout(budget, run).await {
            Ok(Ok(output)) if output.status.success() => {
                tracing::info!(repository, "vulnerability database downloaded");
                return Ok((*repository).to_string());
            }
            Ok(Ok(output)) => failures.push(format!(
                "{repository}: {}",
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .last()
                    .unwrap_or("failed")
                    .trim()
            )),
            Ok(Err(err)) => failures.push(format!("{repository}: {err}")),
            Err(_) => failures.push(format!(
                "{repository}: no progress within {}s",
                budget.as_secs()
            )),
        }
    }

    Err(SecurityError::Scan(format!(
        "could not download the vulnerability database. Tried:\n  {}",
        failures.join("\n  ")
    )))
}

async fn version_of(path: &Path) -> Result<String> {
    let output = tokio::process::Command::new(path)
        .args(["version", "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    Ok(parsed
        .get("Version")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// Scan one image with the local binary.
pub async fn scan_image(binary: &Path, image: &str) -> Result<Vec<Vulnerability>> {
    let run = tokio::process::Command::new(binary)
        .args([
            "image",
            "--quiet",
            "--format",
            "json",
            // Vulnerabilities only: secret scanning on every image in a cluster
            // is far slower and answers a different question.
            "--scanners",
            "vuln",
            image,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let output = match tokio::time::timeout(SCAN_TIMEOUT, run).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(SecurityError::Scan(format!(
                "scanning `{image}` timed out after {} minutes",
                SCAN_TIMEOUT.as_secs() / 60
            )));
        }
    };

    if !output.status.success() {
        return Err(SecurityError::Scan(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| SecurityError::Scan(format!("unreadable trivy output: {err}")))?;

    let mut out = Vec::new();
    for result in parsed
        .get("Results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for entry in result
            .get("Vulnerabilities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push(Vulnerability {
                id: entry
                    .get("VulnerabilityID")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                severity: Severity::parse(
                    entry
                        .get("Severity")
                        .and_then(Value::as_str)
                        .unwrap_or("UNKNOWN"),
                ),
                package: entry
                    .get("PkgName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                installed_version: entry
                    .get("InstalledVersion")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                fixed_version: entry
                    .get("FixedVersion")
                    .and_then(Value::as_str)
                    .filter(|version| !version.is_empty())
                    .map(String::from),
                title: entry
                    .get("Title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                image: image.to_string(),
                namespace: None,
                workload: None,
            });
        }
    }
    Ok(out)
}

/// Turn vulnerabilities into findings, one per image and severity band.
///
/// A single image can carry hundreds of CVEs; listing each as its own finding
/// drowns everything else. The detail stays available per image.
pub fn summarise(vulnerabilities: &[Vulnerability]) -> Vec<Finding> {
    use std::collections::BTreeMap;

    let mut by_image: BTreeMap<(&str, Severity), Vec<&Vulnerability>> = BTreeMap::new();
    for vulnerability in vulnerabilities {
        if matches!(vulnerability.severity, Severity::Low | Severity::Info) {
            continue;
        }
        by_image
            .entry((vulnerability.image.as_str(), vulnerability.severity))
            .or_default()
            .push(vulnerability);
    }

    by_image
        .into_iter()
        .map(|((image, severity), entries)| {
            let fixable = entries.iter().filter(|v| v.fixed_version.is_some()).count();
            let example = entries
                .iter()
                .take(3)
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");

            Finding {
                id: format!("CVE-{severity:?}").to_uppercase(),
                title: format!("{:?} vulnerabilities in image", severity),
                severity,
                source: Source::Image,
                kind: "Image".into(),
                namespace: entries.first().and_then(|v| v.namespace.clone()),
                name: image.to_string(),
                resource: "core/v1/pods".into(),
                container: None,
                message: format!(
                    "{} {:?} vulnerabilities, {fixable} with a fix available. For example: {example}.",
                    entries.len(),
                    severity
                ),
                remediation: if fixable > 0 {
                    "Rebuild on a newer base image, or update the affected packages."
                        .to_string()
                } else {
                    "No upstream fix yet. Consider whether the affected package is reachable."
                        .to_string()
                },
                builtin: false,
            }
        })
        .collect()
}

/// Build a report from whatever source is available.
pub fn report(vulnerabilities: Vec<Vulnerability>, scanner: &Scanner, images: usize) -> ScanReport {
    let limitations = match scanner {
        Scanner::None { reason } => vec![format!("No image scanning: {reason}")],
        Scanner::TrivyBinary {
            database_ready: false,
            ..
        } => vec![
            "The vulnerability database has not been downloaded yet — about 110 MiB to fetch, \
             around 1.2 GB on disk. Download it before scanning."
                .to_string(),
        ],
        Scanner::TrivyBinary { .. } => vec![
            "Scanned on demand with the local trivy binary; results reflect the moment of the \
             scan."
                .to_string(),
        ],
        Scanner::TrivyOperator { .. } => Vec::new(),
    };
    ScanReport::new(summarise(&vulnerabilities), images, limitations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vulnerability(id: &str, severity: Severity, fixed: Option<&str>) -> Vulnerability {
        Vulnerability {
            id: id.into(),
            severity,
            package: "openssl".into(),
            installed_version: "1.1.1".into(),
            fixed_version: fixed.map(String::from),
            title: "something bad".into(),
            image: "nginx:1.14".into(),
            namespace: Some("default".into()),
            workload: Some("web".into()),
        }
    }

    /// Hundreds of CVEs per image is normal; one finding per CVE would bury
    /// every other kind of finding.
    #[test]
    fn vulnerabilities_are_grouped_per_image_and_severity() {
        let found = vec![
            vulnerability("CVE-1", Severity::Critical, Some("1.1.2")),
            vulnerability("CVE-2", Severity::Critical, None),
            vulnerability("CVE-3", Severity::High, Some("1.1.2")),
        ];
        let findings = summarise(&found);
        assert_eq!(findings.len(), 2);

        let critical = findings
            .iter()
            .find(|f| f.severity == Severity::Critical)
            .unwrap();
        assert!(
            critical.message.contains("2 Critical"),
            "{}",
            critical.message
        );
        assert!(
            critical.message.contains("1 with a fix"),
            "{}",
            critical.message
        );
    }

    /// Low and informational noise would otherwise dominate every image.
    #[test]
    fn low_severity_is_left_out_of_findings() {
        let found = vec![
            vulnerability("CVE-4", Severity::Low, None),
            vulnerability("CVE-5", Severity::Info, None),
        ];
        assert!(summarise(&found).is_empty());
    }

    #[test]
    fn unfixable_vulnerabilities_get_different_advice() {
        let found = vec![vulnerability("CVE-6", Severity::High, None)];
        let findings = summarise(&found);
        assert!(findings[0].remediation.contains("No upstream fix"));
    }

    /// The first scan a user runs would otherwise spend minutes downloading a
    /// database and then report a timeout, which reads like a broken feature.
    #[test]
    fn a_missing_database_is_called_out_before_scanning() {
        let scanner = Scanner::TrivyBinary {
            path: "/usr/local/bin/trivy".into(),
            version: "0.74.0".into(),
            database_ready: false,
        };
        let report = report(Vec::new(), &scanner, 5);
        assert!(
            report.limitations[0].contains("database has not been downloaded"),
            "{:?}",
            report.limitations
        );
    }

    /// An empty list must not read as "nothing to worry about" when there was
    /// simply nothing to scan with.
    #[test]
    fn a_missing_scanner_is_stated_as_a_limitation() {
        let scanner = Scanner::None {
            reason: "install trivy".into(),
        };
        let report = report(Vec::new(), &scanner, 0);
        assert!(report.findings.is_empty());
        assert_eq!(report.limitations.len(), 1);
        assert!(report.limitations[0].contains("No image scanning"));
    }

    /// `trivy version --format json` carries the database timestamp; the
    /// earlier probe used a flag combination trivy rejects outright, so it
    /// reported "no database" even with one in place.
    #[test]
    fn database_presence_is_read_from_the_version_document() {
        let present: Value = serde_json::from_str(
            r#"{"Version":"0.74.0","VulnerabilityDB":{"DownloadedAt":"2026-08-20T02:07:59Z"}}"#,
        )
        .unwrap();
        assert!(
            present
                .pointer("/VulnerabilityDB/DownloadedAt")
                .and_then(Value::as_str)
                .is_some_and(|stamp| !stamp.is_empty())
        );

        let absent: Value = serde_json::from_str(r#"{"Version":"0.74.0"}"#).unwrap();
        assert!(absent.pointer("/VulnerabilityDB/DownloadedAt").is_none());
    }

    #[test]
    fn severity_parsing_is_case_insensitive_and_defaults_to_info() {
        assert_eq!(Severity::parse("critical"), Severity::Critical);
        assert_eq!(Severity::parse("HIGH"), Severity::High);
        assert_eq!(Severity::parse("nonsense"), Severity::Info);
    }
}
