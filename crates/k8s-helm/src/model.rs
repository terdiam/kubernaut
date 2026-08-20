//! Types shared by the release store and the CLI wrapper.

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum HelmError {
    #[error(transparent)]
    Core(#[from] k8s_core::CoreError),

    #[error("kubernetes api error: {0}")]
    Api(Box<kube::Error>),

    #[error("helm is not available: {0}")]
    NoBinary(String),

    #[error("helm {command} failed: {message}")]
    Command { command: String, message: String },

    #[error("could not read release `{release}`: {reason}")]
    Decode { release: String, reason: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl From<kube::Error> for HelmError {
    fn from(err: kube::Error) -> Self {
        Self::Api(Box::new(err))
    }
}

impl HelmError {
    pub fn other(msg: impl std::fmt::Display) -> Self {
        Self::Other(msg.to_string())
    }
}

pub type Result<T, E = HelmError> = std::result::Result<T, E>;

/// A release as listed. Deliberately close to `helm list` so the two agree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub name: String,
    pub namespace: String,
    pub revision: i64,
    /// `deployed` | `failed` | `pending-upgrade` | `superseded` | …
    pub status: String,
    pub chart: String,
    pub chart_version: String,
    pub app_version: Option<String>,
    /// RFC3339, from the release's own metadata.
    pub updated: Option<String>,
    pub description: Option<String>,
    /// True when a newer revision exists in a non-terminal state.
    #[serde(default)]
    pub pending: bool,
}

/// One entry in a release's history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRevision {
    pub revision: i64,
    pub status: String,
    pub chart_version: String,
    pub app_version: Option<String>,
    pub updated: Option<String>,
    pub description: Option<String>,
}

/// Everything the release detail pane shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDetail {
    pub release: Release,
    /// Values the user supplied, as YAML. Empty when the release used defaults.
    pub user_values: String,
    /// Chart defaults merged with the user's values — what actually rendered.
    pub effective_values: String,
    /// Rendered manifest currently installed.
    pub manifest: String,
    pub notes: String,
}

/// A configured chart repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub name: String,
    pub url: String,
}

/// A chart found by searching configured repositories.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartResult {
    /// `repo/chart`
    pub name: String,
    pub version: String,
    pub app_version: Option<String>,
    pub description: Option<String>,
}

/// One object that differs between the installed and proposed manifests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentChange {
    pub kind: String,
    pub name: String,
    /// `added` | `removed` | `modified`
    pub change: String,
    /// True when the only difference is generated Secret material — a
    /// certificate or password the chart regenerates on every render.
    pub generated_only: bool,
}

/// Result of comparing a proposed upgrade against what is installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeDiff {
    pub unified: String,
    pub changed: bool,
    /// Per-object summary — far easier to judge than a hundred diff lines.
    #[serde(default)]
    pub documents: Vec<DocumentChange>,
    /// True when every difference is regenerated Secret material.
    ///
    /// Charts that call `genSelfSignedCert` or `randAlphaNum` produce new
    /// values on every render, so a plain text diff reports changes even when
    /// nothing meaningful differs. Saying so is the difference between a diff
    /// people trust and one they learn to ignore.
    #[serde(default)]
    pub generated_only: bool,
}

/// Where the helm binary came from, shown in diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelmInfo {
    pub path: String,
    pub version: String,
    /// True when the binary shipped with the app rather than found on PATH.
    pub bundled: bool,
}
