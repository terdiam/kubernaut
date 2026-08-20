//! Reading releases straight from the cluster.
//!
//! Helm keeps each release revision in a Secret of type `helm.sh/release.v1`,
//! so listing and inspecting releases needs no `helm` binary at all. That
//! matters for two reasons: the app still works when helm is missing, and every
//! release shows up — including ones installed by CI, Flux, Rancher or a
//! colleague's laptop, which is exactly the set people are surprised to lose
//! when a UI only tracks what it installed itself.

use std::{collections::HashMap, io::Read, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD};
use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, ResourceExt, api::ListParams};
use serde_json::Value;

use crate::model::{HelmError, Release, ReleaseDetail, ReleaseRevision, Result};

/// Label helm sets on every release secret.
const OWNER_SELECTOR: &str = "owner=helm";

/// Decode a release payload: Kubernetes base64, then helm's own base64, then
/// gzip, then JSON. The double encoding is helm's, not a mistake here.
fn decode(secret: &Secret) -> Result<Value> {
    let name = secret.name_any();
    let fail = |reason: String| HelmError::Decode {
        release: name.clone(),
        reason,
    };

    let raw = secret
        .data
        .as_ref()
        .and_then(|data| data.get("release"))
        .ok_or_else(|| fail("secret has no `release` key".into()))?;

    // `ByteString` already holds the Kubernetes-decoded bytes; helm's own
    // base64 layer is what remains.
    let unwrapped = STANDARD
        .decode(&raw.0)
        .map_err(|err| fail(format!("inner base64: {err}")))?;

    let mut decoder = flate2::read::GzDecoder::new(unwrapped.as_slice());
    let mut json = String::new();
    decoder
        .read_to_string(&mut json)
        .map_err(|err| fail(format!("gzip: {err}")))?;

    serde_json::from_str(&json).map_err(|err| fail(format!("json: {err}")))
}

fn release_from(value: &Value, pending_newer: bool) -> Option<Release> {
    let chart = value.pointer("/chart/metadata")?;
    Some(Release {
        name: value.get("name")?.as_str()?.to_string(),
        namespace: value
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        revision: value.get("version").and_then(Value::as_i64).unwrap_or(0),
        status: value
            .pointer("/info/status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        chart: chart
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        chart_version: chart
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        app_version: chart
            .get("appVersion")
            .and_then(Value::as_str)
            .map(String::from),
        updated: value
            .pointer("/info/last_deployed")
            .and_then(Value::as_str)
            .map(String::from),
        description: value
            .pointer("/info/description")
            .and_then(Value::as_str)
            .map(String::from),
        pending: pending_newer,
    })
}

/// Every release in the cluster, latest revision each.
///
/// One list call returns all revisions; the newest per release wins. Helm keeps
/// superseded revisions around for rollback, so listing without collapsing them
/// would show a release once per upgrade it has ever had.
pub async fn list(cluster: &Arc<ClusterHandle>, namespace: Option<&str>) -> Result<Vec<Release>> {
    let api: Api<Secret> = match namespace {
        Some(ns) => Api::namespaced(cluster.client.clone(), ns),
        None => Api::all(cluster.client.clone()),
    };

    let mut newest: HashMap<(String, String), (i64, Secret)> = HashMap::new();
    let mut params = ListParams::default().labels(OWNER_SELECTOR).limit(500);

    loop {
        let page = api.list(&params).await?;
        let next = page.metadata.continue_.clone().filter(|c| !c.is_empty());

        for secret in page.items {
            let labels = secret.labels();
            let Some(name) = labels.get("name").cloned() else {
                continue;
            };
            let version: i64 = labels
                .get("version")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let namespace = secret.namespace().unwrap_or_default();

            let key = (namespace, name);
            match newest.get(&key) {
                Some((known, _)) if *known >= version => {}
                _ => {
                    newest.insert(key, (version, secret));
                }
            }
        }

        match next {
            Some(token) => params = params.continue_token(&token),
            None => break,
        }
    }

    let mut releases: Vec<Release> = Vec::new();
    for ((_, _), (_, secret)) in newest {
        // A single unreadable release should not blank the whole list.
        match decode(&secret).ok().and_then(|value| {
            let status = value
                .pointer("/info/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            let pending = status.starts_with("pending");
            release_from(&value, pending)
        }) {
            Some(release) => releases.push(release),
            None => tracing::debug!(secret = %secret.name_any(), "unreadable helm release secret"),
        }
    }

    releases.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));
    Ok(releases)
}

/// Every stored revision of one release, newest first.
pub async fn history(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
    name: &str,
) -> Result<Vec<ReleaseRevision>> {
    let api: Api<Secret> = Api::namespaced(cluster.client.clone(), namespace);
    let params = ListParams::default().labels(&format!("{OWNER_SELECTOR},name={name}"));
    let list = api.list(&params).await?;

    let mut revisions: Vec<ReleaseRevision> = list
        .items
        .iter()
        .filter_map(|secret| {
            let value = decode(secret).ok()?;
            let chart = value.pointer("/chart/metadata");
            Some(ReleaseRevision {
                revision: value.get("version").and_then(Value::as_i64).unwrap_or(0),
                status: value
                    .pointer("/info/status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                chart_version: chart
                    .and_then(|c| c.get("version"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_version: chart
                    .and_then(|c| c.get("appVersion"))
                    .and_then(Value::as_str)
                    .map(String::from),
                updated: value
                    .pointer("/info/last_deployed")
                    .and_then(Value::as_str)
                    .map(String::from),
                description: value
                    .pointer("/info/description")
                    .and_then(Value::as_str)
                    .map(String::from),
            })
        })
        .collect();

    revisions.sort_by(|a, b| b.revision.cmp(&a.revision));
    Ok(revisions)
}

/// Full detail for one revision. `revision` of `None` means the latest.
pub async fn detail(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
    name: &str,
    revision: Option<i64>,
) -> Result<ReleaseDetail> {
    let api: Api<Secret> = Api::namespaced(cluster.client.clone(), namespace);
    let selector = match revision {
        Some(version) => format!("{OWNER_SELECTOR},name={name},version={version}"),
        None => format!("{OWNER_SELECTOR},name={name}"),
    };
    let list = api.list(&ListParams::default().labels(&selector)).await?;

    let secret = list
        .items
        .into_iter()
        .max_by_key(|secret| {
            secret
                .labels()
                .get("version")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0)
        })
        .ok_or_else(|| HelmError::other(format!("no release `{name}` in `{namespace}`")))?;

    let value = decode(&secret)?;
    let release = release_from(&value, false)
        .ok_or_else(|| HelmError::other("release payload is missing chart metadata"))?;

    let defaults = value.pointer("/chart/values").cloned();
    let overrides = value.get("config").cloned();

    let user_values = match &overrides {
        Some(config) if !is_empty_map(config) => to_yaml(config)?,
        _ => String::new(),
    };
    // Merging here rather than shelling out to `helm get values --all` keeps
    // this path binary-free, and the inputs are exactly what helm used.
    let effective = merge(
        defaults.unwrap_or(Value::Null),
        overrides.unwrap_or(Value::Null),
    );
    let effective_values = if is_empty_map(&effective) {
        String::new()
    } else {
        to_yaml(&effective)?
    };

    Ok(ReleaseDetail {
        release,
        user_values,
        effective_values,
        manifest: value
            .get("manifest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        notes: value
            .pointer("/info/notes")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn to_yaml(value: &Value) -> Result<String> {
    serde_yaml_ng::to_string(value).map_err(HelmError::other)
}

fn is_empty_map(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Deep-merge `overrides` onto `defaults`, the way helm coalesces values.
///
/// Maps merge key by key; anything else replaces wholesale. Notably a list is
/// replaced, never appended to — that is helm's rule, and expecting otherwise
/// is a common source of surprise when overriding an array of ports or hosts.
pub fn merge(defaults: Value, overrides: Value) -> Value {
    match (defaults, overrides) {
        (Value::Object(mut base), Value::Object(extra)) => {
            for (key, value) in extra {
                let merged = match base.remove(&key) {
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (base, Value::Null) => base,
        (_, extra) => extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_maps_merge_key_by_key() {
        let defaults = json!({"image": {"repo": "nginx", "tag": "1.0"}, "replicas": 1});
        let overrides = json!({"image": {"tag": "2.0"}});
        assert_eq!(
            merge(defaults, overrides),
            json!({"image": {"repo": "nginx", "tag": "2.0"}, "replicas": 1})
        );
    }

    /// Helm replaces lists rather than appending; a UI that implied otherwise
    /// would show values that never rendered.
    #[test]
    fn lists_are_replaced_not_appended() {
        let defaults = json!({"hosts": ["a", "b"]});
        let overrides = json!({"hosts": ["c"]});
        assert_eq!(merge(defaults, overrides), json!({"hosts": ["c"]}));
    }

    #[test]
    fn null_override_keeps_the_default() {
        let defaults = json!({"replicas": 3});
        assert_eq!(merge(defaults, Value::Null), json!({"replicas": 3}));
    }

    #[test]
    fn missing_keys_are_added() {
        let defaults = json!({"a": 1});
        let overrides = json!({"b": 2});
        assert_eq!(merge(defaults, overrides), json!({"a": 1, "b": 2}));
    }

    #[test]
    fn empty_maps_read_as_empty() {
        assert!(is_empty_map(&Value::Null));
        assert!(is_empty_map(&json!({})));
        assert!(!is_empty_map(&json!({"a": 1})));
    }
}
