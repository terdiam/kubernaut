//! Verify that extracting a context from the system kubeconfig produces
//! something that actually connects.
//!
//!   cargo run -p k8s-core --example import_smoke -- <context>
//!
//! This is the risky part of importing: if extraction drops the user's
//! certificate data or exec credentials, the imported cluster looks fine in the
//! list and fails the moment it is used. Nothing is printed but counts and
//! outcomes — no credential is ever written to the terminal.

use k8s_core::{ClusterManager, ConnectOptions, kubeconfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let context = std::env::args()
        .nth(1)
        .ok_or("usage: import_smoke <context>")?;

    k8s_core::paths::hydrate_process_path(&[]).await;

    let available = kubeconfig::system_contexts();
    println!("contexts in the system kubeconfig: {}", available.len());
    for entry in &available {
        println!(
            "  {}{}",
            entry.name,
            if entry.is_current { " (current)" } else { "" }
        );
    }

    let source = kubeconfig::read_system()?;
    let extracted = kubeconfig::extract(&source, std::slice::from_ref(&context))?;
    println!("\nextracted `{context}`: {} bytes", extracted.len());

    // Structural check without revealing anything.
    let parsed: serde_json::Value = serde_yaml_ng::from_str(&extracted)?;
    let count = |key: &str| {
        parsed
            .get(key)
            .and_then(|value| value.as_array())
            .map(|array| array.len())
            .unwrap_or(0)
    };
    println!(
        "  {} context(s), {} cluster(s), {} user(s), current-context={:?}",
        count("contexts"),
        count("clusters"),
        count("users"),
        parsed.get("current-context").and_then(|v| v.as_str())
    );

    let user_has_credentials = parsed
        .pointer("/users/0/user")
        .and_then(|user| user.as_object())
        .map(|user| user.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    println!("  credential fields carried over: {user_has_credentials:?}");

    // The real test: write it exactly as an import would, then connect.
    let path = kubeconfig::write_private(&extracted)?;
    println!("\nwrote a managed copy, connecting through it…");

    let manager = ClusterManager::from_managed(vec![path.clone()])?;
    let contexts = manager.contexts();
    println!(
        "  the managed file alone offers {} context(s)",
        contexts.len()
    );

    let result = manager.connect(&context, ConnectOptions::default()).await;
    let _ = std::fs::remove_file(&path);

    match result {
        Ok(handle) => {
            println!("  connected: {:?}", handle.status());
            manager.disconnect(&context);
            println!("\nimport produces a working cluster");
            Ok(())
        }
        Err(err) => {
            println!("  FAILED: {err}");
            Err("the extracted kubeconfig does not connect".into())
        }
    }
}
