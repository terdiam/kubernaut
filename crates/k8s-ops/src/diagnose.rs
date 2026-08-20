//! Why is this pod not running, and what should be done about it.
//!
//! A pod that will not start already carries the answer: the waiting reason,
//! the previous container's exit code, the scheduler's message on the
//! `PodScheduled` condition. The work nobody wants to do at 3am is knowing
//! which of those fields to look at for this particular failure, and what the
//! next command is. That mapping is what lives here.
//!
//! The rules are deliberately evidence-first: every finding quotes the exact
//! text the cluster produced, so the advice can be checked rather than
//! trusted. Nothing here mutates anything.

use std::{collections::BTreeMap, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::{
    Container, ContainerStatus, Node, Pod, PodSpec, PodStatus, Probe,
};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::{Api, ResourceExt};
use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    logs,
    related::{self, EventRow},
};

// ------------------------------------------------------------------ model

/// A concrete next action. Either something Kubernaut can do itself, or a
/// `kubectl` line to paste elsewhere — usually both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StepAction {
    /// Open the Logs tab, optionally on one container and on the previous
    /// instance — the only place a crash loop explains itself.
    Logs {
        container: Option<String>,
        previous: bool,
    },
    /// Open a shell in the pod.
    Terminal,
    /// Open the editor on this object.
    Edit,
    /// Open another object entirely (the node, a PVC, the owning workload).
    Open {
        resource: String,
        namespace: Option<String>,
        name: String,
    },
}

/// One thing to do, in order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub text: String,
    /// Equivalent `kubectl` invocation, for a terminal or a ticket.
    pub command: Option<String>,
    pub action: Option<StepAction>,
}

impl Step {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            command: None,
            action: None,
        }
    }

    fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    fn action(mut self, action: StepAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// One diagnosed problem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// `error` | `warning` | `info`
    pub severity: String,
    /// Machine-readable cause, so the UI can key off it and tests can assert
    /// on it without matching prose.
    pub code: String,
    /// One line, naming the container where that is what went wrong.
    pub title: String,
    /// What it means, in words that assume no prior knowledge of the reason
    /// string.
    pub explanation: String,
    pub container: Option<String>,
    /// Exact facts read from the cluster. Never paraphrased.
    pub evidence: Vec<String>,
    pub steps: Vec<Step>,
}

/// Everything known about why one pod is unhappy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnosis {
    pub pod: String,
    pub namespace: Option<String>,
    pub phase: String,
    /// `true` when nothing worth acting on was found.
    pub healthy: bool,
    /// One line for a collapsed row.
    pub summary: String,
    pub findings: Vec<Finding>,
}

/// The result for whatever was selected: one pod, or a workload's pods.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosisReport {
    pub pods: Vec<Diagnosis>,
    /// Pods looked at, including the healthy ones left out of `pods`.
    pub examined: usize,
    pub healthy: usize,
    /// Set when the workload has more pods than were examined.
    pub truncated: bool,
}

// ------------------------------------------------------------------ entry

/// Cap on pods examined for one workload. Each pod costs an events request;
/// beyond this the answer is "the whole workload is broken" anyway.
const MAX_PODS: usize = 12;

/// Diagnose a pod, or every pod behind a workload.
pub async fn diagnose(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<DiagnosisReport> {
    let namespace = namespace.unwrap_or_default().to_string();
    let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &namespace);

    let pod_names: Vec<String> = if resource == "core/v1/pods" {
        vec![name.to_string()]
    } else {
        logs::workload_pods(cluster, resource, &namespace, name).await?
    };
    let truncated = pod_names.len() > MAX_PODS;

    let mut diagnoses = Vec::new();
    let mut healthy = 0usize;
    let mut examined = 0usize;
    // Nodes are shared between replicas far more often than not; fetching one
    // per pod would be a request per replica for the same object.
    let mut nodes: BTreeMap<String, Option<Node>> = BTreeMap::new();

    for pod_name in pod_names.iter().take(MAX_PODS) {
        let pod = match api.get(pod_name).await {
            Ok(pod) => pod,
            // A pod can vanish between listing and reading it; that is not a
            // diagnosis failure.
            Err(_) => continue,
        };
        examined += 1;

        let events = related::events(cluster, Some(&namespace), pod_name)
            .await
            .unwrap_or_default();

        let node = match pod.spec.as_ref().and_then(|s| s.node_name.clone()) {
            Some(node_name) => {
                if !nodes.contains_key(&node_name) {
                    let node_api: Api<Node> = Api::all(cluster.client.clone());
                    nodes.insert(node_name.clone(), node_api.get(&node_name).await.ok());
                }
                nodes.get(&node_name).and_then(|n| n.clone())
            }
            None => None,
        };

        let diagnosis = analyse(&pod, &events, node.as_ref());
        if diagnosis.healthy {
            healthy += 1;
        } else {
            diagnoses.push(diagnosis);
        }
    }

    Ok(DiagnosisReport {
        pods: diagnoses,
        examined,
        healthy,
        truncated,
    })
}

// --------------------------------------------------------------- analysis

/// Classify one pod. Pure, so the rules can be tested against fixtures rather
/// than against whatever a cluster happens to be doing today.
pub fn analyse(pod: &Pod, events: &[EventRow], node: Option<&Node>) -> Diagnosis {
    let name = pod.name_any();
    let namespace = pod.namespace();
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();
    let phase = status
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Unknown".into());

    let mut findings = Vec::new();

    check_terminating(pod, &mut findings);
    check_node(pod, node, &mut findings);

    match phase.as_str() {
        "Succeeded" => check_succeeded(pod, &mut findings),
        "Failed" => check_failed(pod, status, events, &mut findings),
        "Pending" => check_pending(pod, spec, status, events, &mut findings),
        _ => {}
    }

    check_containers(pod, spec, status, events, &mut findings);

    if phase == "Running" {
        check_readiness(spec, status, events, &mut findings);
    }

    // Errors first; within a severity keep the order the checks produced,
    // which runs outermost cause (node, scheduling) to innermost (container).
    let rank = |severity: &str| match severity {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    };
    findings.sort_by_key(|f: &Finding| rank(&f.severity));
    findings.dedup_by(|a, b| a.code == b.code && a.container == b.container);

    // `PodFailed` only says that something exited non-zero. Once a container
    // finding names which one and why, repeating it is noise at the top of the
    // list — exactly where the real cause should be.
    if findings
        .iter()
        .any(|f| f.container.is_some() && f.severity == "error")
    {
        findings.retain(|f| f.code != "PodFailed");
    }

    let actionable = findings.iter().any(|f| f.severity != "info");
    let summary = findings
        .first()
        .map(|f| f.title.clone())
        .unwrap_or_else(|| format!("Pod is {phase} with nothing to report"));

    Diagnosis {
        pod: name,
        namespace,
        phase,
        healthy: !actionable,
        summary,
        findings,
    }
}

fn check_terminating(pod: &Pod, out: &mut Vec<Finding>) {
    if pod.metadata.deletion_timestamp.is_none() {
        return;
    }
    let finalizers = pod.metadata.finalizers.clone().unwrap_or_default();
    let mut evidence = vec![format!(
        "metadata.deletionTimestamp is set ({})",
        pod.metadata
            .deletion_timestamp
            .as_ref()
            .map(|t| t.0.to_string())
            .unwrap_or_default()
    )];
    if !finalizers.is_empty() {
        evidence.push(format!("finalizers: {}", finalizers.join(", ")));
    }

    let mut steps = vec![Step::new(
        "Deletion is in progress. A pod normally disappears within its termination grace period; \
         longer than that means something is holding it.",
    )];
    if finalizers.is_empty() {
        steps.push(
            Step::new(
                "No finalizers, so the kubelet is still stopping it — usually a container ignoring \
                 SIGTERM, or a node that stopped reporting.",
            )
            .command(format!(
                "kubectl delete pod {} -n {} --grace-period=0 --force",
                pod.name_any(),
                pod.namespace().unwrap_or_default()
            )),
        );
    } else {
        steps.push(Step::new(format!(
            "A finalizer blocks removal until its controller clears it. Check that the controller \
             behind `{}` is running.",
            finalizers.join(", ")
        )));
        steps.push(Step::new(
            "Removing a finalizer by hand skips whatever cleanup it was protecting — do it only \
             once you know that controller is gone for good.",
        ));
    }

    out.push(Finding {
        severity: "warning".into(),
        code: "Terminating".into(),
        title: "Pod is stuck terminating".into(),
        explanation: "The object stays visible until every finalizer is cleared and the kubelet \
                      confirms the containers are gone."
            .into(),
        container: None,
        evidence,
        steps,
    });
}

fn check_node(pod: &Pod, node: Option<&Node>, out: &mut Vec<Finding>) {
    let Some(node) = node else { return };
    let node_name = node.name_any();

    let ready = node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|c| c.iter().find(|c| c.type_ == "Ready"))
        .map(|c| c.status.clone());

    if ready.as_deref() != Some("True") {
        out.push(Finding {
            severity: "error".into(),
            code: "NodeNotReady".into(),
            title: format!("Node {node_name} is not Ready"),
            explanation: "The pod's problem is probably not the pod. A NotReady node stops \
                          reporting, and its pods are marked Unknown and eventually evicted."
                .into(),
            container: None,
            evidence: vec![format!(
                "node/{node_name} Ready = {}",
                ready.as_deref().unwrap_or("not reported")
            )],
            steps: vec![
                Step::new("Open the node and read its conditions and events first.").action(
                    StepAction::Open {
                        resource: "core/v1/nodes".into(),
                        namespace: None,
                        name: node_name.clone(),
                    },
                ),
                Step::new("Check the kubelet on that host.")
                    .command(format!("kubectl describe node {node_name}")),
            ],
        });
    }

    // Pressure conditions explain evictions and pods that never start.
    for condition in node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .into_iter()
        .flatten()
    {
        let pressure = matches!(
            condition.type_.as_str(),
            "MemoryPressure" | "DiskPressure" | "PIDPressure" | "NetworkUnavailable"
        );
        if pressure && condition.status == "True" {
            out.push(Finding {
                severity: "warning".into(),
                code: format!("Node{}", condition.type_),
                title: format!("Node {node_name} reports {}", condition.type_),
                explanation: "The kubelet refuses new pods and evicts existing ones while this \
                              condition holds."
                    .into(),
                container: None,
                evidence: vec![format!(
                    "{} = True: {}",
                    condition.type_,
                    condition.message.clone().unwrap_or_default()
                )],
                steps: vec![
                    Step::new("Free the resource on that node, or move this pod elsewhere.")
                        .action(StepAction::Open {
                            resource: "core/v1/nodes".into(),
                            namespace: None,
                            name: node_name.clone(),
                        }),
                ],
            });
        }
    }

    if node.spec.as_ref().and_then(|s| s.unschedulable) == Some(true)
        && pod.metadata.deletion_timestamp.is_none()
    {
        out.push(Finding {
            severity: "info".into(),
            code: "NodeCordoned".into(),
            title: format!("Node {node_name} is cordoned"),
            explanation: "Existing pods keep running, but a restart will not be rescheduled here."
                .into(),
            container: None,
            evidence: vec!["spec.unschedulable = true".into()],
            steps: vec![Step::new(
                "Uncordon the node when maintenance is finished, or the next restart of this pod \
                 will land somewhere else.",
            )],
        });
    }
}

fn check_succeeded(pod: &Pod, out: &mut Vec<Finding>) {
    let owner = pod
        .metadata
        .owner_references
        .as_ref()
        .and_then(|refs| refs.first().map(|r| r.kind.clone()))
        .unwrap_or_default();
    let explanation = if owner == "Job" || owner == "CronJob" {
        "This is a finished Job pod. Kubernetes keeps it so its logs stay readable; it is not a \
         failure."
    } else {
        "Every container exited 0. For a long-running workload that usually means the entrypoint \
         returned instead of staying up."
    };

    out.push(Finding {
        severity: "info".into(),
        code: "Succeeded".into(),
        title: "Pod completed".into(),
        explanation: explanation.into(),
        container: None,
        evidence: vec!["status.phase = Succeeded".into()],
        steps: vec![
            Step::new("Read what it printed before exiting.").action(StepAction::Logs {
                container: None,
                previous: false,
            }),
        ],
    });
}

fn check_failed(
    pod: &Pod,
    status: Option<&PodStatus>,
    events: &[EventRow],
    out: &mut Vec<Finding>,
) {
    let reason = status.and_then(|s| s.reason.clone()).unwrap_or_default();
    let message = status.and_then(|s| s.message.clone()).unwrap_or_default();

    match reason.as_str() {
        "Evicted" => {
            let mut evidence = vec!["status.reason = Evicted".into()];
            if !message.is_empty() {
                evidence.push(message.clone());
            }
            if let Some(event) = latest(events, &["Evicted"]) {
                evidence.push(format!("{}: {}", event.reason, event.message));
            }
            out.push(Finding {
                severity: "error".into(),
                code: "Evicted".into(),
                title: "Pod was evicted".into(),
                explanation: "The kubelet reclaimed resources on the node and killed this pod to \
                              do it. The message says which resource ran out."
                    .into(),
                container: None,
                evidence,
                steps: vec![
                    Step::new(
                        "Eviction order goes by QoS: pods with no requests go first, then pods \
                         over their requests. Setting requests that match real usage is what \
                         stops this recurring.",
                    ),
                    Step::new("Set requests on the owning workload.").action(StepAction::Edit),
                    Step::new("Check the node that evicted it for disk or memory pressure.")
                        .command(format!(
                            "kubectl describe node {}",
                            pod.spec
                                .as_ref()
                                .and_then(|s| s.node_name.clone())
                                .unwrap_or_default()
                        )),
                    Step::new(
                        "This pod object is a tombstone; it will not restart. The owning \
                               controller creates a replacement.",
                    ),
                ],
            });
        }
        "DeadlineExceeded" => out.push(Finding {
            severity: "error".into(),
            code: "DeadlineExceeded".into(),
            title: "Pod ran past its activeDeadlineSeconds".into(),
            explanation: "The Job or pod set a wall-clock limit and Kubernetes killed it when the \
                          limit passed."
                .into(),
            container: None,
            evidence: vec![format!("status.reason = DeadlineExceeded. {message}")],
            steps: vec![
                Step::new("Read the logs to see how far it got.").action(StepAction::Logs {
                    container: None,
                    previous: false,
                }),
                Step::new(
                    "Either raise `spec.activeDeadlineSeconds` on the Job, or make the work \
                     finish inside it.",
                )
                .action(StepAction::Edit),
            ],
        }),
        "Shutdown" | "NodeShutdown" | "Terminated" => out.push(Finding {
            severity: "warning".into(),
            code: "NodeShutdown".into(),
            title: "Pod was killed by a node shutdown".into(),
            explanation: "The node went down gracefully and terminated its pods. The replacement \
                          is scheduled elsewhere."
                .into(),
            container: None,
            evidence: vec![format!("status.reason = {reason}. {message}")],
            steps: vec![Step::new(
                "Nothing to fix on the pod. Confirm the replacement is Running, and delete this \
                 tombstone if it clutters the list.",
            )],
        }),
        _ => out.push(Finding {
            severity: "error".into(),
            code: "PodFailed".into(),
            title: "Pod failed".into(),
            explanation: "Every container has terminated and at least one exited non-zero. The \
                          container findings below say which."
                .into(),
            container: None,
            evidence: {
                let mut evidence = vec!["status.phase = Failed".into()];
                if !reason.is_empty() {
                    evidence.push(format!("status.reason = {reason}"));
                }
                if !message.is_empty() {
                    evidence.push(message);
                }
                evidence
            },
            steps: vec![
                Step::new("Read the logs of the container that exited non-zero.").action(
                    StepAction::Logs {
                        container: None,
                        previous: false,
                    },
                ),
            ],
        }),
    }
}

fn check_pending(
    pod: &Pod,
    spec: Option<&PodSpec>,
    status: Option<&PodStatus>,
    events: &[EventRow],
    out: &mut Vec<Finding>,
) {
    let scheduled = spec.and_then(|s| s.node_name.as_deref()).is_some();

    if !scheduled {
        let condition = status
            .and_then(|s| s.conditions.as_ref())
            .and_then(|c| c.iter().find(|c| c.type_ == "PodScheduled"));
        let message = condition
            .and_then(|c| c.message.clone())
            .or_else(|| latest(events, &["FailedScheduling"]).map(|e| e.message.clone()))
            .unwrap_or_else(|| "No scheduling message yet.".into());

        let mut steps = vec![Step::new(
            "The message above is the scheduler's own tally: it lists, per node, the reason that \
             node was rejected. Fix whichever reason covers the most nodes.",
        )];
        steps.extend(scheduling_steps(&message, spec));
        steps.push(
            Step::new("Compare what the pod asks for against what nodes have free.")
                .command("kubectl describe nodes | grep -A 5 'Allocated resources'"),
        );

        out.push(Finding {
            severity: "warning".into(),
            code: "Unschedulable".into(),
            title: "Pod is not scheduled to any node".into(),
            explanation: "It exists in the API but no node was accepted for it, so no container \
                          has been created yet."
                .into(),
            container: None,
            evidence: vec![message],
            steps,
        });
        return;
    }

    // Scheduled but still pending: the kubelet is stuck before the containers
    // start, and the reason is in an event, not in the pod status.
    if let Some(event) = latest(
        events,
        &[
            "FailedMount",
            "FailedAttachVolume",
            "FailedCreatePodSandBox",
            "FailedPodSandBoxStatus",
        ],
    ) {
        let volume = matches!(event.reason.as_str(), "FailedMount" | "FailedAttachVolume");
        let mut steps = Vec::new();
        if volume {
            steps.push(Step::new(
                "The kubelet cannot attach or mount a volume. The message names the volume; the \
                 usual causes are a PVC that is not Bound, a ConfigMap or Secret that does not \
                 exist, or a disk still attached to the previous node.",
            ));
            steps.push(
                Step::new("Check the claims this pod mounts.").command(format!(
                    "kubectl get pvc -n {}",
                    pod.namespace().unwrap_or_default()
                )),
            );
            for (kind, name, resource) in mounted_config(spec) {
                steps.push(
                    Step::new(format!("Confirm {kind} `{name}` exists in this namespace.")).action(
                        StepAction::Open {
                            resource: resource.into(),
                            namespace: pod.namespace(),
                            name,
                        },
                    ),
                );
            }
        } else {
            steps.push(Step::new(
                "The container runtime could not create the pod sandbox. This is a node-level \
                 fault — usually CNI (no IP available, plugin not ready) or the runtime itself.",
            ));
            if let Some(node) = spec.and_then(|s| s.node_name.clone()) {
                steps.push(
                    Step::new("Look at the node it landed on.").action(StepAction::Open {
                        resource: "core/v1/nodes".into(),
                        namespace: None,
                        name: node,
                    }),
                );
            }
        }

        out.push(Finding {
            severity: "error".into(),
            code: if volume {
                "VolumeMountFailed".into()
            } else {
                "SandboxFailed".into()
            },
            title: if volume {
                "Volume will not mount".into()
            } else {
                "Pod sandbox could not be created".into()
            },
            explanation: "The pod is scheduled, but the kubelet has not reached the point of \
                          starting containers."
                .into(),
            container: None,
            evidence: vec![format!("{}: {}", event.reason, event.message)],
            steps,
        });
    }
}

/// Turn the scheduler's rejection tally into advice about the specific
/// constraint that caused it.
fn scheduling_steps(message: &str, spec: Option<&PodSpec>) -> Vec<Step> {
    let mut steps = Vec::new();
    let lower = message.to_lowercase();

    if lower.contains("insufficient") {
        let requests = requests_summary(spec);
        steps.push(
            Step::new(format!(
                "No node has enough free capacity for what this pod requests{requests}. Lower the \
                 requests, free capacity by scaling something down, or add a node."
            ))
            .action(StepAction::Edit),
        );
        steps.push(Step::new(
            "Requests are what the scheduler counts, not usage. A cluster can look idle in \
             metrics and still be full.",
        ));
    }
    if lower.contains("taint") {
        let tolerations = spec
            .and_then(|s| s.tolerations.as_ref())
            .map(|t| t.len())
            .unwrap_or(0);
        steps.push(Step::new(format!(
            "Nodes carry taints this pod does not tolerate (it has {tolerations} toleration(s)). \
             Add a matching toleration, or untaint the nodes."
        )));
    }
    if lower.contains("affinity") || lower.contains("selector") {
        let selector = spec
            .and_then(|s| s.node_selector.as_ref())
            .map(|s| {
                s.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let detail = if selector.is_empty() {
            "Check `spec.affinity` — the labels it requires may not exist on any node.".to_string()
        } else {
            format!(
                "`nodeSelector: {selector}` matches no node with room. Check the labels nodes actually carry."
            )
        };
        steps.push(Step::new(detail).command("kubectl get nodes --show-labels"));
    }
    if lower.contains("volume") {
        steps.push(Step::new(
            "A volume pins the pod to one zone or node — a bound PVC cannot move. The node it is \
             bound to must be the one with room.",
        ));
    }
    if lower.contains("didn't match pod anti-affinity") || lower.contains("anti-affinity") {
        steps.push(Step::new(
            "Anti-affinity refuses to co-locate replicas. With fewer eligible nodes than replicas, \
             the surplus stays Pending by design.",
        ));
    }
    steps
}

fn check_containers(
    pod: &Pod,
    spec: Option<&PodSpec>,
    status: Option<&PodStatus>,
    events: &[EventRow],
    out: &mut Vec<Finding>,
) {
    let Some(status) = status else { return };

    let init: Vec<(&ContainerStatus, bool)> = status
        .init_container_statuses
        .iter()
        .flatten()
        .map(|c| (c, true))
        .collect();
    let app: Vec<(&ContainerStatus, bool)> = status
        .container_statuses
        .iter()
        .flatten()
        .map(|c| (c, false))
        .collect();

    // Init containers run to completion before app containers start, so an
    // init failure is the cause and the app container's `PodInitializing` is
    // only the symptom. Report them in that order.
    for (container, is_init) in init.into_iter().chain(app) {
        container_finding(pod, spec, container, is_init, events, out);
    }
}

fn container_finding(
    pod: &Pod,
    spec: Option<&PodSpec>,
    container: &ContainerStatus,
    is_init: bool,
    events: &[EventRow],
    out: &mut Vec<Finding>,
) {
    let name = container.name.clone();
    let label = if is_init {
        "init container"
    } else {
        "container"
    };
    let waiting = container
        .state
        .as_ref()
        .and_then(|s| s.waiting.as_ref())
        .cloned();
    let reason = waiting
        .as_ref()
        .and_then(|w| w.reason.clone())
        .unwrap_or_default();
    let waiting_message = waiting
        .as_ref()
        .and_then(|w| w.message.clone())
        .unwrap_or_default();

    match reason.as_str() {
        "ImagePullBackOff" | "ErrImagePull" | "ImageInspectError" | "RegistryUnavailable" => {
            let mut evidence = vec![
                format!("{name}: {reason}"),
                format!("image: {}", container.image),
            ];
            if !waiting_message.is_empty() {
                evidence.push(waiting_message.clone());
            }
            if let Some(event) = latest(events, &["Failed", "BackOff"]) {
                evidence.push(format!("{}: {}", event.reason, event.message));
            }

            let combined = format!("{waiting_message} {}", evidence.join(" ")).to_lowercase();
            let mut steps = Vec::new();

            if combined.contains("not found")
                || combined.contains("manifest unknown")
                || combined.contains("does not exist")
            {
                steps.push(Step::new(format!(
                    "The registry answered, but has no such tag. Check `{}` — a typo or a tag that \
                     was never pushed looks exactly like this.",
                    container.image
                )));
            }
            if combined.contains("unauthorized")
                || combined.contains("authentication required")
                || combined.contains("denied")
                || combined.contains("forbidden")
            {
                let secrets = spec
                    .and_then(|s| s.image_pull_secrets.as_ref())
                    .map(|s| {
                        s.iter()
                            .map(|r| r.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                steps.push(Step::new(if secrets.is_empty() {
                    "The registry rejected an anonymous pull and the pod has no `imagePullSecrets`. \
                     Create a docker-registry Secret in this namespace and reference it from the \
                     pod template."
                        .to_string()
                } else {
                    format!(
                        "The pod references imagePullSecrets `{secrets}`. Confirm those Secrets \
                         exist in this namespace — a pull secret is namespaced, so copying the \
                         workload to a new namespace silently loses it."
                    )
                }));
                steps.push(
                    Step::new("List the pull secrets available here.").command(format!(
                        "kubectl get secret -n {} --field-selector type=kubernetes.io/dockerconfigjson",
                        pod.namespace().unwrap_or_default()
                    )),
                );
            }
            if combined.contains("timeout")
                || combined.contains("connection refused")
                || combined.contains("no such host")
                || combined.contains("i/o timeout")
            {
                steps.push(Step::new(format!(
                    "The node could not reach the registry at all. Check egress and DNS from the \
                     node for host `{}`.",
                    registry_host(&container.image)
                )));
            }
            if steps.is_empty() {
                steps.push(Step::new(
                    "Read the message above verbatim — the registry's own wording says whether \
                     this is a missing tag, a credential problem, or a network one.",
                ));
            }
            steps.push(
                Step::new(
                    "Fix the image or the pull secret on the owning workload, not on this pod.",
                )
                .action(StepAction::Edit),
            );
            steps.push(
                Step::new("Verify the tag exists from a machine with the same credentials.")
                    .command(format!("docker manifest inspect {}", container.image)),
            );

            out.push(Finding {
                severity: "error".into(),
                code: "ImagePullFailed".into(),
                title: format!("Cannot pull the image for {label} `{name}`"),
                explanation: "The kubelet asked the registry for this image and did not get it. \
                              Nothing about the application has run yet."
                    .into(),
                container: Some(name),
                evidence,
                steps,
            });
        }

        "InvalidImageName" => out.push(Finding {
            severity: "error".into(),
            code: "InvalidImageName".into(),
            title: format!("Image reference for {label} `{name}` is not valid"),
            explanation: "The string in `image:` is malformed, so the kubelet never got as far as \
                          contacting a registry."
                .into(),
            container: Some(name),
            evidence: vec![format!("image: {}", container.image), waiting_message],
            steps: vec![
                Step::new(
                    "Look for an unsubstituted template variable, a stray space, or an uppercase \
                     letter in the repository part — repository names must be lowercase.",
                )
                .action(StepAction::Edit),
            ],
        }),

        "CreateContainerConfigError" => {
            let missing = missing_reference(&waiting_message);
            let mut steps = vec![Step::new(
                "The container's environment or volumes reference a ConfigMap, Secret or key that \
                 does not exist. The message names it.",
            )];
            if let Some(reference) = missing.clone() {
                let resource = if reference.kind == "Secret" {
                    "core/v1/secrets"
                } else {
                    "core/v1/configmaps"
                };
                steps.push(
                    Step::new(match reference.key.as_deref() {
                        // The object exists; only the key is missing. Saying
                        // "create it" here would be wrong advice.
                        Some(key) => format!(
                            "{} `{}` exists but has no key `{key}`. Add the key, or point the \
                             reference at one that is there.",
                            reference.kind, reference.name
                        ),
                        None => format!(
                            "Open {} `{}` — or create it if it is missing.",
                            reference.kind, reference.name
                        ),
                    })
                    .action(StepAction::Open {
                        resource: resource.into(),
                        namespace: pod.namespace(),
                        name: reference.name.clone(),
                    }),
                );
            }
            steps.push(Step::new(
                "A reference that exists in one namespace and not another is the usual cause after \
                 a copy-paste of a manifest. Mark the reference `optional: true` only if the \
                 application really can start without it.",
            ));

            out.push(Finding {
                severity: "error".into(),
                code: "CreateContainerConfigError".into(),
                title: format!("Configuration for {label} `{name}` cannot be resolved"),
                explanation: "The image is present; the kubelet cannot assemble the container's \
                              config from the objects the spec points at."
                    .into(),
                container: Some(name.clone()),
                evidence: vec![format!("{name}: {reason}"), waiting_message],
                steps,
            });
        }

        "CreateContainerError" | "RunContainerError" => out.push(Finding {
            severity: "error".into(),
            code: "CreateContainerError".into(),
            title: format!("Runtime refused to start {label} `{name}`"),
            explanation:
                "The container runtime rejected the container definition — a bad command, \
                          a mount that collides with an existing path, or a security constraint."
                    .into(),
            container: Some(name.clone()),
            evidence: vec![format!("{name}: {reason}"), waiting_message.clone()],
            steps: vec![
                Step::new(
                    "Read the message: `executable file not found` means the `command` is not in \
                     the image, and `is a directory` or `not a directory` means a volumeMount \
                     collides with a path the image already has.",
                ),
                Step::new("Compare the command and mounts against what the image ships.")
                    .action(StepAction::Edit),
            ],
        }),

        "CrashLoopBackOff" => {
            let terminated = container
                .last_state
                .as_ref()
                .and_then(|s| s.terminated.as_ref());
            let exit_code = terminated.map(|t| t.exit_code);
            let term_reason = terminated
                .and_then(|t| t.reason.clone())
                .unwrap_or_default();

            let mut evidence = vec![format!(
                "{name}: CrashLoopBackOff after {} restart(s)",
                container.restart_count
            )];
            if let Some(code) = exit_code {
                evidence.push(format!(
                    "last exit code {code}{}{}",
                    if term_reason.is_empty() {
                        String::new()
                    } else {
                        format!(" ({term_reason})")
                    },
                    exit_meaning(code)
                        .map(|m| format!(" — {m}"))
                        .unwrap_or_default()
                ));
            }
            if let Some(message) = terminated.and_then(|t| t.message.clone()) {
                evidence.push(message);
            }

            let mut steps = vec![
                Step::new(
                    "Read the previous instance's logs. The running container is asleep in \
                     backoff; only the previous one holds the error.",
                )
                .command(format!(
                    "kubectl logs {} -n {} -c {name} --previous",
                    pod.name_any(),
                    pod.namespace().unwrap_or_default()
                ))
                .action(StepAction::Logs {
                    container: Some(container.name.clone()),
                    previous: true,
                }),
            ];

            if term_reason == "OOMKilled" || exit_code == Some(137) {
                steps.push(Step::new(format!(
                    "It was killed for exceeding its memory limit{}. Raise the limit, or find what \
                     grows — an OOM kill is instant and leaves no error in the log.",
                    memory_limit(spec, &container.name)
                        .map(|l| format!(" ({l})"))
                        .unwrap_or_default()
                )));
                steps.push(
                    Step::new("Compare live usage against the limit before changing it.")
                        .action(StepAction::Edit),
                );
            } else if exit_code == Some(127) {
                steps.push(Step::new(
                    "Exit 127 is `command not found`: the entrypoint or `command` does not exist \
                     in this image. Check it against the image, not against the host.",
                ));
            } else if exit_code == Some(126) {
                steps.push(Step::new(
                    "Exit 126 means the entrypoint is not executable — usually a script mounted \
                     without the execute bit, or with CRLF line endings.",
                ));
            } else if exit_code == Some(1) || exit_code == Some(2) {
                steps.push(Step::new(
                    "The application itself exited with an error. Its own log line is the answer; \
                     configuration and unreachable dependencies are the two common ones.",
                ));
            }

            if has_probe(spec, &container.name) {
                steps.push(Step::new(
                    "If the log shows the app starting normally and then dying, the liveness probe \
                     may be killing it before it is ready. Use a startupProbe rather than a long \
                     initialDelaySeconds.",
                ));
            }
            steps.push(
                Step::new(
                    "To inspect the filesystem, start a debug container — this pod's own shell is \
                     not up long enough to use.",
                )
                .action(StepAction::Terminal),
            );

            out.push(Finding {
                severity: "error".into(),
                code: "CrashLoopBackOff".into(),
                title: format!("{} `{name}` keeps crashing", capitalise(label)),
                explanation: "The container starts, exits, and is restarted with a growing delay \
                              (up to 5 minutes). The delay is the symptom; the exit is the problem."
                    .into(),
                container: Some(name),
                evidence,
                steps,
            });
        }

        "PodInitializing" if !is_init => {}

        "ContainerCreating" | "PodInitializing" | "" => {
            // Not itself a fault. Terminated states below still apply.
        }

        other => out.push(Finding {
            severity: "warning".into(),
            code: other.to_string(),
            title: format!("{} `{name}` is waiting: {other}", capitalise(label)),
            explanation:
                "The kubelet reports this container as waiting for a reason Kubernaut has \
                          no specific advice for."
                    .into(),
            container: Some(name),
            evidence: vec![format!("{other}: {waiting_message}")],
            steps: vec![Step::new(
                "Check the pod's events for the underlying cause.",
            )],
        }),
    }

    // A terminated container that is not in backoff yet still explains itself.
    if let Some(terminated) = container.state.as_ref().and_then(|s| s.terminated.as_ref())
        && terminated.exit_code != 0
    {
        let term_reason = terminated.reason.clone().unwrap_or_default();
        let oom = term_reason == "OOMKilled";
        let message = terminated.message.clone().unwrap_or_default();
        // A container that never started has no application log to read, so
        // the usual "check the logs" step would send the reader nowhere.
        let start_error = term_reason == "StartError"
            || message.contains("OCI runtime create failed")
            || message.contains("unable to start container process");

        let mut steps = if start_error {
            start_error_steps(&message)
        } else {
            vec![
                Step::new("Read what it printed before it died.")
                    .action(StepAction::Logs {
                        container: Some(container.name.clone()),
                        previous: false,
                    })
                    .command(format!(
                        "kubectl logs {} -n {} -c {}",
                        pod.name_any(),
                        pod.namespace().unwrap_or_default(),
                        container.name
                    )),
            ]
        };
        if oom {
            steps.push(Step::new(format!(
                "OOMKilled means the kernel killed it for exceeding `limits.memory`{}. There is no \
                 error in the application log — it was stopped mid-instruction.",
                memory_limit(spec, &container.name)
                    .map(|l| format!(" ({l})"))
                    .unwrap_or_default()
            )));
            steps.push(
                Step::new(
                    "Raise the limit on the owning workload, or reduce what the process holds.",
                )
                .action(StepAction::Edit),
            );
        }

        out.push(Finding {
            severity: "error".into(),
            code: if oom {
                "OOMKilled".into()
            } else if start_error {
                "StartError".into()
            } else {
                "ContainerExited".into()
            },
            title: if oom {
                format!(
                    "{} `{}` was killed for using too much memory",
                    capitalise(label),
                    container.name
                )
            } else if start_error {
                format!("{} `{}` never started", capitalise(label), container.name)
            } else {
                format!(
                    "{} `{}` exited {}",
                    capitalise(label),
                    container.name,
                    terminated.exit_code
                )
            },
            explanation: if start_error {
                "The container runtime failed before the process ran. There is no application log \
                 for this attempt — the message below is the runtime's."
                    .into()
            } else {
                exit_meaning(terminated.exit_code)
                    .unwrap_or("The container stopped on its own with a non-zero status.")
                    .to_string()
            },
            container: Some(container.name.clone()),
            evidence: {
                let mut evidence = vec![format!(
                    "exit code {}{}",
                    terminated.exit_code,
                    if term_reason.is_empty() {
                        String::new()
                    } else {
                        format!(" ({term_reason})")
                    }
                )];
                // How long it ran narrows the cause more than the exit code
                // alone: seconds rules out a timeout, instant rules out the
                // application having done any work at all.
                if let Some(ran) = ran_for(terminated) {
                    evidence.push(format!("ran for {ran} before exiting"));
                }
                if let Some(message) = terminated.message.clone() {
                    evidence.push(message);
                }
                evidence
            },
            steps,
        });
    }

    // An init container that has not finished blocks everything after it.
    if is_init
        && container
            .state
            .as_ref()
            .and_then(|s| s.running.as_ref())
            .is_some()
    {
        out.push(Finding {
            severity: "warning".into(),
            code: "InitContainerRunning".into(),
            title: format!("Init container `{}` has not finished", container.name),
            explanation: "App containers do not start until every init container exits 0. An init \
                          container that waits for a dependency will hold the pod here \
                          indefinitely."
                .into(),
            container: Some(container.name.clone()),
            evidence: vec![format!("{}: running, not completed", container.name)],
            steps: vec![
                Step::new("Read this init container's logs — it is usually waiting for something.")
                    .action(StepAction::Logs {
                        container: Some(container.name.clone()),
                        previous: false,
                    })
                    .command(format!(
                        "kubectl logs {} -n {} -c {}",
                        pod.name_any(),
                        pod.namespace().unwrap_or_default(),
                        container.name
                    )),
                Step::new(
                    "Whatever it polls — a database, a migration, another Service — is the actual \
                     thing to fix.",
                ),
            ],
        });
    }
}

/// Advice for a container the runtime refused to start. The runc message is
/// specific enough to name the cause, and generic log-reading advice is not
/// useful here because nothing was ever logged.
fn start_error_steps(message: &str) -> Vec<Step> {
    let lower = message.to_lowercase();
    let mut steps = Vec::new();

    if lower.contains("executable file not found") {
        steps.push(Step::new(
            "The entrypoint or `command` does not exist inside the image. Check it against the \
             image's own filesystem, not the host's — `sh` is absent from distroless and scratch \
             images.",
        ));
    } else if lower.contains("permission denied") {
        steps.push(Step::new(
            "The entrypoint is not executable, or `runAsUser` cannot execute it. A script mounted \
             from a ConfigMap arrives without the execute bit unless `defaultMode` sets it.",
        ));
    } else if lower.contains("no such file or directory") {
        steps.push(Step::new(
            "A path in the container spec does not exist in the image — usually the entrypoint \
             itself, or a script the entrypoint sources.",
        ));
    } else if lower.contains("operation not permitted") || lower.contains("permission") {
        steps.push(Step::new(
            "The runtime blocked the process: check `securityContext`, dropped capabilities, and \
             any seccomp or AppArmor profile applied to this pod.",
        ));
    } else {
        steps.push(Step::new(
            "Read the runtime message above verbatim — it names the exact syscall or path that \
             failed. This is a node-and-image problem, not an application one.",
        ));
    }

    steps.push(
        Step::new("Fix the command, mounts or securityContext on the owning workload.")
            .action(StepAction::Edit),
    );
    steps
}

fn check_readiness(
    spec: Option<&PodSpec>,
    status: Option<&PodStatus>,
    events: &[EventRow],
    out: &mut Vec<Finding>,
) {
    let Some(status) = status else { return };

    for container in status.container_statuses.iter().flatten() {
        if container.ready {
            continue;
        }
        let running = container
            .state
            .as_ref()
            .and_then(|s| s.running.as_ref())
            .is_some();
        if !running {
            continue;
        }

        let probe = spec
            .map(|s| s.containers.as_slice())
            .unwrap_or_default()
            .iter()
            .find(|c| c.name == container.name)
            .and_then(|c: &Container| c.readiness_probe.clone());

        let mut evidence = vec![format!("{}: running but not ready", container.name)];
        if let Some(probe) = probe.as_ref() {
            evidence.push(format!("readinessProbe: {}", describe_probe(probe)));
        }
        if let Some(event) = latest(events, &["Unhealthy", "ProbeWarning"]) {
            evidence.push(format!("{}: {}", event.reason, event.message));
        }

        let mut steps = Vec::new();
        if probe.is_some() {
            steps.push(Step::new(
                "The readiness probe is failing, so the pod is excluded from Service endpoints — \
                 traffic will not reach it even though the process is up.",
            ));
            steps.push(Step::new(
                "Check the probe's port and path against what the application actually serves. A \
                 probe on the wrong port fails identically to a dead application.",
            ));
            steps.push(
                Step::new("Try the probe by hand from inside the container.")
                    .action(StepAction::Terminal),
            );
            steps.push(
                Step::new(
                    "If the app is simply slow to start, raise `initialDelaySeconds` or add a \
                     startupProbe rather than loosening the readiness threshold.",
                )
                .action(StepAction::Edit),
            );
        } else {
            steps.push(Step::new(
                "No readiness probe is defined, so the kubelet is reporting the container as not \
                 ready on its own — check the logs for what it is doing.",
            ));
        }
        steps.push(
            Step::new("Read the current logs.").action(StepAction::Logs {
                container: Some(container.name.clone()),
                previous: false,
            }),
        );

        out.push(Finding {
            severity: "warning".into(),
            code: "NotReady".into(),
            title: format!("Container `{}` is running but not ready", container.name),
            explanation: "The pod counts as unavailable: Services skip it, and a rollout waits on \
                          it."
            .into(),
            container: Some(container.name.clone()),
            evidence,
            steps,
        });
    }
}

// ---------------------------------------------------------------- helpers

/// Newest event whose reason is one of `reasons`.
fn latest<'a>(events: &'a [EventRow], reasons: &[&str]) -> Option<&'a EventRow> {
    events
        .iter()
        .filter(|event| reasons.contains(&event.reason.as_str()))
        .max_by(|a, b| a.last_seen.cmp(&b.last_seen))
}

/// How long a terminated container ran, as a short human string.
fn ran_for(terminated: &k8s_openapi::api::core::v1::ContainerStateTerminated) -> Option<String> {
    let started = terminated.started_at.as_ref()?;
    let finished = terminated.finished_at.as_ref()?;
    // A container the runtime refused gets a zero `startedAt` — the epoch, or
    // year one — because it never ran. Subtracting from that yields decades.
    if started.0.as_second() <= 0 || finished.0.as_second() <= 0 {
        return None;
    }
    let seconds = finished.0.as_second() - started.0.as_second();
    if seconds < 0 {
        return None;
    }
    Some(match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60),
    })
}

/// What a non-zero exit status conventionally means.
fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "The container finished successfully.",
        1 => "A generic application error: the process chose to exit non-zero.",
        2 => "Shell misuse — often a bad flag in the entrypoint.",
        126 => "The entrypoint exists but is not executable.",
        127 => "Command not found: the entrypoint is not present in the image.",
        128 => "Invalid exit argument.",
        137 => "Killed with SIGKILL — an out-of-memory kill, or a stop that ignored SIGTERM.",
        139 => "Segmentation fault (SIGSEGV) inside the process.",
        143 => "Terminated with SIGTERM — a normal shutdown request the process obeyed.",
        _ => return None,
    })
}

fn capitalise(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Registry hostname an image pulls from, for a network hint.
fn registry_host(image: &str) -> String {
    let head = image.split('/').next().unwrap_or_default();
    if image.contains('/') && (head.contains('.') || head.contains(':') || head == "localhost") {
        head.to_string()
    } else {
        "docker.io".into()
    }
}

/// The object a `CreateContainerConfigError` is about.
#[derive(Debug, Clone, PartialEq)]
struct MissingRef {
    /// `Secret` or `ConfigMap`.
    kind: String,
    name: String,
    /// Set when the object exists but the key inside it does not — a different
    /// fix from creating the object.
    key: Option<String>,
}

/// Pull the missing object out of a kubelet message.
///
/// The kubelet writes two different shapes, and they mean different things:
/// `configmap "x" not found` (the object is absent) and `couldn't find key K
/// in Secret ns/name` (the object is there, the key is not).
fn missing_reference(message: &str) -> Option<MissingRef> {
    let kind_of = |text: &str| {
        let lower = text.to_lowercase();
        if lower.contains("secret") {
            Some("Secret")
        } else if lower.contains("configmap") {
            Some("ConfigMap")
        } else {
            None
        }
    };

    // `couldn't find key DB_PASSWORD in Secret app/credentials`
    if let Some(rest) = message.split("find key ").nth(1)
        && let Some((key, tail)) = rest.split_once(" in ")
    {
        let kind = kind_of(tail)?;
        let name = tail
            .split_whitespace()
            .nth(1)
            .map(|reference| reference.rsplit('/').next().unwrap_or(reference))
            .unwrap_or_default();
        if !name.is_empty() {
            return Some(MissingRef {
                kind: kind.into(),
                name: name.to_string(),
                key: Some(key.trim().to_string()),
            });
        }
    }

    // `configmap "api-settings" not found`
    let kind = kind_of(message)?;
    let name = message
        .split('"')
        .nth(1)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(MissingRef {
        kind: kind.into(),
        name,
        key: None,
    })
}

/// ConfigMaps and Secrets mounted as volumes, for the volume-failure hints.
fn mounted_config(spec: Option<&PodSpec>) -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    for volume in spec.and_then(|s| s.volumes.as_ref()).into_iter().flatten() {
        if let Some(map) = volume.config_map.as_ref() {
            out.push(("ConfigMap", map.name.clone(), "core/v1/configmaps"));
        }
        if let Some(secret) = volume.secret.as_ref()
            && let Some(name) = secret.secret_name.clone()
        {
            out.push(("Secret", name, "core/v1/secrets"));
        }
    }
    out.truncate(6);
    out
}

/// ` (500m CPU, 1Gi memory)` for the scheduling advice, or empty when the pod
/// requests nothing — which is itself worth not claiming.
fn requests_summary(spec: Option<&PodSpec>) -> String {
    let mut cpu = Vec::new();
    let mut memory = Vec::new();
    for container in spec.map(|s| s.containers.as_slice()).unwrap_or_default() {
        let requests = container
            .resources
            .as_ref()
            .and_then(|r| r.requests.as_ref());
        if let Some(value) = requests.and_then(|r| r.get("cpu")) {
            cpu.push(value.0.clone());
        }
        if let Some(value) = requests.and_then(|r| r.get("memory")) {
            memory.push(value.0.clone());
        }
    }
    let mut parts = Vec::new();
    if !cpu.is_empty() {
        parts.push(format!("cpu {}", cpu.join("+")));
    }
    if !memory.is_empty() {
        parts.push(format!("memory {}", memory.join("+")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

fn memory_limit(spec: Option<&PodSpec>, container: &str) -> Option<String> {
    spec?
        .containers
        .iter()
        .chain(spec?.init_containers.iter().flatten())
        .find(|c| c.name == container)?
        .resources
        .as_ref()?
        .limits
        .as_ref()?
        .get("memory")
        .map(|q| format!("limits.memory = {}", q.0))
}

fn has_probe(spec: Option<&PodSpec>, container: &str) -> bool {
    spec.map(|s| s.containers.as_slice())
        .unwrap_or_default()
        .iter()
        .find(|c| c.name == container)
        .is_some_and(|c| c.liveness_probe.is_some())
}

/// A probe in one line, so the reader can compare it with the app's real port
/// without opening the manifest.
fn describe_probe(probe: &Probe) -> String {
    let port = |port: &IntOrString| match port {
        IntOrString::Int(value) => value.to_string(),
        IntOrString::String(value) => value.clone(),
    };

    let target = if let Some(http) = probe.http_get.as_ref() {
        format!(
            "{}://:{}{}",
            http.scheme
                .clone()
                .unwrap_or_else(|| "HTTP".into())
                .to_lowercase(),
            port(&http.port),
            http.path.clone().unwrap_or_else(|| "/".into())
        )
    } else if let Some(tcp) = probe.tcp_socket.as_ref() {
        format!("tcp :{}", port(&tcp.port))
    } else if let Some(exec) = probe.exec.as_ref() {
        format!(
            "exec {}",
            exec.command.clone().unwrap_or_default().join(" ")
        )
    } else if let Some(grpc) = probe.grpc.as_ref() {
        format!("grpc :{}", grpc.port)
    } else {
        "no handler".into()
    };

    format!(
        "{target}, delay {}s, period {}s, {} failure(s) to fail",
        probe.initial_delay_seconds.unwrap_or(0),
        probe.period_seconds.unwrap_or(10),
        probe.failure_threshold.unwrap_or(3)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn pod(value: Value) -> Pod {
        serde_json::from_value(value).expect("pod fixture")
    }

    fn event(reason: &str, message: &str) -> EventRow {
        EventRow {
            kind: "Warning".into(),
            reason: reason.into(),
            message: message.into(),
            count: 1,
            first_seen: None,
            last_seen: Some("2026-08-20T14:00:00Z".into()),
            source: None,
            object: "pod/x".into(),
        }
    }

    fn find<'a>(diagnosis: &'a Diagnosis, code: &str) -> &'a Finding {
        diagnosis
            .findings
            .iter()
            .find(|f| f.code == code)
            .unwrap_or_else(|| {
                panic!(
                    "no `{code}` finding; got {:?}",
                    diagnosis
                        .findings
                        .iter()
                        .map(|f| f.code.as_str())
                        .collect::<Vec<_>>()
                )
            })
    }

    #[test]
    fn a_healthy_pod_produces_nothing() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "web-1", "namespace": "app" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "web", "image": "nginx:1" }] },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "web", "image": "nginx:1", "ready": true, "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-08-20T13:00:00Z" } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        assert!(diagnosis.healthy, "{:?}", diagnosis.findings);
        assert!(diagnosis.findings.is_empty());
    }

    #[test]
    fn crash_loop_points_at_the_previous_logs() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": {
                "nodeName": "node-a",
                "containers": [{
                    "name": "api", "image": "api:1",
                    "resources": { "limits": { "memory": "256Mi" } }
                }]
            },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "api", "image": "api:1", "ready": false, "restartCount": 7,
                    "state": { "waiting": { "reason": "CrashLoopBackOff", "message": "back-off 5m0s" } },
                    "lastState": { "terminated": { "exitCode": 137, "reason": "OOMKilled", "finishedAt": "2026-08-20T13:59:00Z" } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        assert!(!diagnosis.healthy);

        let finding = find(&diagnosis, "CrashLoopBackOff");
        assert_eq!(finding.severity, "error");
        assert_eq!(finding.container.as_deref(), Some("api"));
        // The exit code is the whole diagnosis; it must be quoted, not summarised.
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.contains("137") && e.contains("OOMKilled")),
            "{:?}",
            finding.evidence
        );
        // Reading the *previous* instance is the one step that matters here.
        assert!(finding.steps.iter().any(|s| s.action
            == Some(StepAction::Logs {
                container: Some("api".into()),
                previous: true
            })));
        assert!(
            finding.steps.iter().any(|s| s.text.contains("256Mi")),
            "the memory limit it hit should be named"
        );
    }

    #[test]
    fn an_unauthorised_pull_without_a_secret_says_so() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "api", "image": "registry.example.com/team/api:1" }] },
            "status": {
                "phase": "Pending",
                "containerStatuses": [{
                    "name": "api", "image": "registry.example.com/team/api:1", "ready": false, "restartCount": 0,
                    "state": { "waiting": {
                        "reason": "ImagePullBackOff",
                        "message": "Back-off pulling image \"registry.example.com/team/api:1\": unauthorized: authentication required"
                    } }
                }]
            }
        }));

        let finding = {
            let diagnosis = analyse(&pod, &[], None);
            diagnosis
                .findings
                .iter()
                .find(|f| f.code == "ImagePullFailed")
                .cloned()
                .expect("image pull finding")
        };
        assert!(
            finding
                .steps
                .iter()
                .any(|s| s.text.contains("imagePullSecrets")),
            "{:?}",
            finding.steps.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_missing_tag_is_distinguished_from_a_credential_problem() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "api", "image": "api:typo" }] },
            "status": {
                "phase": "Pending",
                "containerStatuses": [{
                    "name": "api", "image": "api:typo", "ready": false, "restartCount": 0,
                    "state": { "waiting": { "reason": "ErrImagePull", "message": "manifest unknown" } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "ImagePullFailed");
        assert!(
            finding.steps[0].text.contains("no such tag"),
            "{:?}",
            finding.steps[0].text
        );
    }

    #[test]
    fn an_unschedulable_pod_quotes_the_scheduler() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": {
                "containers": [{
                    "name": "api", "image": "api:1",
                    "resources": { "requests": { "cpu": "4", "memory": "8Gi" } }
                }]
            },
            "status": {
                "phase": "Pending",
                "conditions": [{
                    "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                    "message": "0/3 nodes are available: 3 Insufficient cpu."
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "Unschedulable");
        assert!(finding.evidence[0].contains("Insufficient cpu"));
        // The advice must name what this pod actually asks for, or it is generic.
        assert!(
            finding
                .steps
                .iter()
                .any(|s| s.text.contains("cpu 4") && s.text.contains("memory 8Gi")),
            "{:?}",
            finding.steps.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_missing_configmap_links_to_the_object() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "api", "image": "api:1" }] },
            "status": {
                "phase": "Pending",
                "containerStatuses": [{
                    "name": "api", "image": "api:1", "ready": false, "restartCount": 0,
                    "state": { "waiting": {
                        "reason": "CreateContainerConfigError",
                        "message": "configmap \"api-settings\" not found"
                    } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "CreateContainerConfigError");
        assert!(finding.steps.iter().any(|s| s.action
            == Some(StepAction::Open {
                resource: "core/v1/configmaps".into(),
                namespace: Some("app".into()),
                name: "api-settings".into()
            })));
    }

    #[test]
    fn a_missing_key_is_not_reported_as_a_missing_object() {
        // Seen verbatim on a live cluster. The Secret exists; only the key is
        // absent, so "create it" would send the reader down the wrong path.
        let reference = missing_reference(
            "couldn't find key NOTIFY_WEBHOOK_URL in Secret platform-dev/platform-dev-secrets",
        )
        .expect("reference");
        assert_eq!(reference.kind, "Secret");
        assert_eq!(reference.name, "platform-dev-secrets");
        assert_eq!(reference.key.as_deref(), Some("NOTIFY_WEBHOOK_URL"));

        let absent = missing_reference("configmap \"shared-config\" not found").expect("reference");
        assert_eq!(absent.kind, "ConfigMap");
        assert_eq!(absent.name, "shared-config");
        assert_eq!(absent.key, None);
    }

    #[test]
    fn an_unfinished_init_container_is_the_cause_not_the_app_container() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": {
                "nodeName": "node-a",
                "initContainers": [{ "name": "wait-db", "image": "busybox" }],
                "containers": [{ "name": "api", "image": "api:1" }]
            },
            "status": {
                "phase": "Pending",
                "initContainerStatuses": [{
                    "name": "wait-db", "image": "busybox", "ready": false, "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-08-20T13:00:00Z" } }
                }],
                "containerStatuses": [{
                    "name": "api", "image": "api:1", "ready": false, "restartCount": 0,
                    "state": { "waiting": { "reason": "PodInitializing" } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "InitContainerRunning");
        assert_eq!(finding.container.as_deref(), Some("wait-db"));
        // `PodInitializing` on the app container is noise once the init
        // container is named; it must not become a second finding.
        assert!(
            !diagnosis
                .findings
                .iter()
                .any(|f| f.code == "PodInitializing"),
            "{:?}",
            diagnosis
                .findings
                .iter()
                .map(|f| &f.code)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_failing_readiness_probe_describes_the_probe() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": {
                "nodeName": "node-a",
                "containers": [{
                    "name": "api", "image": "api:1",
                    "readinessProbe": {
                        "httpGet": { "path": "/healthz", "port": 8080 },
                        "initialDelaySeconds": 5, "periodSeconds": 10, "failureThreshold": 3
                    }
                }]
            },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "api", "image": "api:1", "ready": false, "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-08-20T13:00:00Z" } }
                }]
            }
        }));

        let events = vec![event(
            "Unhealthy",
            "Readiness probe failed: connection refused",
        )];
        let diagnosis = analyse(&pod, &events, None);
        let finding = find(&diagnosis, "NotReady");
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.contains("/healthz") && e.contains("8080")),
            "{:?}",
            finding.evidence
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.contains("connection refused"))
        );
    }

    #[test]
    fn an_eviction_is_reported_against_the_node() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "api", "image": "api:1" }] },
            "status": {
                "phase": "Failed", "reason": "Evicted",
                "message": "The node was low on resource: ephemeral-storage."
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "Evicted");
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.contains("ephemeral-storage"))
        );
    }

    #[test]
    fn a_finished_job_pod_is_not_a_problem() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {
                "name": "backup-1", "namespace": "app",
                "ownerReferences": [{ "apiVersion": "batch/v1", "kind": "Job", "name": "backup", "uid": "u" }]
            },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "backup", "image": "backup:1" }] },
            "status": {
                "phase": "Succeeded",
                "containerStatuses": [{
                    "name": "backup", "image": "backup:1", "ready": false, "restartCount": 0,
                    "state": { "terminated": { "exitCode": 0, "reason": "Completed", "finishedAt": "2026-08-20T13:00:00Z" } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        // Info-only: a completed Job pod must not turn the tab red.
        assert!(diagnosis.healthy, "{:?}", diagnosis.findings);
        assert_eq!(find(&diagnosis, "Succeeded").severity, "info");
    }

    #[test]
    fn a_not_ready_node_outranks_the_container_symptom() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "api", "image": "api:1" }] },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "api", "image": "api:1", "ready": false, "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-08-20T13:00:00Z" } }
                }]
            }
        }));
        let node: Node = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Node",
            "metadata": { "name": "node-a" },
            "status": { "conditions": [{ "type": "Ready", "status": "False", "lastHeartbeatTime": "2026-08-20T13:00:00Z", "lastTransitionTime": "2026-08-20T13:00:00Z", "reason": "KubeletNotReady", "message": "kubelet stopped posting" }] }
        }))
        .unwrap();

        let diagnosis = analyse(&pod, &[], Some(&node));
        // Both are true, but the node is the cause — it must lead the list so
        // nobody debugs the application for an hour.
        assert_eq!(diagnosis.findings[0].code, "NodeNotReady");
        assert!(diagnosis.summary.contains("node-a"));
    }

    #[test]
    fn a_container_the_runtime_refused_does_not_send_the_reader_to_the_logs() {
        // Message shortened from a live cluster.
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "bot-1", "namespace": "notifications" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "bot", "image": "bot:1" }] },
            "status": {
                "phase": "Failed",
                "containerStatuses": [{
                    "name": "bot", "image": "bot:1", "ready": false, "restartCount": 0,
                    "state": { "terminated": {
                        "exitCode": 128, "reason": "StartError",
                        "finishedAt": "2026-08-20T13:00:00Z",
                        "message": "failed to create containerd task: OCI runtime create failed: runc create failed: unable to start container process: exec: \"/app/run.sh\": executable file not found in $PATH"
                    } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "StartError");
        assert!(finding.title.contains("never started"));
        assert!(
            !finding
                .steps
                .iter()
                .any(|s| matches!(s.action, Some(StepAction::Logs { .. }))),
            "there is no log for a container that never ran"
        );
        assert!(
            finding.steps[0]
                .text
                .contains("does not exist inside the image")
        );

        // The generic `PodFailed` adds nothing once the container is named.
        assert!(
            !diagnosis.findings.iter().any(|f| f.code == "PodFailed"),
            "{:?}",
            diagnosis
                .findings
                .iter()
                .map(|f| &f.code)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_exited_container_reports_how_long_it_ran() {
        // Values from a real CronJob pod: 48 seconds then exit 1. That rules
        // out a start failure (instant) and a deadline (would be minutes), so
        // the duration is worth stating alongside the code.
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "cleanup-1", "namespace": "production" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "cleanup", "image": "cleanup:1" }] },
            "status": {
                "phase": "Failed",
                "containerStatuses": [{
                    "name": "cleanup", "image": "cleanup:1", "ready": false, "restartCount": 0,
                    "state": { "terminated": {
                        "exitCode": 1, "reason": "Error",
                        "startedAt": "2026-06-07T04:00:20Z",
                        "finishedAt": "2026-06-07T04:01:08Z"
                    } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "ContainerExited");
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e == "ran for 48s before exiting"),
            "{:?}",
            finding.evidence
        );
    }

    #[test]
    fn a_container_that_never_ran_reports_no_duration() {
        // A live cluster writes `startedAt: 1970-01-01T00:00:00Z` for a
        // container the runtime refused. Subtracting from that printed
        // "ran for 493728h" before this guard existed.
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "bot-1", "namespace": "notifications" },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "bot", "image": "bot:1" }] },
            "status": {
                "phase": "Failed",
                "containerStatuses": [{
                    "name": "bot", "image": "bot:1", "ready": false, "restartCount": 0,
                    "state": { "terminated": {
                        "exitCode": 128, "reason": "StartError",
                        "startedAt": "1970-01-01T00:00:00Z",
                        "finishedAt": "2026-08-20T13:00:00Z"
                    } }
                }]
            }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "StartError");
        assert!(
            !finding.evidence.iter().any(|e| e.contains("ran for")),
            "{:?}",
            finding.evidence
        );
    }

    #[test]
    fn a_stuck_finalizer_is_named() {
        let pod = pod(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {
                "name": "api-1", "namespace": "app",
                "deletionTimestamp": "2026-08-20T13:00:00Z",
                "finalizers": ["example.com/drain"]
            },
            "spec": { "nodeName": "node-a", "containers": [{ "name": "api", "image": "api:1" }] },
            "status": { "phase": "Running" }
        }));

        let diagnosis = analyse(&pod, &[], None);
        let finding = find(&diagnosis, "Terminating");
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.contains("example.com/drain"))
        );
    }

    #[test]
    fn exit_codes_have_meanings_worth_printing() {
        assert!(exit_meaning(127).unwrap().contains("Command not found"));
        assert!(exit_meaning(137).unwrap().contains("SIGKILL"));
        assert_eq!(exit_meaning(42), None);
    }

    #[test]
    fn registry_host_falls_back_to_docker_hub() {
        assert_eq!(registry_host("nginx"), "docker.io");
        assert_eq!(registry_host("library/nginx:1"), "docker.io");
        assert_eq!(registry_host("ghcr.io/org/app:1"), "ghcr.io");
        assert_eq!(registry_host("localhost:5000/app"), "localhost:5000");
    }
}
