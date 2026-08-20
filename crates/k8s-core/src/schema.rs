//! JSON Schema for a resource, taken from the cluster's own OpenAPI document.
//!
//! Bundling schemas with the app would be wrong twice over: they would drift
//! from the cluster's Kubernetes version, and they would cover no CRDs at all.
//! Reading `/openapi/v3` instead means validation and form generation match
//! exactly what this apiserver accepts, custom resources included.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    cluster::ClusterHandle,
    discovery::ResourceDescriptor,
    error::{CoreError, Result},
};

/// A JSON Schema draft-07 document ready for `monaco-yaml` or a form builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSchema {
    /// `group/version/plural`.
    pub resource: String,
    pub kind: String,
    /// Self-contained schema: every `$ref` points inside `$defs`.
    pub schema: Value,
}

/// Fetch and convert the schema for one resource type.
pub async fn resource_schema(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
) -> Result<ResourceSchema> {
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };
    let descriptor = discovery.require(resource)?.clone();
    let document = fetch_openapi(&cluster.client, &descriptor).await?;
    let schema = build_schema(&document, &descriptor)?;

    Ok(ResourceSchema {
        resource: resource.to_string(),
        kind: descriptor.kind.clone(),
        schema,
    })
}

async fn fetch_openapi(client: &Client, descriptor: &ResourceDescriptor) -> Result<Value> {
    let path = if descriptor.group.is_empty() {
        format!("/openapi/v3/api/{}", descriptor.version)
    } else {
        format!(
            "/openapi/v3/apis/{}/{}",
            descriptor.group, descriptor.version
        )
    };

    let request = http::Request::builder()
        .uri(path)
        .header(http::header::ACCEPT, "application/json")
        .body(Vec::new())
        .map_err(|err| CoreError::other(format!("could not build openapi request: {err}")))?;

    Ok(client.request::<Value>(request).await?)
}

/// Turn the OpenAPI v3 document into a standalone JSON Schema for one kind.
fn build_schema(document: &Value, descriptor: &ResourceDescriptor) -> Result<Value> {
    let components = document
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .ok_or_else(|| CoreError::other("openapi document has no component schemas"))?;

    // Kinds are identified by the `x-kubernetes-group-version-kind` extension
    // rather than by name, because the Go-package-derived names are not
    // predictable for CRDs.
    let root_name = components
        .iter()
        .find(|(_, schema)| matches_gvk(schema, descriptor))
        .map(|(name, _)| name.clone())
        .ok_or_else(|| {
            CoreError::other(format!(
                "cluster openapi has no schema for {}",
                descriptor.kind
            ))
        })?;

    // Only the transitively referenced definitions are copied; a full group
    // document is megabytes and most of it is unrelated.
    let mut wanted: HashSet<String> = HashSet::new();
    collect_refs(components, &root_name, &mut wanted);

    let mut defs = Map::new();
    for name in &wanted {
        if let Some(schema) = components.get(name) {
            defs.insert(name.clone(), rewrite_refs(schema));
        }
    }

    let mut root = rewrite_refs(
        components
            .get(&root_name)
            .ok_or_else(|| CoreError::other("root schema vanished"))?,
    );
    if let Value::Object(map) = &mut root {
        map.insert(
            "$schema".into(),
            Value::String("http://json-schema.org/draft-07/schema#".into()),
        );
        map.insert("$defs".into(), Value::Object(defs));
        // The apiserver marks these optional, but an editor buffer without them
        // cannot be applied, so the schema should say so.
        map.insert(
            "required".into(),
            Value::Array(vec![
                Value::String("apiVersion".into()),
                Value::String("kind".into()),
            ]),
        );
    }
    Ok(root)
}

fn matches_gvk(schema: &Value, descriptor: &ResourceDescriptor) -> bool {
    schema
        .get("x-kubernetes-group-version-kind")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("kind").and_then(Value::as_str) == Some(&descriptor.kind)
                    && entry.get("version").and_then(Value::as_str) == Some(&descriptor.version)
                    && entry.get("group").and_then(Value::as_str).unwrap_or("") == descriptor.group
            })
        })
}

/// Walk `$ref`s from `name`, adding every reachable definition to `out`.
fn collect_refs(components: &Map<String, Value>, name: &str, out: &mut HashSet<String>) {
    if !out.insert(name.to_string()) {
        return; // already visited; also breaks reference cycles
    }
    let Some(schema) = components.get(name) else {
        return;
    };
    for referenced in refs_in(schema) {
        collect_refs(components, &referenced, out);
    }
}

fn refs_in(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref"
                    && let Some(target) = child.as_str()
                    && let Some(name) = target.strip_prefix("#/components/schemas/")
                {
                    out.push(name.to_string());
                }
                out.extend(refs_in(child));
            }
        }
        Value::Array(items) => {
            for item in items {
                out.extend(refs_in(item));
            }
        }
        _ => {}
    }
    out
}

/// Point `$ref`s at `$defs` so the document validates standalone.
fn rewrite_refs(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if key == "$ref"
                    && let Some(target) = child.as_str()
                    && let Some(name) = target.strip_prefix("#/components/schemas/")
                {
                    out.insert(key.clone(), Value::String(format!("#/$defs/{name}")));
                    continue;
                }
                out.insert(key.clone(), rewrite_refs(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(rewrite_refs).collect()),
        other => other.clone(),
    }
}

/// Cache so switching between objects of the same kind does not refetch a
/// multi-megabyte OpenAPI document each time.
#[derive(Default)]
pub struct SchemaCache {
    entries: parking_lot::Mutex<BTreeMap<(String, String), Arc<ResourceSchema>>>,
}

impl SchemaCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(
        &self,
        cluster: &Arc<ClusterHandle>,
        resource: &str,
    ) -> Result<Arc<ResourceSchema>> {
        let key = (cluster.id.clone(), resource.to_string());
        if let Some(hit) = self.entries.lock().get(&key) {
            return Ok(hit.clone());
        }
        let schema = Arc::new(resource_schema(cluster, resource).await?);
        self.entries.lock().insert(key, schema.clone());
        Ok(schema)
    }

    pub fn clear_cluster(&self, cluster: &str) {
        self.entries.lock().retain(|(id, _), _| id != cluster);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor() -> ResourceDescriptor {
        ResourceDescriptor {
            key: "apps/v1/deployments".into(),
            group: "apps".into(),
            version: "v1".into(),
            kind: "Deployment".into(),
            plural: "deployments".into(),
            api_version: "apps/v1".into(),
            namespaced: true,
            verbs: Vec::new(),
            short_names: Vec::new(),
            is_crd: false,
            printer_columns: Vec::new(),
            watchable: true,
            editable: true,
            deletable: true,
        }
    }

    fn document() -> Value {
        json!({
            "components": {
                "schemas": {
                    "io.k8s.api.apps.v1.Deployment": {
                        "type": "object",
                        "properties": {
                            "spec": { "$ref": "#/components/schemas/io.k8s.api.apps.v1.DeploymentSpec" }
                        },
                        "x-kubernetes-group-version-kind": [
                            { "group": "apps", "version": "v1", "kind": "Deployment" }
                        ]
                    },
                    "io.k8s.api.apps.v1.DeploymentSpec": {
                        "type": "object",
                        "properties": { "replicas": { "type": "integer" } }
                    },
                    "io.k8s.api.core.v1.Unrelated": { "type": "object" }
                }
            }
        })
    }

    #[test]
    fn extracts_the_matching_kind() {
        let schema = build_schema(&document(), &descriptor()).unwrap();
        assert_eq!(
            schema.pointer("/properties/spec/$ref").unwrap(),
            "#/$defs/io.k8s.api.apps.v1.DeploymentSpec"
        );
    }

    #[test]
    fn copies_only_reachable_definitions() {
        let schema = build_schema(&document(), &descriptor()).unwrap();
        let defs = schema.get("$defs").unwrap().as_object().unwrap();
        assert!(defs.contains_key("io.k8s.api.apps.v1.DeploymentSpec"));
        assert!(
            !defs.contains_key("io.k8s.api.core.v1.Unrelated"),
            "unreferenced schemas must not be copied"
        );
    }

    #[test]
    fn apiversion_and_kind_are_required() {
        let schema = build_schema(&document(), &descriptor()).unwrap();
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v == "apiVersion"));
        assert!(required.iter().any(|v| v == "kind"));
    }

    #[test]
    fn unknown_kind_is_an_error() {
        let mut other = descriptor();
        other.kind = "Nonexistent".into();
        assert!(build_schema(&document(), &other).is_err());
    }

    /// A schema that references itself must not hang the collector.
    #[test]
    fn reference_cycles_terminate() {
        let doc = json!({
            "components": { "schemas": {
                "A": {
                    "properties": { "b": { "$ref": "#/components/schemas/B" } },
                    "x-kubernetes-group-version-kind": [
                        { "group": "apps", "version": "v1", "kind": "Deployment" }
                    ]
                },
                "B": { "properties": { "a": { "$ref": "#/components/schemas/A" } } }
            }}
        });
        let schema = build_schema(&doc, &descriptor()).unwrap();
        assert!(schema.get("$defs").unwrap().get("B").is_some());
    }
}
