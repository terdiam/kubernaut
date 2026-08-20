//! Read-only check of Helm support against a real cluster.
//!
//!   cargo run -p k8s-helm --example helm_smoke -- <context> [release] [namespace]
//!
//! Lists releases from the cluster's own Secrets (no helm binary involved),
//! then inspects one. Repository and search calls use the binary but change
//! nothing. No install, upgrade, rollback or uninstall is performed.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_helm::{Helm, store};

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
        .ok_or("usage: helm_smoke <context> [release] [namespace]")?;
    let wanted = args.next();
    let wanted_namespace = args.next();

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    // ---- releases, straight from the cluster ------------------------------
    let releases = store::list(&cluster, None).await?;
    println!("releases found without the helm binary: {}", releases.len());
    for release in releases.iter().take(8) {
        println!(
            "  {:<34} {:<28} rev {:<3} {:<12} {} ({})",
            format!("{}/{}", release.namespace, release.name),
            release.chart,
            release.revision,
            release.status,
            release.chart_version,
            release.app_version.as_deref().unwrap_or("-")
        );
    }
    if releases.len() > 8 {
        println!("  … {} more", releases.len() - 8);
    }

    // ---- one release in detail --------------------------------------------
    let chosen = match (&wanted, &wanted_namespace) {
        (Some(name), Some(ns)) => releases
            .iter()
            .find(|r| &r.name == name && &r.namespace == ns),
        (Some(name), None) => releases.iter().find(|r| &r.name == name),
        _ => releases.first(),
    };

    if let Some(release) = chosen {
        let detail = store::detail(&cluster, &release.namespace, &release.name, None).await?;
        println!(
            "\ndetail for {}/{}:",
            detail.release.namespace, detail.release.name
        );
        println!(
            "  chart: {} {}",
            detail.release.chart, detail.release.chart_version
        );
        println!("  status: {}", detail.release.status);
        println!("  user values: {} bytes", detail.user_values.len());
        println!(
            "  effective values: {} bytes",
            detail.effective_values.len()
        );
        println!("  manifest: {} bytes", detail.manifest.len());
        println!("  notes: {} bytes", detail.notes.len());

        let history = store::history(&cluster, &release.namespace, &release.name).await?;
        println!("  revisions stored: {}", history.len());
        for revision in history.iter().take(4) {
            println!(
                "    rev {:<3} {:<12} {:<22} {}",
                revision.revision,
                revision.status,
                revision.chart_version,
                revision.description.as_deref().unwrap_or("")
            );
        }
    }

    // ---- the binary, for the operations that need it ----------------------
    match Helm::resolve(None) {
        Ok(helm) => {
            match helm.info().await {
                Ok(info) => println!(
                    "\nhelm binary: {} ({}){}",
                    info.version,
                    info.path,
                    if info.bundled { " [bundled]" } else { "" }
                ),
                Err(err) => println!("\nhelm binary found but not usable: {err}"),
            }

            match helm.repo_list().await {
                Ok(repos) => {
                    println!("configured repositories: {}", repos.len());
                    for repo in repos.iter().take(5) {
                        println!("  {:<20} {}", repo.name, repo.url);
                    }
                }
                Err(err) => println!("repo list failed: {err}"),
            }

            match helm.search("ingress-nginx").await {
                Ok(results) => println!("search `ingress-nginx`: {} charts", results.len()),
                Err(err) => println!("search failed: {err}"),
            }
        }
        Err(err) => println!("\nhelm binary unavailable: {err}"),
    }

    manager.disconnect(&context);
    println!("\ndone (no writes were made)");
    Ok(())
}
