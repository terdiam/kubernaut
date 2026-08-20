//! Read-only check of the sizing recommendation against a real cluster.
//!
//!   cargo run -p k8s-metrics --example sizing_smoke -- <context> <namespace> <deployment>
//!
//! Observation is demand-driven, so this waits a couple of sampling intervals
//! to show what a freshly opened panel actually reports.

use std::time::Duration;

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_metrics::{MetricsManager, recommend};

fn mib(bytes: f64) -> String {
    format!("{:.0}Mi", bytes / 1024.0 / 1024.0)
}

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
        .ok_or("usage: sizing_smoke <context> <namespace> <deployment>")?;
    let namespace = args.next().ok_or("missing namespace")?;
    let name = args.next().ok_or("missing deployment")?;
    let resource = "apps/v1/deployments";

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    let metrics = MetricsManager::new();
    let sampler = metrics.ensure(&cluster);

    let object = k8s_core::objects::get(&cluster, resource, Some(&namespace), &name).await?;
    let value = k8s_core::objects::to_value(&object)?;

    let selector: std::collections::BTreeMap<String, String> = value
        .pointer("/spec/selector/matchLabels")
        .and_then(|labels| labels.as_object())
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let key = format!("{namespace}/{resource}/{name}");
    sampler.observe_workload(&key, &namespace, selector);
    println!("observing {namespace}/{name}…");

    // Three intervals: enough to show the confidence wording change, not enough
    // to pretend the numbers are authoritative.
    for round in 1..=3 {
        tokio::time::sleep(Duration::from_secs(17)).await;
        println!("  after {} sample rounds", round);
    }

    let containers = value
        .pointer("/spec/template/spec/containers")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let number = |container: &serde_json::Value, bucket: &str, field: &str| -> f64 {
        container
            .pointer(&format!("/resources/{bucket}/{field}"))
            .and_then(|v| v.as_str())
            .and_then(k8s_metrics::quantity::parse)
            .unwrap_or(0.0)
    };

    for container in &containers {
        let Some(container_name) = container.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let samples = sampler.container_history(&key, container_name);
        let window = match (samples.first(), samples.last()) {
            (Some((first, _, _)), Some((last, _, _))) => (last - first) / 1000,
            _ => 0,
        };
        let pairs: Vec<(f64, f64)> = samples.iter().map(|(_, c, m)| (*c, *m)).collect();

        let recommendation = recommend::build(
            container_name,
            &pairs,
            window,
            number(container, "requests", "cpu"),
            number(container, "limits", "cpu"),
            number(container, "requests", "memory"),
            number(container, "limits", "memory"),
        );

        println!("\ncontainer `{container_name}`");
        println!(
            "  samples {} over {}s → confidence {:?}",
            recommendation.samples, recommendation.window_seconds, recommendation.confidence
        );
        println!(
            "  cpu    p95 {:.4} peak {:.4} | current request {:.3} → suggested {:.3}",
            recommendation.cpu_p95,
            recommendation.cpu_max,
            recommendation.current_cpu_request,
            recommendation.recommended_cpu_request
        );
        println!(
            "  memory p95 {} peak {} | current request {} limit {} → suggested request {} limit {}",
            mib(recommendation.memory_p95),
            mib(recommendation.memory_max),
            mib(recommendation.current_memory_request),
            mib(recommendation.current_memory_limit),
            mib(recommendation.recommended_memory_request),
            mib(recommendation.recommended_memory_limit)
        );
        for note in &recommendation.notes {
            println!("  · {note}");
        }
    }

    manager.disconnect(&context);
    println!("\ndone (read-only)");
    Ok(())
}
