//! Editing a node's labels and taints.
//!
//! Labels steer scheduling through selectors and affinity; taints keep pods
//! off a node unless they tolerate it. Both are small, so they are edited as
//! lists rather than as YAML, and saved as a merge patch that carries the
//! `resourceVersion` the edit started from: if something else changed the node
//! meanwhile, the apiserver refuses rather than this overwriting its work.

use std::{collections::BTreeMap, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use kube::api::{Patch, PatchParams};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{OpsError, Result};

pub const EFFECTS: [&str; 3] = ["NoSchedule", "PreferNoSchedule", "NoExecute"];

/// A taint as Kubernetes keeps it. An empty value means none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Taint {
    pub key: String,
    pub value: String,
    pub effect: String,
}

impl Taint {
    /// `key=value:Effect`, as `kubectl taint` writes it.
    pub fn render(&self) -> String {
        if self.value.is_empty() {
            format!("{}:{}", self.key, self.effect)
        } else {
            format!("{}={}:{}", self.key, self.value, self.effect)
        }
    }
}

/// A node's labels and taints, and the version they were read at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeTags {
    pub labels: BTreeMap<String, String>,
    pub taints: Vec<Taint>,
    pub resource_version: String,
}

pub async fn read(cluster: &Arc<ClusterHandle>, node: &str) -> Result<NodeTags> {
    let object = k8s_core::objects::get(cluster, "core/v1/nodes", None, node).await?;
    let taints = object
        .data
        .pointer("/spec/taints")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|taint| {
                    Some(Taint {
                        key: taint.get("key")?.as_str()?.to_string(),
                        value: taint
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        effect: taint.get("effect")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(NodeTags {
        labels: object.metadata.labels.unwrap_or_default(),
        taints,
        resource_version: object.metadata.resource_version.unwrap_or_default(),
    })
}

/// What `Name` and value segments may contain.
fn is_name(text: &str, max: usize, allow_empty: bool) -> bool {
    if text.is_empty() {
        return allow_empty;
    }
    let edge = |c: char| c.is_ascii_alphanumeric();
    text.len() <= max
        && text.starts_with(edge)
        && text.ends_with(edge)
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A label or taint key: `name`, or `prefix/name` with a DNS-subdomain prefix.
pub fn check_key(key: &str) -> std::result::Result<(), String> {
    if key.is_empty() {
        return Err("the key is empty".into());
    }
    let (prefix, name) = key.split_once('/').map_or(("", key), |(p, n)| (p, n));
    if key.contains('/') {
        let dns = |p: &str| {
            !p.is_empty()
                && p.len() <= 253
                && p.split('.').all(|part| {
                    !part.is_empty()
                        && part.len() <= 63
                        && !part.starts_with('-')
                        && !part.ends_with('-')
                        && part
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                })
        };
        if !dns(prefix) {
            return Err(format!(
                "`{prefix}` is not a valid prefix: lowercase letters, digits, `-` and `.`"
            ));
        }
    }
    if !is_name(name, 63, false) {
        return Err(format!(
            "`{name}` is not a valid name: at most 63 letters, digits, `-`, `_` or `.`, starting and ending with a letter or digit"
        ));
    }
    Ok(())
}

pub fn check_value(value: &str) -> std::result::Result<(), String> {
    if is_name(value, 63, true) {
        Ok(())
    } else {
        Err(format!(
            "`{value}` is not a valid value: at most 63 letters, digits, `-`, `_` or `.`, starting and ending with a letter or digit"
        ))
    }
}

pub fn check_taint(taint: &Taint) -> std::result::Result<(), String> {
    check_key(&taint.key)?;
    check_value(&taint.value)?;
    if !EFFECTS.contains(&taint.effect.as_str()) {
        return Err(format!(
            "`{}` is not an effect: {}",
            taint.effect,
            EFFECTS.join(", ")
        ));
    }
    Ok(())
}

/// Reject a draft that the apiserver would, with a message that says which part.
pub fn check_draft(
    labels: &BTreeMap<String, String>,
    taints: &[Taint],
) -> std::result::Result<(), String> {
    for (key, value) in labels {
        check_key(key).map_err(|e| format!("label {key}: {e}"))?;
        check_value(value).map_err(|e| format!("label {key}: {e}"))?;
    }
    for (index, taint) in taints.iter().enumerate() {
        check_taint(taint).map_err(|e| format!("taint {}: {e}", taint.render()))?;
        if taints
            .iter()
            .take(index)
            .any(|other| other.key == taint.key && other.effect == taint.effect)
        {
            return Err(format!(
                "taint {}: another taint has the same key and effect",
                taint.render()
            ));
        }
    }
    Ok(())
}

/// What saving would change, one line each, for the confirmation.
pub fn describe(
    before: &NodeTags,
    labels: &BTreeMap<String, String>,
    taints: &[Taint],
) -> Vec<String> {
    let mut lines = Vec::new();
    for (key, value) in labels {
        match before.labels.get(key) {
            None => lines.push(format!("add label {key}={value}")),
            Some(old) if old != value => {
                lines.push(format!("change label {key}: {old} → {value}"));
            }
            Some(_) => {}
        }
    }
    for key in before.labels.keys().filter(|k| !labels.contains_key(*k)) {
        lines.push(format!("remove label {key}"));
    }
    for taint in taints.iter().filter(|t| !before.taints.contains(t)) {
        lines.push(format!("add taint {}", taint.render()));
    }
    for taint in before.taints.iter().filter(|t| !taints.contains(t)) {
        lines.push(format!("remove taint {}", taint.render()));
    }
    lines
}

/// Whether the draft adds a taint that evicts pods already running.
pub fn evicts(before: &NodeTags, taints: &[Taint]) -> bool {
    taints
        .iter()
        .any(|t| t.effect == "NoExecute" && !before.taints.contains(t))
}

/// The merge patch for the draft, or `None` when nothing changed.
pub fn patch(
    before: &NodeTags,
    labels: &BTreeMap<String, String>,
    taints: &[Taint],
) -> Option<Value> {
    let mut metadata = serde_json::Map::new();
    let mut label_patch = serde_json::Map::new();
    for (key, value) in labels {
        if before.labels.get(key) != Some(value) {
            label_patch.insert(key.clone(), json!(value));
        }
    }
    for key in before.labels.keys().filter(|k| !labels.contains_key(*k)) {
        label_patch.insert(key.clone(), Value::Null);
    }
    if !label_patch.is_empty() {
        metadata.insert("labels".into(), Value::Object(label_patch));
    }

    let mut patch = serde_json::Map::new();
    if before.taints != taints {
        // A merge patch replaces a list whole, which is why the version below
        // matters.
        let list: Vec<Value> = taints
            .iter()
            .map(|t| {
                let mut taint = json!({ "key": t.key, "effect": t.effect });
                if !t.value.is_empty() {
                    taint["value"] = json!(t.value);
                }
                taint
            })
            .collect();
        patch.insert(
            "spec".into(),
            json!({ "taints": if list.is_empty() { Value::Null } else { Value::Array(list) } }),
        );
    }
    if metadata.is_empty() && patch.is_empty() {
        return None;
    }
    if !before.resource_version.is_empty() {
        metadata.insert("resourceVersion".into(), json!(before.resource_version));
    }
    patch.insert("metadata".into(), Value::Object(metadata));
    Some(Value::Object(patch))
}

/// Save the draft. Does nothing when it equals what was read.
pub async fn apply(
    cluster: &Arc<ClusterHandle>,
    node: &str,
    before: &NodeTags,
    labels: &BTreeMap<String, String>,
    taints: &[Taint],
) -> Result<()> {
    check_draft(labels, taints).map_err(OpsError::Other)?;
    let Some(patch) = patch(before, labels, taints) else {
        return Ok(());
    };
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };
    let descriptor = discovery.require("core/v1/nodes")?;
    let api: kube::Api<kube::api::DynamicObject> =
        kube::Api::all_with(cluster.client.clone(), &descriptor.api_resource());
    match api
        .patch(node, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.code == 409 => Err(OpsError::other(
            "the node changed since it was read; reload it and make the change again",
        )),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{NodeTags, Taint, check_draft, check_key, check_value, describe, evicts, patch};

    fn taint(key: &str, value: &str, effect: &str) -> Taint {
        Taint {
            key: key.into(),
            value: value.into(),
            effect: effect.into(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn node() -> NodeTags {
        NodeTags {
            labels: labels(&[("tier", "web"), ("zone", "a")]),
            taints: vec![taint("dedicated", "gpu", "NoSchedule")],
            resource_version: "42".into(),
        }
    }

    #[test]
    fn keys_follow_the_kubernetes_rules() {
        assert!(check_key("env").is_ok());
        assert!(check_key("node-role.kubernetes.io/control-plane").is_ok());
        assert!(check_key("").is_err());
        assert!(check_key("-x").is_err());
        assert!(check_key("Bad Key").is_err());
        assert!(
            check_key("UPPER.example.com/x").is_err(),
            "prefix is lowercase"
        );
        assert!(check_key("a/b/c").is_err());
        assert!(check_key(&"a".repeat(64)).is_err());
    }

    #[test]
    fn values_may_be_empty_but_not_odd() {
        assert!(check_value("").is_ok());
        assert!(check_value("v1.2_3").is_ok());
        assert!(check_value("has space").is_err());
        assert!(check_value(".dot").is_err());
    }

    #[test]
    fn a_draft_with_a_bad_effect_or_duplicate_taint_is_refused() {
        let ok = labels(&[("a", "b")]);
        assert!(check_draft(&ok, &[taint("k", "", "NoSchedule")]).is_ok());
        assert!(check_draft(&ok, &[taint("k", "", "Nope")]).is_err());
        assert!(
            check_draft(
                &ok,
                &[taint("k", "1", "NoSchedule"), taint("k", "2", "NoSchedule")]
            )
            .is_err()
        );
        assert!(
            check_draft(
                &ok,
                &[taint("k", "1", "NoSchedule"), taint("k", "1", "NoExecute")]
            )
            .is_ok(),
            "the same key under another effect is a different taint"
        );
    }

    #[test]
    fn nothing_changed_means_no_patch() {
        let before = node();
        assert!(patch(&before, &before.labels, &before.taints).is_none());
        assert!(describe(&before, &before.labels, &before.taints).is_empty());
    }

    #[test]
    fn the_patch_carries_only_what_changed_and_the_version() {
        let before = node();
        let now = labels(&[("tier", "api"), ("env", "prod")]);
        let found = patch(&before, &now, &before.taints).unwrap();
        assert_eq!(
            found,
            json!({
                "metadata": {
                    "labels": {"tier": "api", "env": "prod", "zone": null},
                    "resourceVersion": "42"
                }
            })
        );
    }

    #[test]
    fn taints_are_replaced_whole_and_an_empty_list_clears_them() {
        let before = node();
        let found = patch(
            &before,
            &before.labels,
            &[
                taint("dedicated", "gpu", "NoSchedule"),
                taint("x", "", "NoExecute"),
            ],
        )
        .unwrap();
        assert_eq!(
            found["spec"]["taints"],
            json!([
                {"key": "dedicated", "effect": "NoSchedule", "value": "gpu"},
                {"key": "x", "effect": "NoExecute"}
            ])
        );
        let cleared = patch(&before, &before.labels, &[]).unwrap();
        assert!(cleared["spec"]["taints"].is_null());
    }

    #[test]
    fn the_description_names_each_change_and_flags_eviction() {
        let before = node();
        let now = labels(&[("tier", "api"), ("env", "prod")]);
        let taints = [taint("drain", "", "NoExecute")];
        let lines = describe(&before, &now, &taints);
        assert_eq!(
            lines,
            [
                "add label env=prod",
                "change label tier: web → api",
                "remove label zone",
                "add taint drain:NoExecute",
                "remove taint dedicated=gpu:NoSchedule",
            ]
        );
        assert!(evicts(&before, &taints));
        assert!(!evicts(&before, &before.taints));
    }
}
