//! Run the pod diagnosis against a real cluster.
//!
//!   cargo run -p k8s-ops --example diagnose_smoke -- <context> [namespace]
//!
//! Read-only: it lists pods, reads their events and nodes, and prints what the
//! rules make of them. Nothing is written.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_openapi::api::core::v1::Pod;
use k8s_ops::diagnose;
use kube::{Api, ResourceExt, api::ListParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or("usage: diagnose_smoke <context> [namespace]")?;
    let namespace = args.next();

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    println!("connected to `{context}`\n");

    let api: Api<Pod> = match namespace.as_deref() {
        Some(ns) => Api::namespaced(cluster.client.clone(), ns),
        None => Api::all(cluster.client.clone()),
    };
    let pods = api.list(&ListParams::default().limit(500)).await?;

    // Only the ones a human would ask about.
    let unhappy: Vec<&Pod> = pods
        .iter()
        .filter(|pod| {
            let status = pod.status.as_ref();
            let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");
            let not_ready = status
                .and_then(|s| s.container_statuses.as_ref())
                .map(|c| c.iter().any(|c| !c.ready))
                .unwrap_or(false);
            phase != "Succeeded" && (phase != "Running" || not_ready)
        })
        .collect();

    println!(
        "{} pods total, {} not running cleanly\n",
        pods.items.len(),
        unhappy.len()
    );

    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();

    for pod in unhappy.iter().take(25) {
        let ns = pod.namespace().unwrap_or_default();
        let report =
            diagnose::diagnose(&cluster, "core/v1/pods", Some(&ns), &pod.name_any()).await?;

        for diagnosis in &report.pods {
            println!("── {}/{} [{}]", ns, diagnosis.pod, diagnosis.phase);
            for finding in &diagnosis.findings {
                *counts.entry(finding.code.clone()).or_default() += 1;
                println!(
                    "   [{}] {} — {}",
                    finding.severity, finding.code, finding.title
                );
                for line in &finding.evidence {
                    if !line.is_empty() {
                        println!("     · {}", line.chars().take(160).collect::<String>());
                    }
                }
                for (index, step) in finding.steps.iter().enumerate() {
                    println!(
                        "     {}. {}",
                        index + 1,
                        step.text.chars().take(160).collect::<String>()
                    );
                    if let Some(command) = &step.command {
                        println!("        $ {command}");
                    }
                    if let Some(action) = &step.action {
                        println!("        → {action:?}");
                    }
                }
            }
            println!();
        }
        if report.pods.is_empty() {
            println!("── {}/{}: nothing to report\n", ns, pod.name_any());
        }
    }

    println!("findings by code: {counts:?}");
    manager.disconnect(&context);
    Ok(())
}
