//! Applying manifests the user brought with them.
//!
//! The editor path assumes an object that already exists: it is bound to one
//! name and one resource, and it refuses a document that names anything else.
//! Creating a resource, or pasting a file that holds several, needs the
//! opposite shape — read what the document says it is, resolve that against
//! what the cluster actually serves, and report per document.
//!
//! Everything is server-side apply with the same field manager as the editor,
//! so a create and an update are one code path and ownership stays consistent
//! either way.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

use k8s_core::cluster::ClusterHandle;
use kube::{
    Api,
    api::{DeleteParams, DynamicObject, ListParams, Patch, PatchParams},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    apply::{FIELD_MANAGER, FieldConflict},
    error::{OpsError, Result},
};

/// Fields the apiserver owns. A manifest exported with `kubectl get -o yaml`
/// carries all of them, and every one of them makes a create fail — most
/// bluntly `resourceVersion`, which the apiserver rejects outright on an object
/// that does not exist yet.
const SERVER_OWNED: &[&str] = &[
    "managedFields",
    "resourceVersion",
    "uid",
    "creationTimestamp",
    "generation",
    "selfLink",
];

/// What one document in a manifest would do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocPlan {
    /// Position in the file, so a document that fails to parse can still be
    /// pointed at.
    pub index: usize,
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    /// `group/version/plural`, once the kind resolved against this cluster.
    pub resource: Option<String>,
    /// `create` | `update` | `unchanged` | `conflict` | `error`
    pub action: String,
    /// Unified diff, live → proposed. For a create, the whole object.
    pub unified: String,
    pub conflicts: Vec<FieldConflict>,
    /// Things that are not errors but change what the apply means.
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPlan {
    pub docs: Vec<DocPlan>,
    /// Objects a prune would delete: ones this app applied earlier that the
    /// manifest no longer lists. Empty unless a prune was asked for.
    #[serde(default)]
    pub prune: Vec<PruneTarget>,
}

impl ManifestPlan {
    /// True when nothing in the file can be applied as it stands.
    pub fn blocked(&self) -> bool {
        self.docs.iter().any(|doc| doc.action == "error")
    }
}

/// An object a prune would delete.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneTarget {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    /// `group/version/plural`.
    pub resource: String,
}

/// A prune the person has previewed and agreed to.
pub struct PruneRequest<'a> {
    pub selector: &'a str,
    /// What the preview listed. Anything else that qualifies now is left alone,
    /// so the confirmation always describes exactly what is deleted.
    pub allowed: &'a [PruneTarget],
}

/// What one document actually did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocResult {
    pub index: usize,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    /// `created` | `configured` | `unchanged` | `conflict` | `error`
    pub status: String,
    pub conflicts: Vec<FieldConflict>,
    pub error: Option<String>,
}

/// One parsed document, before it meets the cluster.
#[derive(Debug)]
struct Doc {
    index: usize,
    value: Value,
    api_version: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    warnings: Vec<String>,
}

/// Split a multi-document YAML file.
///
/// Documents that hold nothing — a trailing `---`, a block of comments — are
/// dropped rather than reported as errors, because a file that ends with a
/// separator is normal and complaining about it is noise.
fn documents(yaml: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (index, document) in serde_yaml_ng::Deserializer::from_str(yaml).enumerate() {
        let value = Value::deserialize(document)
            .map_err(|err| OpsError::Yaml(format!("document {}: {err}", index + 1)))?;
        if value.is_null() {
            continue;
        }
        out.push(value);
    }
    Ok(out)
}

/// Read what a document claims to be, and clean the parts the apiserver owns.
fn describe(
    index: usize,
    mut value: Value,
    default_namespace: Option<&str>,
) -> std::result::Result<Doc, Box<DocPlan>> {
    let string_at = |value: &Value, pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    let api_version = string_at(&value, "/apiVersion");
    let kind = string_at(&value, "/kind");
    let name = string_at(&value, "/metadata/name");
    let generate_name = string_at(&value, "/metadata/generateName");
    let namespace = value
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| default_namespace.map(str::to_string));

    let fail = |message: String| {
        Box::new(DocPlan {
            index,
            api_version: api_version.clone(),
            kind: kind.clone(),
            name: name.clone(),
            namespace: namespace.clone(),
            resource: None,
            action: "error".into(),
            unified: String::new(),
            conflicts: Vec::new(),
            warnings: Vec::new(),
            error: Some(message),
        })
    };

    if api_version.is_empty() || kind.is_empty() {
        return Err(fail(
            "apiVersion and kind are required; this does not look like a Kubernetes object".into(),
        ));
    }
    if name.is_empty() {
        return Err(fail(if generate_name.is_empty() {
            "metadata.name is required".into()
        } else {
            // Not a limitation worth hiding: SSA identifies an object by name,
            // and there is nothing to identify before the server picks one.
            format!(
                "metadata.generateName (`{generate_name}`) cannot be applied — server-side apply \
                 needs a name it can address. Give it a metadata.name."
            )
        }));
    }

    let mut warnings = Vec::new();

    if let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        let removed: Vec<&str> = SERVER_OWNED
            .iter()
            .filter(|field| metadata.remove(**field).is_some())
            .copied()
            .collect();
        if !removed.is_empty() {
            warnings.push(format!(
                "removed server-owned metadata ({}) — an exported object cannot be re-applied \
                 with them",
                removed.join(", ")
            ));
        }

        // Copying an exported child object carries a reference to a parent
        // that may not exist here. Garbage collection deletes the result
        // within seconds, which looks like the apply silently failing.
        if let Some(owners) = metadata.get("ownerReferences").and_then(Value::as_array)
            && !owners.is_empty()
        {
            let described: Vec<String> = owners
                .iter()
                .map(|owner| {
                    format!(
                        "{}/{}",
                        owner.get("kind").and_then(Value::as_str).unwrap_or("?"),
                        owner.get("name").and_then(Value::as_str).unwrap_or("?")
                    )
                })
                .collect();
            warnings.push(format!(
                "carries ownerReferences ({}) — if that owner does not exist in this cluster, \
                 garbage collection deletes this object almost immediately",
                described.join(", ")
            ));
        }
    }

    if value.get("status").is_some() {
        value.as_object_mut().map(|object| object.remove("status"));
        warnings.push("removed status — the apiserver owns it".into());
    }

    Ok(Doc {
        index,
        value,
        api_version,
        kind,
        name,
        namespace,
        warnings,
    })
}

/// Resolve a document against what this cluster serves, and build its `Api`.
async fn bind(
    cluster: &Arc<ClusterHandle>,
    doc: &Doc,
) -> std::result::Result<(String, Api<DynamicObject>), String> {
    let discovery = match cluster.discovery() {
        Some(discovery) => discovery,
        None => cluster
            .refresh_discovery()
            .await
            .map_err(|err| err.to_string())?,
    };

    let descriptor = match discovery.resolve_gvk(&doc.api_version, &doc.kind) {
        Some(descriptor) => descriptor,
        None => {
            let served = discovery.versions_of_kind(&doc.kind);
            return Err(if served.is_empty() {
                format!(
                    "this cluster serves no `{}` — check the kind, or install the CRD that \
                     provides it",
                    doc.kind
                )
            } else {
                format!(
                    "this cluster serves {} as {}, not `{}`",
                    doc.kind,
                    served.join(" or "),
                    doc.api_version
                )
            });
        }
    };

    if descriptor.namespaced && doc.namespace.is_none() {
        return Err(format!(
            "{} is namespaced but no namespace was given, in the document or as a default",
            doc.kind
        ));
    }
    if !descriptor.namespaced
        && doc
            .value
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .is_some()
    {
        return Err(format!(
            "{} is cluster-scoped but the document sets metadata.namespace",
            doc.kind
        ));
    }

    let api_resource = descriptor.api_resource();
    let api = match (doc.namespace.as_deref(), descriptor.namespaced) {
        (Some(namespace), true) => {
            Api::namespaced_with(cluster.client.clone(), namespace, &api_resource)
        }
        _ => Api::all_with(cluster.client.clone(), &api_resource),
    };
    Ok((descriptor.key.clone(), api))
}

fn object_of(doc: &Doc) -> std::result::Result<DynamicObject, String> {
    serde_json::from_value(doc.value.clone()).map_err(|err| err.to_string())
}

/// What applying this manifest would do, without doing it.
pub async fn plan(
    cluster: &Arc<ClusterHandle>,
    yaml: &str,
    default_namespace: Option<&str>,
    force: bool,
    prune_selector: Option<&str>,
) -> Result<ManifestPlan> {
    let values = documents(yaml)?;
    if values.is_empty() {
        return Err(OpsError::Yaml("nothing to apply".into()));
    }

    let mut docs = Vec::new();
    for (index, value) in values.into_iter().enumerate() {
        let doc = match describe(index, value, default_namespace) {
            Ok(doc) => doc,
            Err(plan) => {
                docs.push(*plan);
                continue;
            }
        };

        let mut entry = DocPlan {
            index: doc.index,
            api_version: doc.api_version.clone(),
            kind: doc.kind.clone(),
            name: doc.name.clone(),
            namespace: doc.namespace.clone(),
            resource: None,
            action: "error".into(),
            unified: String::new(),
            conflicts: Vec::new(),
            warnings: doc.warnings.clone(),
            error: None,
        };

        let (resource, api) = match bind(cluster, &doc).await {
            Ok(bound) => bound,
            Err(message) => {
                entry.error = Some(message);
                docs.push(entry);
                continue;
            }
        };
        entry.resource = Some(resource);

        let object = match object_of(&doc) {
            Ok(object) => object,
            Err(message) => {
                entry.error = Some(message);
                docs.push(entry);
                continue;
            }
        };

        // A 404 is the answer, not a failure: it is what makes this a create.
        let live = match api.get(&doc.name).await {
            Ok(live) => Some(live),
            Err(kube::Error::Api(status)) if status.code == 404 => None,
            Err(err) => {
                entry.error = Some(err.to_string());
                docs.push(entry);
                continue;
            }
        };

        let mut params = PatchParams::apply(FIELD_MANAGER).dry_run();
        if force {
            params = params.force();
        }

        match api.patch(&doc.name, &params, &Patch::Apply(&object)).await {
            Ok(proposed) => {
                let before = match &live {
                    Some(live) => crate::apply::render(live)?,
                    None => String::new(),
                };
                let after = crate::apply::render(&proposed)?;
                let unified = crate::apply::unified_diff(&before, &after, &doc.name);
                entry.action = match (&live, unified.is_empty()) {
                    (None, _) => "create",
                    (Some(_), true) => "unchanged",
                    (Some(_), false) => "update",
                }
                .into();
                entry.unified = unified;
            }
            Err(kube::Error::Api(status)) if status.code == 409 => {
                entry.action = "conflict".into();
                entry.conflicts = crate::apply::conflicts_from(&status);
            }
            Err(err) => entry.error = Some(err.to_string()),
        }

        docs.push(entry);
    }

    let prune = match prune_selector {
        Some(selector) => prune_candidates(cluster, yaml, default_namespace, selector).await?,
        None => Vec::new(),
    };
    Ok(ManifestPlan { docs, prune })
}

/// What a prune with `selector` would delete.
///
/// Three things must all hold for an object, so a prune cannot reach beyond what
/// this app put there from this manifest's kinds: it is of a kind the manifest
/// contains, in a namespace the manifest uses, matches the selector the person
/// typed, and was applied by this app (its server-side-apply manager is ours).
/// An object another tool created, or one made by hand, is never a candidate.
pub async fn prune_candidates(
    cluster: &Arc<ClusterHandle>,
    yaml: &str,
    default_namespace: Option<&str>,
    selector: &str,
) -> Result<Vec<PruneTarget>> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(OpsError::other(
            "pruning needs a label selector, such as `app=web`: without one, every object \
             this app ever applied of these kinds would be a candidate",
        ));
    }
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };

    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (index, value) in documents(yaml)?.into_iter().enumerate() {
        let Ok(doc) = describe(index, value, default_namespace) else {
            continue;
        };
        let Some(descriptor) = discovery.resolve_gvk(&doc.api_version, &doc.kind) else {
            continue;
        };
        let namespace = descriptor
            .namespaced
            .then(|| doc.namespace.clone())
            .flatten();
        let group = groups
            .entry(descriptor.key.clone())
            .or_insert_with(|| Group {
                api_version: descriptor.api_version.clone(),
                kind: descriptor.kind.clone(),
                api_resource: descriptor.api_resource(),
                namespaced: descriptor.namespaced,
                namespaces: BTreeSet::new(),
                kept: HashSet::new(),
            });
        group.namespaces.insert(namespace.clone());
        group.kept.insert((namespace, doc.name));
    }

    let params = ListParams::default().labels(selector);
    let mut out = Vec::new();
    for (key, group) in &groups {
        for namespace in &group.namespaces {
            let api: Api<DynamicObject> = match (namespace, group.namespaced) {
                (Some(namespace), true) => {
                    Api::namespaced_with(cluster.client.clone(), namespace, &group.api_resource)
                }
                _ => Api::all_with(cluster.client.clone(), &group.api_resource),
            };
            for object in api.list(&params).await? {
                if !is_prunable(&object, &group.kept) {
                    continue;
                }
                out.push(PruneTarget {
                    api_version: group.api_version.clone(),
                    kind: group.kind.clone(),
                    name: object.metadata.name.clone().unwrap_or_default(),
                    namespace: object.metadata.namespace.clone(),
                    resource: key.clone(),
                });
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// One kind in a manifest, with where and under what names it appears.
struct Group {
    api_version: String,
    kind: String,
    api_resource: kube::api::ApiResource,
    namespaced: bool,
    namespaces: BTreeSet<Option<String>>,
    kept: HashSet<(Option<String>, String)>,
}

/// Whether a live object is one this app applied that the manifest dropped.
fn is_prunable(object: &DynamicObject, kept: &HashSet<(Option<String>, String)>) -> bool {
    let Some(name) = object.metadata.name.clone() else {
        return false;
    };
    if object.metadata.deletion_timestamp.is_some()
        || kept.contains(&(object.metadata.namespace.clone(), name))
    {
        return false;
    }
    object
        .metadata
        .managed_fields
        .iter()
        .flatten()
        .any(|entry| {
            entry.manager.as_deref() == Some(FIELD_MANAGER)
                && entry.operation.as_deref() == Some("Apply")
        })
}

/// Delete what a previewed prune listed, if it still qualifies.
async fn prune(
    cluster: &Arc<ClusterHandle>,
    yaml: &str,
    default_namespace: Option<&str>,
    request: &PruneRequest<'_>,
    out: &mut Vec<DocResult>,
) -> Result<()> {
    let now = prune_candidates(cluster, yaml, default_namespace, request.selector).await?;
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };
    for target in now.iter().filter(|t| request.allowed.contains(t)) {
        let mut result = DocResult {
            index: out.len(),
            kind: target.kind.clone(),
            name: target.name.clone(),
            namespace: target.namespace.clone(),
            status: "pruned".into(),
            conflicts: Vec::new(),
            error: None,
        };
        let outcome = match discovery.require(&target.resource) {
            Ok(descriptor) => {
                let api_resource = descriptor.api_resource();
                let api: Api<DynamicObject> = match &target.namespace {
                    Some(namespace) if descriptor.namespaced => {
                        Api::namespaced_with(cluster.client.clone(), namespace, &api_resource)
                    }
                    _ => Api::all_with(cluster.client.clone(), &api_resource),
                };
                api.delete(&target.name, &DeleteParams::default())
                    .await
                    .map(|_| ())
                    .map_err(|err| err.to_string())
            }
            Err(err) => Err(err.to_string()),
        };
        if let Err(message) = outcome {
            result.status = "error".into();
            result.error = Some(message);
        }
        out.push(result);
    }
    Ok(())
}

/// Apply every document, reporting each one separately.
///
/// Documents are independent: one that fails does not stop the rest, because a
/// manifest is usually a set of related objects and stopping halfway leaves a
/// worse state than finishing and reporting.
pub async fn apply(
    cluster: &Arc<ClusterHandle>,
    yaml: &str,
    default_namespace: Option<&str>,
    force: bool,
    prune_request: Option<&PruneRequest<'_>>,
) -> Result<Vec<DocResult>> {
    let values = documents(yaml)?;
    if values.is_empty() {
        return Err(OpsError::Yaml("nothing to apply".into()));
    }

    let mut out = Vec::new();
    for (index, value) in values.into_iter().enumerate() {
        let doc = match describe(index, value, default_namespace) {
            Ok(doc) => doc,
            Err(plan) => {
                out.push(DocResult {
                    index,
                    kind: plan.kind.clone(),
                    name: plan.name.clone(),
                    namespace: plan.namespace.clone(),
                    status: "error".into(),
                    conflicts: Vec::new(),
                    error: plan.error.clone(),
                });
                continue;
            }
        };

        let mut result = DocResult {
            index: doc.index,
            kind: doc.kind.clone(),
            name: doc.name.clone(),
            namespace: doc.namespace.clone(),
            status: "error".into(),
            conflicts: Vec::new(),
            error: None,
        };

        let (_, api) = match bind(cluster, &doc).await {
            Ok(bound) => bound,
            Err(message) => {
                result.error = Some(message);
                out.push(result);
                continue;
            }
        };
        let object = match object_of(&doc) {
            Ok(object) => object,
            Err(message) => {
                result.error = Some(message);
                out.push(result);
                continue;
            }
        };

        let existed = match api.get(&doc.name).await {
            Ok(live) => Some(live.metadata.resource_version.clone()),
            Err(kube::Error::Api(status)) if status.code == 404 => None,
            Err(err) => {
                result.error = Some(err.to_string());
                out.push(result);
                continue;
            }
        };

        let mut params = PatchParams::apply(FIELD_MANAGER);
        if force {
            params = params.force();
        }

        match api.patch(&doc.name, &params, &Patch::Apply(&object)).await {
            Ok(applied) => {
                result.status = match &existed {
                    None => "created",
                    // The resourceVersion moves only when something changed, so
                    // it distinguishes a real update from a no-op apply without
                    // diffing the object again.
                    Some(before) if *before == applied.metadata.resource_version => "unchanged",
                    Some(_) => "configured",
                }
                .into();
            }
            Err(kube::Error::Api(status)) if status.code == 409 => {
                result.status = "conflict".into();
                result.conflicts = crate::apply::conflicts_from(&status);
            }
            Err(err) => result.error = Some(err.to_string()),
        }

        out.push(result);
    }

    // Deleting after a partial apply would remove objects the manifest was
    // about to replace, so a prune waits for every document to go through.
    if let Some(request) = prune_request
        && out
            .iter()
            .all(|r| r.status != "error" && r.status != "conflict")
    {
        prune(cluster, yaml, default_namespace, request, &mut out).await?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(
        yaml: &str,
        default_namespace: Option<&str>,
    ) -> std::result::Result<Doc, Box<DocPlan>> {
        let values = documents(yaml).expect("parse");
        describe(
            0,
            values.into_iter().next().expect("one doc"),
            default_namespace,
        )
    }

    #[test]
    fn a_multi_document_file_splits_and_drops_empty_documents() {
        let yaml = "\
# leading comment
---
apiVersion: v1
kind: Service
metadata: { name: web }
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: web }
---
";
        let docs = documents(yaml).expect("parse");
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["kind"], "Service");
        assert_eq!(docs[1]["kind"], "Deployment");
    }

    #[test]
    fn an_exported_object_is_stripped_so_it_can_be_created_again() {
        // Shape of `kubectl get deploy -o yaml`. Every one of these fields
        // makes the apply fail, so silently keeping them would make import
        // useless for the most common source of manifests.
        let doc = parse_one(
            "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: app
  uid: 85bf0627-56d3-4822-a8c6-45d9788b5fe9
  resourceVersion: \"33605537\"
  generation: 4
  creationTimestamp: \"2026-08-20T14:04:22Z\"
  managedFields: [{ manager: rancher }]
spec: { replicas: 2 }
status: { readyReplicas: 2 }
",
            None,
        )
        .expect("described");

        let metadata = doc.value.get("metadata").expect("metadata");
        for field in SERVER_OWNED {
            assert!(metadata.get(field).is_none(), "`{field}` survived");
        }
        assert!(doc.value.get("status").is_none());
        assert_eq!(doc.namespace.as_deref(), Some("app"));

        // The user is told what was changed on their behalf.
        assert!(doc.warnings.iter().any(|w| w.contains("resourceVersion")));
        assert!(doc.warnings.iter().any(|w| w.contains("status")));
    }

    #[test]
    fn an_owner_reference_is_flagged_rather_than_removed() {
        let doc = parse_one(
            "\
apiVersion: v1
kind: Pod
metadata:
  name: web-1
  ownerReferences:
    - apiVersion: apps/v1
      kind: ReplicaSet
      name: web-7f99ccf7f9
      uid: abc
spec: { containers: [] }
",
            Some("app"),
        )
        .expect("described");

        // Removing it would change what the user asked for; warning does not.
        assert!(doc.value.pointer("/metadata/ownerReferences").is_some());
        assert!(
            doc.warnings
                .iter()
                .any(|w| w.contains("ReplicaSet/web-7f99ccf7f9") && w.contains("garbage")),
            "{:?}",
            doc.warnings
        );
    }

    #[test]
    fn generate_name_is_refused_with_the_reason() {
        let plan = parse_one(
            "apiVersion: v1\nkind: Pod\nmetadata: { generateName: web- }\n",
            Some("app"),
        )
        .expect_err("no name");
        assert_eq!(plan.action, "error");
        assert!(plan.error.as_ref().unwrap().contains("generateName"));
        assert!(plan.error.as_ref().unwrap().contains("needs a name"));
    }

    #[test]
    fn valid_yaml_that_is_not_a_kubernetes_object_says_so() {
        let plan = parse_one("foo: bar\n", None).unwrap_err();
        assert!(
            plan.error.as_ref().unwrap().contains("apiVersion and kind"),
            "{:?}",
            plan.error
        );
    }

    #[test]
    fn broken_yaml_names_the_document_it_broke_in() {
        // With several documents in a file, "mapping values are not allowed"
        // on its own leaves the reader hunting.
        let err = documents(
            "apiVersion: v1\nkind: Service\nmetadata: { name: web }\n---\njust: some: bad\n",
        )
        .expect_err("invalid yaml");
        assert!(err.to_string().contains("document 2"), "{err}");
    }

    #[test]
    fn the_default_namespace_only_fills_a_gap() {
        let explicit = parse_one(
            "apiVersion: v1\nkind: Service\nmetadata: { name: web, namespace: staging }\n",
            Some("app"),
        )
        .expect("described");
        assert_eq!(explicit.namespace.as_deref(), Some("staging"));

        let implied = parse_one(
            "apiVersion: v1\nkind: Service\nmetadata: { name: web }\n",
            Some("app"),
        )
        .expect("described");
        assert_eq!(implied.namespace.as_deref(), Some("app"));
    }

    #[test]
    fn a_clean_manifest_is_left_alone() {
        let doc = parse_one(
            "apiVersion: v1\nkind: ConfigMap\nmetadata: { name: settings }\ndata: { a: b }\n",
            Some("app"),
        )
        .expect("described");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    fn live(name: &str, manager: &str, operation: &str, deleting: bool) -> DynamicObject {
        let mut metadata = serde_json::json!({
            "name": name,
            "namespace": "app",
            "managedFields": [{"manager": manager, "operation": operation}]
        });
        if deleting {
            metadata["deletionTimestamp"] = "2026-01-01T00:00:00Z".into();
        }
        serde_json::from_value(serde_json::json!({
            "apiVersion": "v1", "kind": "ConfigMap", "metadata": metadata
        }))
        .expect("object")
    }

    fn kept(names: &[&str]) -> HashSet<(Option<String>, String)> {
        names
            .iter()
            .map(|n| (Some("app".to_string()), (*n).to_string()))
            .collect()
    }

    #[test]
    fn only_objects_this_app_applied_and_the_manifest_dropped_are_pruned() {
        let ours = live("old", FIELD_MANAGER, "Apply", false);
        assert!(is_prunable(&ours, &kept(&["new"])));
        assert!(
            !is_prunable(&ours, &kept(&["old"])),
            "still in the manifest"
        );
        assert!(
            !is_prunable(&live("old", "rancher", "Apply", false), &kept(&[])),
            "another tool applied it"
        );
        assert!(
            !is_prunable(&live("old", FIELD_MANAGER, "Update", false), &kept(&[])),
            "edited by hand under our name, not applied"
        );
        assert!(
            !is_prunable(&live("old", FIELD_MANAGER, "Apply", true), &kept(&[])),
            "already being deleted"
        );
    }

    #[test]
    fn the_same_name_in_another_namespace_is_not_kept() {
        let object = live("web", FIELD_MANAGER, "Apply", false);
        let elsewhere: HashSet<(Option<String>, String)> =
            [(Some("other".to_string()), "web".to_string())].into();
        assert!(is_prunable(&object, &elsewhere));
    }
}
