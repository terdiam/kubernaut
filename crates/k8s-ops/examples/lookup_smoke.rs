//! List what the form selects would offer, against a real cluster.
//!
//!   cargo run -p k8s-ops --example lookup_smoke -- <context> <namespace> [service]
//!
//! Read-only.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_ops::lookup;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or("usage: lookup_smoke <context> <namespace> [service]")?;
    let namespace = args.next().ok_or("missing namespace")?;
    let service = args.next();

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    println!("connected to `{context}`, namespace `{namespace}`\n");

    let sources = [
        "dockerConfigSecrets",
        "tlsSecrets",
        "secrets",
        "configMaps",
        "serviceAccounts",
        "persistentVolumeClaims",
        "services",
        "ingressClasses",
        "storageClasses",
        "priorityClasses",
        "nodes",
        "workloads",
    ];

    for source in sources {
        match lookup::lookup(&cluster, source, Some(&namespace), None).await {
            Ok(options) => {
                println!("{source}: {} option(s)", options.len());
                for option in options.iter().take(4) {
                    println!(
                        "   {:<40} {}",
                        option.label,
                        option.detail.as_deref().unwrap_or("")
                    );
                }
                if options.len() > 4 {
                    println!("   … {} more", options.len() - 4);
                }
            }
            Err(err) => println!("{source}: {err}"),
        }
        println!();
    }

    if let Some(service) = service {
        let ports =
            lookup::lookup(&cluster, "servicePorts", Some(&namespace), Some(&service)).await?;
        println!("servicePorts for `{service}`: {} option(s)", ports.len());
        for port in &ports {
            println!(
                "   value={:<10} label={:<16} {}",
                port.value,
                port.label,
                port.detail.as_deref().unwrap_or("")
            );
        }
        let missing = lookup::lookup(
            &cluster,
            "servicePorts",
            Some(&namespace),
            Some("does-not-exist"),
        )
        .await?;
        println!(
            "\nports for a service that does not exist: {} (expected 0)",
            missing.len()
        );
    }

    let unknown = lookup::lookup(&cluster, "nonsense", Some(&namespace), None).await;
    println!(
        "\nunknown source: {:?}",
        unknown.err().map(|e| e.to_string())
    );

    manager.disconnect(&context);
    Ok(())
}
