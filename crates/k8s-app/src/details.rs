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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
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
            ("CPU", text(object, "/status/capacity/cpu")),
            ("Memory", text(object, "/status/capacity/memory")),
            ("Pods", text(object, "/status/capacity/pods")),
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
