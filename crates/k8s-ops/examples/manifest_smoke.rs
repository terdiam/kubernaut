//! Plan a manifest against a real cluster.
//!
//!   cargo run -p k8s-ops --example manifest_smoke -- <context> [namespace] [file]
//!
//! Planning only — every apply here is `dryRun=All`. With no file, a built-in
//! manifest exercises create, wrong-version, unknown-kind and generateName.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_ops::manifest;

const SAMPLE: &str = r#"
apiVersion: v1
kind: ConfigMap
metadata:
  name: kubernaut-smoke-settings
data:
  greeting: hello
---
apiVersion: v1
kind: Service
metadata:
  name: kubernaut-smoke
spec:
  selector: { app: kubernaut-smoke }
  ports: [{ port: 80, targetPort: 8080 }]
---
apiVersion: extensions/v1beta1
kind: Ingress
metadata:
  name: kubernaut-smoke
spec: {}
---
apiVersion: nonsense.example.com/v1
kind: Imaginary
metadata:
  name: nope
---
apiVersion: v1
kind: Pod
metadata:
  generateName: kubernaut-smoke-
spec:
  containers: [{ name: c, image: busybox }]
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or("usage: manifest_smoke <context> [namespace] [file]")?;
    let namespace = args.next();
    let yaml = match args.next() {
        Some(path) => std::fs::read_to_string(path)?,
        None => SAMPLE.to_string(),
    };

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;
    println!("connected to `{context}`\n");

    let plan = manifest::plan(&cluster, &yaml, namespace.as_deref(), false).await?;

    // `KUBERNAUT_JSON=1` prints the plan as the UI receives it, so the render
    // can be checked against real cluster output.
    if std::env::var("KUBERNAUT_JSON").is_ok() {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        manager.disconnect(&context);
        return Ok(());
    }

    for doc in &plan.docs {
        println!(
            "[{}] {:<12} {:<32} → {}",
            doc.index + 1,
            doc.kind,
            doc.name,
            doc.action
        );
        if let Some(resource) = &doc.resource {
            println!("     resource: {resource}");
        }
        for warning in &doc.warnings {
            println!("     ⚠ {warning}");
        }
        if let Some(error) = &doc.error {
            println!("     ✕ {error}");
        }
        for conflict in &doc.conflicts {
            println!("     ! {} owns {}", conflict.manager, conflict.field);
        }
        if !doc.unified.is_empty() {
            let lines: Vec<&str> = doc.unified.lines().take(8).collect();
            for line in lines {
                println!("     | {line}");
            }
            let total = doc.unified.lines().count();
            if total > 8 {
                println!("     | … {} more lines", total - 8);
            }
        }
        println!();
    }
    println!("blocked: {}", plan.blocked());

    manager.disconnect(&context);
    println!("\ndone (dry-run only, nothing was written)");
    Ok(())
}
