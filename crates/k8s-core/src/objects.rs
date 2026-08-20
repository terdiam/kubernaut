//! Single-object reads: what the detail pane and the editor load.

use std::sync::Arc;

use k8s_openapi::api::core::v1::Namespace;
use kube::{
    Api, ResourceExt,
    api::{DynamicObject, ListParams},
};
use serde_json::Value;

use crate::{cluster::ClusterHandle, error::Result};

/// Fetch one object in full.
pub async fn get(
    cluster: &Arc<ClusterHandle>,
    resource_key: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<DynamicObject> {
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };
    let descriptor = discovery.require(resource_key)?;
    let ar = descriptor.api_resource();

    let api: Api<DynamicObject> = match (namespace, descriptor.namespaced) {
        (Some(ns), true) => Api::namespaced_with(cluster.client.clone(), ns, &ar),
        _ => Api::all_with(cluster.client.clone(), &ar),
    };
    Ok(api.get(name).await?)
}

/// Namespaces the user can see, for the namespace picker.
///
/// Falls back to the context's default namespace when listing is forbidden —
/// a very common setup for developers with namespace-scoped RBAC.
pub async fn list_namespaces(cluster: &Arc<ClusterHandle>) -> Vec<String> {
    let api: Api<Namespace> = Api::all(cluster.client.clone());
    match api.list(&ListParams::default()).await {
        Ok(list) => {
            let mut names: Vec<String> = list.items.iter().map(|ns| ns.name_any()).collect();
            names.sort_unstable();
            names
        }
        Err(err) => {
            tracing::debug!(cluster = %cluster.id, %err, "namespace listing forbidden");
            vec![cluster.default_namespace.clone()]
        }
    }
}

/// Render an object as YAML for the editor.
///
/// `managedFields` is dropped unless explicitly requested: it is server-side
/// bookkeeping that triples the size of a typical Deployment and is noise in an
/// editor. `resourceVersion` and friends are kept, because server-side apply
/// needs them for conflict detection.
pub fn to_yaml(obj: &DynamicObject, include_managed_fields: bool) -> Result<String> {
    let mut value = to_value(obj)?;
    if !include_managed_fields {
        strip_managed_fields(&mut value);
    }
    serde_yaml_ng::to_string(&value).map_err(crate::error::CoreError::other)
}

/// Drop `metadata.managedFields` in place.
///
/// Editors show it to nobody's benefit, and an apply that carries it back is
/// rejected outright: `metadata.managedFields must be nil`.
pub fn strip_managed_fields(value: &mut Value) {
    if let Some(meta) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        meta.remove("managedFields");
    }
}

/// Full object JSON with apiVersion/kind restored.
///
/// `DynamicObject` splits typed metadata from untyped data and stores the GVK
/// out of band, so a naive `to_value` loses `apiVersion`/`kind`.
pub fn to_value(obj: &DynamicObject) -> Result<Value> {
    let mut value = obj.data.clone();
    let map = value
        .as_object_mut()
        .ok_or_else(|| crate::error::CoreError::other("object body is not a JSON object"))?;

    if let Some(types) = &obj.types {
        map.insert(
            "apiVersion".into(),
            Value::String(types.api_version.clone()),
        );
        map.insert("kind".into(), Value::String(types.kind.clone()));
    }
    map.insert("metadata".into(), serde_json::to_value(&obj.metadata)?);

    // Key order follows what people expect to read top-down in an editor.
    let ordered = ["apiVersion", "kind", "metadata", "spec", "status"];
    let mut sorted = serde_json::Map::new();
    for key in ordered {
        if let Some(v) = map.remove(key) {
            sorted.insert(key.to_string(), v);
        }
    }
    for (k, v) in std::mem::take(map) {
        sorted.insert(k, v);
    }
    Ok(Value::Object(sorted))
}
