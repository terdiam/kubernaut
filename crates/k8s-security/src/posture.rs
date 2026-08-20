//! Workload posture: what a pod spec allows, regardless of what it does.
//!
//! Every check reads the object's own spec, so this needs no scanner, no CRDs
//! and no extra permissions beyond listing workloads. Checks are pure functions
//! over a pod template, which is what makes them testable — and a security
//! check nobody can test is a security check nobody should trust.

use k8s_openapi::api::core::v1::{Container, PodSpec};
use serde_json::Value;

use crate::model::{Finding, Severity, Source};

/// Host paths whose exposure is equivalent to owning the node.
const DANGEROUS_HOST_PATHS: &[(&str, &str)] = &[
    (
        "/var/run/docker.sock",
        "the container runtime socket — full control of the node",
    ),
    (
        "/run/containerd/containerd.sock",
        "the container runtime socket — full control of the node",
    ),
    (
        "/var/run/crio/crio.sock",
        "the container runtime socket — full control of the node",
    ),
    ("/etc", "the node's configuration, including credentials"),
    ("/root", "the node's root home directory"),
    (
        "/var/lib/kubelet",
        "kubelet state, including every pod's secrets",
    ),
    ("/proc", "the host process table"),
    ("/", "the entire node filesystem"),
];

/// Capabilities that individually amount to node compromise.
const DANGEROUS_CAPABILITIES: &[&str] = &[
    "ALL",
    "SYS_ADMIN",
    "SYS_PTRACE",
    "SYS_MODULE",
    "NET_ADMIN",
    "NET_RAW",
    "DAC_READ_SEARCH",
    "SETUID",
    "SETGID",
];

/// Identity of the object a finding belongs to.
pub struct Subject<'a> {
    pub kind: &'a str,
    pub resource: &'a str,
    pub namespace: Option<&'a str>,
    pub name: &'a str,
}

fn finding(
    subject: &Subject<'_>,
    container: Option<&str>,
    id: &str,
    title: &str,
    severity: Severity,
    message: String,
    remediation: &str,
) -> Finding {
    Finding {
        id: id.to_string(),
        title: title.to_string(),
        severity,
        source: Source::Posture,
        kind: subject.kind.to_string(),
        namespace: subject.namespace.map(String::from),
        name: subject.name.to_string(),
        resource: subject.resource.to_string(),
        container: container.map(String::from),
        message,
        remediation: remediation.to_string(),
        builtin: false,
    }
}

/// Every posture finding for one pod template.
pub fn check_pod_spec(subject: &Subject<'_>, spec: &PodSpec) -> Vec<Finding> {
    let mut findings = Vec::new();

    // ---- pod-level host namespaces ----------------------------------------
    if spec.host_pid == Some(true) {
        findings.push(finding(
            subject,
            None,
            "POD-HOST-PID",
            "Shares the host process namespace",
            Severity::High,
            "`hostPID: true` lets this pod see and signal every process on the node.".into(),
            "Remove `hostPID` unless the workload is a node agent that genuinely needs it.",
        ));
    }
    if spec.host_ipc == Some(true) {
        findings.push(finding(
            subject,
            None,
            "POD-HOST-IPC",
            "Shares the host IPC namespace",
            Severity::High,
            "`hostIPC: true` exposes the node's shared memory to this pod.".into(),
            "Remove `hostIPC`.",
        ));
    }
    if spec.host_network == Some(true) {
        findings.push(finding(
            subject,
            None,
            "POD-HOST-NETWORK",
            "Uses the host network",
            Severity::High,
            "`hostNetwork: true` bypasses NetworkPolicy entirely and exposes every port the \
             pod binds directly on the node."
                .into(),
            "Use a Service instead, unless this is a CNI or monitoring agent.",
        ));
    }

    // Scoped to the *default* service account on purpose.
    //
    // Almost no workload sets `automountServiceAccountToken: false`, so flagging
    // every one of them produces a finding per workload in the cluster and says
    // nothing. Mounting the *default* account's token is the case that is both
    // common and gratuitous: the workload gained a cluster credential it never
    // asked for.
    let account = spec.service_account_name.as_deref().unwrap_or("default");
    if spec.automount_service_account_token != Some(false) && account == "default" {
        findings.push(finding(
            subject,
            None,
            "POD-DEFAULT-SA-TOKEN",
            "Mounts the default service account token",
            Severity::Low,
            "This pod uses the `default` service account and mounts its token, so anything \
             that can read the container filesystem can talk to the API server as that account."
                .into(),
            "Set `automountServiceAccountToken: false`, or give the workload its own service \
             account with only the permissions it needs.",
        ));
    }

    for volume in spec.volumes.iter().flatten() {
        let Some(host_path) = &volume.host_path else {
            continue;
        };
        let path = host_path.path.as_str();
        let matched = DANGEROUS_HOST_PATHS.iter().find(|(dangerous, _)| {
            path == *dangerous || path.starts_with(&format!("{dangerous}/"))
        });

        let (severity, why) = match matched {
            Some((_, why)) => (Severity::Critical, (*why).to_string()),
            None => (
                Severity::Medium,
                "a path on the node, outside the container's own filesystem".to_string(),
            ),
        };
        findings.push(finding(
            subject,
            None,
            "POD-HOSTPATH",
            "Mounts a host path",
            severity,
            format!("Volume `{}` mounts `{path}` — {why}.", volume.name),
            "Replace the hostPath with a PersistentVolumeClaim, ConfigMap or emptyDir.",
        ));
    }

    // ---- per-container ----------------------------------------------------
    for container in spec
        .containers
        .iter()
        .chain(spec.init_containers.iter().flatten())
    {
        findings.extend(check_container(subject, container));
    }

    findings
}

fn check_container(subject: &Subject<'_>, container: &Container) -> Vec<Finding> {
    let mut findings = Vec::new();
    let name = container.name.as_str();
    let security = container.security_context.as_ref();

    if security.and_then(|s| s.privileged) == Some(true) {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-PRIVILEGED",
            "Runs privileged",
            Severity::Critical,
            format!(
                "Container `{name}` runs with `privileged: true`, which disables essentially \
                 every container boundary. Escaping to the node is a documented one-liner."
            ),
            "Drop `privileged` and grant only the specific capabilities the workload needs.",
        ));
    }

    if security.and_then(|s| s.allow_privilege_escalation) != Some(false) {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-PRIV-ESCALATION",
            "Allows privilege escalation",
            Severity::Medium,
            format!(
                "Container `{name}` does not set `allowPrivilegeEscalation: false`, so a setuid \
                 binary can gain more privileges than the container started with."
            ),
            "Set `securityContext.allowPrivilegeEscalation: false`.",
        ));
    }

    let run_as_user = security.and_then(|s| s.run_as_user);
    let run_as_non_root = security.and_then(|s| s.run_as_non_root);
    if run_as_user == Some(0) {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-ROOT",
            "Runs as root",
            Severity::Medium,
            format!("Container `{name}` sets `runAsUser: 0`."),
            "Run as a non-zero UID and set `runAsNonRoot: true`.",
        ));
    } else if run_as_non_root != Some(true) && run_as_user.is_none() {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-ROOT-UNSET",
            "May run as root",
            Severity::Low,
            format!(
                "Container `{name}` sets neither `runAsNonRoot` nor `runAsUser`, so it runs as \
                 whatever user the image declares — which is root for most images."
            ),
            "Set `runAsNonRoot: true` so the kubelet refuses to start a root container.",
        ));
    }

    if let Some(capabilities) = security.and_then(|s| s.capabilities.as_ref()) {
        for added in capabilities.add.iter().flatten() {
            let upper = added.to_ascii_uppercase();
            let dangerous = DANGEROUS_CAPABILITIES.contains(&upper.as_str());
            findings.push(finding(
                subject,
                Some(name),
                "CONTAINER-CAPABILITY",
                "Adds a Linux capability",
                if dangerous {
                    Severity::High
                } else {
                    Severity::Low
                },
                format!(
                    "Container `{name}` adds `{upper}`{}.",
                    if dangerous {
                        ", which is enough on its own to take over the node"
                    } else {
                        ""
                    }
                ),
                "Drop the capability, or narrow it to the single operation that needs it.",
            ));
        }
    }

    if security.and_then(|s| s.read_only_root_filesystem) != Some(true) {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-WRITABLE-ROOT",
            "Writable root filesystem",
            Severity::Low,
            format!(
                "Container `{name}` can write anywhere in its image, which lets an attacker \
                 persist tools between restarts."
            ),
            "Set `readOnlyRootFilesystem: true` and mount an emptyDir for scratch space.",
        ));
    }

    // ---- image ------------------------------------------------------------
    if let Some(image) = &container.image {
        let tag = image.rsplit_once(':').map(|(_, tag)| tag).unwrap_or("");
        let digest_pinned = image.contains("@sha256:");
        if !digest_pinned && (tag.is_empty() || tag == "latest" || !image.contains(':')) {
            findings.push(finding(
                subject,
                Some(name),
                "IMAGE-MUTABLE-TAG",
                "Image is not pinned",
                Severity::Medium,
                format!(
                    "Container `{name}` uses `{image}`. A moving tag means the running code can \
                     change under you, and there is no way to say afterwards what was deployed."
                ),
                "Pin an immutable tag, or better a digest (`image@sha256:…`).",
            ));
        }
    }

    // ---- resources --------------------------------------------------------
    let resources = container.resources.as_ref();
    let has = |field: Option<
        &std::collections::BTreeMap<
            String,
            k8s_openapi::apimachinery::pkg::api::resource::Quantity,
        >,
    >,
               key: &str| { field.is_some_and(|map| map.contains_key(key)) };

    if !has(resources.and_then(|r| r.limits.as_ref()), "memory") {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-NO-MEMORY-LIMIT",
            "No memory limit",
            Severity::Medium,
            format!(
                "Container `{name}` has no memory limit, so a leak here can exhaust the node \
                 and evict unrelated workloads."
            ),
            "Set `resources.limits.memory`.",
        ));
    }
    if !has(resources.and_then(|r| r.requests.as_ref()), "cpu")
        || !has(resources.and_then(|r| r.requests.as_ref()), "memory")
    {
        findings.push(finding(
            subject,
            Some(name),
            "CONTAINER-NO-REQUESTS",
            "No resource requests",
            Severity::Low,
            format!(
                "Container `{name}` does not request CPU and memory, so the scheduler cannot \
                 place it sensibly and it is evicted first under pressure."
            ),
            "Set `resources.requests.cpu` and `resources.requests.memory`.",
        ));
    }

    findings
}

/// Pull the pod template out of a workload, or the spec out of a bare pod.
pub fn pod_spec_of(object: &Value) -> Option<PodSpec> {
    let spec = object
        .pointer("/spec/template/spec")
        .or_else(|| object.pointer("/spec/jobTemplate/spec/template/spec"))
        .or_else(|| object.pointer("/spec"))?;
    serde_json::from_value(spec.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{
        Capabilities, HostPathVolumeSource, ResourceRequirements, SecurityContext, Volume,
    };
    use k8s_openapi::apimachinery::pkg::api::resource::Quantity;

    fn subject() -> Subject<'static> {
        Subject {
            kind: "Deployment",
            resource: "apps/v1/deployments",
            namespace: Some("default"),
            name: "web",
        }
    }

    /// A container with nothing risky set, so tests can add one thing at a time.
    fn safe_container() -> Container {
        Container {
            name: "app".into(),
            image: Some("registry.example.com/app@sha256:abc".into()),
            security_context: Some(SecurityContext {
                privileged: Some(false),
                allow_privilege_escalation: Some(false),
                run_as_non_root: Some(true),
                read_only_root_filesystem: Some(true),
                ..Default::default()
            }),
            resources: Some(ResourceRequirements {
                limits: Some(
                    [("memory".to_string(), Quantity("256Mi".into()))]
                        .into_iter()
                        .collect(),
                ),
                requests: Some(
                    [
                        ("cpu".to_string(), Quantity("100m".into())),
                        ("memory".to_string(), Quantity("128Mi".into())),
                    ]
                    .into_iter()
                    .collect(),
                ),
                claims: None,
            }),
            ..Default::default()
        }
    }

    fn safe_spec() -> PodSpec {
        PodSpec {
            containers: vec![safe_container()],
            automount_service_account_token: Some(false),
            service_account_name: Some("app".into()),
            ..Default::default()
        }
    }

    fn ids(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.id.as_str()).collect()
    }

    /// The baseline must be silent. A checker that fires on a well-configured
    /// workload trains people to ignore it.
    #[test]
    fn a_hardened_workload_produces_nothing() {
        let findings = check_pod_spec(&subject(), &safe_spec());
        assert!(findings.is_empty(), "unexpected: {:?}", ids(&findings));
    }

    /// A workload with its own service account is doing the right thing even
    /// if it mounts the token; only the default account is gratuitous.
    #[test]
    fn only_the_default_service_account_token_is_flagged() {
        let mut spec = safe_spec();
        spec.automount_service_account_token = None;
        assert!(
            !ids(&check_pod_spec(&subject(), &spec)).contains(&"POD-DEFAULT-SA-TOKEN"),
            "a dedicated service account is not a finding"
        );

        spec.service_account_name = None;
        assert!(ids(&check_pod_spec(&subject(), &spec)).contains(&"POD-DEFAULT-SA-TOKEN"));
    }

    #[test]
    fn privileged_is_critical() {
        let mut spec = safe_spec();
        spec.containers[0]
            .security_context
            .as_mut()
            .unwrap()
            .privileged = Some(true);
        let findings = check_pod_spec(&subject(), &spec);
        let privileged = findings
            .iter()
            .find(|f| f.id == "CONTAINER-PRIVILEGED")
            .expect("should be reported");
        assert_eq!(privileged.severity, Severity::Critical);
        assert_eq!(privileged.container.as_deref(), Some("app"));
    }

    #[test]
    fn host_namespaces_are_reported_separately() {
        let mut spec = safe_spec();
        spec.host_pid = Some(true);
        spec.host_network = Some(true);
        let findings = check_pod_spec(&subject(), &spec);
        assert!(ids(&findings).contains(&"POD-HOST-PID"));
        assert!(ids(&findings).contains(&"POD-HOST-NETWORK"));
    }

    /// Mounting the runtime socket is node takeover, not a medium-severity nit.
    #[test]
    fn runtime_socket_mount_is_critical() {
        let mut spec = safe_spec();
        spec.volumes = Some(vec![Volume {
            name: "sock".into(),
            host_path: Some(HostPathVolumeSource {
                path: "/var/run/docker.sock".into(),
                type_: None,
            }),
            ..Default::default()
        }]);
        let findings = check_pod_spec(&subject(), &spec);
        let mount = findings.iter().find(|f| f.id == "POD-HOSTPATH").unwrap();
        assert_eq!(mount.severity, Severity::Critical);
    }

    #[test]
    fn an_ordinary_host_path_is_only_medium() {
        let mut spec = safe_spec();
        spec.volumes = Some(vec![Volume {
            name: "data".into(),
            host_path: Some(HostPathVolumeSource {
                path: "/opt/app-data".into(),
                type_: None,
            }),
            ..Default::default()
        }]);
        let findings = check_pod_spec(&subject(), &spec);
        let mount = findings.iter().find(|f| f.id == "POD-HOSTPATH").unwrap();
        assert_eq!(mount.severity, Severity::Medium);
    }

    #[test]
    fn dangerous_capabilities_outrank_ordinary_ones() {
        let mut spec = safe_spec();
        spec.containers[0]
            .security_context
            .as_mut()
            .unwrap()
            .capabilities = Some(Capabilities {
            add: Some(vec!["SYS_ADMIN".into(), "CHOWN".into()]),
            drop: None,
        });
        let findings = check_pod_spec(&subject(), &spec);
        let capabilities: Vec<_> = findings
            .iter()
            .filter(|f| f.id == "CONTAINER-CAPABILITY")
            .collect();
        assert_eq!(capabilities.len(), 2);
        assert!(capabilities.iter().any(|f| f.severity == Severity::High));
        assert!(capabilities.iter().any(|f| f.severity == Severity::Low));
    }

    #[test]
    fn a_digest_pinned_image_is_accepted() {
        let findings = check_pod_spec(&subject(), &safe_spec());
        assert!(!ids(&findings).contains(&"IMAGE-MUTABLE-TAG"));
    }

    #[test]
    fn latest_and_untagged_images_are_flagged() {
        for image in ["nginx", "nginx:latest", "registry.io/team/app"] {
            let mut spec = safe_spec();
            spec.containers[0].image = Some(image.into());
            let findings = check_pod_spec(&subject(), &spec);
            assert!(
                ids(&findings).contains(&"IMAGE-MUTABLE-TAG"),
                "`{image}` should be flagged"
            );
        }
    }

    /// A registry port must not be mistaken for a tag.
    #[test]
    fn a_registry_port_is_not_a_tag() {
        let mut spec = safe_spec();
        spec.containers[0].image = Some("registry.io:5000/app:1.2.3".into());
        let findings = check_pod_spec(&subject(), &spec);
        assert!(!ids(&findings).contains(&"IMAGE-MUTABLE-TAG"));
    }

    #[test]
    fn missing_memory_limit_is_reported() {
        let mut spec = safe_spec();
        spec.containers[0].resources.as_mut().unwrap().limits = None;
        let findings = check_pod_spec(&subject(), &spec);
        assert!(ids(&findings).contains(&"CONTAINER-NO-MEMORY-LIMIT"));
    }

    #[test]
    fn init_containers_are_checked_too() {
        let mut spec = safe_spec();
        let mut init = safe_container();
        init.name = "setup".into();
        init.security_context.as_mut().unwrap().privileged = Some(true);
        spec.init_containers = Some(vec![init]);

        let findings = check_pod_spec(&subject(), &spec);
        let privileged = findings
            .iter()
            .find(|f| f.id == "CONTAINER-PRIVILEGED")
            .unwrap();
        assert_eq!(privileged.container.as_deref(), Some("setup"));
    }

    #[test]
    fn pod_template_is_found_in_workloads_and_bare_pods() {
        let deployment = serde_json::json!({
            "spec": {"template": {"spec": {"containers": [{"name": "a"}]}}}
        });
        assert_eq!(pod_spec_of(&deployment).unwrap().containers[0].name, "a");

        let cronjob = serde_json::json!({
            "spec": {"jobTemplate": {"spec": {"template": {"spec": {
                "containers": [{"name": "b"}]
            }}}}}
        });
        assert_eq!(pod_spec_of(&cronjob).unwrap().containers[0].name, "b");

        let pod = serde_json::json!({"spec": {"containers": [{"name": "c"}]}});
        assert_eq!(pod_spec_of(&pod).unwrap().containers[0].name, "c");
    }
}
