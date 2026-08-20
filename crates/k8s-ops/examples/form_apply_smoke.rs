//! What the form editor sends, and whether the apiserver accepts it.
//!
//!   cargo run -p k8s-ops --example form_apply_smoke -- <context> <namespace> <deployment>
//!
//! Two payloads are dry-run applied against a real object:
//!
//! * the whole live object, which is what the form used to send. It carries
//!   `metadata.managedFields` (rejected outright) and claims every field in the
//!   object, so it conflicts with whoever owns the parts nobody edited.
//! * only the changed field, which is what the form sends now.
//!
//! Non-mutating: both applies are `dryRun=All`.

use k8s_core::{ClusterManager, ConnectOptions, objects};
use k8s_ops::apply::{self, EditRequest};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or("usage: form_apply_smoke <context> <namespace> <deployment>")?;
    let namespace = args.next().ok_or("missing namespace")?;
    let name = args.next().ok_or("missing deployment name")?;

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    let resource = "apps/v1/deployments";
    let live = objects::get(&cluster, resource, Some(&namespace), &name).await?;
    let json = objects::to_value(&live)?;

    let owners: Vec<String> = json
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("manager")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    println!("{namespace}/{name}");
    println!("  field managers: {}", owners.join(", "));

    let replicas = json.pointer("/spec/replicas").cloned().unwrap_or(json!(1));

    let forced = |label: &str, body: Value, force: bool| EditRequest {
        resource: resource.into(),
        namespace: Some(namespace.clone()),
        name: name.clone(),
        yaml: serde_json::to_string_pretty(&body).unwrap_or_else(|_| label.into()),
        force,
    };
    let request = |label: &str, body: Value| forced(label, body, false);

    // Old behaviour: the live object, verbatim.
    let whole = apply::preview(&cluster, &request("whole", json.clone())).await?;
    report("whole object", &whole);

    // New behaviour: identity plus the one field the form changed.
    let pruned = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": name, "namespace": namespace },
        "spec": { "replicas": replicas },
    });
    let changed_only = apply::preview(&cluster, &request("pruned", pruned)).await?;
    report("changed field only", &changed_only);

    // Editing a field somebody else owns: a conflict here is correct, and the
    // user has to decide whether to take the field.
    let container = json
        .pointer("/spec/template/spec/containers/0")
        .and_then(Value::as_object)
        .ok_or("deployment has no containers")?;
    let container_name = container.get("name").cloned().unwrap_or(json!("app"));
    let image = container.get("image").and_then(Value::as_str).unwrap_or("");
    let contested = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": name, "namespace": namespace },
        "spec": { "template": { "spec": { "containers": [
            { "name": container_name, "image": format!("{image}-probe") }
        ]}}},
    });
    let owned = apply::preview(&cluster, &request("contested", contested.clone())).await?;
    report("image changed", &owned);

    let taken = apply::preview(&cluster, &forced("contested", contested, true)).await?;
    report("image changed, forced", &taken);

    Ok(())
}

fn report(label: &str, diff: &apply::DiffResult) {
    if diff.conflicts.is_empty() {
        println!("  {label}: accepted (changed={})", diff.changed);
    } else {
        println!("  {label}: refused, {} conflict(s)", diff.conflicts.len());
        for conflict in &diff.conflicts {
            println!("    {} owns {}", conflict.manager, conflict.field);
        }
    }
}
