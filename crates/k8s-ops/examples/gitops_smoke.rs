//! Read-only check of the GitOps survey against a real cluster.
//!
//!   cargo run -p k8s-ops --example gitops_smoke -- <context>

use k8s_core::{ClusterManager, ConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KUBERNAUT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let context = std::env::args()
        .nth(1)
        .ok_or("usage: gitops_smoke <context>")?;
    k8s_core::paths::hydrate_process_path(&[]).await;

    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    let summary = k8s_ops::gitops::survey(&cluster, None).await?;
    println!(
        "controllers installed: {}",
        if summary.controllers.is_empty() {
            "none".to_string()
        } else {
            summary.controllers.join(", ")
        }
    );
    for limitation in &summary.limitations {
        println!("  limitation: {limitation}");
    }

    println!("\n{} objects managed:", summary.entries.len());
    for entry in &summary.entries {
        println!(
            "  [{}] {:<14} {:<28} {:<22} reconcilable={}",
            entry.health,
            entry.kind,
            format!(
                "{}/{}",
                entry.namespace.as_deref().unwrap_or("-"),
                entry.name
            ),
            entry.status,
            entry.reconcilable
        );
        if let Some(source) = &entry.source {
            println!(
                "      source {source}{}",
                entry
                    .path
                    .as_deref()
                    .map(|p| format!(" · {p}"))
                    .unwrap_or_default()
            );
        }
        if let Some(revision) = &entry.applied_revision {
            println!("      applied {revision}");
        }
        if let Some(message) = &entry.message {
            println!("      {}", message.chars().take(110).collect::<String>());
        }
    }

    manager.disconnect(&context);
    println!("\ndone (read-only)");
    Ok(())
}
