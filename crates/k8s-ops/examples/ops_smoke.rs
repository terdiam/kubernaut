//! Headless check of the P1 operations against a real cluster.
//!
//!   cargo run -p k8s-ops --example ops_smoke -- <context> <namespace> <deployment>
//!
//! Deliberately non-mutating: logs and schemas are reads, the diff uses
//! `dryRun=All`, and the port forward binds loopback and is torn down again.
//! Nothing in this example writes to the cluster.

use std::time::Duration;

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_ops::{
    apply::{self, EditRequest},
    exec::{LocalShellOptions, TerminalEvent, TerminalManager},
    forward::{ForwardManager, ForwardSpec},
    logs::{self, LogEvent, LogManager, LogOptions, LogTarget},
};

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
        .ok_or("usage: ops_smoke <context> <namespace> <deployment>")?;
    let namespace = args.next().ok_or("missing namespace")?;
    let workload = args.next().ok_or("missing deployment name")?;

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    println!("connected to `{context}`\n");

    // ---- schema -----------------------------------------------------------
    let schema = k8s_core::schema::resource_schema(&cluster, "apps/v1/deployments").await?;
    let defs = schema
        .schema
        .get("$defs")
        .and_then(|d| d.as_object())
        .map(|d| d.len())
        .unwrap_or(0);
    let has_replicas = schema.schema.pointer("/properties/spec").is_some();
    println!("schema: {} definitions, spec present: {has_replicas}", defs);

    // ---- pods and containers ---------------------------------------------
    let pods = logs::workload_pods(&cluster, "apps/v1/deployments", &namespace, &workload).await?;
    println!("\npods behind {workload}: {}", pods.join(", "));
    if let Some(pod) = pods.first() {
        let containers = logs::containers(&cluster, &namespace, pod).await?;
        for c in &containers {
            println!(
                "  container {} ({}) image={} ready={} restarts={}",
                c.name, c.role, c.image, c.ready, c.restarts
            );
        }
    }

    // ---- multi-pod log tail ----------------------------------------------
    println!("\ntailing logs for 8s (all pods of the workload)…");
    let log_manager = LogManager::new();
    let session = log_manager
        .start(
            &cluster,
            LogTarget::Workload {
                namespace: namespace.clone(),
                resource: "apps/v1/deployments".into(),
                name: workload.clone(),
            },
            LogOptions {
                tail_lines: Some(5),
                ..LogOptions::default()
            },
        )
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut lines = 0usize;
    let mut dropped = 0u64;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            batch = session.next_batch() => match batch {
                Some(events) => {
                    for event in events {
                        match event {
                            LogEvent::Line { pod, container, text } => {
                                lines += 1;
                                if lines <= 10 {
                                    println!("  [{pod}/{container}] {text}");
                                }
                            }
                            LogEvent::Dropped { count } => dropped += count,
                            LogEvent::PodEnded { pod, reason } => println!("  — {pod}: {reason}"),
                            LogEvent::PodFailed { pod, message } => {
                                println!("  ✕ {pod}: {message}")
                            }
                        }
                    }
                }
                None => break,
            },
        }
    }
    session.stop();
    println!("  total {lines} lines, {dropped} dropped");

    // ---- port forward -----------------------------------------------------
    let ports =
        k8s_ops::forward::target_ports(&cluster, "apps/v1/deployments", &namespace, &workload)
            .await?;
    println!(
        "\nports exposed: {}",
        ports
            .iter()
            .map(|p| format!(
                "{}{}",
                p.port,
                p.name
                    .as_deref()
                    .map(|n| format!("({n})"))
                    .unwrap_or_default()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if let Some(port) = ports.first() {
        let forwards = ForwardManager::new();
        let status = forwards
            .start(
                &cluster,
                ForwardSpec {
                    namespace: namespace.clone(),
                    resource: "apps/v1/deployments".into(),
                    name: workload.clone(),
                    remote_port: port.port,
                    local_port: None,
                    expose_on_network: false,
                },
            )
            .await?;
        println!(
            "forward listening on {}:{} → {}",
            status.local_address, status.local_port, status.remote_port
        );

        // Prove the listener actually reaches the pod.
        match tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect((status.local_address.as_str(), status.local_port)),
        )
        .await
        {
            Ok(Ok(_)) => println!("  tcp connect through the forward: ok"),
            Ok(Err(err)) => println!("  tcp connect failed: {err}"),
            Err(_) => println!("  tcp connect timed out"),
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        for entry in forwards.list() {
            println!(
                "  counters: conns={} sent={}B received={}B error={:?}",
                entry.active_connections, entry.bytes_sent, entry.bytes_received, entry.last_error
            );
        }
        forwards.stop(status.id);
    }

    // ---- dry-run diff of the object against itself ------------------------
    let live = k8s_core::objects::get(&cluster, "apps/v1/deployments", Some(&namespace), &workload)
        .await?;
    let yaml = k8s_core::objects::to_yaml(&live, false)?;
    let diff = apply::preview(
        &cluster,
        &EditRequest {
            resource: "apps/v1/deployments".into(),
            namespace: Some(namespace.clone()),
            name: workload.clone(),
            yaml,
            force: false,
        },
    )
    .await?;
    println!(
        "\ndry-run diff of the live manifest against itself: changed={} conflicts={:?}",
        diff.changed, diff.conflicts
    );
    if diff.changed {
        println!("{}", diff.unified);
    }

    // ---- related resources and events -------------------------------------
    let related =
        k8s_ops::related::related(&cluster, "apps/v1/deployments", Some(&namespace), &workload)
            .await?;
    println!(
        "\nrelated: {} pods, {} services, {} ingresses, {} controllers, {} config, {} storage, {} policies, {} nodes",
        related.pods.len(),
        related.services.len(),
        related.ingresses.len(),
        related.controllers.len(),
        related.config.len(),
        related.storage.len(),
        related.policies.len(),
        related.nodes.len()
    );
    for entry in related
        .pods
        .iter()
        .chain(related.services.iter())
        .chain(related.ingresses.iter())
        .chain(related.config.iter())
        .take(8)
    {
        println!(
            "  {:<26} {:<40} {}",
            entry.kind,
            entry.name,
            entry.detail.as_deref().unwrap_or("")
        );
    }

    let pod_names: Vec<String> = related.pods.iter().map(|p| p.name.clone()).collect();
    let events = k8s_ops::related::events_for_pods(&cluster, &namespace, &pod_names).await?;
    println!("\nevents for those pods: {}", events.len());
    for event in events.iter().rev().take(5) {
        println!(
            "  [{}] {} {}: {}",
            event.kind,
            event.object,
            event.reason,
            event.message.chars().take(90).collect::<String>()
        );
    }

    // ---- local kubectl shell ----------------------------------------------
    // Non-mutating: a shell on this machine with KUBECONFIG pinned to the open
    // context. Proves the minified kubeconfig works and the pty is wired up.
    let kubeconfig = manager.minified_kubeconfig(&context, Some(&namespace))?;

    // Parse rather than string-match: the point of minifying is a security
    // claim (only this context's credentials reach disk), so it deserves a
    // structural check.
    let parsed: serde_json::Value = serde_yaml_ng::from_str(&kubeconfig)?;
    let count = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    };
    println!(
        "\nminified kubeconfig: {} context(s), {} cluster(s), {} user(s), current-context={:?}, ns={:?}",
        count("contexts"),
        count("clusters"),
        count("users"),
        parsed.get("current-context").and_then(|v| v.as_str()),
        parsed
            .pointer("/contexts/0/context/namespace")
            .and_then(|v| v.as_str()),
    );

    let terminals = TerminalManager::new();
    let shell = terminals
        .open_local_shell(
            &cluster,
            kubeconfig,
            LocalShellOptions {
                namespace: Some(namespace.clone()),
                columns: 100,
                rows: 30,
            },
        )
        .await?;
    println!("local shell: {}", shell.descriptor.title);

    // Interactive shells swallow input typed before their line editor is
    // ready; a human types after the prompt, so wait for it here too.
    tokio::time::sleep(Duration::from_secs(3)).await;
    shell
        .write(b"kubectl config current-context; exit\n".to_vec())
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let mut transcript = String::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            batch = shell.next_batch() => match batch {
                Some(events) => {
                    let mut ended = false;
                    for event in events {
                        match event {
                            TerminalEvent::Output { data } => transcript.push_str(&data),
                            TerminalEvent::Closed { status } => {
                                println!("  shell closed: {status}");
                                ended = true;
                            }
                            TerminalEvent::Failed { message } => println!("  shell failed: {message}"),
                            TerminalEvent::Status { message } => println!("  {message}"),
                        }
                    }
                    if ended { break; }
                }
                None => break,
            },
        }
    }
    shell.stop();

    let saw_context = transcript.contains(&context);
    println!(
        "  shell resolved the pinned context: {saw_context}{}",
        if saw_context {
            ""
        } else {
            " (transcript below)"
        }
    );
    if !saw_context {
        for line in transcript.lines().take(12) {
            println!("    | {line}");
        }
    }

    manager.disconnect(&context);
    println!("\ndone (no writes were made)");
    Ok(())
}
