//! Findings and their vocabulary.

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error(transparent)]
    Core(#[from] k8s_core::CoreError),

    #[error("kubernetes api error: {0}")]
    Api(Box<kube::Error>),

    #[error("no vulnerability scanner available: {0}")]
    NoScanner(String),

    #[error("scan failed: {0}")]
    Scan(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl From<kube::Error> for SecurityError {
    fn from(err: kube::Error) -> Self {
        Self::Api(Box::new(err))
    }
}

impl SecurityError {
    pub fn other(msg: impl std::fmt::Display) -> Self {
        Self::Other(msg.to_string())
    }
}

pub type Result<T, E = SecurityError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Ordered worst-first so a plain sort puts what matters at the top.
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_uppercase().as_str() {
            "CRITICAL" => Self::Critical,
            "HIGH" => Self::High,
            "MEDIUM" => Self::Medium,
            "LOW" => Self::Low,
            _ => Self::Info,
        }
    }
}

/// Where a finding came from, so the UI can explain what is and is not covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// Read from the object's own spec — always available.
    Posture,
    /// Derived from Roles and bindings.
    Rbac,
    /// From a vulnerability scanner.
    Image,
}

/// One thing worth someone's attention.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Stable check identifier, so a finding can be suppressed or looked up.
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub source: Source,

    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    /// `group/version/plural`, so the UI can open the object.
    pub resource: String,
    /// Container the finding is about, when it is narrower than the object.
    pub container: Option<String>,

    /// What is wrong, in the specific.
    pub message: String,
    /// What to do about it.
    pub remediation: String,
    /// True for objects Kubernetes or the distribution ships itself.
    ///
    /// A cluster has hundreds of built-in Roles with broad permissions by
    /// design. Reporting them beside a developer's over-privileged Role buries
    /// the one that matters, so they are marked and filtered out by default.
    #[serde(default)]
    pub builtin: bool,
}

/// Counts by severity, for the summary header.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityCounts {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub info: usize,
}

impl SeverityCounts {
    pub fn tally(findings: &[Finding]) -> Self {
        let mut counts = Self::default();
        for finding in findings {
            match finding.severity {
                Severity::Critical => counts.critical += 1,
                Severity::High => counts.high += 1,
                Severity::Medium => counts.medium += 1,
                Severity::Low => counts.low += 1,
                Severity::Info => counts.info += 1,
            }
        }
        counts
    }
}

/// What a scan covered and what it could not.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub findings: Vec<Finding>,
    pub counts: SeverityCounts,
    /// Objects examined, so "0 findings" can be told from "nothing scanned".
    pub examined: usize,
    /// Findings hidden because they concern built-in objects.
    pub builtin_hidden: usize,
    /// Anything the scan could not do, in plain words.
    #[serde(default)]
    pub limitations: Vec<String>,
    pub scanned_at: String,
}

impl ScanReport {
    pub fn new(mut findings: Vec<Finding>, examined: usize, limitations: Vec<String>) -> Self {
        findings.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.namespace.cmp(&b.namespace))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
        let builtin_hidden = findings.iter().filter(|f| f.builtin).count();
        let counts = SeverityCounts::tally(&findings);

        Self {
            findings,
            counts,
            examined,
            builtin_hidden,
            limitations,
            scanned_at: k8s_openapi::jiff::Timestamp::now().to_string(),
        }
    }
}
