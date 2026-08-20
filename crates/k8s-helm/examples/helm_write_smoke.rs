//! Full write cycle for Helm support: install → upgrade → rollback → uninstall.
//!
//!   cargo run -p k8s-helm --example helm_write_smoke -- <context> [namespace]
//!
//! **This one writes to the cluster.** It installs a chart whose objects are
//! all namespaced, into a namespace of its own, with `replicaCount: 0` so no
//! pod is scheduled and no image is pulled. Everything it creates is removed
//! again at the end.
//!
//! Every step goes through this crate rather than the helm CLI, so what is
//! verified is the app's own code path.

use std::time::Duration;

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_helm::{
    Helm, UpgradeOptions,
    cli::{ChartRef, ReleaseTarget},
    store,
};

const CHART: &str = "bitnami/nginx";
const CHART_VERSION: &str = "23.0.3";
const RELEASE: &str = "kubernaut-selftest";

/// Nothing runs, so nothing has to be waited for or pulled.
const VALUES_V1: &str = "replicaCount: 0\n";
const VALUES_V2: &str = "replicaCount: 0\nfullnameOverride: kubernaut-selftest-renamed\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KUBERNAUT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or("usage: helm_write_smoke <context> [namespace]")?;
    let namespace = args.next().unwrap_or_else(|| "kubernaut-test".to_string());

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    let helm = Helm::resolve(None)?;
    let kubeconfig = manager.minified_kubeconfig(&context, Some(&namespace))?;

    let target = ReleaseTarget {
        release: RELEASE,
        namespace: &namespace,
    };
    let chart = ChartRef {
        reference: CHART,
        version: Some(CHART_VERSION),
    };

    // Waiting is pointless with zero replicas, and `atomic` implies waiting.
    let options = UpgradeOptions {
        create_namespace: true,
        atomic: false,
        ..UpgradeOptions::default()
    };

    println!("cluster: {context}\nnamespace: {namespace}\nchart: {CHART} {CHART_VERSION}\n");

    // ---- 1. install -------------------------------------------------------
    println!("1. installing…");
    helm.upgrade(&cluster, &kubeconfig, &target, &chart, VALUES_V1, &options)
        .await?;

    let installed = find(&cluster, &namespace).await?;
    println!(
        "   revision {} status {} chart {}",
        installed.revision, installed.status, installed.chart_version
    );
    assert_eq!(installed.revision, 1, "a fresh install must be revision 1");

    // ---- 2. diff preview --------------------------------------------------
    println!("2. previewing an upgrade…");
    let proposed = helm
        .template(&cluster, &kubeconfig, &target, &chart, VALUES_V2)
        .await?;
    let current = store::detail(&cluster, &namespace, RELEASE, None)
        .await?
        .manifest;
    let diff = k8s_helm::diff_manifests(&current, &proposed, RELEASE);
    println!(
        "   changed: {}, objects affected: {}",
        diff.changed,
        diff.documents.len()
    );
    for document in &diff.documents {
        println!(
            "     {} {} {}",
            document.change, document.kind, document.name
        );
    }
    assert!(diff.changed, "renaming every object must show as a change");
    assert!(
        !diff.generated_only,
        "renaming objects is a real change, not regenerated material"
    );

    // Re-rendering the same values must not look like a pending change.
    //
    // This chart calls `genSelfSignedCert`, so the rendered Secret genuinely
    // differs every time. The diff still reports it — hiding a Secret change
    // would be wrong — but flags it as regenerated material so the preview does
    // not cry wolf on every chart that mints a certificate or password.
    let same = helm
        .template(&cluster, &kubeconfig, &target, &chart, VALUES_V1)
        .await?;
    let no_change = k8s_helm::diff_manifests(&current, &same, RELEASE);
    println!(
        "   identical values → changed: {}, generated-only: {}, objects: {:?}",
        no_change.changed,
        no_change.generated_only,
        no_change
            .documents
            .iter()
            .map(|d| format!("{} {}", d.kind, d.name))
            .collect::<Vec<_>>()
    );
    assert!(
        !no_change.changed || no_change.generated_only,
        "identical values must diff clean, or be recognised as regenerated material"
    );

    // ---- 3. upgrade -------------------------------------------------------
    println!("3. upgrading…");
    helm.upgrade(&cluster, &kubeconfig, &target, &chart, VALUES_V2, &options)
        .await?;
    let upgraded = find(&cluster, &namespace).await?;
    println!(
        "   revision {} status {}",
        upgraded.revision, upgraded.status
    );
    assert_eq!(upgraded.revision, 2);

    let detail = store::detail(&cluster, &namespace, RELEASE, None).await?;
    println!(
        "   user values now: {}",
        detail.user_values.replace('\n', " ").trim()
    );
    assert!(
        detail.manifest.contains("kubernaut-selftest-renamed"),
        "the stored manifest should reflect the upgrade"
    );

    let history = store::history(&cluster, &namespace, RELEASE).await?;
    println!("   revisions stored: {}", history.len());
    assert_eq!(history.len(), 2);

    // ---- 4. rollback ------------------------------------------------------
    println!("4. rolling back to revision 1…");
    helm.rollback(&cluster, &kubeconfig, RELEASE, &namespace, 1)
        .await?;
    let rolled = find(&cluster, &namespace).await?;
    println!("   revision {} status {}", rolled.revision, rolled.status);
    assert_eq!(rolled.revision, 3, "a rollback creates a new revision");

    let after = store::detail(&cluster, &namespace, RELEASE, None).await?;
    assert!(
        !after.manifest.contains("kubernaut-selftest-renamed"),
        "rollback should restore the original manifest"
    );
    println!("   manifest restored to the revision 1 content");

    // ---- 5. uninstall -----------------------------------------------------
    println!("5. uninstalling…");
    helm.uninstall(&cluster, &kubeconfig, RELEASE, &namespace, false)
        .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let remaining = store::list(&cluster, Some(&namespace)).await?;
    println!("   releases left in {namespace}: {}", remaining.len());
    assert!(
        !remaining.iter().any(|r| r.name == RELEASE),
        "the release should be gone"
    );

    manager.disconnect(&context);
    println!("\nall steps passed. The namespace `{namespace}` is left in place; remove it with:");
    println!("  kubectl --context {context} delete namespace {namespace}");
    Ok(())
}

async fn find(
    cluster: &std::sync::Arc<k8s_core::ClusterHandle>,
    namespace: &str,
) -> Result<k8s_helm::Release, Box<dyn std::error::Error>> {
    let releases = store::list(cluster, Some(namespace)).await?;
    releases
        .into_iter()
        .find(|release| release.name == RELEASE)
        .ok_or_else(|| format!("release `{RELEASE}` not found in `{namespace}`").into())
}
