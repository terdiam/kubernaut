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
