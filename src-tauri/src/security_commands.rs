//! Security Center: posture, RBAC and image vulnerabilities.

use k8s_security::{
    ScanReport,
    scan::{self, ImageUsage},
    vulnerabilities::{self, Scanner, Vulnerability},
};
use serde::Serialize;
use tauri::State;

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

/// Posture and RBAC in one pass. Neither needs a scanner.
#[tauri::command]
pub async fn security_scan(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
) -> CommandResult<ScanReport> {
    let handle = state.clusters.require(&cluster)?;
    scan::full_scan(&handle, namespace.as_deref())
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn posture_scan(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
) -> CommandResult<ScanReport> {
    let handle = state.clusters.require(&cluster)?;
    scan::posture_scan(&handle, namespace.as_deref())
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn rbac_scan(state: State<'_, AppState>, cluster: String) -> CommandResult<ScanReport> {
    let handle = state.clusters.require(&cluster)?;
    scan::rbac_scan(&handle).await.map_err(CommandError::new)
}

/// Distinct images running in the cluster, most used first.
#[tauri::command]
pub async fn cluster_images(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
) -> CommandResult<Vec<ImageUsage>> {
    let handle = state.clusters.require(&cluster)?;
    scan::cluster_images(&handle, namespace.as_deref())
        .await
        .map_err(CommandError::new)
}

/// Download the vulnerability database.
///
/// Its own command because it is the slow, one-gigabyte step; folding it into
/// the first scan makes that scan look broken.
#[tauri::command]
pub async fn download_vulnerability_database(
    state: State<'_, AppState>,
    cluster: String,
) -> CommandResult<Scanner> {
    let handle = state.clusters.require(&cluster)?;
    let sidecar = state.sidecar_dir();
    let scanner = vulnerabilities::detect(&handle, sidecar.as_deref()).await;

    if let Scanner::TrivyBinary { path, .. } = &scanner {
        let repository = vulnerabilities::download_database(std::path::Path::new(path))
            .await
            .map_err(CommandError::new)?;
        tracing::info!(%repository, "vulnerability database ready");
    }

    Ok(vulnerabilities::detect(&handle, sidecar.as_deref()).await)
}

/// What can scan images, if anything.
#[tauri::command]
pub async fn vulnerability_scanner(
    state: State<'_, AppState>,
    cluster: String,
) -> CommandResult<Scanner> {
    let handle = state.clusters.require(&cluster)?;
    Ok(vulnerabilities::detect(&handle, state.sidecar_dir().as_deref()).await)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VulnerabilityReport {
    pub scanner: Scanner,
    pub vulnerabilities: Vec<Vulnerability>,
    pub report: ScanReport,
}

/// Vulnerabilities from whichever source is available.
///
/// With the operator this is a single read. With the local binary it scans the
/// images actually running, which is slow, so `limit` bounds how many.
#[tauri::command]
pub async fn vulnerability_scan(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
    limit: Option<usize>,
) -> CommandResult<VulnerabilityReport> {
    let handle = state.clusters.require(&cluster)?;
    let scanner = vulnerabilities::detect(&handle, state.sidecar_dir().as_deref()).await;

    let images = scan::cluster_images(&handle, namespace.as_deref())
        .await
        .map_err(CommandError::new)?;

    let found = match &scanner {
        Scanner::TrivyOperator { .. } => {
            vulnerabilities::from_operator(&handle, namespace.as_deref())
                .await
                .map_err(CommandError::new)?
        }
        Scanner::TrivyBinary { path, .. } => {
            let binary = std::path::PathBuf::from(path);
            let limit = limit.unwrap_or(10).min(images.len());
            let mut found = Vec::new();
            // Sequential on purpose: trivy is IO- and CPU-heavy, and running a
            // dozen at once makes the machine unusable for no gain.
            for usage in images.iter().take(limit) {
                match vulnerabilities::scan_image(&binary, &usage.image).await {
                    Ok(mut result) => found.append(&mut result),
                    Err(err) => tracing::warn!(image = %usage.image, %err, "image scan failed"),
                }
            }
            found
        }
        Scanner::None { .. } => Vec::new(),
    };

    let mut report = vulnerabilities::report(found.clone(), &scanner, images.len());
    if let Scanner::TrivyBinary { .. } = scanner {
        let scanned = limit.unwrap_or(10).min(images.len());
        if scanned < images.len() {
            report.limitations.push(format!(
                "Scanned the {scanned} most-used images of {}; raise the limit to cover the rest.",
                images.len()
            ));
        }
    }

    Ok(VulnerabilityReport {
        scanner,
        vulnerabilities: found,
        report,
    })
}
