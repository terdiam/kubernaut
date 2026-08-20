//! Options for form fields that reference other objects.
//!
//! A field like "image pull secret" or "ingress class" is a reference: typing
//! it means knowing what already exists, and a typo produces a resource that
//! applies cleanly and then does not work. The form asks the cluster instead.
//!
//! Every lookup is a plain list, scoped to a namespace where the kind is
//! namespaced. Failures are not fatal — a form that cannot reach the API
//! should still let the field be typed, so callers fall back to free text.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::{
    apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{ConfigMap, Node, PersistentVolumeClaim, Secret, Service, ServiceAccount},
    networking::v1::IngressClass,
    scheduling::v1::PriorityClass,
    storage::v1::StorageClass,
};
use kube::{Api, ResourceExt, api::ListParams};
use serde::{Deserialize, Serialize};

use crate::error::{OpsError, Result};

/// One choice in a form select.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LookupOption {
    /// What goes into the manifest.
    pub value: String,
    /// What the reader sees, when it differs from the value.
    pub label: String,
    /// One line of context — a type, a provisioner, a phase.
    pub detail: Option<String>,
}

impl LookupOption {
    fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            detail: None,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

/// Secret type that holds registry credentials — the only kind an
/// `imagePullSecrets` entry can usefully name.
const DOCKER_CONFIG: &str = "kubernetes.io/dockerconfigjson";

/// Marks the StorageClass used when a claim names none.
const DEFAULT_CLASS: &str = "storageclass.kubernetes.io/is-default-class";

fn params() -> ListParams {
    // Enough for a picker; a namespace with more than this needs typing, not
    // scrolling.
    ListParams::default().limit(500)
}

/// Options for one form field.
///
/// `param` carries the value a dependent lookup needs — the Service whose
/// ports are being listed, or the workload kind being scaled.
pub async fn lookup(
    cluster: &Arc<ClusterHandle>,
    source: &str,
    namespace: Option<&str>,
    param: Option<&str>,
) -> Result<Vec<LookupOption>> {
    let namespaced = |name: &str| -> Result<String> {
        namespace
            .map(str::to_string)
            .ok_or_else(|| OpsError::other(format!("{name} needs a namespace")))
    };

    let mut out: Vec<LookupOption> = match source {
        "secrets" | "dockerConfigSecrets" => {
            let api: Api<Secret> = Api::namespaced(cluster.client.clone(), &namespaced("secrets")?);
            let only_registry = source == "dockerConfigSecrets";
            api.list(&params())
                .await?
                .iter()
                .filter(|secret| !only_registry || secret.type_.as_deref() == Some(DOCKER_CONFIG))
                .map(|secret| {
                    LookupOption::new(secret.name_any())
                        .detail(secret.type_.clone().unwrap_or_default())
                })
                .collect()
        }

        "configMaps" => {
            let api: Api<ConfigMap> =
                Api::namespaced(cluster.client.clone(), &namespaced("configMaps")?);
            api.list(&params())
                .await?
                .iter()
                .map(|map| {
                    let keys = map.data.as_ref().map(|d| d.len()).unwrap_or(0);
                    LookupOption::new(map.name_any()).detail(format!("{keys} key(s)"))
                })
                .collect()
        }

        "serviceAccounts" => {
            let api: Api<ServiceAccount> =
                Api::namespaced(cluster.client.clone(), &namespaced("serviceAccounts")?);
            api.list(&params())
                .await?
                .iter()
                .map(|account| LookupOption::new(account.name_any()))
                .collect()
        }

        "persistentVolumeClaims" => {
            let api: Api<PersistentVolumeClaim> =
                Api::namespaced(cluster.client.clone(), &namespaced("claims")?);
            api.list(&params())
                .await?
                .iter()
                .map(|claim| {
                    let phase = claim
                        .status
                        .as_ref()
                        .and_then(|s| s.phase.clone())
                        .unwrap_or_default();
                    let size = claim
                        .spec
                        .as_ref()
                        .and_then(|s| s.resources.as_ref())
                        .and_then(|r| r.requests.as_ref())
                        .and_then(|r| r.get("storage"))
                        .map(|q| q.0.clone())
                        .unwrap_or_default();
                    LookupOption::new(claim.name_any())
                        .detail([phase, size].join(" ").trim().to_string())
                })
                .collect()
        }

        "services" => {
            let api: Api<Service> =
                Api::namespaced(cluster.client.clone(), &namespaced("services")?);
            api.list(&params())
                .await?
                .iter()
                .map(|service| {
                    let spec = service.spec.as_ref();
                    let kind = spec
                        .and_then(|s| s.type_.clone())
                        .unwrap_or_else(|| "ClusterIP".into());
                    let ports: Vec<String> = spec
                        .map(|s| {
                            s.ports
                                .iter()
                                .flatten()
                                .map(|p| p.port.to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    LookupOption::new(service.name_any())
                        .detail(format!("{kind} · {}", ports.join(", ")))
                })
                .collect()
        }

        // Dependent on the Service already chosen: an Ingress backend port has
        // to be one this Service actually exposes, or the route 503s.
        "servicePorts" => {
            let service =
                param.ok_or_else(|| OpsError::other("servicePorts needs a service name"))?;
            if service.is_empty() {
                return Ok(Vec::new());
            }
            let api: Api<Service> =
                Api::namespaced(cluster.client.clone(), &namespaced("services")?);
            let found = match api.get(service).await {
                Ok(found) => found,
                // Choosing a Service that is not created yet is legitimate;
                // there is simply nothing to offer for it.
                Err(kube::Error::Api(status)) if status.code == 404 => return Ok(Vec::new()),
                Err(err) => return Err(err.into()),
            };
            found
                .spec
                .iter()
                .flat_map(|spec| spec.ports.iter().flatten())
                .map(|port| {
                    // A named port survives renumbering, so prefer it as the
                    // value when the Service defines one.
                    let value = port.name.clone().unwrap_or_else(|| port.port.to_string());
                    let target = port
                        .target_port
                        .as_ref()
                        .map(|target| match target {
                            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(v) => {
                                v.to_string()
                            }
                            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(
                                v,
                            ) => v.clone(),
                        })
                        .unwrap_or_default();
                    LookupOption::new(value)
                        .label(match &port.name {
                            Some(name) => format!("{} · {name}", port.port),
                            None => port.port.to_string(),
                        })
                        .detail(if target.is_empty() {
                            String::new()
                        } else {
                            format!("→ {target}")
                        })
                })
                .collect()
        }

        "ingressClasses" => {
            let api: Api<IngressClass> = Api::all(cluster.client.clone());
            api.list(&params())
                .await?
                .iter()
                .map(|class| {
                    let controller = class
                        .spec
                        .as_ref()
                        .and_then(|s| s.controller.clone())
                        .unwrap_or_default();
                    LookupOption::new(class.name_any()).detail(controller)
                })
                .collect()
        }

        "storageClasses" => {
            let api: Api<StorageClass> = Api::all(cluster.client.clone());
            api.list(&params())
                .await?
                .iter()
                .map(|class| {
                    let default = class
                        .metadata
                        .annotations
                        .as_ref()
                        .and_then(|a| a.get(DEFAULT_CLASS))
                        .map(|value| value == "true")
                        .unwrap_or(false);
                    LookupOption::new(class.name_any()).detail(format!(
                        "{}{}",
                        class.provisioner,
                        if default { " · default" } else { "" }
                    ))
                })
                .collect()
        }

        "priorityClasses" => {
            let api: Api<PriorityClass> = Api::all(cluster.client.clone());
            api.list(&params())
                .await?
                .iter()
                .map(|class| {
                    LookupOption::new(class.name_any())
                        .detail(class.value.map(|v| v.to_string()).unwrap_or_default())
                })
                .collect()
        }

        "nodes" => {
            let api: Api<Node> = Api::all(cluster.client.clone());
            api.list(&params())
                .await?
                .iter()
                .map(|node| LookupOption::new(node.name_any()))
                .collect()
        }

        // Scale targets for an HPA, filtered to the kind already chosen.
        "workloads" => {
            let namespace = namespaced("workloads")?;
            let client = cluster.client.clone();
            match param.unwrap_or("Deployment") {
                "StatefulSet" => Api::<StatefulSet>::namespaced(client, &namespace)
                    .list(&params())
                    .await?
                    .iter()
                    .map(|item| LookupOption::new(item.name_any()))
                    .collect(),
                "DaemonSet" => Api::<DaemonSet>::namespaced(client, &namespace)
                    .list(&params())
                    .await?
                    .iter()
                    .map(|item| LookupOption::new(item.name_any()))
                    .collect(),
                "ReplicaSet" => Api::<ReplicaSet>::namespaced(client, &namespace)
                    .list(&params())
                    .await?
                    .iter()
                    .map(|item| LookupOption::new(item.name_any()))
                    .collect(),
                "Job" => Api::<Job>::namespaced(client, &namespace)
                    .list(&params())
                    .await?
                    .iter()
                    .map(|item| LookupOption::new(item.name_any()))
                    .collect(),
                "CronJob" => Api::<CronJob>::namespaced(client, &namespace)
                    .list(&params())
                    .await?
                    .iter()
                    .map(|item| LookupOption::new(item.name_any()))
                    .collect(),
                _ => Api::<Deployment>::namespaced(client, &namespace)
                    .list(&params())
                    .await?
                    .iter()
                    .map(|item| LookupOption::new(item.name_any()))
                    .collect(),
            }
        }

        other => return Err(OpsError::other(format!("unknown lookup `{other}`"))),
    };

    out.sort_by(|a, b| a.value.cmp(&b.value));
    out.dedup_by(|a, b| a.value == b.value);
    Ok(out)
}
