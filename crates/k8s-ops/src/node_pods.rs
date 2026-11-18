//! A node's pod limit.
//!
//! The limit is `maxPods` in the node's kubelet configuration, not a field the
//! API accepts: `status.capacity.pods` is whatever the kubelet reports, and a
//! patch to it is overwritten. Changing it means editing the config on the node
//! and restarting the kubelet, so this runs a short-lived privileged pod that
//! enters the node's namespaces, as `kubectl debug node/<name>` does.

use std::{sync::Arc, time::Duration};

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::api::{Api, DeleteParams, ListParams, LogParams, PostParams};
use serde_json::json;

use crate::{
    error::{OpsError, Result},
    exec::{DEFAULT_DEBUG_IMAGE, short_suffix},
};

/// Most pods the kubelet accepts a limit for.
pub const MAX_LIMIT: u32 = 65_535;

/// The script runs and finishes well inside this; the rest is image pull.
const RUN_TIMEOUT: Duration = Duration::from_secs(180);
/// A kubelet restart takes seconds; this allows a slow node.
const RETURN_TIMEOUT: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(800);

/// What a node reports about its pods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PodLimit {
    /// `status.capacity.pods`: the kubelet's `maxPods`.
    pub capacity: u32,
    /// Pods scheduled to the node now, finished ones included.
    pub used: u32,
}

pub async fn read(cluster: &Arc<ClusterHandle>, node: &str) -> Result<PodLimit> {
    let capacity = capacity(cluster, node).await?;
    let pods: Api<Pod> = Api::all(cluster.client.clone());
    let params = ListParams::default().fields(&format!("spec.nodeName={node}"));
    let used = pods.list(&params).await?.items.len();
    Ok(PodLimit {
        capacity,
        used: u32::try_from(used).unwrap_or(u32::MAX),
    })
}

async fn capacity(cluster: &Arc<ClusterHandle>, node: &str) -> Result<u32> {
    let api: Api<Node> = Api::all(cluster.client.clone());
    let node = api.get(node).await?;
    Ok(reported_pods(&node).unwrap_or(0))
}

fn reported_pods(node: &Node) -> Option<u32> {
    node.status
        .as_ref()?
        .capacity
        .as_ref()?
        .get("pods")?
        .0
        .parse()
        .ok()
}

fn is_ready(node: &Node) -> bool {
    node.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|list| {
            list.iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
}

/// Why `limit` cannot be applied, if it cannot.
pub fn check_limit(limit: u32) -> Result<()> {
    if (1..=MAX_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(OpsError::other(format!(
            "the limit must be between 1 and {MAX_LIMIT}"
        )))
    }
}

/// Set the node's kubelet `maxPods` to `limit` and wait for the node to report
/// it. Restarts the kubelet: pods keep running, but the node goes NotReady for
/// a few seconds.
pub async fn set(
    cluster: &Arc<ClusterHandle>,
    node: &str,
    limit: u32,
    namespace: &str,
    confirmation: &str,
) -> Result<()> {
    if confirmation != node {
        return Err(OpsError::other(format!(
            "confirmation `{confirmation}` does not match node `{node}`; no pod was created"
        )));
    }
    check_limit(limit)?;

    let pods: Api<Pod> = Api::namespaced(cluster.client.clone(), namespace);
    let name = format!("kubernaut-max-pods-{}", short_suffix());
    pods.create(&PostParams::default(), &pod(&name, namespace, node, limit)?)
        .await?;

    let ran = run_to_end(&pods, &name).await;
    // The pod is privileged: it must not outlive the edit, whatever happened.
    pods.delete(&name, &DeleteParams::default().grace_period(0))
        .await
        .ok();
    ran?;

    wait_for_limit(cluster, node, limit).await
}

fn pod(name: &str, namespace: &str, node: &str, limit: u32) -> Result<Pod> {
    Ok(serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "kubernaut",
                "kubernaut.dev/purpose": "max-pods",
            },
        },
        "spec": {
            "nodeName": node,
            "hostPID": true,
            "restartPolicy": "Never",
            // The node being edited may be tainted; the pod must still run on it.
            "tolerations": [{ "operator": "Exists" }],
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "edit",
                "image": DEFAULT_DEBUG_IMAGE,
                "securityContext": { "privileged": true },
                "command": [
                    "nsenter", "--target", "1", "--mount", "--uts", "--ipc", "--net", "--pid",
                    "--", "sh", "-c", &script(limit)
                ],
            }],
        }
    }))?)
}

/// Runs in the node's mount namespace. Edits the kubelet's own config file when
/// it has one, else the k3s/RKE2 drop-in directory, then restarts the service.
/// Prints `kubernaut: ...` and exits non-zero when it cannot, which is what the
/// caller shows.
fn script(limit: u32) -> String {
    format!(
        r#"set -eu
n={limit}
fail() {{ echo "kubernaut: $1"; exit 1; }}
command -v systemctl >/dev/null 2>&1 || fail "this node has no systemd, so the service cannot be restarted from here"
restart() {{ systemctl restart "$1" || {{ eval "$2"; fail "could not restart $1; the change was undone"; }}; }}
args=""
for p in /proc/[0-9]*; do
  [ "$(cat "$p/comm" 2>/dev/null)" = kubelet ] || continue
  args=$(tr '\0' ' ' < "$p/cmdline"); break
done
if [ -n "$args" ]; then
  case "$args" in *--max-pods*) fail "the kubelet starts with --max-pods, which overrides the config file; change that flag instead";; esac
  cfg=$(echo "$args" | sed -n 's/.*--config[= ]\([^ ]*\).*/\1/p')
  [ -f "$cfg" ] || fail "the kubelet has no --config file to edit"
  cp "$cfg" "$cfg.kubernaut.bak"
  if grep -q '^maxPods:' "$cfg"; then sed -i "s/^maxPods:.*/maxPods: $n/" "$cfg"; else printf '\nmaxPods: %s\n' "$n" >> "$cfg"; fi
  restart kubelet "cp '$cfg.kubernaut.bak' '$cfg'"
  exit 0
fi
for d in k3s:k3s-agent rke2:rke2-agent; do
  dir=/etc/rancher/${{d%%:*}}
  [ -d "$dir" ] || continue
  svc=${{d##*:}}
  systemctl cat "$svc" >/dev/null 2>&1 || svc=${{d%%:*}}
  mkdir -p "$dir/config.yaml.d"
  drop="$dir/config.yaml.d/90-kubernaut-max-pods.yaml"
  printf 'kubelet-arg+:\n  - max-pods=%s\n' "$n" > "$drop"
  restart "$svc" "rm -f '$drop'"
  exit 0
done
fail "found no kubelet config file and no k3s or RKE2 install on this node"
"#
    )
}

/// Wait for the edit pod to finish; its last output is the reason if it fails.
async fn run_to_end(pods: &Api<Pod>, name: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(OpsError::other(format!(
                "pod `{name}` did not finish within {}s",
                RUN_TIMEOUT.as_secs()
            )));
        }
        let pod = pods.get(name).await?;
        let status = pod.status.as_ref();
        match status.and_then(|s| s.phase.as_deref()).unwrap_or("") {
            "Succeeded" => return Ok(()),
            "Failed" => {
                let log = pods
                    .logs(name, &LogParams::default())
                    .await
                    .unwrap_or_default();
                let reason = log
                    .trim()
                    .lines()
                    .last()
                    .unwrap_or("it exited with an error");
                return Err(OpsError::other(reason.trim_start_matches("kubernaut: ")));
            }
            _ => {}
        }
        if let Some(reason) = status
            .and_then(|s| s.container_statuses.as_ref())
            .and_then(|list| list.first())
            .and_then(|c| c.state.as_ref())
            .and_then(|s| s.waiting.as_ref())
            .and_then(|w| w.reason.as_deref())
            && matches!(
                reason,
                "ErrImagePull" | "ImagePullBackOff" | "InvalidImageName"
            )
        {
            return Err(OpsError::other(format!(
                "pod `{name}` cannot start: {reason}. Is {DEFAULT_DEBUG_IMAGE} reachable from this cluster?"
            )));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The restart is quick, but the node reports the new value only once the
/// kubelet is back and has updated its status.
async fn wait_for_limit(cluster: &Arc<ClusterHandle>, node: &str, limit: u32) -> Result<()> {
    let api: Api<Node> = Api::all(cluster.client.clone());
    let deadline = tokio::time::Instant::now() + RETURN_TIMEOUT;
    loop {
        if let Ok(current) = api.get(node).await
            && is_ready(&current)
            && reported_pods(&current) == Some(limit)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            return Err(OpsError::other(format!(
                "the kubelet was restarted but `{node}` does not report {limit} pods yet; \
                 check that it is Ready and reload"
            )));
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{check_limit, pod, script};

    #[test]
    fn limit_must_be_a_positive_port_sized_number() {
        assert!(check_limit(0).is_err());
        assert!(check_limit(1).is_ok());
        assert!(check_limit(250).is_ok());
        assert!(check_limit(65_535).is_ok());
        assert!(check_limit(65_536).is_err());
    }

    #[test]
    fn the_script_carries_only_the_number() {
        let text = script(250);
        assert!(text.starts_with("set -eu\nn=250\n"));
    }

    #[test]
    fn the_pod_is_pinned_to_the_node_and_tolerates_taints() {
        let pod = pod("p", "default", "worker-1", 200).expect("manifest parses");
        let spec = pod.spec.expect("spec");
        assert_eq!(spec.node_name.as_deref(), Some("worker-1"));
        assert_eq!(spec.host_pid, Some(true));
        assert_eq!(spec.restart_policy.as_deref(), Some("Never"));
        assert!(spec.tolerations.is_some_and(|t| t.len() == 1));
    }
}
