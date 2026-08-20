//! Running the checks over a cluster.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::{
    apps::v1::{DaemonSet, Deployment, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::Pod,
    rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding},
};
use kube::{Api, ResourceExt, api::ListParams};
use serde::Serialize;

use crate::{
    model::{Finding, Result, ScanReport},
    posture::{self, Subject},
    rbac,
};

/// Whether an object was created by a controller we already checked.
///
/// Checking every pod would report the same misconfiguration once per replica.
/// Reporting a Deployment's flaw three times because it runs three pods makes
/// the list longer without making it more useful.
fn is_controlled(pod: &Pod) -> bool {
    pod.metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| owners.iter().any(|owner| owner.controller == Some(true)))
}

fn workload_findings<K>(
    objects: Vec<K>,
    kind: &str,
    resource: &str,
    template: impl Fn(&K) -> Option<serde_json::Value>,
) -> Vec<Finding>
where
    K: kube::Resource<DynamicType = ()>,
{
    objects
        .iter()
        .flat_map(|object| {
            let name = object.name_any();
            let namespace = object.namespace();
            let Some(value) = template(object) else {
                return Vec::new();
            };
            let Some(spec) = posture::pod_spec_of(&value) else {
                return Vec::new();
            };
            posture::check_pod_spec(
                &Subject {
                    kind,
                    resource,
                    namespace: namespace.as_deref(),
                    name: &name,
                },
                &spec,
            )
        })
        .collect()
}

/// Posture findings across every workload in the cluster.
pub async fn posture_scan(
    cluster: &Arc<ClusterHandle>,
    namespace: Option<&str>,
) -> Result<ScanReport> {
    let client = cluster.client.clone();

    macro_rules! list {
        ($ty:ty) => {{
            let api: Api<$ty> = match namespace {
                Some(ns) => Api::namespaced(client.clone(), ns),
                None => Api::all(client.clone()),
            };
            api.list(&ListParams::default()).await
        }};
    }

    let (deployments, statefulsets, daemonsets, cronjobs, jobs, pods) = (
        list!(Deployment),
        list!(StatefulSet),
        list!(DaemonSet),
        list!(CronJob),
        list!(Job),
        list!(Pod),
    );

    let mut findings = Vec::new();
    let mut examined = 0usize;
    let mut limitations = Vec::new();

    let mut note = |what: &str, err: &kube::Error| {
        limitations.push(format!("could not list {what}: {err}"));
    };

    match deployments {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(workload_findings(
                list.items,
                "Deployment",
                "apps/v1/deployments",
                |object| serde_json::to_value(object).ok(),
            ));
        }
        Err(err) => note("deployments", &err),
    }
    match statefulsets {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(workload_findings(
                list.items,
                "StatefulSet",
                "apps/v1/statefulsets",
                |object| serde_json::to_value(object).ok(),
            ));
        }
        Err(err) => note("statefulsets", &err),
    }
    match daemonsets {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(workload_findings(
                list.items,
                "DaemonSet",
                "apps/v1/daemonsets",
                |object| serde_json::to_value(object).ok(),
            ));
        }
        Err(err) => note("daemonsets", &err),
    }
    match cronjobs {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(workload_findings(
                list.items,
                "CronJob",
                "batch/v1/cronjobs",
                |object| serde_json::to_value(object).ok(),
            ));
        }
        Err(err) => note("cronjobs", &err),
    }
    match jobs {
        Ok(list) => {
            // Jobs created by a CronJob repeat its template.
            let standalone: Vec<Job> = list
                .items
                .into_iter()
                .filter(|job| {
                    !job.metadata
                        .owner_references
                        .as_ref()
                        .is_some_and(|owners| owners.iter().any(|o| o.kind == "CronJob"))
                })
                .collect();
            examined += standalone.len();
            findings.extend(workload_findings(
                standalone,
                "Job",
                "batch/v1/jobs",
                |object| serde_json::to_value(object).ok(),
            ));
        }
        Err(err) => note("jobs", &err),
    }
    match pods {
        Ok(list) => {
            let standalone: Vec<Pod> = list
                .items
                .into_iter()
                .filter(|pod| !is_controlled(pod))
                .collect();
            examined += standalone.len();
            findings.extend(workload_findings(
                standalone,
                "Pod",
                "core/v1/pods",
                |object| serde_json::to_value(object).ok(),
            ));
        }
        Err(err) => note("pods", &err),
    }

    Ok(ScanReport::new(findings, examined, limitations))
}

/// RBAC findings across the cluster.
pub async fn rbac_scan(cluster: &Arc<ClusterHandle>) -> Result<ScanReport> {
    let client = cluster.client.clone();

    let cluster_roles: Api<ClusterRole> = Api::all(client.clone());
    let cluster_bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    let roles: Api<Role> = Api::all(client.clone());
    let bindings: Api<RoleBinding> = Api::all(client.clone());

    let params = ListParams::default();
    let (cluster_roles, cluster_bindings, roles, bindings) = tokio::join!(
        cluster_roles.list(&params),
        cluster_bindings.list(&params),
        roles.list(&params),
        bindings.list(&params),
    );

    let mut findings = Vec::new();
    let mut examined = 0usize;
    let mut limitations = Vec::new();

    match cluster_roles {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(list.iter().flat_map(rbac::check_cluster_role));
        }
        Err(err) => limitations.push(format!("could not list clusterroles: {err}")),
    }
    match cluster_bindings {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(list.iter().flat_map(rbac::check_cluster_role_binding));
        }
        Err(err) => limitations.push(format!("could not list clusterrolebindings: {err}")),
    }
    match roles {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(list.iter().flat_map(rbac::check_role));
        }
        Err(err) => limitations.push(format!("could not list roles: {err}")),
    }
    match bindings {
        Ok(list) => {
            examined += list.items.len();
            findings.extend(list.iter().flat_map(rbac::check_role_binding));
        }
        Err(err) => limitations.push(format!("could not list rolebindings: {err}")),
    }

    Ok(ScanReport::new(findings, examined, limitations))
}

/// An image running in the cluster, and where.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageUsage {
    pub image: String,
    /// `namespace/pod` for each place it runs, capped for display.
    pub used_by: Vec<String>,
    pub pod_count: usize,
}

/// Every distinct image running in the cluster.
///
/// Deduplicated by reference: scanning the same image once per pod would take
/// hundreds of scans to say one thing.
pub async fn cluster_images(
    cluster: &Arc<ClusterHandle>,
    namespace: Option<&str>,
) -> Result<Vec<ImageUsage>> {
    let api: Api<Pod> = match namespace {
        Some(ns) => Api::namespaced(cluster.client.clone(), ns),
        None => Api::all(cluster.client.clone()),
    };
    let list = api.list(&ListParams::default()).await?;

    let mut by_image: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for pod in list.iter() {
        let where_ = format!("{}/{}", pod.namespace().unwrap_or_default(), pod.name_any());
        let Some(spec) = &pod.spec else { continue };
        for container in spec
            .containers
            .iter()
            .chain(spec.init_containers.iter().flatten())
        {
            if let Some(image) = &container.image {
                by_image
                    .entry(image.clone())
                    .or_default()
                    .push(where_.clone());
            }
        }
    }

    let mut images: Vec<ImageUsage> = by_image
        .into_iter()
        .map(|(image, used_by)| ImageUsage {
            image,
            pod_count: used_by.len(),
            used_by: used_by.into_iter().take(10).collect(),
        })
        .collect();
    images.sort_by(|a, b| b.pod_count.cmp(&a.pod_count).then(a.image.cmp(&b.image)));
    Ok(images)
}

/// Run every check that needs no scanner.
pub async fn full_scan(
    cluster: &Arc<ClusterHandle>,
    namespace: Option<&str>,
) -> Result<ScanReport> {
    let (posture, rbac) = tokio::join!(posture_scan(cluster, namespace), rbac_scan(cluster));

    let mut findings = Vec::new();
    let mut examined = 0;
    let mut limitations = Vec::new();

    for report in [posture, rbac].into_iter().flatten() {
        findings.extend(report.findings);
        examined += report.examined;
        limitations.extend(report.limitations);
    }

    Ok(ScanReport::new(findings, examined, limitations))
}
