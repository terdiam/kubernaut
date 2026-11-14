//! The Security Center's reads: configuration findings and image
//! vulnerabilities.
//!
//! Nothing here changes a cluster, so none of it is audited or refused for a
//! protected context. The one thing it writes is the scan cache on this
//! machine, so a slow local scan is not repeated within a day.

use std::{path::PathBuf, time::Duration};

use k8s_security::{
    ScanReport, Scanner, Vulnerability,
    scan::{self, ImageUsage},
    vulnerabilities,
};

use crate::state::AppCore;

/// A day is generous for a database that itself updates once a day at most, and
/// short enough that a freshly fixed image is scanned again within a working
/// session.
const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Images a local scan covers when no limit is given. Trivy is slow and heavy,
/// so the most-used ones come first.
pub const DEFAULT_IMAGE_LIMIT: usize = 10;

/// Posture and RBAC findings in one pass. Neither needs a scanner.
pub async fn findings(
    core: &AppCore,
    cluster: &str,
    namespace: Option<&str>,
) -> Result<ScanReport, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    scan::full_scan(&handle, namespace)
        .await
        .map_err(|e| e.to_string())
}

/// What can scan images, if anything.
pub async fn scanner(core: &AppCore, cluster: &str) -> Result<Scanner, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    Ok(vulnerabilities::detect(&handle, core.sidecar_dir().as_deref()).await)
}

/// Download the vulnerability database for the local scanner, then report what
/// can scan now. Its own step because it is the large one: folding it into the
/// first scan makes that scan look broken.
pub async fn download_database(core: &AppCore, cluster: &str) -> Result<Scanner, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let sidecar = core.sidecar_dir();
    if let Scanner::TrivyBinary { path, .. } =
        vulnerabilities::detect(&handle, sidecar.as_deref()).await
    {
        vulnerabilities::download_database(std::path::Path::new(&path))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(vulnerabilities::detect(&handle, sidecar.as_deref()).await)
}

/// What an image scan found and how.
pub struct ImageScan {
    pub scanner: Scanner,
    pub vulnerabilities: Vec<Vulnerability>,
    pub report: ScanReport,
}

/// Vulnerabilities from whichever source is available.
///
/// With the Trivy Operator this is a single read. With the local binary it
/// scans the images actually running, most used first, up to `limit`.
pub async fn image_scan(
    core: &AppCore,
    cluster: &str,
    namespace: Option<&str>,
    limit: usize,
    force: bool,
) -> Result<ImageScan, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let scanner = vulnerabilities::detect(&handle, core.sidecar_dir().as_deref()).await;
    let images: Vec<ImageUsage> = scan::cluster_images(&handle, namespace)
        .await
        .map_err(|e| e.to_string())?;

    let mut failed = 0usize;
    let found = match &scanner {
        Scanner::TrivyOperator { .. } => vulnerabilities::from_operator(&handle, namespace)
            .await
            .map_err(|e| e.to_string())?,
        Scanner::TrivyBinary { path, .. } => {
            let binary = PathBuf::from(path);
            let mut found = Vec::new();
            // One at a time: trivy is heavy on disk and CPU, and a dozen at once
            // makes the machine unusable for no gain.
            for usage in images.iter().take(limit) {
                if !force && let Some(cached) = core.scan_cache.get(&usage.image, CACHE_MAX_AGE) {
                    found.extend(cached);
                    continue;
                }
                match vulnerabilities::scan_image(&binary, &usage.image).await {
                    Ok(result) => {
                        core.scan_cache.put(&usage.image, &result);
                        found.extend(result);
                    }
                    Err(err) => {
                        failed += 1;
                        tracing::warn!(image = %usage.image, %err, "image scan failed");
                    }
                }
            }
            found
        }
        Scanner::None { .. } => Vec::new(),
    };

    let mut report = vulnerabilities::report(found.clone(), &scanner, images.len());
    if matches!(scanner, Scanner::TrivyBinary { .. }) {
        let scanned = limit.min(images.len());
        if scanned < images.len() {
            report.limitations.push(format!(
                "Scanned the {scanned} most-used images of {}.",
                images.len()
            ));
        }
        if failed > 0 {
            report
                .limitations
                .push(format!("{failed} image(s) could not be scanned."));
        }
    }
    Ok(ImageScan {
        scanner,
        vulnerabilities: found,
        report,
    })
}
