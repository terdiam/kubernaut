//! Read-only check of Alertmanager discovery against a real cluster.
//!
//!   cargo run -p k8s-metrics --example alertmanager_smoke -- <context>
//!
//! Absence is a normal, successful outcome: most clusters have no Prometheus
//! stack at all.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_metrics::alertmanager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let context = std::env::args()
        .nth(1)
        .ok_or("usage: alertmanager_smoke <context>")?;
    k8s_core::paths::hydrate_process_path(&[]).await;

    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    match alertmanager::discover(&cluster).await {
        Some(target) => {
            println!(
                "found: {}/{}:{} ({})",
                target.namespace, target.service, target.port, target.discovered_by
            );
            let alerts = alertmanager::list_alerts(&cluster, &target).await?;
            println!("{} active alert(s)", alerts.len());
            for alert in alerts {
                println!(
                    "  {} [{}] {}",
                    alert
                        .labels
                        .get("alertname")
                        .map(String::as_str)
                        .unwrap_or("?"),
                    alert.state,
                    alert
                        .annotations
                        .get("summary")
                        .map(String::as_str)
                        .unwrap_or("")
                );
            }
        }
        None => println!("no Alertmanager found in `{context}` (expected on a plain cluster)"),
    }
    Ok(())
}
