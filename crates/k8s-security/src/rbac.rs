//! RBAC analysis: who can do what, and which of it is worth worrying about.
//!
//! A real cluster ships hundreds of Roles with broad permissions by design —
//! controllers need them. The job here is not to list wide permissions, it is
//! to separate the ones somebody granted from the ones Kubernetes and the
//! distribution installed. Without that distinction the output is noise, and
//! noise is how the one dangerous binding goes unnoticed.

use k8s_openapi::api::rbac::v1::{
    ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, Subject as RbacSubject,
};
use kube::ResourceExt;

use crate::model::{Finding, Severity, Source};

/// Label Kubernetes puts on the Roles it bootstraps.
const BOOTSTRAP_LABEL: &str = "kubernetes.io/bootstrapping";

/// Verbs that let a subject grant itself more than it has.
const ESCALATION_VERBS: &[&str] = &["escalate", "bind", "impersonate"];

/// Subjects that mean "anyone who can reach the API server".
const ANONYMOUS_SUBJECTS: &[&str] = &["system:anonymous", "system:unauthenticated"];

fn matches_any(values: &[String], needle: &str) -> bool {
    values.iter().any(|value| value == "*" || value == needle)
}

fn is_wildcard(values: &[String]) -> bool {
    values.iter().any(|value| value == "*")
}

/// True for Roles the cluster ships itself.
///
/// Both signals are needed: the bootstrap label covers upstream Kubernetes,
/// while distributions (RKE2, Rancher, cloud providers) install `system:`-named
/// roles without it.
fn is_builtin(name: &str, labels: &std::collections::BTreeMap<String, String>) -> bool {
    name.starts_with("system:")
        || name.starts_with("kubeadm:")
        || labels.contains_key(BOOTSTRAP_LABEL)
        || matches!(
            name,
            "cluster-admin" | "admin" | "edit" | "view" | "aggregate-to-admin"
        )
}

struct Target<'a> {
    kind: &'a str,
    resource: &'a str,
    namespace: Option<String>,
    name: String,
    builtin: bool,
}

#[allow(clippy::too_many_arguments)]
fn finding(
    target: &Target<'_>,
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
        source: Source::Rbac,
        kind: target.kind.to_string(),
        namespace: target.namespace.clone(),
        name: target.name.clone(),
        resource: target.resource.to_string(),
        container: None,
        message,
        remediation: remediation.to_string(),
        builtin: target.builtin,
    }
}

/// Findings for one rule set.
fn check_rules(target: &Target<'_>, rules: &[PolicyRule], cluster_scoped: bool) -> Vec<Finding> {
    let mut findings = Vec::new();

    for rule in rules {
        let verbs = &rule.verbs;
        let groups = rule.api_groups.clone().unwrap_or_default();
        let resources = rule.resources.clone().unwrap_or_default();

        // Full control.
        if is_wildcard(verbs) && is_wildcard(&resources) && is_wildcard(&groups) {
            findings.push(finding(
                target,
                "RBAC-FULL-CONTROL",
                "Grants every verb on every resource",
                if cluster_scoped {
                    Severity::Critical
                } else {
                    Severity::High
                },
                format!(
                    "`{}` allows `*` on `*` in all API groups{}.",
                    target.name,
                    if cluster_scoped {
                        ", cluster-wide"
                    } else {
                        ", in this namespace"
                    }
                ),
                "Replace the wildcards with the verbs and resources actually used.",
            ));
            // Everything below is implied by this; saying it again is noise.
            continue;
        }

        for verb in ESCALATION_VERBS {
            if verbs.iter().any(|v| v == verb) {
                findings.push(finding(
                    target,
                    "RBAC-ESCALATION-VERB",
                    "Allows privilege escalation",
                    Severity::High,
                    format!(
                        "`{}` grants `{verb}`, which lets the holder obtain permissions it was \
                         not given directly.",
                        target.name
                    ),
                    "Remove the verb unless this is a controller that manages RBAC.",
                ));
            }
        }

        // Reading secrets cluster-wide is reading every credential in the
        // cluster, including service account tokens.
        let reads = verbs
            .iter()
            .any(|v| v == "get" || v == "list" || v == "watch" || v == "*");
        if cluster_scoped && reads && matches_any(&resources, "secrets") {
            findings.push(finding(
                target,
                "RBAC-SECRET-READ",
                "Reads every Secret in the cluster",
                Severity::High,
                format!(
                    "`{}` can read Secrets in every namespace — that is every credential the \
                     cluster holds.",
                    target.name
                ),
                "Scope the rule to a Role in the namespaces that need it.",
            ));
        }

        // Creating a pod that mounts any service account, or exec'ing into an
        // existing one, is a path to whatever that pod can do.
        if matches_any(&resources, "pods/exec") || matches_any(&resources, "pods/attach") {
            findings.push(finding(
                target,
                "RBAC-POD-EXEC",
                "Can execute inside running pods",
                Severity::High,
                format!(
                    "`{}` grants `pods/exec`, which gives a shell in any matching pod and \
                     therefore that pod's identity and mounted secrets.",
                    target.name
                ),
                "Restrict to the namespaces where debugging is expected, or remove it.",
            ));
        }

        if is_wildcard(verbs) && !is_wildcard(&resources) && !resources.is_empty() {
            findings.push(finding(
                target,
                "RBAC-WILDCARD-VERBS",
                "Grants every verb on specific resources",
                Severity::Medium,
                format!("`{}` allows `*` on {}.", target.name, resources.join(", ")),
                "List the verbs the workload actually calls.",
            ));
        }
    }

    // A role often expresses the same permission across several rules — one
    // for each API group, say. `dedup_by` only removes *adjacent* duplicates,
    // which is why the same finding was appearing twice; dedupe on identity
    // instead.
    let mut seen = std::collections::HashSet::new();
    findings.retain(|finding| seen.insert((finding.id.clone(), finding.message.clone())));
    findings
}

pub fn check_cluster_role(role: &ClusterRole) -> Vec<Finding> {
    let name = role.name_any();
    let target = Target {
        kind: "ClusterRole",
        resource: "rbac.authorization.k8s.io/v1/clusterroles",
        namespace: None,
        builtin: is_builtin(&name, role.labels()),
        name,
    };
    check_rules(&target, role.rules.as_deref().unwrap_or_default(), true)
}

pub fn check_role(role: &Role) -> Vec<Finding> {
    let name = role.name_any();
    let target = Target {
        kind: "Role",
        resource: "rbac.authorization.k8s.io/v1/roles",
        namespace: role.namespace(),
        builtin: is_builtin(&name, role.labels()),
        name,
    };
    check_rules(&target, role.rules.as_deref().unwrap_or_default(), false)
}

fn describe(subject: &RbacSubject) -> String {
    match subject.namespace.as_deref() {
        Some(namespace) if subject.kind == "ServiceAccount" => {
            format!("{}/{}", namespace, subject.name)
        }
        _ => subject.name.clone(),
    }
}

/// Findings about who a binding grants to.
pub fn check_cluster_role_binding(binding: &ClusterRoleBinding) -> Vec<Finding> {
    let name = binding.name_any();
    let target = Target {
        kind: "ClusterRoleBinding",
        resource: "rbac.authorization.k8s.io/v1/clusterrolebindings",
        namespace: None,
        builtin: is_builtin(&name, binding.labels()),
        name,
    };

    let mut findings = Vec::new();
    let role = &binding.role_ref.name;
    let subjects = binding.subjects.clone().unwrap_or_default();

    for subject in &subjects {
        if ANONYMOUS_SUBJECTS.contains(&subject.name.as_str()) {
            findings.push(finding(
                &target,
                "RBAC-ANONYMOUS-BINDING",
                "Grants permissions to unauthenticated callers",
                Severity::Critical,
                format!(
                    "`{}` binds `{role}` to `{}` — anyone who can reach the API server gets it.",
                    target.name, subject.name
                ),
                "Remove the binding, or scope it to a specific authenticated identity.",
            ));
        }
    }

    if role == "cluster-admin" {
        // Every cluster has system components bound to cluster-admin; the
        // question is whether anything else is.
        let granted: Vec<String> = subjects
            .iter()
            .filter(|subject| !subject.name.starts_with("system:"))
            .map(describe)
            .collect();

        if !granted.is_empty() {
            findings.push(finding(
                &target,
                "RBAC-CLUSTER-ADMIN",
                "Grants cluster-admin",
                if target.builtin {
                    Severity::Info
                } else {
                    Severity::High
                },
                format!(
                    "`{}` binds cluster-admin to {}.",
                    target.name,
                    granted.join(", ")
                ),
                "Bind a narrower role, or a Role scoped to the namespaces involved.",
            ));
        }
    }

    findings
}

pub fn check_role_binding(binding: &RoleBinding) -> Vec<Finding> {
    let name = binding.name_any();
    let target = Target {
        kind: "RoleBinding",
        resource: "rbac.authorization.k8s.io/v1/rolebindings",
        namespace: binding.namespace(),
        builtin: is_builtin(&name, binding.labels()),
        name,
    };

    let mut findings = Vec::new();
    for subject in binding.subjects.iter().flatten() {
        if ANONYMOUS_SUBJECTS.contains(&subject.name.as_str()) {
            findings.push(finding(
                &target,
                "RBAC-ANONYMOUS-BINDING",
                "Grants permissions to unauthenticated callers",
                Severity::Critical,
                format!(
                    "`{}` binds `{}` to `{}` in this namespace.",
                    target.name, binding.role_ref.name, subject.name
                ),
                "Remove the binding, or scope it to a specific authenticated identity.",
            ));
        }
    }

    // A namespaced binding to a ClusterRole is normal, but binding to
    // cluster-admin still grants everything *within* the namespace.
    if binding.role_ref.kind == "ClusterRole" && binding.role_ref.name == "cluster-admin" {
        let granted: Vec<String> = binding
            .subjects
            .iter()
            .flatten()
            .filter(|subject| !subject.name.starts_with("system:"))
            .map(describe)
            .collect();
        if !granted.is_empty() {
            findings.push(finding(
                &target,
                "RBAC-NAMESPACE-ADMIN",
                "Grants cluster-admin within a namespace",
                Severity::Medium,
                format!(
                    "`{}` gives {} every permission in this namespace, including reading its \
                     Secrets.",
                    target.name,
                    granted.join(", ")
                ),
                "Use the `admin` or `edit` role unless full control is genuinely needed.",
            ));
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::ObjectMeta;

    fn rule(verbs: &[&str], groups: &[&str], resources: &[&str]) -> PolicyRule {
        PolicyRule {
            verbs: verbs.iter().map(|v| v.to_string()).collect(),
            api_groups: Some(groups.iter().map(|g| g.to_string()).collect()),
            resources: Some(resources.iter().map(|r| r.to_string()).collect()),
            ..Default::default()
        }
    }

    fn cluster_role(name: &str, rules: Vec<PolicyRule>) -> ClusterRole {
        ClusterRole {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            rules: Some(rules),
            ..Default::default()
        }
    }

    #[test]
    fn full_wildcards_are_critical_cluster_wide() {
        let findings = check_cluster_role(&cluster_role(
            "god-mode",
            vec![rule(&["*"], &["*"], &["*"])],
        ));
        assert_eq!(findings.len(), 1, "one finding, not one per implication");
        assert_eq!(findings[0].id, "RBAC-FULL-CONTROL");
        assert_eq!(findings[0].severity, Severity::Critical);
        assert!(!findings[0].builtin);
    }

    /// The distinction the whole module exists for: a cluster ships hundreds of
    /// broad roles, and mixing them with a developer's mistake hides it.
    #[test]
    fn shipped_roles_are_marked_builtin() {
        let findings = check_cluster_role(&cluster_role(
            "system:controller:deployment-controller",
            vec![rule(&["*"], &["*"], &["*"])],
        ));
        assert!(findings[0].builtin);

        let mut labelled = cluster_role("some-vendor-role", vec![rule(&["*"], &["*"], &["*"])]);
        labelled.metadata.labels = Some(
            [(BOOTSTRAP_LABEL.to_string(), "rbac-defaults".to_string())]
                .into_iter()
                .collect(),
        );
        assert!(check_cluster_role(&labelled)[0].builtin);
    }

    /// The same permission expressed in two rules is one problem, not two.
    #[test]
    fn duplicate_findings_from_separate_rules_are_collapsed() {
        let findings = check_cluster_role(&cluster_role(
            "controller",
            vec![
                rule(&["create"], &[""], &["pods/exec"]),
                rule(&["get"], &["apps"], &["pods/exec"]),
            ],
        ));
        assert_eq!(
            findings.iter().filter(|f| f.id == "RBAC-POD-EXEC").count(),
            1
        );
    }

    #[test]
    fn escalation_verbs_are_reported() {
        let findings = check_cluster_role(&cluster_role(
            "sneaky",
            vec![rule(
                &["escalate", "bind"],
                &["rbac.authorization.k8s.io"],
                &["roles"],
            )],
        ));
        let ids: Vec<_> = findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids.iter()
                .filter(|id| **id == "RBAC-ESCALATION-VERB")
                .count(),
            2
        );
    }

    #[test]
    fn cluster_wide_secret_read_is_high() {
        let findings = check_cluster_role(&cluster_role(
            "reader",
            vec![rule(&["get", "list"], &[""], &["secrets"])],
        ));
        let secret = findings
            .iter()
            .find(|f| f.id == "RBAC-SECRET-READ")
            .unwrap();
        assert_eq!(secret.severity, Severity::High);
    }

    /// The same rule in a namespaced Role is far less dangerous, and must not
    /// be reported with the same weight.
    #[test]
    fn namespaced_secret_read_is_not_reported_as_cluster_wide() {
        let role = Role {
            metadata: ObjectMeta {
                name: Some("reader".into()),
                namespace: Some("team-a".into()),
                ..Default::default()
            },
            rules: Some(vec![rule(&["get", "list"], &[""], &["secrets"])]),
        };
        let findings = check_role(&role);
        assert!(!findings.iter().any(|f| f.id == "RBAC-SECRET-READ"));
    }

    #[test]
    fn pod_exec_is_reported() {
        let findings = check_cluster_role(&cluster_role(
            "debugger",
            vec![rule(&["create"], &[""], &["pods/exec"])],
        ));
        assert!(findings.iter().any(|f| f.id == "RBAC-POD-EXEC"));
    }

    #[test]
    fn anonymous_bindings_are_critical() {
        let binding = ClusterRoleBinding {
            metadata: ObjectMeta {
                name: Some("open-door".into()),
                ..Default::default()
            },
            role_ref: k8s_openapi::api::rbac::v1::RoleRef {
                api_group: Some("rbac.authorization.k8s.io".into()),
                kind: "ClusterRole".into(),
                name: "edit".into(),
            },
            subjects: Some(vec![RbacSubject {
                kind: "Group".into(),
                name: "system:unauthenticated".into(),
                ..Default::default()
            }]),
        };
        let findings = check_cluster_role_binding(&binding);
        assert_eq!(findings[0].id, "RBAC-ANONYMOUS-BINDING");
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn cluster_admin_bindings_ignore_system_subjects() {
        let make = |subject: &str| ClusterRoleBinding {
            metadata: ObjectMeta {
                name: Some("admins".into()),
                ..Default::default()
            },
            role_ref: k8s_openapi::api::rbac::v1::RoleRef {
                api_group: Some("rbac.authorization.k8s.io".into()),
                kind: "ClusterRole".into(),
                name: "cluster-admin".into(),
            },
            subjects: Some(vec![RbacSubject {
                kind: "User".into(),
                name: subject.into(),
                ..Default::default()
            }]),
        };

        assert!(check_cluster_role_binding(&make("system:masters")).is_empty());

        let human = check_cluster_role_binding(&make("alice@example.com"));
        assert_eq!(human[0].id, "RBAC-CLUSTER-ADMIN");
        assert_eq!(human[0].severity, Severity::High);
    }

    #[test]
    fn service_account_subjects_are_shown_with_their_namespace() {
        let subject = RbacSubject {
            kind: "ServiceAccount".into(),
            name: "deployer".into(),
            namespace: Some("ci".into()),
            ..Default::default()
        };
        assert_eq!(describe(&subject), "ci/deployer");
    }
}
