//! Namespace `ResourceQuota`/`LimitRange` usage: what Rancher's namespace view
//! shows and this app's per-namespace metrics (in `resolve.rs`) do not — those
//! are pod-usage samples, this is the API objects' own declared hard/used and
//! default limits.

use std::{collections::BTreeMap, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaResource {
    pub name: String,
    /// Quantity strings as the API reports them (`"4"`, `"8Gi"`), keyed by
    /// resource name (`cpu`, `memory`, `pods`, ...).
    pub hard: BTreeMap<String, String>,
    pub used: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRangeItem {
    /// "Container" | "Pod" | "PersistentVolumeClaim".
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub default: BTreeMap<String, String>,
    #[serde(default, rename = "defaultRequest")]
    pub default_request: BTreeMap<String, String>,
    #[serde(default)]
    pub max: BTreeMap<String, String>,
    #[serde(default)]
    pub min: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceQuotaInfo {
    pub namespace: String,
    pub quotas: Vec<QuotaResource>,
    pub limit_ranges: Vec<LimitRangeItem>,
}

pub async fn namespace_quota(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
) -> Result<NamespaceQuotaInfo, String> {
    let quota_objs = k8s_core::objects::list(cluster, "core/v1/resourcequotas", Some(namespace))
        .await
        .map_err(|err| err.to_string())?;
    let limit_objs = k8s_core::objects::list(cluster, "core/v1/limitranges", Some(namespace))
        .await
        .map_err(|err| err.to_string())?;

    let quotas = quota_objs
        .into_iter()
        .map(|obj| QuotaResource {
            name: obj.metadata.name.clone().unwrap_or_default(),
            hard: obj
                .data
                .pointer("/status/hard")
                .and_then(quantity_map)
                .unwrap_or_default(),
            used: obj
                .data
                .pointer("/status/used")
                .and_then(quantity_map)
                .unwrap_or_default(),
        })
        .collect();

    let limit_ranges = limit_objs
        .into_iter()
        .flat_map(|obj| {
            obj.data
                .pointer("/spec/limits")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|item| serde_json::from_value::<LimitRangeItem>(item).ok())
        })
        .collect();

    Ok(NamespaceQuotaInfo {
        namespace: namespace.to_string(),
        quotas,
        limit_ranges,
    })
}

/// A JSON object of quantity strings, dropping any entry that isn't one.
fn quantity_map(value: &Value) -> Option<BTreeMap<String, String>> {
    Some(
        value
            .as_object()?
            .iter()
            .filter_map(|(key, val)| val.as_str().map(|s| (key.clone(), s.to_string())))
            .collect(),
    )
}
