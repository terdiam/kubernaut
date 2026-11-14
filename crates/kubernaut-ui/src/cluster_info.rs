//! What the Info entry of a cluster's menu says about it.
//!
//! Plain data in, labelled rows out, so what is shown can be tested without a
//! window. Rows that have nothing to say are left out rather than shown empty.

use k8s_app::preferences::ClusterProfile;
use k8s_core::ContextEntry;

/// Label and value, in the order they are shown.
pub type Row = (&'static str, String);

pub fn rows(
    entry: Option<&ContextEntry>,
    status: &str,
    namespaces: Option<usize>,
    profile: &ClusterProfile,
    protected: bool,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut add = |label: &'static str, value: String| rows.push((label, value));

    add("Status", status.to_string());
    if let Some(entry) = entry {
        add("Context", entry.name.clone());
        add(
            "Server",
            entry.server.clone().unwrap_or_else(|| "not set".into()),
        );
        add("Cluster", entry.cluster.clone());
        add("User", entry.user.clone());
        if let Some(command) = &entry.exec_command {
            add(
                "Sign-in",
                if entry.missing_exec_plugin {
                    format!("{command} (not found on PATH: add its folder in Settings)")
                } else {
                    format!("{command} plugin")
                },
            );
        }
    }
    let default_namespace = profile
        .default_namespace
        .clone()
        .or_else(|| entry.and_then(|entry| entry.namespace.clone()));
    if let Some(namespace) = default_namespace {
        add("Default namespace", namespace);
    }
    if let Some(count) = namespaces {
        add("Namespaces", count.to_string());
    }
    if let Some(group) = &profile.workspace {
        add("Group", group.clone());
    }
    add(
        "Protected",
        if protected {
            "Yes: changes from this app are refused".into()
        } else {
            "No".into()
        },
    );
    // Settings that change how safe the connection is are worth stating.
    if profile.accept_invalid_certs {
        add("Certificates", "Server identity is not checked".into());
    }
    if let Some(user) = &profile.impersonate_user {
        add("Acting as", user.clone());
    }
    if let Some(proxy) = &profile.proxy_url {
        add("Proxy", proxy.clone());
    }
    rows
}

#[cfg(test)]
mod tests {
    use k8s_app::preferences::ClusterProfile;
    use k8s_core::ContextEntry;

    use super::rows;

    fn entry() -> ContextEntry {
        ContextEntry {
            name: "prod-eu".into(),
            cluster: "eks-eu".into(),
            user: "alice".into(),
            namespace: Some("shop".into()),
            server: Some("https://10.0.0.1:6443".into()),
            is_current: false,
            exec_command: Some("aws".into()),
            missing_exec_plugin: false,
        }
    }

    fn value<'a>(rows: &'a [(&'static str, String)], label: &str) -> Option<&'a str> {
        rows.iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_plain_cluster_lists_where_it_is_and_who_connects() {
        let found = rows(
            Some(&entry()),
            "Connected",
            Some(12),
            &ClusterProfile::default(),
            false,
        );
        assert_eq!(value(&found, "Server"), Some("https://10.0.0.1:6443"));
        assert_eq!(value(&found, "User"), Some("alice"));
        assert_eq!(value(&found, "Sign-in"), Some("aws plugin"));
        assert_eq!(value(&found, "Default namespace"), Some("shop"));
        assert_eq!(value(&found, "Namespaces"), Some("12"));
        assert_eq!(value(&found, "Protected"), Some("No"));
        assert!(
            value(&found, "Certificates").is_none(),
            "nothing to warn about"
        );
    }

    #[test]
    fn what_makes_a_connection_less_safe_or_unusual_is_stated() {
        let profile = ClusterProfile {
            accept_invalid_certs: true,
            impersonate_user: Some("ops".into()),
            proxy_url: Some("http://proxy:3128".into()),
            default_namespace: Some("billing".into()),
            ..ClusterProfile::default()
        };
        let mut e = entry();
        e.missing_exec_plugin = true;
        let found = rows(Some(&e), "Unreachable", None, &profile, true);
        assert_eq!(
            value(&found, "Certificates"),
            Some("Server identity is not checked")
        );
        assert_eq!(value(&found, "Acting as"), Some("ops"));
        assert_eq!(value(&found, "Proxy"), Some("http://proxy:3128"));
        assert_eq!(
            value(&found, "Default namespace"),
            Some("billing"),
            "the app's setting wins"
        );
        assert!(value(&found, "Sign-in").is_some_and(|v| v.contains("not found")));
        assert!(value(&found, "Protected").is_some_and(|v| v.starts_with("Yes")));
        assert!(
            value(&found, "Namespaces").is_none(),
            "unknown while not connected"
        );
    }

    #[test]
    fn a_cluster_gone_from_the_kubeconfig_still_has_a_status_and_a_protection_line() {
        let found = rows(
            None,
            "Not connected",
            None,
            &ClusterProfile::default(),
            false,
        );
        assert_eq!(found.first().map(|(l, _)| *l), Some("Status"));
        assert!(value(&found, "Server").is_none());
        assert!(value(&found, "Protected").is_some());
    }
}
