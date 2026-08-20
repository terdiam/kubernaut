//! API discovery: what resources this cluster actually has.
//!
//! Everything downstream (tables, editors, watches) is driven off this cache
//! rather than a hardcoded list of kinds, so a CRD installed five minutes ago
//! behaves exactly like a built-in Deployment.

use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::jiff::Timestamp;
use kube::{
    Api, Client,
    api::ListParams,
    discovery::{ApiCapabilities, ApiResource, Discovery, Scope, verbs},
};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Stable identifier for a resource type, used as the key in every UI request.
///
/// Format: `group/version/plural` (`apps/v1/deployments`), with the core group
/// written as `core/v1/pods` so the string always has three segments.
pub fn resource_key(group: &str, version: &str, plural: &str) -> String {
    let group = if group.is_empty() { "core" } else { group };
    format!("{group}/{version}/{plural}")
}

/// One printer column, either from a CRD's `additionalPrinterColumns` or from
/// our built-in table for well-known kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDef {
    pub name: String,
    /// JSONPath relative to the object root, e.g. `.spec.replicas`.
    pub json_path: String,
    /// `string` | `integer` | `number` | `boolean` | `date`
    pub kind: String,
    /// Higher priority columns are hidden behind "show more" in the UI.
    #[serde(default)]
    pub priority: i32,
    pub description: Option<String>,
}

/// A resource type the cluster exposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDescriptor {
    /// See [`resource_key`].
    pub key: String,
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: String,
    pub api_version: String,
    pub namespaced: bool,
    pub verbs: Vec<String>,
    /// Short names from the apiserver (`po`, `deploy`) — powers the command palette.
    #[serde(default)]
    pub short_names: Vec<String>,
    /// True when this type comes from a CustomResourceDefinition.
    pub is_crd: bool,
    /// Columns declared by the CRD, if any.
    #[serde(default)]
    pub printer_columns: Vec<ColumnDef>,
    pub watchable: bool,
    pub editable: bool,
    pub deletable: bool,
}

impl ResourceDescriptor {
    /// Rebuild the `ApiResource` needed by the dynamic `Api` constructors.
    pub fn api_resource(&self) -> ApiResource {
        ApiResource {
            group: self.group.clone(),
            version: self.version.clone(),
            api_version: self.api_version.clone(),
            kind: self.kind.clone(),
            plural: self.plural.clone(),
        }
    }
}

/// A group in the sidebar tree (`apps`, `networking.k8s.io`, `core`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroup {
    pub name: String,
    /// Version discovery marked preferred; used when the user does not pin one.
    pub preferred_version: String,
    pub resources: Vec<ResourceDescriptor>,
}

/// Snapshot of one cluster's API surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCache {
    pub cluster: String,
    pub groups: Vec<ResourceGroup>,
    pub fetched_at: Timestamp,
    /// True when the CRD list could not be read (missing RBAC). Printer columns
    /// then fall back to built-ins, which is a degradation worth showing.
    pub crd_metadata_available: bool,
    #[serde(skip)]
    index: BTreeMap<String, ResourceDescriptor>,
}

impl DiscoveryCache {
    /// Run full discovery, then enrich CRD-backed types with printer columns.
    pub async fn run(cluster: &str, client: Client) -> Result<Self> {
        let discovery = Discovery::new(client.clone())
            .run()
            .await
            .map_err(|source| CoreError::discovery(cluster, source))?;

        // Printer columns live on the CRD object, not in discovery output, so
        // this is a second call. It is optional: plenty of users can list pods
        // but not CRDs.
        let crd_meta = crd_metadata(&client).await;

        let mut groups: Vec<ResourceGroup> = Vec::new();
        let mut index: BTreeMap<String, ResourceDescriptor> = BTreeMap::new();

        for group in discovery.groups() {
            let preferred = group.preferred_version_or_latest().to_string();
            let mut resources = Vec::new();

            for version in group.versions() {
                for (ar, caps) in group.versioned_resources(version) {
                    // Subresources (`pods/log`) are addressed through their
                    // parent, never listed on their own.
                    if ar.plural.contains('/') {
                        continue;
                    }
                    let descriptor = describe(&ar, &caps, &crd_meta);
                    index.insert(descriptor.key.clone(), descriptor.clone());
                    resources.push(descriptor);
                }
            }

            resources.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.version.cmp(&b.version)));
            groups.push(ResourceGroup {
                name: if group.name().is_empty() {
                    "core".to_string()
                } else {
                    group.name().to_string()
                },
                preferred_version: preferred,
                resources,
            });
        }

        groups.sort_by(|a, b| {
            // Core first, then alphabetical: matches how people scan the tree.
            let rank = |n: &str| if n == "core" { 0 } else { 1 };
            rank(&a.name)
                .cmp(&rank(&b.name))
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(Self {
            cluster: cluster.to_string(),
            groups,
            fetched_at: Timestamp::now(),
            crd_metadata_available: crd_meta.available,
            index,
        })
    }

    pub fn get(&self, key: &str) -> Option<&ResourceDescriptor> {
        self.index.get(key)
    }

    pub fn require(&self, key: &str) -> Result<&ResourceDescriptor> {
        self.get(key)
            .ok_or_else(|| CoreError::UnknownResource(key.to_string()))
    }

    /// Resolve a user-typed name (`po`, `pods`, `Pod`, `deploy.apps`) to a
    /// resource. Preferred versions win so `deployments` never resolves to a
    /// deprecated `apps/v1beta1`.
    pub fn resolve(&self, needle: &str) -> Option<&ResourceDescriptor> {
        let needle_lc = needle.to_ascii_lowercase();
        let (name, group_hint) = match needle_lc.split_once('.') {
            Some((n, g)) => (n.to_string(), Some(g.to_string())),
            None => (needle_lc, None),
        };

        let mut best: Option<&ResourceDescriptor> = None;
        for group in &self.groups {
            if let Some(hint) = &group_hint
                && &group.name != hint
            {
                continue;
            }
            for res in &group.resources {
                let matches = res.plural.eq_ignore_ascii_case(&name)
                    || res.kind.eq_ignore_ascii_case(&name)
                    || res
                        .short_names
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&name));
                if !matches {
                    continue;
                }
                let preferred = res.version == group.preferred_version;
                if preferred {
                    return Some(res);
                }
                best.get_or_insert(res);
            }
        }
        best
    }

    /// Exact match on `apiVersion` + `kind`, as a manifest names them.
    ///
    /// Deliberately not a fuzzy match: applying a document under a version the
    /// cluster does not serve fails server-side with a far worse message than
    /// "this cluster serves it as X".
    pub fn resolve_gvk(&self, api_version: &str, kind: &str) -> Option<&ResourceDescriptor> {
        self.index
            .values()
            .find(|res| res.api_version == api_version && res.kind == kind)
    }

    /// Versions of `kind` this cluster does serve, for the error when the
    /// document names one it does not.
    pub fn versions_of_kind(&self, kind: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .index
            .values()
            .filter(|res| res.kind == kind)
            .map(|res| res.api_version.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every watchable, listable resource — the default sidebar contents.
    pub fn listable(&self) -> impl Iterator<Item = &ResourceDescriptor> {
        self.index.values().filter(|r| r.watchable)
    }
}

fn describe(ar: &ApiResource, caps: &ApiCapabilities, crd: &CrdMetadata) -> ResourceDescriptor {
    let key = resource_key(&ar.group, &ar.version, &ar.plural);
    let columns = crd
        .columns
        .get(&(ar.group.clone(), ar.version.clone(), ar.kind.clone()))
        .cloned()
        .unwrap_or_default();

    ResourceDescriptor {
        key,
        group: ar.group.clone(),
        version: ar.version.clone(),
        kind: ar.kind.clone(),
        plural: ar.plural.clone(),
        api_version: ar.api_version.clone(),
        namespaced: matches!(caps.scope, Scope::Namespaced),
        watchable: caps.supports_operation(verbs::WATCH) && caps.supports_operation(verbs::LIST),
        editable: caps.supports_operation(verbs::PATCH) || caps.supports_operation(verbs::UPDATE),
        deletable: caps.supports_operation(verbs::DELETE),
        verbs: caps.operations.clone(),
        short_names: Vec::new(),
        // When the CRD list is readable we know exactly which groups are
        // custom; otherwise fall back to "not a built-in Kubernetes group".
        is_crd: if crd.available {
            crd.groups.contains(&ar.group)
        } else {
            !is_builtin_group(&ar.group)
        },
        printer_columns: columns,
    }
}

/// Groups shipped by Kubernetes itself. Anything else is CRD territory.
fn is_builtin_group(group: &str) -> bool {
    group.is_empty()
        || group == "apps"
        || group == "batch"
        || group == "policy"
        || group == "autoscaling"
        || group.ends_with("k8s.io")
}

/// What we can learn from the CustomResourceDefinition objects themselves:
/// which groups are custom, and their declared printer columns.
/// Key: (group, version, kind). Keying on (group, version) alone is wrong —
/// a single group routinely holds many kinds (`catalog.cattle.io` has both
/// `App` and `UIPlugin`), and they would overwrite each other's columns.
type CrdColumnKey = (String, String, String);

#[derive(Debug, Default)]
struct CrdMetadata {
    columns: BTreeMap<CrdColumnKey, Vec<ColumnDef>>,
    groups: BTreeSet<String>,
    /// False when the CRD list could not be read (usually missing RBAC).
    available: bool,
}

/// A missing/forbidden CRD list is not an error — we fall back to built-in
/// columns and tell the UI via `crd_metadata_available`.
async fn crd_metadata(client: &Client) -> CrdMetadata {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    let mut meta = CrdMetadata::default();
    let mut params = ListParams::default().limit(500);

    loop {
        let list = match api.list(&params).await {
            Ok(list) => list,
            Err(err) => {
                tracing::debug!(%err, "CRD listing unavailable; using built-in columns");
                return CrdMetadata::default();
            }
        };
        let next = list.metadata.continue_.clone().filter(|c| !c.is_empty());

        for crd in list.items {
            let group = crd.spec.group.clone();
            let kind = crd.spec.names.kind.clone();
            meta.groups.insert(group.clone());
            for version in crd.spec.versions {
                let cols: Vec<ColumnDef> = version
                    .additional_printer_columns
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| ColumnDef {
                        name: c.name,
                        json_path: c.json_path,
                        kind: c.type_,
                        priority: c.priority.unwrap_or(0),
                        description: c.description,
                    })
                    .collect();
                if !cols.is_empty() {
                    meta.columns
                        .insert((group.clone(), version.name, kind.clone()), cols);
                }
            }
        }

        match next {
            Some(token) => params = params.continue_token(&token),
            None => break,
        }
    }

    meta.available = true;
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::discovery::verbs;

    fn caps() -> ApiCapabilities {
        ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: Vec::new(),
            operations: vec![
                verbs::LIST.into(),
                verbs::WATCH.into(),
                verbs::PATCH.into(),
                verbs::DELETE.into(),
            ],
        }
    }

    fn api_resource(group: &str, version: &str, kind: &str, plural: &str) -> ApiResource {
        ApiResource {
            group: group.into(),
            version: version.into(),
            api_version: format!("{group}/{version}"),
            kind: kind.into(),
            plural: plural.into(),
        }
    }

    fn column(name: &str, path: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            json_path: path.into(),
            kind: "string".into(),
            priority: 0,
            description: None,
        }
    }

    /// Regression: two kinds in the same group/version must not share columns.
    /// `catalog.cattle.io` ships both `App` and `UIPlugin`, and keying the CRD
    /// column index on (group, version) alone made whichever was indexed last
    /// win for every kind in the group.
    #[test]
    fn crd_columns_are_keyed_per_kind() {
        let mut crd = CrdMetadata {
            available: true,
            ..CrdMetadata::default()
        };
        crd.groups.insert("catalog.cattle.io".into());
        crd.columns.insert(
            ("catalog.cattle.io".into(), "v1".into(), "App".into()),
            vec![column("Chart", ".spec.chart.metadata.name")],
        );
        crd.columns.insert(
            ("catalog.cattle.io".into(), "v1".into(), "UIPlugin".into()),
            vec![column("Plugin Name", ".spec.plugin.name")],
        );

        let app = describe(
            &api_resource("catalog.cattle.io", "v1", "App", "apps"),
            &caps(),
            &crd,
        );
        let plugin = describe(
            &api_resource("catalog.cattle.io", "v1", "UIPlugin", "uiplugins"),
            &caps(),
            &crd,
        );

        assert_eq!(
            app.printer_columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Chart"]
        );
        assert_eq!(
            plugin
                .printer_columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Plugin Name"]
        );
        assert!(app.is_crd && plugin.is_crd);
    }

    /// Built-in groups keep their own columns even when a CRD group is present.
    #[test]
    fn builtin_kinds_are_not_marked_as_crds() {
        let crd = CrdMetadata {
            available: true,
            ..CrdMetadata::default()
        };
        let deployment = describe(
            &api_resource("apps", "v1", "Deployment", "deployments"),
            &caps(),
            &crd,
        );
        assert!(!deployment.is_crd);
        assert!(deployment.watchable && deployment.editable && deployment.deletable);
    }

    #[test]
    fn resource_key_writes_core_group_explicitly() {
        assert_eq!(resource_key("", "v1", "pods"), "core/v1/pods");
        assert_eq!(
            resource_key("apps", "v1", "deployments"),
            "apps/v1/deployments"
        );
    }
}
