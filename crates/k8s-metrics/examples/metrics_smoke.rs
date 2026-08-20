//! Read-only check of the overview aggregation against a real cluster.
//!
//!   cargo run -p k8s-metrics --example metrics_smoke -- <context>
//!
//! Cross-check the printed numbers against `kubectl top nodes` and
//! `kubectl describe node`.

use std::time::Duration;

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_metrics::{MetricsManager, NodeScope};

fn gib(bytes: f64) -> String {
    format!("{:.2}GiB", bytes / 1024.0 / 1024.0 / 1024.0)
}

/// Scaled units — a container's writable layer is measured in kilobytes, and
/// printing it as "0.00GiB" hides whether the value arrived at all.
fn human(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}

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
        .ok_or("usage: metrics_smoke <context>")?;
    k8s_core::paths::hydrate_process_path(&[]).await;

    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    let metrics = MetricsManager::new();
    let sampler = metrics.ensure(&cluster);

    println!("sampling `{context}` (waiting for the first sample)…\n");
    for _ in 0..40 {
        if sampler.overview(NodeScope::All).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    for scope in [NodeScope::All, NodeScope::ControlPlane, NodeScope::Workers] {
        let Some(view) = sampler.overview(scope) else {
            println!("{}: no sample yet", scope.label());
            continue;
        };
        println!("== {} ==", scope.label());
        println!(
            "  nodes: {} total, {} ready, {} cordoned",
            view.nodes.total, view.nodes.ready, view.nodes.unschedulable
        );
        println!(
            "  cpu:    usage {:.3} | requests {:.3} | limits {:.3} | allocatable {:.2} | capacity {:.2}",
            view.cpu.usage,
            view.cpu.requests,
            view.cpu.limits,
            view.cpu.allocatable,
            view.cpu.capacity
        );
        println!(
            "  memory: usage {} | requests {} | limits {} | allocatable {} | capacity {}",
            gib(view.memory.usage),
            gib(view.memory.requests),
            gib(view.memory.limits),
            gib(view.memory.allocatable),
            gib(view.memory.capacity)
        );
        println!(
            "  pods:   {} running of {} allocatable ({} capacity)",
            view.pods.usage, view.pods.allocatable, view.pods.capacity
        );
        println!(
            "  metrics available: {}{}",
            view.metrics_available,
            view.metrics_error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        );
        println!("  issues: {}", view.issues.len());
        for issue in view.issues.iter().take(8) {
            println!(
                "    [{:?}] {} {}/{}: {}",
                issue.severity,
                issue.resource,
                issue.namespace.as_deref().unwrap_or("-"),
                issue.name,
                issue.message
            );
        }
        if view.issues.len() > 8 {
            println!("    … {} more", view.issues.len() - 8);
        }
        println!();
    }

    // ---- Prometheus discovery --------------------------------------------
    match sampler.prometheus() {
        Some(target) => println!(
            "prometheus: {}/{}:{} (found by {})",
            target.namespace, target.service, target.port, target.discovered_by
        ),
        None if sampler.prometheus_checked() => {
            println!("prometheus: none found (metrics-server only)")
        }
        None => println!("prometheus: discovery still running"),
    }

    // ---- heatmap ----------------------------------------------------------
    sampler.request_pod_metrics();
    tokio::time::sleep(Duration::from_secs(17)).await;
    let rows = sampler.namespace_usage();
    println!("\nnamespace heatmap: {} rows", rows.len());
    for row in rows.iter().take(6) {
        let ratio = if row.cpu_requests > 0.0 {
            format!("{:.0}%", row.cpu_usage / row.cpu_requests * 100.0)
        } else {
            "no request".to_string()
        };
        println!(
            "  {:<32} pods={:<4} cpu={:.3} req={:.3} ({}) mem={}",
            row.namespace,
            row.pods,
            row.cpu_usage,
            row.cpu_requests,
            ratio,
            gib(row.memory_usage)
        );
    }

    // ---- topology ---------------------------------------------------------
    if let Some(row) = rows.first() {
        let pods = sampler.pods();
        match k8s_metrics::topology::build(&cluster, std::slice::from_ref(&row.namespace), &pods)
            .await
        {
            Ok(graph) => {
                let count = |kind: &str| graph.nodes.iter().filter(|n| n.kind == kind).count();
                println!(
                    "\ntopology for `{}`: {} nodes ({} ingress, {} service, {} workload, {} pod, {} node), {} edges{}",
                    row.namespace,
                    graph.nodes.len(),
                    count("Ingress"),
                    count("Service"),
                    count("Workload"),
                    count("Pod"),
                    count("Node"),
                    graph.edges.len(),
                    if graph.truncated { " [truncated]" } else { "" }
                );
                for node in graph.nodes.iter().filter(|n| n.health == "error").take(5) {
                    println!(
                        "  unhealthy {} {}: {}",
                        node.kind,
                        node.name,
                        node.detail.as_deref().unwrap_or("")
                    );
                }
            }
            Err(err) => println!("topology failed: {err}"),
        }
    }

    // ---- per-node summaries -----------------------------------------------
    // The first call marks demand; disk arrives on the next tick.
    let _ = sampler.node_summaries();
    tokio::time::sleep(Duration::from_secs(17)).await;
    let summaries = sampler.node_summaries();
    println!("\nnode summaries: {}", summaries.len());
    for node in &summaries {
        println!(
            "  {:<16} cpu {:>6.2}/{:<5.0} ram {:>8}/{:<8} pods {:>3}/{:<4} {} {}",
            node.name,
            node.cpu_usage,
            node.cpu_allocatable,
            human(node.memory_usage),
            human(node.memory_allocatable),
            node.pods_used,
            node.pods_allocatable,
            node.operating_system.as_deref().unwrap_or("-"),
            node.architecture.as_deref().unwrap_or("-")
        );
        let percent = |used: f64, total: f64| {
            if total > 0.0 {
                format!("{:.0}%", used / total * 100.0)
            } else {
                "-".to_string()
            }
        };
        println!(
            "                   disk {}/{} ({}) image-fs {}/{} | available: {}",
            human(node.disk_used),
            human(node.disk_capacity),
            percent(node.disk_used, node.disk_capacity),
            human(node.image_disk_used),
            human(node.image_disk_capacity),
            node.disk_available
        );
        println!(
            "                   os: {} | kernel {} | runtime {}",
            node.os_image.as_deref().unwrap_or("-"),
            node.kernel_version.as_deref().unwrap_or("-"),
            node.container_runtime.as_deref().unwrap_or("-")
        );
    }

    // ---- kubelet network and filesystem stats -----------------------------
    let pods = sampler.pods();
    // Prefer a pod the caller named, so the check runs against something busy.
    let wanted = std::env::args().nth(2);
    if let Some(pod) = pods.iter().find(|pod| {
        use kube::ResourceExt;
        let running = pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running")
            && pod
                .spec
                .as_ref()
                .and_then(|s| s.node_name.as_deref())
                .is_some();
        match &wanted {
            Some(name) => running && pod.name_any() == *name,
            None => running,
        }
    }) {
        use kube::ResourceExt;
        let namespace = pod.namespace().unwrap_or_default();
        let name = pod.name_any();
        let target = k8s_metrics::MetricTarget::Pod {
            namespace: namespace.clone(),
            name: name.clone(),
        };

        println!("\ncharting {namespace}/{name} (needs two samples for network rates)…");
        // Two ticks: the first establishes the counter baseline.
        let _ = sampler.target_history(&target);
        tokio::time::sleep(Duration::from_secs(32)).await;
        let points = sampler.target_history(&target);

        match points.last() {
            Some(point) => println!(
                "  cpu={:.4} mem={} net rx={}/s tx={}/s fs={} volumes={}",
                point.cpu,
                human(point.memory),
                human(point.network_rx),
                human(point.network_tx),
                human(point.fs_used),
                human(point.volume_used)
            ),
            None => println!("  no points collected"),
        }
        if let Some(note) = sampler.io_note() {
            println!("  io note: {note}");
        }
    }

    let history = sampler.history(NodeScope::All, 0);
    println!("\nhistory points collected: {}", history.len());

    manager.disconnect(&context);
    Ok(())
}
