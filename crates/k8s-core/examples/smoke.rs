//! Headless end-to-end check of the P0 path, without launching the GUI.
//!
//!   cargo run -p k8s-core --example smoke -- <context> [resource] [namespace]
//!
//! Connects, runs discovery, subscribes to a watch and prints batches for ten
//! seconds. Everything here is read-only (list/watch/get).

use std::time::Duration;

use k8s_core::{
    ClusterManager, ConnectOptions,
    watch::{WatchManager, WatchRequest},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KUBERNAUT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,kube=warn")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let context = args.next();
    let resource = args.next().unwrap_or_else(|| "core/v1/pods".to_string());
    let namespace = args.next();

    let entries = k8s_core::paths::hydrate_process_path(&[]).await;
    println!("PATH entries resolved: {}", entries.len());

    let manager = ClusterManager::from_env()?;
    let contexts = manager.contexts();
    println!("\ncontexts in kubeconfig:");
    for ctx in &contexts {
        let plugin = match (&ctx.exec_command, ctx.missing_exec_plugin) {
            (Some(cmd), true) => format!("  exec={cmd} (NOT ON PATH)"),
            (Some(cmd), false) => format!("  exec={cmd}"),
            (None, _) => String::new(),
        };
        println!(
            "  {}{}{}",
            ctx.name,
            if ctx.is_current { " (current)" } else { "" },
            plugin
        );
    }

    let Some(context) = context.or_else(|| {
        contexts
            .iter()
            .find(|c| c.is_current)
            .map(|c| c.name.clone())
    }) else {
        eprintln!("\nno context given and no current-context set");
        return Ok(());
    };

    println!("\nconnecting to `{context}`…");
    // Print Display, not Debug: the Display text is what the UI shows users.
    let cluster = match manager.connect(&context, ConnectOptions::default()).await {
        Ok(cluster) => cluster,
        Err(err) => {
            eprintln!("\n{err}");
            std::process::exit(1);
        }
    };
    println!("  status: {:?}", cluster.status());
    println!("  default namespace: {}", cluster.default_namespace);

    let discovery = cluster.refresh_discovery().await?;
    let listable = discovery.listable().count();
    let crds = discovery.listable().filter(|r| r.is_crd).count();
    println!(
        "\ndiscovery: {} groups, {listable} watchable resources ({crds} from CRDs), \
         crd metadata readable: {}",
        discovery.groups.len(),
        discovery.crd_metadata_available
    );

    // `--list` dumps the resource inventory instead of watching, which is how
    // the sidebar's categorisation gets checked against a real cluster.
    if resource == "--list" {
        for entry in discovery.listable() {
            println!("{}|{}|{}", entry.group, entry.kind, entry.is_crd);
        }
        manager.disconnect(&context);
        return Ok(());
    }

    let descriptor = discovery.require(&resource)?;
    println!(
        "\nwatching {} ({}), namespace: {}",
        descriptor.kind,
        descriptor.key,
        namespace.as_deref().unwrap_or("<all>")
    );

    let watches = WatchManager::new();
    let mut subscription = watches
        .subscribe(
            &cluster,
            WatchRequest {
                resource: resource.clone(),
                namespace: namespace.clone(),
                label_selector: None,
                field_selector: None,
            },
        )
        .await?;

    let headers: Vec<&str> = subscription
        .spec
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    println!("columns: NAME | {} | AGE", headers.join(" | "));
    print_batch("initial", &subscription.initial);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            received = subscription.receiver.recv() => match received {
                Ok(batch) => print_batch("delta", &batch),
                Err(err) => {
                    println!("stream ended: {err}");
                    break;
                }
            }
        }
    }

    watches.unsubscribe(subscription.id);
    manager.disconnect(&context);
    println!("\ndone");
    Ok(())
}

fn print_batch(label: &str, batch: &k8s_core::WatchBatch) {
    println!(
        "\n[{label}] epoch={} snapshot={} upserts={} deletes={} state={:?}",
        batch.epoch,
        batch.snapshot,
        batch.upserts.len(),
        batch.deletes.len(),
        batch.state
    );
    for row in batch.upserts.iter().take(15) {
        println!(
            "  {:<10} {:<44} {}",
            format!("{:?}", row.health),
            format!(
                "{}{}",
                row.namespace
                    .as_deref()
                    .map(|n| format!("{n}/"))
                    .unwrap_or_default(),
                row.name
            ),
            row.cells.join(" | ")
        );
    }
    if batch.upserts.len() > 15 {
        println!("  … {} more", batch.upserts.len() - 15);
    }
    for uid in batch.deletes.iter().take(5) {
        println!("  deleted {uid}");
    }
}
