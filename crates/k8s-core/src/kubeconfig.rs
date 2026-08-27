//! Kubeconfig loading and context enumeration.

use std::{collections::BTreeMap, path::PathBuf};

use kube::config::{KubeConfigOptions, Kubeconfig};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// A context the user can connect to, projected for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    /// Context name — also the `ClusterId` used everywhere else.
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub namespace: Option<String>,
    pub server: Option<String>,
    /// True when this is the kubeconfig's `current-context`.
    pub is_current: bool,
    /// Auth plugin this context shells out to, if any (`aws`, `gke-gcloud-auth-plugin`, …).
    pub exec_command: Option<String>,
    /// Set when `exec_command` is present but not resolvable on `PATH`.
    /// The UI turns this into "install X / add its directory in Settings".
    pub missing_exec_plugin: bool,
}

/// The merged kubeconfig plus where it came from.
#[derive(Debug, Clone)]
pub struct LoadedKubeconfig {
    pub config: Kubeconfig,
    pub sources: Vec<PathBuf>,
}

/// Load and merge every kubeconfig on `KUBECONFIG` (or `~/.kube/config`).
///
/// `Kubeconfig::read()` already implements kubectl's merge semantics
/// (first-wins per key, `KUBECONFIG` path list); we only add source tracking so
/// the UI can show which file a context came from.
pub fn load() -> Result<LoadedKubeconfig> {
    let sources = kubeconfig_paths();
    let config = Kubeconfig::read()?;
    Ok(LoadedKubeconfig { config, sources })
}

/// Paths kubectl would read, in order.
pub fn kubeconfig_paths() -> Vec<PathBuf> {
    if let Some(raw) = std::env::var_os("KUBECONFIG").filter(|v| !v.is_empty()) {
        return std::env::split_paths(&raw)
            .filter(|p| p.is_file())
            .collect();
    }
    directories::UserDirs::new()
        .map(|d| d.home_dir().join(".kube").join("config"))
        .filter(|p| p.is_file())
        .into_iter()
        .collect()
}

/// Project a kubeconfig into the context list shown in the cluster picker.
pub fn contexts(loaded: &LoadedKubeconfig) -> Vec<ContextEntry> {
    let current = loaded.config.current_context.as_deref();

    loaded
        .config
        .contexts
        .iter()
        .filter_map(|named| {
            let ctx = named.context.as_ref()?;
            let server = loaded
                .config
                .clusters
                .iter()
                .find(|c| c.name == ctx.cluster)
                .and_then(|c| c.cluster.as_ref())
                .and_then(|c| c.server.clone());

            let exec_command = loaded
                .config
                .auth_infos
                .iter()
                .find(|a| Some(&a.name) == ctx.user.as_ref())
                .and_then(|a| a.auth_info.as_ref())
                .and_then(|a| a.exec.as_ref())
                .and_then(|e| e.command.clone());

            let missing_exec_plugin = exec_command
                .as_deref()
                .is_some_and(|cmd| crate::paths::which(cmd).is_none());

            Some(ContextEntry {
                name: named.name.clone(),
                cluster: ctx.cluster.clone(),
                user: ctx.user.clone().unwrap_or_default(),
                namespace: ctx.namespace.clone(),
                server,
                is_current: current == Some(named.name.as_str()),
                exec_command,
                missing_exec_plugin,
            })
        })
        .collect()
}

/// Build the `KubeConfigOptions` for a named context.
/// How a context proves who it is.
///
/// What "re-authenticate" means depends entirely on this: a cloud context runs
/// a provider login, a kubeadm context needs a fresh admin kubeconfig from the
/// control plane, and a token context needs a new token issued. Telling the
/// second to run `aws eks update-kubeconfig` sends them nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// A credential plugin (`aws`, `gcloud`, `az`, `kubelogin`).
    Exec,
    /// A bearer token written into the kubeconfig.
    Token,
    /// A client certificate, as kubeadm and RKE hand out.
    ClientCertificate,
    Basic,
    Unknown,
}

/// Which credential the named context uses.
pub fn auth_kind(config: &Kubeconfig, context: &str) -> AuthKind {
    let user = config
        .contexts
        .iter()
        .find(|entry| entry.name == context)
        .and_then(|entry| entry.context.as_ref())
        .and_then(|ctx| ctx.user.clone())
        .unwrap_or_default();

    let Some(auth) = config
        .auth_infos
        .iter()
        .find(|entry| entry.name == user)
        .and_then(|entry| entry.auth_info.as_ref())
    else {
        return AuthKind::Unknown;
    };

    // Ordered by which one the client actually uses when several are present.
    if auth.exec.is_some() {
        AuthKind::Exec
    } else if auth.token.is_some() || auth.token_file.is_some() {
        AuthKind::Token
    } else if auth.client_certificate.is_some() || auth.client_certificate_data.is_some() {
        AuthKind::ClientCertificate
    } else if auth.username.is_some() {
        AuthKind::Basic
    } else {
        AuthKind::Unknown
    }
}

pub fn options_for(context: &str) -> KubeConfigOptions {
    KubeConfigOptions {
        context: Some(context.to_string()),
        cluster: None,
        user: None,
    }
}

/// Build a kubeconfig containing **only** one context, its cluster and its user.
///
/// This is what gets written to disk for an embedded shell. Handing the shell
/// the user's full kubeconfig would expose credentials for every cluster they
/// have — including production ones they did not open — to any process that can
/// read that file. Minifying keeps the blast radius to the cluster already on
/// screen. Mirrors `kubectl config view --minify --flatten`.
pub fn minified(
    loaded: &LoadedKubeconfig,
    context: &str,
    namespace: Option<&str>,
) -> Result<String> {
    let source = &loaded.config;

    let named_context = source
        .contexts
        .iter()
        .find(|c| c.name == context)
        .cloned()
        .ok_or_else(|| CoreError::UnknownContext(context.to_string()))?;

    let inner = named_context
        .context
        .clone()
        .ok_or_else(|| CoreError::UnknownContext(context.to_string()))?;

    let cluster = source
        .clusters
        .iter()
        .find(|c| c.name == inner.cluster)
        .cloned()
        .ok_or_else(|| {
            CoreError::other(format!("cluster `{}` not in kubeconfig", inner.cluster))
        })?;

    let auth_info = inner
        .user
        .as_ref()
        .and_then(|user| source.auth_infos.iter().find(|a| &a.name == user))
        .cloned();

    let mut named_context = named_context;
    if let Some(namespace) = namespace
        && let Some(ctx) = named_context.context.as_mut()
    {
        ctx.namespace = Some(namespace.to_string());
    }

    let minified = Kubeconfig {
        preferences: source.preferences.clone(),
        clusters: vec![cluster],
        auth_infos: auth_info.into_iter().collect(),
        contexts: vec![named_context],
        current_context: Some(context.to_string()),
        ..Kubeconfig::default()
    };

    serde_yaml_ng::to_string(&minified).map_err(CoreError::other)
}

/// Write a kubeconfig only this user can read, and return its path.
///
/// Used by anything that shells out — an embedded terminal, the Helm CLI — so
/// the child process authenticates as the user without the app handling tokens
/// itself. The caller owns the file and must delete it when done.
pub fn write_private(contents: &str) -> Result<std::path::PathBuf> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("kubernaut-").suffix(".kubeconfig");
    let file = builder.tempfile()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 0600 before anything is written: the file holds cluster credentials
        // and lives in a world-readable directory.
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))?;
    }

    let (mut handle, path) = file
        .keep()
        .map_err(|err| CoreError::other(format!("could not persist kubeconfig: {}", err.error)))?;
    std::io::Write::write_all(&mut handle, contents.as_bytes())?;
    Ok(path)
}

/// Load only the given files, ignoring the system kubeconfig entirely.
///
/// The app adds clusters explicitly rather than inheriting whatever is in
/// `~/.kube/config`: a tool that can reach production because a file happened
/// to be on disk is a tool that reaches production by accident.
pub fn load_files(paths: &[std::path::PathBuf]) -> Result<LoadedKubeconfig> {
    let mut config = Kubeconfig::default();
    let mut sources = Vec::new();

    for path in paths {
        match Kubeconfig::read_from(path) {
            Ok(additional) => {
                config = config.merge(additional)?;
                sources.push(path.clone());
            }
            Err(err) => {
                // One unreadable file must not hide every other cluster.
                tracing::warn!(path = %path.display(), %err, "skipping unreadable kubeconfig");
            }
        }
    }

    Ok(LoadedKubeconfig { config, sources })
}

/// Contexts the *system* kubeconfig offers, for the import picker.
///
/// Reading is not adding: nothing here reaches a cluster until the user picks
/// a context and it is copied into this app's own storage.
pub fn system_contexts() -> Vec<ContextEntry> {
    let sources = kubeconfig_paths();
    match Kubeconfig::read() {
        Ok(config) => contexts(&LoadedKubeconfig { config, sources }),
        Err(err) => {
            tracing::debug!(%err, "no readable system kubeconfig");
            Vec::new()
        }
    }
}

/// Extract named contexts, with the clusters and users they reference, as a
/// standalone kubeconfig document.
pub fn extract(source: &Kubeconfig, wanted: &[String]) -> Result<String> {
    let contexts: Vec<_> = source
        .contexts
        .iter()
        .filter(|named| wanted.contains(&named.name))
        .cloned()
        .collect();

    if contexts.is_empty() {
        return Err(CoreError::other("none of those contexts exist"));
    }

    let cluster_names: Vec<String> = contexts
        .iter()
        .filter_map(|named| named.context.as_ref().map(|c| c.cluster.clone()))
        .collect();
    let user_names: Vec<String> = contexts
        .iter()
        .filter_map(|named| named.context.as_ref().and_then(|c| c.user.clone()))
        .collect();

    let extracted = Kubeconfig {
        preferences: source.preferences.clone(),
        clusters: source
            .clusters
            .iter()
            .filter(|c| cluster_names.contains(&c.name))
            .cloned()
            .collect(),
        auth_infos: source
            .auth_infos
            .iter()
            .filter(|a| user_names.contains(&a.name))
            .cloned()
            .collect(),
        current_context: contexts.first().map(|named| named.name.clone()),
        contexts,
        ..Kubeconfig::default()
    };

    serde_yaml_ng::to_string(&extracted).map_err(CoreError::other)
}

/// Read the system kubeconfig, for extracting contexts from it.
pub fn read_system() -> Result<Kubeconfig> {
    Ok(Kubeconfig::read()?)
}

/// Load the system kubeconfig merged with additional files.
///
/// The system file comes first because `Kubeconfig::merge` is first-wins:
/// something the user configured with kubectl must not be shadowed by a file
/// this app manages.
pub fn load_merged(extra: &[std::path::PathBuf]) -> Result<LoadedKubeconfig> {
    let mut sources = kubeconfig_paths();
    let mut config = Kubeconfig::read().unwrap_or_default();

    for path in extra {
        match Kubeconfig::read_from(path) {
            Ok(additional) => {
                config = config.merge(additional)?;
                sources.push(path.clone());
            }
            Err(err) => {
                // One unreadable managed file must not hide every other cluster.
                tracing::warn!(path = %path.display(), %err, "skipping unreadable kubeconfig");
            }
        }
    }

    Ok(LoadedKubeconfig { config, sources })
}

/// Contexts a kubeconfig document would contribute, without loading it.
///
/// Used to show what an import will add — and what it would collide with —
/// before anything is written.
pub fn preview(yaml: &str) -> Result<Vec<String>> {
    let config = Kubeconfig::from_yaml(yaml)?;
    if config.contexts.is_empty() {
        return Err(CoreError::other(
            "this file defines no contexts, so there is nothing to add",
        ));
    }
    Ok(config.contexts.iter().map(|c| c.name.clone()).collect())
}

/// Rename contexts in a kubeconfig document.
///
/// Importing a file whose context is also called `default` would otherwise
/// shadow, or be shadowed by, one already there — with no indication which
/// cluster a click reaches.
/// Make every cluster and user name in a document unique to it.
///
/// `Kubeconfig::merge` is first-wins across the files this app manages, and it
/// merges by *name*. Two kubeadm clusters both call their user
/// `kubernetes-admin` and their cluster `kubernetes`, so importing a second one
/// leaves its context pointing at the first one's certificate — which the
/// second apiserver rejects with a 401 that looks exactly like an expired
/// credential. Qualifying the names on the way in removes the collision
/// entirely; nothing outside this file refers to them.
pub fn qualify_entries(yaml: &str, suffix: &str) -> Result<String> {
    let mut config = Kubeconfig::from_yaml(yaml)?;
    let suffix = suffix.trim();
    if suffix.is_empty() {
        return serde_yaml_ng::to_string(&config).map_err(CoreError::other);
    }

    let qualify = |name: &str| {
        // Idempotent: re-importing a file this app already wrote must not
        // stack suffixes.
        if name.ends_with(&format!("__{suffix}")) {
            name.to_string()
        } else {
            format!("{name}__{suffix}")
        }
    };

    let mut clusters: BTreeMap<String, String> = BTreeMap::new();
    for entry in &mut config.clusters {
        let renamed = qualify(&entry.name);
        clusters.insert(entry.name.clone(), renamed.clone());
        entry.name = renamed;
    }

    let mut users: BTreeMap<String, String> = BTreeMap::new();
    for entry in &mut config.auth_infos {
        let renamed = qualify(&entry.name);
        users.insert(entry.name.clone(), renamed.clone());
        entry.name = renamed;
    }

    for context in &mut config.contexts {
        let Some(ctx) = context.context.as_mut() else {
            continue;
        };
        if let Some(renamed) = clusters.get(&ctx.cluster) {
            ctx.cluster = renamed.clone();
        }
        if let Some(user) = ctx.user.as_ref()
            && let Some(renamed) = users.get(user)
        {
            ctx.user = Some(renamed.clone());
        }
    }

    serde_yaml_ng::to_string(&config).map_err(CoreError::other)
}

pub fn rename_contexts(yaml: &str, renames: &BTreeMap<String, String>) -> Result<String> {
    let mut config = Kubeconfig::from_yaml(yaml)?;

    for named in &mut config.contexts {
        if let Some(new_name) = renames.get(&named.name) {
            named.name = new_name.clone();
        }
    }
    if let Some(current) = &config.current_context
        && let Some(new_name) = renames.get(current)
    {
        config.current_context = Some(new_name.clone());
    }

    serde_yaml_ng::to_string(&config).map_err(CoreError::other)
}

#[cfg(test)]
mod auth_kind_tests {
    use super::*;

    fn config(yaml: &str) -> Kubeconfig {
        serde_yaml_ng::from_str(yaml).expect("kubeconfig fixture")
    }

    const KUBEADM: &str = r#"
apiVersion: v1
kind: Config
clusters: [{ name: kubernetes, cluster: { server: https://10.0.0.1:6443 } }]
contexts: [{ name: kubernetes-admin@kubernetes, context: { cluster: kubernetes, user: kubernetes-admin } }]
users: [{ name: kubernetes-admin, user: { client-certificate-data: Zm9v, client-key-data: YmFy } }]
"#;

    const CLOUD: &str = r#"
apiVersion: v1
kind: Config
clusters: [{ name: eks, cluster: { server: https://example.eks.amazonaws.com } }]
contexts: [{ name: eks, context: { cluster: eks, user: eks } }]
users:
  - name: eks
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1beta1
        command: aws
"#;

    #[test]
    fn a_kubeadm_context_is_certificate_based() {
        // The default context name a kubeadm cluster hands out, and the shape
        // that must not be told to run a cloud login command.
        assert_eq!(
            auth_kind(&config(KUBEADM), "kubernetes-admin@kubernetes"),
            AuthKind::ClientCertificate
        );
    }

    #[test]
    fn a_credential_plugin_wins_over_everything_else() {
        assert_eq!(auth_kind(&config(CLOUD), "eks"), AuthKind::Exec);
    }

    #[test]
    fn a_token_context_is_recognised() {
        let yaml = r#"
apiVersion: v1
kind: Config
clusters: [{ name: c, cluster: { server: https://c } }]
contexts: [{ name: c, context: { cluster: c, user: u } }]
users: [{ name: u, user: { token: abc } }]
"#;
        assert_eq!(auth_kind(&config(yaml), "c"), AuthKind::Token);
    }

    #[test]
    fn a_context_that_is_not_there_is_unknown_rather_than_a_panic() {
        assert_eq!(
            auth_kind(&config(KUBEADM), "no-such-context"),
            AuthKind::Unknown
        );
    }

    #[test]
    fn a_user_entry_with_nothing_in_it_is_unknown() {
        let yaml = r#"
apiVersion: v1
kind: Config
clusters: [{ name: c, cluster: { server: https://c } }]
contexts: [{ name: c, context: { cluster: c, user: u } }]
users: [{ name: u, user: {} }]
"#;
        assert_eq!(auth_kind(&config(yaml), "c"), AuthKind::Unknown);
    }
}

#[cfg(test)]
mod qualify_tests {
    use super::*;

    /// Two kubeadm clusters, as they arrive: identical cluster and user names.
    fn kubeadm(server: &str) -> String {
        format!(
            r#"
apiVersion: v1
kind: Config
clusters: [{{ name: kubernetes, cluster: {{ server: {server}, certificate-authority-data: Zm9v }} }}]
contexts: [{{ name: kubernetes-admin@kubernetes, context: {{ cluster: kubernetes, user: kubernetes-admin }} }}]
users: [{{ name: kubernetes-admin, user: {{ client-certificate-data: {server_cert}, client-key-data: a2V5 }} }}]
"#,
            server = server,
            server_cert = if server.contains("one") {
                "Y2VydDE="
            } else {
                "Y2VydDI="
            }
        )
    }

    #[test]
    fn two_kubeadm_imports_stop_colliding() {
        // Contexts collide by name too, but the import flow already asks the
        // user to rename those; this models that step so the test exercises
        // what actually reaches disk.
        let renamed = rename_contexts(
            &kubeadm("https://two:6443"),
            &BTreeMap::from([(
                "kubernetes-admin@kubernetes".to_string(),
                "kubernetes-admin@two".to_string(),
            )]),
        )
        .expect("rename");

        let first = qualify_entries(&kubeadm("https://one:6443"), "one").expect("first");
        let second = qualify_entries(&renamed, "two").expect("second");

        let a = Kubeconfig::from_yaml(&first).unwrap();
        let b = Kubeconfig::from_yaml(&second).unwrap();

        assert_eq!(a.auth_infos[0].name, "kubernetes-admin__one");
        assert_eq!(b.auth_infos[0].name, "kubernetes-admin__two");
        assert_eq!(a.clusters[0].name, "kubernetes__one");

        // The merge the app performs at load time is first-wins by name; with
        // the names qualified, the second file's credential survives it.
        let merged = a.clone().merge(b.clone()).expect("merge");
        assert_eq!(merged.auth_infos.len(), 2);
        assert_eq!(merged.clusters.len(), 2);

        // And each context still points at its own entries.
        for (context, want) in [
            (&merged.contexts[0], "kubernetes-admin__one"),
            (&merged.contexts[1], "kubernetes-admin__two"),
        ] {
            assert_eq!(
                context.context.as_ref().unwrap().user.as_deref(),
                Some(want)
            );
        }
    }

    #[test]
    fn the_context_keeps_pointing_at_its_own_cluster_and_user() {
        let yaml = qualify_entries(&kubeadm("https://one:6443"), "prod").expect("qualified");
        let config = Kubeconfig::from_yaml(&yaml).unwrap();
        let ctx = config.contexts[0].context.as_ref().unwrap();

        assert_eq!(ctx.cluster, config.clusters[0].name);
        assert_eq!(
            ctx.user.as_deref(),
            Some(config.auth_infos[0].name.as_str())
        );
        // The context's own name is left alone; that rename is a separate,
        // user-visible decision.
        assert_eq!(config.contexts[0].name, "kubernetes-admin@kubernetes");
    }

    #[test]
    fn qualifying_twice_does_not_stack_suffixes() {
        let once = qualify_entries(&kubeadm("https://one:6443"), "prod").unwrap();
        let twice = qualify_entries(&once, "prod").unwrap();
        let config = Kubeconfig::from_yaml(&twice).unwrap();
        assert_eq!(config.auth_infos[0].name, "kubernetes-admin__prod");
    }

    #[test]
    fn an_empty_suffix_changes_nothing() {
        let yaml = qualify_entries(&kubeadm("https://one:6443"), "  ").unwrap();
        let config = Kubeconfig::from_yaml(&yaml).unwrap();
        assert_eq!(config.auth_infos[0].name, "kubernetes-admin");
    }

    #[test]
    fn the_credential_itself_is_untouched() {
        let before = Kubeconfig::from_yaml(&kubeadm("https://one:6443")).unwrap();
        let after =
            Kubeconfig::from_yaml(&qualify_entries(&kubeadm("https://one:6443"), "x").unwrap())
                .unwrap();
        assert_eq!(
            before.auth_infos[0]
                .auth_info
                .as_ref()
                .unwrap()
                .client_certificate_data,
            after.auth_infos[0]
                .auth_info
                .as_ref()
                .unwrap()
                .client_certificate_data
        );
    }
}
