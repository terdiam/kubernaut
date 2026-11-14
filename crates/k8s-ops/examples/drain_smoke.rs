//! Headless check of draining a node against a real cluster.
//!
//!   cargo run -p k8s-ops --example drain_smoke -- <context> <node> [--run] [--standalone]
//!
//! Without `--run` this is a dry run: it lists what a drain would evict and what
//! it would leave, and writes nothing. With `--run` it cordons the node and
//! evicts, so point it only at a cluster where that is acceptable.

use std::time::Instant;

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_ops::actions::{self, DrainOptions};

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
        .ok_or("usage: drain_smoke <context> <node> [--run] [--standalone]")?;
    let node = args.next().ok_or("missing node name")?;
    let rest: Vec<String> = args.collect();
    let run = rest.iter().any(|a| a == "--run");
    let standalone = rest.iter().any(|a| a == "--standalone");

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    println!(
        "connected to `{context}`; {} `{node}`",
        if run { "DRAINING" } else { "previewing" }
    );

    let started = Instant::now();
    let report = actions::drain(
        &cluster,
        &node,
        &DrainOptions {
            confirmation: node.clone(),
            delete_standalone_pods: standalone,
            dry_run: !run,
        },
    )
    .await?;
    println!("took {:?}", started.elapsed());

    println!(
        "\n{} evicted{}:",
        report.evicted.len(),
        if run { "" } else { " (would be)" }
    );
    for pod in &report.evicted {
        println!("  {pod}");
    }
    println!("\n{} left alone:", report.skipped.len());
    for pod in &report.skipped {
        println!("  {} ({:?}: {})", pod.name, pod.kind, pod.reason);
    }
    println!("\n{} blocked:", report.blocked.len());
    for pod in &report.blocked {
        println!("  {}: {}", pod.name, pod.message);
    }
    Ok(())
}
