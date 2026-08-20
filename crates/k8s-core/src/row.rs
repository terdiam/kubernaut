//! Table projection: turn an arbitrary object into the row the UI renders.
//!
//! Rows are computed in Rust rather than fetched via the server-side Table API
//! because watch deltas arrive as objects, not table rows — projecting locally
//! keeps a streamed update and an initial list on exactly the same code path.

use std::collections::BTreeMap;

use k8s_openapi::jiff::Timestamp;
use kube::api::DynamicObject;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    discovery::{ColumnDef, ResourceDescriptor},
    jsonpath::{self, JsonPath},
};

/// Coarse health used to colour a row. Deliberately small: the detail pane
/// explains *why*, the table only needs to draw attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowHealth {
    Ok,
    Pending,
    Warning,
    Error,
    Unknown,
}

/// Column metadata sent to the UI once per resource type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSpec {
    pub name: String,
    /// `string` | `integer` | `number` | `boolean` | `date`
    pub kind: String,
    /// Columns above 0 are hidden until the user asks for more.
    pub priority: i32,
    pub description: Option<String>,
}

/// One table row. `cells` is positionally aligned with [`TableSpec::columns`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub uid: String,
    pub name: String,
    pub namespace: Option<String>,
    /// RFC3339; the UI renders age so it stays live without re-sending rows.
    pub created: Option<String>,
    pub resource_version: Option<String>,
    pub cells: Vec<String>,
    pub health: RowHealth,
    /// Set while the object has a deletionTimestamp — usually stuck finalizers.
    pub terminating: bool,
}

/// Columns for one resource type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSpec {
    pub columns: Vec<ColumnSpec>,
    pub namespaced: bool,
}

enum CellSource {
    Path(JsonPath),
    /// Values kubectl computes rather than reads (`READY` as `2/3`).
    Computed(fn(&Value) -> String),
    /// Path that failed to parse — rendered blank instead of failing the row.
    Broken,
}

/// Compiled column set for one resource type. Build once, reuse per row.
pub struct RowProjector {
    columns: Vec<ColumnSpec>,
    sources: Vec<CellSource>,
    namespaced: bool,
    health: fn(&Value) -> RowHealth,
}

impl RowProjector {
    /// CRD printer columns win when present (the author knows their type best);
    /// otherwise fall back to our built-in table, then to name/age only.
    pub fn for_resource(desc: &ResourceDescriptor) -> Self {
        let defs: Vec<ColumnDef> = if !desc.printer_columns.is_empty() {
            desc.printer_columns.clone()
        } else {
            builtin_columns(&desc.group, &desc.kind)
        };

        let mut columns = Vec::with_capacity(defs.len());
        let mut sources = Vec::with_capacity(defs.len());

        for def in defs {
            let source = if let Some(computed) = computed_cell(&desc.kind, &def.json_path) {
                CellSource::Computed(computed)
            } else {
                match JsonPath::parse(&def.json_path) {
                    Ok(path) => CellSource::Path(path),
                    Err(err) => {
                        tracing::warn!(kind = %desc.kind, column = %def.name, %err, "unusable printer column");
                        CellSource::Broken
                    }
                }
            };
            columns.push(ColumnSpec {
                name: def.name,
                kind: def.kind,
                priority: def.priority,
                description: def.description,
            });
            sources.push(source);
        }

        Self {
            columns,
            sources,
            namespaced: desc.namespaced,
            health: health_fn(&desc.kind),
        }
    }

    pub fn spec(&self) -> TableSpec {
        TableSpec {
            columns: self.columns.clone(),
            namespaced: self.namespaced,
        }
    }

    pub fn project(&self, obj: &DynamicObject) -> Row {
        // DynamicObject keeps typed metadata plus untyped `data`; the merged
        // view is what printer-column paths (`.spec.x`, `.metadata.y`) expect.
        let value = merged_value(obj);
        let meta = &obj.metadata;

        let cells = self
            .sources
            .iter()
            .map(|source| match source {
                CellSource::Path(path) => path.eval_display(&value).unwrap_or_default(),
                CellSource::Computed(f) => f(&value),
                CellSource::Broken => String::new(),
            })
            .collect();

        Row {
            uid: meta.uid.clone().unwrap_or_else(|| {
                // Uid is absent only for objects served from a cache without
                // it; fall back to the natural key so rows stay addressable.
                format!(
                    "{}/{}",
                    meta.namespace.as_deref().unwrap_or(""),
                    meta.name.as_deref().unwrap_or("")
                )
            }),
            name: meta.name.clone().unwrap_or_default(),
            namespace: meta.namespace.clone(),
            created: meta.creation_timestamp.as_ref().map(|t| t.0.to_string()),
            resource_version: meta.resource_version.clone(),
            cells,
            health: (self.health)(&value),
            terminating: meta.deletion_timestamp.is_some(),
        }
    }
}

/// Rebuild the full object JSON from `DynamicObject`'s split representation.
fn merged_value(obj: &DynamicObject) -> Value {
    let mut value = obj.data.clone();
    if let Ok(meta) = serde_json::to_value(&obj.metadata)
        && let Value::Object(map) = &mut value
    {
        map.insert("metadata".to_string(), meta);
    }
    value
}

/// Age from a creationTimestamp, formatted like kubectl (`5d`, `3h12m`, `47s`).
pub fn humanize_age(created: Timestamp, now: Timestamp) -> String {
    // jiff subtraction yields a calendar Span; convert to a plain duration.
    let secs = now.duration_since(created).as_secs().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{}s", s / 60, s % 60),
        s if s < 86_400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        s if s < 86_400 * 100 => format!("{}d{}h", s / 86_400, (s % 86_400) / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn col(name: &str, path: &str, kind: &str) -> ColumnDef {
    ColumnDef {
        name: name.to_string(),
        json_path: path.to_string(),
        kind: kind.to_string(),
        priority: 0,
        description: None,
    }
}

/// Marker paths resolved by [`computed_cell`] instead of JSONPath.
const COMPUTED: &str = "@computed:";

fn cc(name: &str, tag: &str, kind: &str) -> ColumnDef {
    col(name, &format!("{COMPUTED}{tag}"), kind)
}

/// kubectl-equivalent columns for the kinds people look at every day.
fn builtin_columns(group: &str, kind: &str) -> Vec<ColumnDef> {
    match (group, kind) {
        ("", "Pod") => vec![
            cc("Ready", "pod_ready", "string"),
            cc("Status", "pod_status", "string"),
            cc("Restarts", "pod_restarts", "integer"),
            col("Node", ".spec.nodeName", "string"),
            col("IP", ".status.podIP", "string"),
        ],
        ("", "Node") => vec![
            cc("Status", "node_status", "string"),
            cc("Roles", "node_roles", "string"),
            col("Version", ".status.nodeInfo.kubeletVersion", "string"),
            col(
                "Internal IP",
                ".status.addresses[?(@.type==\"InternalIP\")].address",
                "string",
            ),
            col("OS Image", ".status.nodeInfo.osImage", "string"),
        ],
        ("", "Service") => vec![
            col("Type", ".spec.type", "string"),
            col("Cluster IP", ".spec.clusterIP", "string"),
            cc("External IP", "service_external_ip", "string"),
            cc("Ports", "service_ports", "string"),
        ],
        ("", "Namespace") => vec![col("Status", ".status.phase", "string")],
        ("", "PersistentVolumeClaim") => vec![
            col("Status", ".status.phase", "string"),
            col("Volume", ".spec.volumeName", "string"),
            col("Capacity", ".status.capacity.storage", "string"),
            cc("Access Modes", "access_modes", "string"),
            col("Storage Class", ".spec.storageClassName", "string"),
        ],
        ("", "PersistentVolume") => vec![
            col("Capacity", ".spec.capacity.storage", "string"),
            cc("Access Modes", "access_modes", "string"),
            col(
                "Reclaim Policy",
                ".spec.persistentVolumeReclaimPolicy",
                "string",
            ),
            col("Status", ".status.phase", "string"),
            col("Storage Class", ".spec.storageClassName", "string"),
        ],
        ("", "ConfigMap") | ("", "Secret") => vec![
            cc("Data", "data_count", "integer"),
            col("Type", ".type", "string"),
        ],
        ("", "ServiceAccount") => vec![cc("Secrets", "secrets_count", "integer")],
        ("", "Event") => vec![
            col("Type", ".type", "string"),
            col("Reason", ".reason", "string"),
            cc("Object", "event_object", "string"),
            col("Count", ".count", "integer"),
            col("Message", ".message", "string"),
        ],
        ("apps", "Deployment") | ("apps", "StatefulSet") => vec![
            cc("Ready", "workload_ready", "string"),
            col("Up-to-date", ".status.updatedReplicas", "integer"),
            col("Available", ".status.availableReplicas", "integer"),
            cc("Images", "images", "string"),
        ],
        ("apps", "ReplicaSet") => vec![
            col("Desired", ".spec.replicas", "integer"),
            col("Current", ".status.replicas", "integer"),
            col("Ready", ".status.readyReplicas", "integer"),
            cc("Images", "images", "string"),
        ],
        ("apps", "DaemonSet") => vec![
            col("Desired", ".status.desiredNumberScheduled", "integer"),
            col("Current", ".status.currentNumberScheduled", "integer"),
            col("Ready", ".status.numberReady", "integer"),
            col("Up-to-date", ".status.updatedNumberScheduled", "integer"),
            col("Available", ".status.numberAvailable", "integer"),
        ],
        ("batch", "Job") => vec![
            cc("Completions", "job_completions", "string"),
            col("Successful", ".status.succeeded", "integer"),
            col("Failed", ".status.failed", "integer"),
        ],
        ("batch", "CronJob") => vec![
            col("Schedule", ".spec.schedule", "string"),
            col("Suspend", ".spec.suspend", "boolean"),
            cc("Active", "cronjob_active", "integer"),
            col("Last Schedule", ".status.lastScheduleTime", "date"),
        ],
        ("networking.k8s.io", "Ingress") => vec![
            col("Class", ".spec.ingressClassName", "string"),
            cc("Hosts", "ingress_hosts", "string"),
            cc("Address", "ingress_address", "string"),
        ],
        ("autoscaling", "HorizontalPodAutoscaler") => vec![
            cc("Reference", "hpa_reference", "string"),
            col("Min", ".spec.minReplicas", "integer"),
            col("Max", ".spec.maxReplicas", "integer"),
            col("Replicas", ".status.currentReplicas", "integer"),
        ],
        ("rbac.authorization.k8s.io", "ClusterRoleBinding")
        | ("rbac.authorization.k8s.io", "RoleBinding") => vec![
            col("Role", ".roleRef.name", "string"),
            cc("Subjects", "subjects", "string"),
        ],
        // Unknown type with no printer columns: name/namespace/age (rendered by
        // the UI from the Row fields) is all we can honestly show.
        _ => Vec::new(),
    }
}

fn computed_cell(_kind: &str, path: &str) -> Option<fn(&Value) -> String> {
    let tag = path.strip_prefix(COMPUTED)?;
    Some(match tag {
        "pod_ready" => pod_ready,
        "pod_status" => pod_status,
        "pod_restarts" => pod_restarts,
        "node_status" => node_status,
        "node_roles" => node_roles,
        "service_external_ip" => service_external_ip,
        "service_ports" => service_ports,
        "access_modes" => access_modes,
        "data_count" => data_count,
        "secrets_count" => secrets_count,
        "event_object" => event_object,
        "workload_ready" => workload_ready,
        "images" => images,
        "job_completions" => job_completions,
        "cronjob_active" => cronjob_active,
        "ingress_hosts" => ingress_hosts,
        "ingress_address" => ingress_address,
        "hpa_reference" => hpa_reference,
        "subjects" => subjects,
        _ => return None,
    })
}

fn arr<'a>(v: &'a Value, path: &[&str]) -> &'a [Value] {
    let mut cur = v;
    for key in path {
        match cur.get(*key) {
            Some(next) => cur = next,
            None => return &[],
        }
    }
    cur.as_array().map(|a| a.as_slice()).unwrap_or(&[])
}

fn num(v: &Value, path: &[&str]) -> i64 {
    let mut cur = v;
    for key in path {
        match cur.get(*key) {
            Some(next) => cur = next,
            None => return 0,
        }
    }
    cur.as_i64().unwrap_or(0)
}

fn pod_ready(v: &Value) -> String {
    let statuses = arr(v, &["status", "containerStatuses"]);
    let ready = statuses
        .iter()
        .filter(|c| c.get("ready").and_then(Value::as_bool).unwrap_or(false))
        .count();
    format!(
        "{ready}/{}",
        statuses.len().max(arr(v, &["spec", "containers"]).len())
    )
}

/// Mirrors kubectl's pod status column: waiting/terminated container reasons
/// beat the phase, because "CrashLoopBackOff" is the useful word, not "Running".
fn pod_status(v: &Value) -> String {
    if v.get("metadata")
        .and_then(|m| m.get("deletionTimestamp"))
        .is_some()
    {
        return "Terminating".to_string();
    }
    for status in arr(v, &["status", "containerStatuses"])
        .iter()
        .chain(arr(v, &["status", "initContainerStatuses"]).iter())
    {
        let state = status.get("state");
        if let Some(reason) = state
            .and_then(|s| s.get("waiting"))
            .and_then(|w| w.get("reason"))
            .and_then(Value::as_str)
        {
            return reason.to_string();
        }
        if let Some(term) = state.and_then(|s| s.get("terminated")) {
            let exit = term.get("exitCode").and_then(Value::as_i64).unwrap_or(0);
            if exit != 0 {
                return term
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Error")
                    .to_string();
            }
        }
    }
    v.get("status")
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string()
}

fn pod_restarts(v: &Value) -> String {
    arr(v, &["status", "containerStatuses"])
        .iter()
        .map(|c| c.get("restartCount").and_then(Value::as_i64).unwrap_or(0))
        .sum::<i64>()
        .to_string()
}

fn node_status(v: &Value) -> String {
    let ready = arr(v, &["status", "conditions"])
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some("Ready"))
        .and_then(|c| c.get("status").and_then(Value::as_str));
    let base = match ready {
        Some("True") => "Ready",
        Some("False") => "NotReady",
        _ => "Unknown",
    };
    let unschedulable = v
        .get("spec")
        .and_then(|s| s.get("unschedulable"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if unschedulable {
        format!("{base},SchedulingDisabled")
    } else {
        base.to_string()
    }
}

fn node_roles(v: &Value) -> String {
    let labels = v
        .get("metadata")
        .and_then(|m| m.get("labels"))
        .and_then(Value::as_object);
    let Some(labels) = labels else {
        return "<none>".to_string();
    };
    let mut roles: Vec<&str> = labels
        .keys()
        .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
        .filter(|r| !r.is_empty())
        .collect();
    roles.sort_unstable();
    if roles.is_empty() {
        "<none>".to_string()
    } else {
        roles.join(",")
    }
}

fn service_external_ip(v: &Value) -> String {
    let mut out: Vec<String> = arr(v, &["status", "loadBalancer", "ingress"])
        .iter()
        .filter_map(|i| {
            i.get("ip")
                .or_else(|| i.get("hostname"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    out.extend(
        arr(v, &["spec", "externalIPs"])
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string)),
    );
    if out.is_empty() {
        "<none>".to_string()
    } else {
        out.join(",")
    }
}

fn service_ports(v: &Value) -> String {
    arr(v, &["spec", "ports"])
        .iter()
        .map(|p| {
            let port = p.get("port").and_then(Value::as_i64).unwrap_or(0);
            let proto = p.get("protocol").and_then(Value::as_str).unwrap_or("TCP");
            match p.get("nodePort").and_then(Value::as_i64) {
                Some(node_port) => format!("{port}:{node_port}/{proto}"),
                None => format!("{port}/{proto}"),
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn access_modes(v: &Value) -> String {
    arr(v, &["spec", "accessModes"])
        .iter()
        .filter_map(Value::as_str)
        .map(|m| match m {
            "ReadWriteOnce" => "RWO",
            "ReadOnlyMany" => "ROX",
            "ReadWriteMany" => "RWX",
            "ReadWriteOncePod" => "RWOP",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn data_count(v: &Value) -> String {
    let data = v
        .get("data")
        .and_then(Value::as_object)
        .map_or(0, |m| m.len());
    let binary = v
        .get("binaryData")
        .and_then(Value::as_object)
        .map_or(0, |m| m.len());
    (data + binary).to_string()
}

fn secrets_count(v: &Value) -> String {
    arr(v, &["secrets"]).len().to_string()
}

fn event_object(v: &Value) -> String {
    let obj = v.get("involvedObject");
    let kind = obj
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = obj
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("{kind}/{name}")
}

fn workload_ready(v: &Value) -> String {
    let ready = num(v, &["status", "readyReplicas"]);
    let desired = v
        .get("spec")
        .and_then(|s| s.get("replicas"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| num(v, &["status", "replicas"]));
    format!("{ready}/{desired}")
}

fn images(v: &Value) -> String {
    arr(v, &["spec", "template", "spec", "containers"])
        .iter()
        .filter_map(|c| c.get("image").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(",")
}

fn job_completions(v: &Value) -> String {
    let succeeded = num(v, &["status", "succeeded"]);
    match v
        .get("spec")
        .and_then(|s| s.get("completions"))
        .and_then(Value::as_i64)
    {
        Some(total) => format!("{succeeded}/{total}"),
        None => format!("{succeeded}/1"),
    }
}

fn cronjob_active(v: &Value) -> String {
    arr(v, &["status", "active"]).len().to_string()
}

fn ingress_hosts(v: &Value) -> String {
    let hosts: Vec<&str> = arr(v, &["spec", "rules"])
        .iter()
        .filter_map(|r| r.get("host").and_then(Value::as_str))
        .collect();
    if hosts.is_empty() {
        "*".to_string()
    } else {
        hosts.join(",")
    }
}

fn ingress_address(v: &Value) -> String {
    arr(v, &["status", "loadBalancer", "ingress"])
        .iter()
        .filter_map(|i| {
            i.get("ip")
                .or_else(|| i.get("hostname"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn hpa_reference(v: &Value) -> String {
    let target = v.get("spec").and_then(|s| s.get("scaleTargetRef"));
    let kind = target
        .and_then(|t| t.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = target
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("{kind}/{name}")
}

fn subjects(v: &Value) -> String {
    arr(v, &["subjects"])
        .iter()
        .filter_map(|s| {
            let kind = s.get("kind").and_then(Value::as_str)?;
            let name = s.get("name").and_then(Value::as_str)?;
            Some(format!("{kind}/{name}"))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn health_fn(kind: &str) -> fn(&Value) -> RowHealth {
    match kind {
        "Pod" => pod_health,
        "Node" => node_health,
        "Deployment" | "StatefulSet" | "ReplicaSet" => workload_health,
        "Job" => job_health,
        "PersistentVolumeClaim" | "PersistentVolume" | "Namespace" => phase_health,
        "Event" => event_health,
        _ => condition_health,
    }
}

fn pod_health(v: &Value) -> RowHealth {
    if v.get("metadata")
        .and_then(|m| m.get("deletionTimestamp"))
        .is_some()
    {
        return RowHealth::Warning;
    }
    match pod_status(v).as_str() {
        "Running" | "Succeeded" | "Completed" => {
            let statuses = arr(v, &["status", "containerStatuses"]);
            let all_ready = !statuses.is_empty()
                && statuses
                    .iter()
                    .all(|c| c.get("ready").and_then(Value::as_bool).unwrap_or(false));
            if all_ready || pod_status(v) != "Running" {
                RowHealth::Ok
            } else {
                RowHealth::Pending
            }
        }
        "Pending" | "ContainerCreating" | "PodInitializing" | "Init" => RowHealth::Pending,
        "Terminating" => RowHealth::Warning,
        _ => RowHealth::Error,
    }
}

fn node_health(v: &Value) -> RowHealth {
    match node_status(v).as_str() {
        "Ready" => RowHealth::Ok,
        s if s.starts_with("Ready,") => RowHealth::Warning,
        "Unknown" => RowHealth::Unknown,
        _ => RowHealth::Error,
    }
}

fn workload_health(v: &Value) -> RowHealth {
    let desired = v
        .get("spec")
        .and_then(|s| s.get("replicas"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let ready = num(v, &["status", "readyReplicas"]);
    if desired == 0 {
        RowHealth::Unknown
    } else if ready >= desired {
        RowHealth::Ok
    } else if ready == 0 {
        RowHealth::Error
    } else {
        RowHealth::Pending
    }
}

fn job_health(v: &Value) -> RowHealth {
    if num(v, &["status", "failed"]) > 0 {
        RowHealth::Error
    } else if num(v, &["status", "succeeded"]) > 0 {
        RowHealth::Ok
    } else if num(v, &["status", "active"]) > 0 {
        RowHealth::Pending
    } else {
        RowHealth::Unknown
    }
}

fn phase_health(v: &Value) -> RowHealth {
    match v
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
    {
        Some("Bound") | Some("Active") | Some("Available") | Some("Succeeded") => RowHealth::Ok,
        Some("Pending") | Some("Released") => RowHealth::Pending,
        Some("Failed") | Some("Terminating") => RowHealth::Error,
        _ => RowHealth::Unknown,
    }
}

fn event_health(v: &Value) -> RowHealth {
    match v.get("type").and_then(Value::as_str) {
        Some("Normal") => RowHealth::Ok,
        Some("Warning") => RowHealth::Warning,
        _ => RowHealth::Unknown,
    }
}

/// Generic fallback: honour `Ready`/`Available` conditions, which is the
/// convention nearly every operator follows for its own CRDs.
fn condition_health(v: &Value) -> RowHealth {
    let conditions = arr(v, &["status", "conditions"]);
    if conditions.is_empty() {
        return RowHealth::Unknown;
    }
    let mut result = RowHealth::Unknown;
    for cond in conditions {
        let ty = cond.get("type").and_then(Value::as_str).unwrap_or("");
        let status = cond.get("status").and_then(Value::as_str).unwrap_or("");
        match (ty, status) {
            ("Ready" | "Available" | "Succeeded", "True") => result = RowHealth::Ok,
            ("Ready" | "Available", "False") => return RowHealth::Error,
            ("Degraded" | "Failed", "True") => return RowHealth::Error,
            _ => {}
        }
    }
    result
}

/// Label/field pairs shown in the detail header. Kept here so the table and
/// detail pane agree on formatting.
pub fn summary_fields(obj: &DynamicObject) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(ns) = &obj.metadata.namespace {
        out.insert("namespace".into(), ns.clone());
    }
    if let Some(name) = &obj.metadata.name {
        out.insert("name".into(), name.clone());
    }
    if let Some(ts) = &obj.metadata.creation_timestamp {
        out.insert("created".into(), ts.0.to_string());
        out.insert("age".into(), humanize_age(ts.0, Timestamp::now()));
    }
    out
}

/// Render any JSON value the way a table cell should show it.
pub fn cell_text(value: &Value) -> String {
    jsonpath::render(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pod_status_prefers_waiting_reason_over_phase() {
        let pod = json!({
            "status": {
                "phase": "Running",
                "containerStatuses": [
                    {"ready": false, "restartCount": 5,
                     "state": {"waiting": {"reason": "CrashLoopBackOff"}}}
                ]
            }
        });
        assert_eq!(pod_status(&pod), "CrashLoopBackOff");
        assert_eq!(pod_restarts(&pod), "5");
        assert_eq!(pod_health(&pod), RowHealth::Error);
    }

    #[test]
    fn pod_ready_counts_ready_containers() {
        let pod = json!({
            "spec": {"containers": [{}, {}]},
            "status": {"phase": "Running", "containerStatuses": [
                {"ready": true}, {"ready": false}
            ]}
        });
        assert_eq!(pod_ready(&pod), "1/2");
        assert_eq!(pod_health(&pod), RowHealth::Pending);
    }

    #[test]
    fn terminating_pod_is_flagged() {
        let pod = json!({"metadata": {"deletionTimestamp": "2026-01-01T00:00:00Z"},
                         "status": {"phase": "Running"}});
        assert_eq!(pod_status(&pod), "Terminating");
        assert_eq!(pod_health(&pod), RowHealth::Warning);
    }

    #[test]
    fn cordoned_node_reads_as_scheduling_disabled() {
        let node = json!({
            "spec": {"unschedulable": true},
            "status": {"conditions": [{"type": "Ready", "status": "True"}]}
        });
        assert_eq!(node_status(&node), "Ready,SchedulingDisabled");
        assert_eq!(node_health(&node), RowHealth::Warning);
    }

    #[test]
    fn service_ports_include_node_port() {
        let svc = json!({"spec": {"ports": [
            {"port": 80, "protocol": "TCP", "nodePort": 30080},
            {"port": 443, "protocol": "TCP"}
        ]}});
        assert_eq!(service_ports(&svc), "80:30080/TCP,443/TCP");
    }

    #[test]
    fn crd_conditions_drive_generic_health() {
        let obj = json!({"status": {"conditions": [{"type": "Ready", "status": "False"}]}});
        assert_eq!(condition_health(&obj), RowHealth::Error);
    }

    #[test]
    fn age_matches_kubectl_shape() {
        let now: Timestamp = "2026-01-02T00:00:00Z".parse().unwrap();
        let created: Timestamp = "2026-01-01T21:30:00Z".parse().unwrap();
        assert_eq!(humanize_age(created, now), "2h30m");
    }
}
