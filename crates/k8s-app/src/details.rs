//! A structured reading of one object for the Details tab: who it is, what it
//! is made of, what owns it, and how it is doing. Pure functions over the
//! object's JSON, so every front end shows the same thing and the rules are
//! tested without a window.

use serde_json::Value;

use crate::present;

/// One entry of `status.conditions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub message: String,
}

/// How the object is doing, in one line a person can take in at a glance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headline {
    pub tone: present::Tone,
    /// `Healthy`, `Rolling out`, `Degraded`…
    pub label: String,
    /// What the label rests on: `2 of 3 replicas ready`.
    pub detail: String,
    /// Ready out of wanted, for a bar. `None` when the object has no such count.
    pub progress: Option<(i64, i64)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    /// How it is doing, for the kinds that have a clear answer.
    pub headline: Option<Headline>,
    /// Label and value, in the order to show them.
    pub summary: Vec<(String, String)>,
    /// A pod's containers, or a workload's: name and image.
    pub containers: Vec<(String, String)>,
    pub labels: Vec<(String, String)>,
    pub annotations: Vec<(String, String)>,
    /// `Kind/name` of each owner.
    pub owners: Vec<String>,
    pub conditions: Vec<Condition>,
}

/// Annotations can hold whole manifests; past this a value is cut so one of
/// them cannot fill the tab.
const VALUE_LIMIT: usize = 400;

/// Names of a ConfigMap's or Secret's keys to list before "and N more".
const KEYS_SHOWN: usize = 12;

fn text(value: &Value, pointer: &str) -> Option<String> {
    match value.pointer(pointer)? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn number(value: &Value, pointer: &str) -> i64 {
    value.pointer(pointer).and_then(Value::as_i64).unwrap_or(0)
}

fn pairs(value: &Value, pointer: &str, limit: usize) -> Vec<(String, String)> {
    let Some(map) = value.pointer(pointer).and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, String)> = map
        .iter()
        .map(|(key, value)| {
            let mut value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            if value.len() > limit {
                let cut = (0..=limit)
                    .rev()
                    .find(|i| value.is_char_boundary(*i))
                    .unwrap_or(0);
                value.truncate(cut);
                value.push('…');
            }
            (key.clone(), value)
        })
        .collect();
    pairs.sort();
    pairs
}

fn join(items: impl IntoIterator<Item = String>) -> Option<String> {
    let items: Vec<String> = items.into_iter().collect();
    (!items.is_empty()).then(|| items.join(", "))
}

/// `a=b, c=d` for a selector or a node selector.
fn selector(value: &Value, pointer: &str) -> Option<String> {
    let map = value.pointer(pointer)?.as_object()?;
    join(
        map.iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default())),
    )
}

/// Bytes as a person reads them: `15.6 GiB`, `983 GiB`.
fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// What a node has, and how much of it pods may use: `8 · 7910m allocatable`.
/// Memory and storage are shown in bytes people read; the apiserver reports them
/// in whatever unit the kubelet chose, often a different one for each figure.
fn capacity(node: &Value, resource: &str) -> Option<String> {
    let shown = |raw: &str| match resource {
        "memory" | "ephemeral-storage" => {
            k8s_metrics::quantity::parse(raw).map_or_else(|| raw.to_string(), human_bytes)
        }
        _ => raw.to_string(),
    };
    let has = shown(&text(node, &format!("/status/capacity/{resource}"))?);
    Some(
        match text(node, &format!("/status/allocatable/{resource}")).map(|raw| shown(&raw)) {
            Some(offered) if offered != has => format!("{has} · {offered} allocatable"),
            _ => has,
        },
    )
}

/// A count the object reports, or `None` when it leaves the field out.
fn count(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_i64().map(|n| n.to_string())
}

/// `key=value:Effect` for each toleration, as `kubectl taint` writes taints.
fn tolerations(value: &Value, pointer: &str) -> Option<String> {
    join(value.pointer(pointer)?.as_array()?.iter().map(|t| {
        let key = t.get("key").and_then(Value::as_str).unwrap_or("*");
        let effect = t.get("effect").and_then(Value::as_str);
        let mut out = match t.get("value").and_then(Value::as_str) {
            Some(v) if !v.is_empty() => format!("{key}={v}"),
            _ => key.to_string(),
        };
        if let Some(effect) = effect {
            out.push(':');
            out.push_str(effect);
        }
        if let Some(seconds) = t.get("tolerationSeconds").and_then(Value::as_i64) {
            out.push_str(&format!(" for {seconds}s"));
        }
        out
    }))
}

/// `key In [a, b]` for one node-selector requirement.
fn requirement(expr: &Value) -> String {
    let key = expr.get("key").and_then(Value::as_str).unwrap_or("?");
    let op = expr.get("operator").and_then(Value::as_str).unwrap_or("?");
    let values: Vec<&str> = expr
        .get("values")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if values.is_empty() {
        format!("{key} {op}")
    } else {
        format!("{key} {op} [{}]", values.join(", "))
    }
}

/// What a pod template asks of where it runs: required and preferred node
/// affinity, and the topology its pod (anti-)affinity spreads or packs over.
fn affinities(value: &Value, pointer: &str) -> Option<String> {
    let affinity = value.pointer(pointer)?.as_object()?;
    let mut parts: Vec<String> = Vec::new();
    for (kind, label) in [
        ("nodeAffinity", "node"),
        ("podAffinity", "pod"),
        ("podAntiAffinity", "pod anti"),
    ] {
        let Some(block) = affinity.get(kind) else {
            continue;
        };
        let required = block.get("requiredDuringSchedulingIgnoredDuringExecution");
        let preferred = block
            .get("preferredDuringSchedulingIgnoredDuringExecution")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if kind == "nodeAffinity" {
            let terms: Vec<String> = required
                .and_then(|r| r.get("nodeSelectorTerms"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|term| {
                    term.get("matchExpressions")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(requirement)
                        .collect::<Vec<_>>()
                        .join(" and ")
                })
                .collect();
            if !terms.is_empty() {
                parts.push(format!("{label} affinity required: {}", terms.join(" or ")));
            }
        } else {
            let keys: Vec<&str> = required
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|term| term.get("topologyKey").and_then(Value::as_str))
                .collect();
            if !keys.is_empty() {
                parts.push(format!("{label} affinity required on {}", keys.join(", ")));
            }
        }
        if preferred > 0 {
            parts.push(format!("{label} affinity preferred ({preferred})"));
        }
    }
    join(parts)
}

fn age_of(object: &Value) -> Option<String> {
    let created = text(object, "/metadata/creationTimestamp")?;
    let seconds = present::now() - present::parse_timestamp(&created)?;
    let mut age = String::new();
    present::write_age(&mut age, seconds.max(0));
    Some(format!("{created} ({age} ago)"))
}

fn ports(value: &Value, pointer: &str) -> Option<String> {
    join(value.pointer(pointer)?.as_array()?.iter().map(|port| {
        let number = port
            .get("port")
            .map(ToString::to_string)
            .unwrap_or_default();
        let protocol = port
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("TCP");
        match port.get("name").and_then(Value::as_str) {
            Some(name) => format!("{name} {number}/{protocol}"),
            None => format!("{number}/{protocol}"),
        }
    }))
}

fn containers(object: &Value, pointer: &str) -> Vec<(String, String)> {
    object
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| {
                    (
                        c.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                        c.get("image")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How a condition reads. Most conditions are good when `True` (`Ready`,
/// `Available`), but some are problems when `True` (`MemoryPressure`,
/// `Failed`). `None` for a condition this does not know, which stays neutral
/// rather than guessing.
pub fn condition_tone(kind: &str, status: &str) -> Option<present::Tone> {
    use present::Tone;
    const GOOD_WHEN_TRUE: &[&str] = &[
        "Ready",
        "Available",
        "Initialized",
        "ContainersReady",
        "PodScheduled",
        "Complete",
        "Established",
        "NamesAccepted",
        "Healthy",
    ];
    let bad_when_true = kind.ends_with("Pressure")
        || kind.ends_with("Unavailable")
        || matches!(kind, "Failed" | "ReplicaFailure" | "Stalled" | "Degraded");
    match (status, GOOD_WHEN_TRUE.contains(&kind), bad_when_true) {
        ("True", true, _) | ("False", _, true) => Some(Tone::Ok),
        ("False", true, _) | ("True", _, true) => Some(Tone::Error),
        ("Unknown", true, _) => Some(Tone::Pending),
        _ => None,
    }
}

/// Rows laid out for a card: two to a line when both are short, else one each.
/// Short values pair up so a card of twelve small facts is six lines, not twelve.
pub fn pack<T>(rows: &[T], is_short: impl Fn(&T) -> bool) -> Vec<&[T]> {
    let mut lines = Vec::with_capacity(rows.len());
    let mut at = 0;
    while at < rows.len() {
        let pair = at + 1 < rows.len() && is_short(&rows[at]) && is_short(&rows[at + 1]);
        let end = if pair { at + 2 } else { at + 1 };
        lines.push(&rows[at..end]);
        at = end;
    }
    lines
}

/// Which card a summary row belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Overview,
    /// Replica counts and how a change is rolled out.
    Rollout,
    /// Where the pods may run and as whom.
    Scheduling,
    /// What a node has and what it offers pods.
    Capacity,
}

/// The card for a summary row, by its label. A label no card claims stays in the
/// overview, so a kind this does not know still shows every row.
pub fn group_of(label: &str) -> Group {
    match label {
        "Replicas" | "Updated" | "Total" | "Available" | "Unavailable" | "Strategy"
        | "Max surge" | "Max unavailable" | "Partition" | "Min ready seconds"
        | "Revision history" | "Progress deadline" | "Paused" | "Revision" | "Selector"
        | "Desired" | "Ready" => Group::Rollout,
        "Node selector" | "Tolerations" | "Affinities" | "Service account" | "Priority class" => {
            Group::Scheduling
        }
        "CPU" | "Memory" | "Pods" | "Ephemeral storage" => Group::Capacity,
        _ => Group::Overview,
    }
}

fn condition_status<'a>(object: &'a Value, kind: &str) -> Option<(&'a str, &'a str)> {
    object
        .pointer("/status/conditions")?
        .as_array()?
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some(kind))
        .map(|c| {
            (
                c.get("status").and_then(Value::as_str).unwrap_or(""),
                c.get("reason").and_then(Value::as_str).unwrap_or(""),
            )
        })
}

/// The one-line state of a workload, pod or node, or `None` for a kind with no
/// clear answer.
pub fn headline(object: &Value) -> Option<Headline> {
    use present::Tone;
    let kind = text(object, "/kind")?;
    let make = |tone, label: &str, detail: String, progress| Headline {
        tone,
        label: label.to_string(),
        detail,
        progress,
    };
    match kind.as_str() {
        "Deployment" | "StatefulSet" | "ReplicaSet" | "ReplicationController" | "DaemonSet" => {
            let daemon = kind == "DaemonSet";
            let (wanted, ready, updated) = if daemon {
                (
                    number(object, "/status/desiredNumberScheduled"),
                    number(object, "/status/numberReady"),
                    number(object, "/status/updatedNumberScheduled"),
                )
            } else {
                (
                    number(object, "/spec/replicas"),
                    number(object, "/status/readyReplicas"),
                    number(object, "/status/updatedReplicas"),
                )
            };
            let noun = if daemon { "pods" } else { "replicas" };
            let detail = format!("{ready} of {wanted} {noun} ready");
            let progress = Some((ready, wanted));
            if let Some(("False", "ProgressDeadlineExceeded")) =
                condition_status(object, "Progressing")
            {
                return Some(make(Tone::Error, "Rollout stalled", detail, progress));
            }
            Some(if wanted == 0 {
                make(Tone::Pending, "Scaled to zero", detail, progress)
            } else if ready >= wanted && updated >= wanted {
                make(Tone::Ok, "Healthy", detail, progress)
            } else if ready == 0 {
                make(Tone::Error, "Unavailable", detail, progress)
            } else if updated < wanted || ready >= wanted {
                make(Tone::Pending, "Rolling out", detail, progress)
            } else {
                make(Tone::Warn, "Degraded", detail, progress)
            })
        }
        "Pod" => {
            let statuses = object
                .pointer("/status/containerStatuses")
                .and_then(Value::as_array);
            let total = statuses.map_or(0, |list| list.len() as i64);
            let ready = statuses.map_or(0, |list| {
                list.iter()
                    .filter(|c| c.get("ready").and_then(Value::as_bool) == Some(true))
                    .count() as i64
            });
            let restarts: i64 = statuses.map_or(0, |list| {
                list.iter()
                    .map(|c| c.get("restartCount").and_then(Value::as_i64).unwrap_or(0))
                    .sum()
            });
            let waiting = statuses.and_then(|list| {
                list.iter().find_map(|c| {
                    c.pointer("/state/waiting/reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
            });
            let detail = format!(
                "{ready} of {total} containers ready · {restarts} restart{}",
                if restarts == 1 { "" } else { "s" }
            );
            let progress = (total > 0).then_some((ready, total));
            let phase = text(object, "/status/phase").unwrap_or_default();
            Some(match (phase.as_str(), waiting) {
                (_, Some(reason)) => make(Tone::Error, &reason, detail, progress),
                ("Succeeded", _) => make(Tone::Ok, "Completed", detail, progress),
                ("Failed", _) => make(Tone::Error, "Failed", detail, progress),
                ("Pending", _) => make(Tone::Pending, "Pending", detail, progress),
                ("Running", _) if total > 0 && ready == total => {
                    make(Tone::Ok, "Running", detail, progress)
                }
                ("Running", _) => make(Tone::Warn, "Not ready", detail, progress),
                (other, _) => make(Tone::Pending, other, detail, progress),
            })
        }
        "Node" => {
            let version = text(object, "/status/nodeInfo/kubeletVersion").unwrap_or_default();
            let cordoned = object
                .pointer("/spec/unschedulable")
                .and_then(Value::as_bool)
                == Some(true);
            Some(match condition_status(object, "Ready") {
                Some(("True", _)) if cordoned => {
                    make(Tone::Warn, "Cordoned", format!("Ready · {version}"), None)
                }
                Some(("True", _)) => make(Tone::Ok, "Ready", version, None),
                Some(("False", _)) => make(Tone::Error, "Not ready", version, None),
                _ => make(Tone::Pending, "Unknown", version, None),
            })
        }
        _ => None,
    }
}

/// Read an object the way a person scanning its detail would.
pub fn describe(object: &Value) -> Details {
    let kind = text(object, "/kind").unwrap_or_default();
    let mut rows: Vec<(&str, Option<String>)> = vec![
        ("Name", text(object, "/metadata/name")),
        ("Namespace", text(object, "/metadata/namespace")),
        ("Created", age_of(object)),
        ("UID", text(object, "/metadata/uid")),
    ];
    let mut containers_at = "/spec/template/spec/containers";

    match kind.as_str() {
        "Pod" => {
            containers_at = "/spec/containers";
            let restarts: i64 = object
                .pointer("/status/containerStatuses")
                .and_then(Value::as_array)
                .map_or(0, |list| {
                    list.iter()
                        .map(|c| c.get("restartCount").and_then(Value::as_i64).unwrap_or(0))
                        .sum()
                });
            rows.extend([
                ("Status", text(object, "/status/phase")),
                ("Node", text(object, "/spec/nodeName")),
                ("Pod IP", text(object, "/status/podIP")),
                ("Host IP", text(object, "/status/hostIP")),
                ("Service account", text(object, "/spec/serviceAccountName")),
                ("QoS class", text(object, "/status/qosClass")),
                ("Restart policy", text(object, "/spec/restartPolicy")),
                ("Restarts", Some(restarts.to_string())),
            ]);
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" | "ReplicationController" => {
            let desired = number(object, "/spec/replicas");
            let ready = number(object, "/status/readyReplicas");
            rows.extend([
                (
                    "Replicas",
                    Some(format!("{ready} ready of {desired} desired")),
                ),
                (
                    "Updated",
                    Some(number(object, "/status/updatedReplicas").to_string()),
                ),
                (
                    "Strategy",
                    text(object, "/spec/strategy/type")
                        .or_else(|| text(object, "/spec/updateStrategy/type")),
                ),
                ("Selector", selector(object, "/spec/selector/matchLabels")),
            ]);
            rows.extend([
                ("Total", count(object, "/status/replicas")),
                ("Available", count(object, "/status/availableReplicas")),
                (
                    "Unavailable",
                    // Left out of a healthy Deployment's status; a reader wants 0.
                    (kind == "Deployment")
                        .then(|| number(object, "/status/unavailableReplicas").to_string()),
                ),
                (
                    "Max surge",
                    text(object, "/spec/strategy/rollingUpdate/maxSurge"),
                ),
                (
                    "Max unavailable",
                    text(object, "/spec/strategy/rollingUpdate/maxUnavailable"),
                ),
                (
                    "Partition",
                    text(object, "/spec/updateStrategy/rollingUpdate/partition"),
                ),
                ("Min ready seconds", count(object, "/spec/minReadySeconds")),
                (
                    "Revision history",
                    count(object, "/spec/revisionHistoryLimit"),
                ),
                (
                    "Progress deadline",
                    count(object, "/spec/progressDeadlineSeconds").map(|n| format!("{n}s")),
                ),
                (
                    "Paused",
                    (object.pointer("/spec/paused").and_then(Value::as_bool) == Some(true))
                        .then(|| "true".to_string()),
                ),
                (
                    "Revision",
                    text(
                        object,
                        "/metadata/annotations/deployment.kubernetes.io~1revision",
                    ),
                ),
            ]);
        }
        "DaemonSet" => rows.extend([
            (
                "Desired",
                Some(number(object, "/status/desiredNumberScheduled").to_string()),
            ),
            (
                "Ready",
                Some(number(object, "/status/numberReady").to_string()),
            ),
            ("Selector", selector(object, "/spec/selector/matchLabels")),
        ]),
        "Job" => {
            rows.extend([
                ("Completions", text(object, "/spec/completions")),
                ("Active", Some(number(object, "/status/active").to_string())),
                (
                    "Succeeded",
                    Some(number(object, "/status/succeeded").to_string()),
                ),
                ("Failed", Some(number(object, "/status/failed").to_string())),
            ]);
        }
        "CronJob" => {
            containers_at = "/spec/jobTemplate/spec/template/spec/containers";
            rows.extend([
                ("Schedule", text(object, "/spec/schedule")),
                (
                    "Suspended",
                    Some(text(object, "/spec/suspend").unwrap_or_else(|| "false".into())),
                ),
                ("Last schedule", text(object, "/status/lastScheduleTime")),
                (
                    "Active jobs",
                    Some(
                        object
                            .pointer("/status/active")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len)
                            .to_string(),
                    ),
                ),
            ]);
        }
        "Service" => rows.extend([
            ("Type", text(object, "/spec/type")),
            ("Cluster IP", text(object, "/spec/clusterIP")),
            ("Ports", ports(object, "/spec/ports")),
            ("Selector", selector(object, "/spec/selector")),
            ("External name", text(object, "/spec/externalName")),
            (
                "Load balancer",
                join(
                    object
                        .pointer("/status/loadBalancer/ingress")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|i| i.get("ip").or_else(|| i.get("hostname")))
                        .filter_map(|v| v.as_str().map(str::to_string)),
                ),
            ),
        ]),
        "Ingress" => rows.extend([
            ("Class", text(object, "/spec/ingressClassName")),
            (
                "Hosts",
                join(
                    object
                        .pointer("/spec/rules")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r.get("host").and_then(Value::as_str).map(str::to_string)),
                ),
            ),
        ]),
        "Node" => rows.extend([
            ("Kubelet", text(object, "/status/nodeInfo/kubeletVersion")),
            ("OS", text(object, "/status/nodeInfo/osImage")),
            ("Kernel", text(object, "/status/nodeInfo/kernelVersion")),
            (
                "Runtime",
                text(object, "/status/nodeInfo/containerRuntimeVersion"),
            ),
            (
                "Architecture",
                text(object, "/status/nodeInfo/architecture"),
            ),
            ("CPU", capacity(object, "cpu")),
            ("Memory", capacity(object, "memory")),
            ("Pods", capacity(object, "pods")),
            ("Ephemeral storage", capacity(object, "ephemeral-storage")),
            ("Unschedulable", text(object, "/spec/unschedulable")),
        ]),
        "PersistentVolumeClaim" => rows.extend([
            ("Status", text(object, "/status/phase")),
            ("Storage class", text(object, "/spec/storageClassName")),
            (
                "Requested",
                text(object, "/spec/resources/requests/storage"),
            ),
            ("Capacity", text(object, "/status/capacity/storage")),
            ("Volume", text(object, "/spec/volumeName")),
            (
                "Access modes",
                join(
                    object
                        .pointer("/spec/accessModes")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|m| m.as_str().map(str::to_string)),
                ),
            ),
        ]),
        "ConfigMap" | "Secret" => {
            // Names only. A Secret's values are never put on screen here.
            let keys: Vec<String> = ["/data", "/binaryData"]
                .iter()
                .filter_map(|p| object.pointer(p).and_then(Value::as_object))
                .flat_map(|map| map.keys().cloned())
                .collect();
            let shown = keys
                .iter()
                .take(KEYS_SHOWN)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = keys.len().saturating_sub(KEYS_SHOWN);
            rows.extend([
                ("Type", text(object, "/type")),
                (
                    "Keys",
                    (!keys.is_empty()).then(|| {
                        if more > 0 {
                            format!("{shown} and {more} more")
                        } else {
                            shown
                        }
                    }),
                ),
            ]);
        }
        _ => {}
    }

    if matches!(
        kind.as_str(),
        "Deployment" | "StatefulSet" | "ReplicaSet" | "ReplicationController" | "DaemonSet"
    ) {
        let spec = "/spec/template/spec";
        rows.extend([
            (
                "Node selector",
                selector(object, &format!("{spec}/nodeSelector")),
            ),
            (
                "Tolerations",
                tolerations(object, &format!("{spec}/tolerations")),
            ),
            (
                "Affinities",
                affinities(object, &format!("{spec}/affinity")),
            ),
            (
                "Service account",
                text(object, &format!("{spec}/serviceAccountName")),
            ),
            (
                "Priority class",
                text(object, &format!("{spec}/priorityClassName")),
            ),
        ]);
    }

    let conditions = object
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| Condition {
                    kind: text(c, "/type").unwrap_or_default(),
                    status: text(c, "/status").unwrap_or_default(),
                    reason: text(c, "/reason").unwrap_or_default(),
                    message: text(c, "/message").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    Details {
        headline: headline(object),
        summary: rows
            .into_iter()
            .filter_map(|(label, value)| Some((label.to_string(), value?)))
            .collect(),
        containers: containers(object, containers_at),
        labels: pairs(object, "/metadata/labels", VALUE_LIMIT),
        annotations: pairs(object, "/metadata/annotations", VALUE_LIMIT),
        owners: object
            .pointer("/metadata/ownerReferences")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|o| Some(format!("{}/{}", text(o, "/kind")?, text(o, "/name")?)))
                    .collect()
            })
            .unwrap_or_default(),
        conditions,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn summary<'a>(details: &'a Details, label: &str) -> Option<&'a str> {
        details
            .summary
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_pod_shows_where_it_runs_and_how_often_it_restarted() {
        let pod = json!({
            "kind": "Pod",
            "metadata": {
                "name": "web-1", "namespace": "shop", "uid": "u",
                "labels": {"b": "2", "a": "1"},
                "ownerReferences": [{"kind": "ReplicaSet", "name": "web-abc"}],
            },
            "spec": {"nodeName": "n1", "containers": [{"name": "app", "image": "nginx:1"}]},
            "status": {
                "phase": "Running", "podIP": "10.0.0.4",
                "containerStatuses": [{"restartCount": 2}, {"restartCount": 3}],
                "conditions": [{"type": "Ready", "status": "True"}],
            },
        });
        let details = describe(&pod);
        assert_eq!(summary(&details, "Node"), Some("n1"));
        assert_eq!(summary(&details, "Pod IP"), Some("10.0.0.4"));
        assert_eq!(summary(&details, "Restarts"), Some("5"));
        assert_eq!(
            details.containers,
            [("app".to_string(), "nginx:1".to_string())]
        );
        assert_eq!(details.labels[0].0, "a", "labels are sorted");
        assert_eq!(details.owners, ["ReplicaSet/web-abc"]);
        assert_eq!(details.conditions[0].kind, "Ready");
    }

    #[test]
    fn a_workload_says_how_many_replicas_are_ready() {
        let deployment = json!({
            "kind": "Deployment",
            "metadata": {"name": "web"},
            "spec": {
                "replicas": 3, "strategy": {"type": "RollingUpdate"},
                "selector": {"matchLabels": {"app": "web"}},
                "template": {"spec": {"containers": [{"name": "c", "image": "i"}]}},
            },
            "status": {"readyReplicas": 2, "updatedReplicas": 3},
        });
        let details = describe(&deployment);
        assert_eq!(summary(&details, "Replicas"), Some("2 ready of 3 desired"));
        assert_eq!(summary(&details, "Selector"), Some("app=web"));
        assert_eq!(details.containers.len(), 1);
    }

    #[test]
    fn a_deployment_shows_its_rollout_settings_and_where_it_may_run() {
        let deployment = json!({
            "kind": "Deployment",
            "metadata": {
                "name": "web",
                "annotations": {"deployment.kubernetes.io/revision": "7"},
            },
            "spec": {
                "replicas": 3, "minReadySeconds": 5, "revisionHistoryLimit": 10,
                "progressDeadlineSeconds": 600,
                "strategy": {"type": "RollingUpdate",
                    "rollingUpdate": {"maxSurge": "25%", "maxUnavailable": 1}},
                "template": {"spec": {
                    "serviceAccountName": "web", "priorityClassName": "high",
                    "nodeSelector": {"disk": "ssd"},
                    "tolerations": [
                        {"key": "dedicated", "operator": "Equal", "value": "web", "effect": "NoSchedule"},
                        {"operator": "Exists"},
                        {"key": "node.kubernetes.io/unreachable", "operator": "Exists",
                         "effect": "NoExecute", "tolerationSeconds": 300},
                    ],
                    "affinity": {
                        "nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                            "nodeSelectorTerms": [{"matchExpressions": [
                                {"key": "zone", "operator": "In", "values": ["a", "b"]}]}]}},
                        "podAntiAffinity": {
                            "requiredDuringSchedulingIgnoredDuringExecution": [
                                {"topologyKey": "kubernetes.io/hostname"}],
                            "preferredDuringSchedulingIgnoredDuringExecution": [{}, {}],
                        },
                    },
                    "containers": [{"name": "c", "image": "i"}],
                }},
            },
            "status": {"replicas": 4, "readyReplicas": 3, "updatedReplicas": 3,
                "availableReplicas": 3, "unavailableReplicas": 1},
        });
        let d = describe(&deployment);
        assert_eq!(summary(&d, "Total"), Some("4"));
        assert_eq!(summary(&d, "Available"), Some("3"));
        assert_eq!(summary(&d, "Unavailable"), Some("1"));
        assert_eq!(summary(&d, "Max surge"), Some("25%"));
        assert_eq!(summary(&d, "Max unavailable"), Some("1"));
        assert_eq!(summary(&d, "Min ready seconds"), Some("5"));
        assert_eq!(summary(&d, "Revision history"), Some("10"));
        assert_eq!(summary(&d, "Progress deadline"), Some("600s"));
        assert_eq!(summary(&d, "Revision"), Some("7"));
        assert_eq!(summary(&d, "Paused"), None);
        assert_eq!(summary(&d, "Node selector"), Some("disk=ssd"));
        assert_eq!(
            summary(&d, "Tolerations"),
            Some("dedicated=web:NoSchedule, *, node.kubernetes.io/unreachable:NoExecute for 300s")
        );
        assert_eq!(
            summary(&d, "Affinities"),
            Some(
                "node affinity required: zone In [a, b], \
                 pod anti affinity required on kubernetes.io/hostname, \
                 pod anti affinity preferred (2)"
            )
        );
        assert_eq!(summary(&d, "Service account"), Some("web"));
        assert_eq!(summary(&d, "Priority class"), Some("high"));
    }

    #[test]
    fn a_healthy_deployment_reports_zero_unavailable_and_omits_what_it_lacks() {
        let deployment = json!({
            "kind": "Deployment", "metadata": {"name": "web"},
            "spec": {"replicas": 1, "template": {"spec": {"containers": []}}},
            "status": {"replicas": 1, "readyReplicas": 1, "availableReplicas": 1},
        });
        let d = describe(&deployment);
        assert_eq!(summary(&d, "Unavailable"), Some("0"));
        for absent in [
            "Tolerations",
            "Affinities",
            "Node selector",
            "Max surge",
            "Paused",
        ] {
            assert_eq!(summary(&d, absent), None, "{absent}");
        }
    }

    fn tone_and_label(object: &Value) -> (present::Tone, String) {
        let h = headline(object).expect("has a headline");
        (h.tone, h.label)
    }

    #[test]
    fn a_workload_headline_says_healthy_rolling_degraded_or_down() {
        use present::Tone;
        let deployment = |wanted: i64, ready: i64, updated: i64| {
            json!({"kind": "Deployment", "metadata": {"name": "w"},
                "spec": {"replicas": wanted},
                "status": {"readyReplicas": ready, "updatedReplicas": updated}})
        };
        assert_eq!(
            tone_and_label(&deployment(3, 3, 3)),
            (Tone::Ok, "Healthy".into())
        );
        assert_eq!(
            tone_and_label(&deployment(0, 0, 0)),
            (Tone::Pending, "Scaled to zero".into())
        );
        assert_eq!(
            tone_and_label(&deployment(3, 0, 3)),
            (Tone::Error, "Unavailable".into())
        );
        assert_eq!(
            tone_and_label(&deployment(3, 2, 1)),
            (Tone::Pending, "Rolling out".into())
        );
        assert_eq!(
            tone_and_label(&deployment(3, 2, 3)),
            (Tone::Warn, "Degraded".into())
        );
        let stalled = json!({"kind": "Deployment", "metadata": {"name": "w"},
            "spec": {"replicas": 2},
            "status": {"readyReplicas": 1, "updatedReplicas": 1, "conditions": [
                {"type": "Progressing", "status": "False", "reason": "ProgressDeadlineExceeded"}]}});
        assert_eq!(
            tone_and_label(&stalled),
            (Tone::Error, "Rollout stalled".into())
        );
        let h = headline(&deployment(3, 2, 3)).expect("headline");
        assert_eq!(h.progress, Some((2, 3)));
        assert_eq!(h.detail, "2 of 3 replicas ready");
    }

    #[test]
    fn a_pod_headline_names_the_reason_a_container_is_waiting() {
        use present::Tone;
        let pod = |phase: &str, ready: bool, waiting: Option<&str>| {
            let state = match waiting {
                Some(reason) => json!({"waiting": {"reason": reason}}),
                None => json!({"running": {}}),
            };
            json!({"kind": "Pod", "metadata": {"name": "p"}, "status": {
                "phase": phase,
                "containerStatuses": [{"ready": ready, "restartCount": 4, "state": state}]}})
        };
        assert_eq!(
            tone_and_label(&pod("Running", true, None)),
            (Tone::Ok, "Running".into())
        );
        assert_eq!(
            tone_and_label(&pod("Running", false, None)),
            (Tone::Warn, "Not ready".into())
        );
        assert_eq!(
            tone_and_label(&pod("Running", false, Some("CrashLoopBackOff"))),
            (Tone::Error, "CrashLoopBackOff".into())
        );
        assert_eq!(
            tone_and_label(&pod("Pending", false, None)),
            (Tone::Pending, "Pending".into())
        );
        let h = headline(&pod("Running", true, None)).expect("headline");
        assert_eq!(h.detail, "1 of 1 containers ready · 4 restarts");
        assert!(headline(&json!({"kind": "ConfigMap", "metadata": {"name": "c"}})).is_none());
    }

    #[test]
    fn short_rows_pair_up_and_a_long_one_stands_alone() {
        let rows = ["a", "b", "long long long", "c", "d", "e"];
        let lines = pack(&rows, |row| row.len() <= 3);
        let shape: Vec<usize> = lines.iter().map(|line| line.len()).collect();
        assert_eq!(shape, [2, 1, 2, 1], "a+b, the long one, c+d, e alone");
        assert!(pack::<&str>(&[], |_| true).is_empty());
    }

    #[test]
    fn rows_are_grouped_by_what_they_are_about_and_unknown_ones_stay_in_the_overview() {
        assert_eq!(group_of("Name"), Group::Overview);
        assert_eq!(group_of("Max surge"), Group::Rollout);
        assert_eq!(group_of("Tolerations"), Group::Scheduling);
        assert_eq!(group_of("Something new"), Group::Overview);
    }

    #[test]
    fn a_node_says_what_it_has_and_what_it_offers() {
        let node = json!({"kind": "Node", "metadata": {"name": "n"}, "status": {
            "capacity": {"cpu": "8", "memory": "16Gi", "pods": "110",
                "ephemeral-storage": "1055761844Ki"},
            "allocatable": {"cpu": "7910m", "memory": "16Gi", "pods": "110",
                "ephemeral-storage": "1027045121038"}}});
        let d = describe(&node);
        assert_eq!(summary(&d, "CPU"), Some("8 · 7910m allocatable"));
        assert_eq!(
            summary(&d, "Memory"),
            Some("16 GiB"),
            "same as allocatable: said once"
        );
        assert_eq!(summary(&d, "Pods"), Some("110"));
        assert_eq!(
            summary(&d, "Ephemeral storage"),
            Some("1006.9 GiB · 956.5 GiB allocatable"),
            "two units for one figure are brought to one"
        );
        assert_eq!(group_of("CPU"), Group::Capacity);
    }

    #[test]
    fn a_node_headline_flags_a_cordoned_node() {
        use present::Tone;
        let node = |ready: &str, cordoned: bool| {
            json!({"kind": "Node", "metadata": {"name": "n"},
                "spec": {"unschedulable": cordoned},
                "status": {"conditions": [{"type": "Ready", "status": ready}],
                    "nodeInfo": {"kubeletVersion": "v1.35.0"}}})
        };
        assert_eq!(
            tone_and_label(&node("True", false)),
            (Tone::Ok, "Ready".into())
        );
        assert_eq!(
            tone_and_label(&node("True", true)),
            (Tone::Warn, "Cordoned".into())
        );
        assert_eq!(
            tone_and_label(&node("False", false)),
            (Tone::Error, "Not ready".into())
        );
    }

    #[test]
    fn a_secret_lists_key_names_and_never_a_value() {
        let secret = json!({
            "kind": "Secret",
            "metadata": {"name": "s"},
            "type": "Opaque",
            "data": {"password": "aHVudGVyMg==", "user": "YWRtaW4="},
        });
        let details = describe(&secret);
        assert_eq!(summary(&details, "Keys"), Some("password, user"));
        let everything = format!("{details:?}");
        assert!(!everything.contains("aHVudGVyMg=="), "{everything}");
    }

    #[test]
    fn long_annotations_are_cut_and_unknown_kinds_still_show_their_metadata() {
        let long = "x".repeat(5000);
        let object = json!({
            "kind": "Widget",
            "metadata": {"name": "w", "annotations": {"big": long}},
        });
        let details = describe(&object);
        assert_eq!(summary(&details, "Name"), Some("w"));
        assert!(details.annotations[0].1.chars().count() <= VALUE_LIMIT + 1);
        assert!(details.annotations[0].1.ends_with('…'));
        assert!(details.conditions.is_empty() && details.owners.is_empty());
    }

    #[test]
    fn a_condition_is_good_or_bad_by_what_it_means_not_only_by_its_value() {
        use present::Tone;
        assert_eq!(condition_tone("Ready", "True"), Some(Tone::Ok));
        assert_eq!(condition_tone("Ready", "False"), Some(Tone::Error));
        assert_eq!(condition_tone("MemoryPressure", "True"), Some(Tone::Error));
        assert_eq!(condition_tone("MemoryPressure", "False"), Some(Tone::Ok));
        assert_eq!(condition_tone("Ready", "Unknown"), Some(Tone::Pending));
        assert_eq!(condition_tone("SomethingNew", "True"), None);
    }

    #[test]
    fn a_cut_never_splits_a_multibyte_character() {
        let object = json!({
            "kind": "Widget",
            "metadata": {"name": "w", "annotations": {"k": "é".repeat(500)}},
        });
        assert!(describe(&object).annotations[0].1.ends_with('…'));
    }
}
