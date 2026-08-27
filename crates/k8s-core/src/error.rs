use std::fmt;

use crate::kubeconfig::AuthKind;

/// Errors surfaced by `k8s-core`.
///
/// Variants are deliberately coarse: the UI shows `to_string()` directly, so
/// each message must be actionable on its own.
/// `kube::Error` is several hundred bytes, so every variant carrying one is
/// boxed — otherwise every `Result` in the crate pays for the error path.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("kubeconfig error: {0}")]
    Kubeconfig(Box<kube::config::KubeconfigError>),

    #[error("failed to build client for context `{context}`: {source}")]
    ClientBuild {
        context: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("kubernetes api error: {0}")]
    Api(Box<kube::Error>),

    #[error("discovery failed for cluster `{cluster}`: {source}")]
    Discovery {
        cluster: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("unknown cluster `{0}` (not connected)")]
    UnknownCluster(String),

    #[error("context `{0}` not found in kubeconfig")]
    UnknownContext(String),

    #[error("resource `{0}` not found in cluster discovery")]
    UnknownResource(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl From<kube::Error> for CoreError {
    fn from(err: kube::Error) -> Self {
        Self::Api(Box::new(err))
    }
}

impl From<kube::config::KubeconfigError> for CoreError {
    fn from(err: kube::config::KubeconfigError) -> Self {
        Self::Kubeconfig(Box::new(err))
    }
}

impl CoreError {
    pub fn other(msg: impl fmt::Display) -> Self {
        Self::Other(msg.to_string())
    }

    pub fn client_build(context: &str, auth: AuthKind, source: kube::Error) -> Self {
        // A rejected credential is the single most common connection failure and
        // the raw "Unauthorized" tells the user nothing about what to do, so
        // turn the two auth codes into an instruction instead.
        if let kube::Error::Api(status) = &source {
            let hint = match status.code {
                // What to re-run depends on how the context authenticates.
                // Sending a kubeadm cluster to `aws eks update-kubeconfig` is
                // worse than saying nothing.
                401 => Some(rejected_hint(auth)),
                403 => Some(
                    "the credentials are valid but this user may not access the cluster \
                     endpoint. Check the RBAC bindings for this account, or pick a different \
                     context"
                        .to_string(),
                ),
                _ => None,
            };
            if let Some(hint) = hint {
                return Self::Other(format!("cannot connect to `{context}`: {hint}"));
            }
        }
        Self::ClientBuild {
            context: context.to_string(),
            source: Box::new(source),
        }
    }

    pub fn discovery(cluster: &str, source: kube::Error) -> Self {
        Self::Discovery {
            cluster: cluster.to_string(),
            source: Box::new(source),
        }
    }
}

/// What to do about a rejected credential, given how the context supplies one.
fn rejected_hint(auth: AuthKind) -> String {
    let common = "credentials were rejected (401)";
    match auth {
        AuthKind::Exec => format!(
            "{common}. This context authenticates through a credential plugin, so the \
             plugin's session has expired — re-run your provider's login \
             (`aws eks update-kubeconfig`, `gcloud container clusters get-credentials`, \
             `az aks get-credentials`, `kubelogin`) and reload the kubeconfig"
        ),
        AuthKind::ClientCertificate => format!(
            "{common}. This context authenticates with a client certificate, which kubeadm \
             issues for one year. On a control-plane node, `sudo kubeadm certs \
             check-expiration` shows whether it has lapsed; `sudo kubeadm certs renew \
             admin.conf` reissues it, and the fresh /etc/kubernetes/admin.conf then has to \
             be imported here. A rebuilt cluster invalidates the old certificate the same \
             way, even before it expires"
        ),
        AuthKind::Token => format!(
            "{common}. The bearer token in this kubeconfig has expired or was revoked — \
             issue a new one and import the updated kubeconfig"
        ),
        AuthKind::Basic => format!(
            "{common}. The username and password in this kubeconfig were refused. Most \
             clusters no longer accept basic auth at all"
        ),
        AuthKind::Unknown => format!(
            "{common}, and this context carries no credential the client recognises — check \
             that its `user` entry exists in the kubeconfig and is complete"
        ),
    }
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(auth: AuthKind) -> String {
        let status = kube::core::Status {
            code: 401,
            ..Default::default()
        };
        match CoreError::client_build("ctx", auth, kube::Error::Api(Box::new(status))) {
            CoreError::Other(message) => message,
            other => panic!("expected a hint, got {other:?}"),
        }
    }

    #[test]
    fn a_certificate_context_is_not_sent_to_a_cloud_login() {
        // `kubernetes-admin@kubernetes` is kubeadm; there is no `aws eks
        // update-kubeconfig` to run, and suggesting one wastes the reader's time.
        let message = rejected(AuthKind::ClientCertificate);
        assert!(message.contains("kubeadm certs renew"), "{message}");
        assert!(!message.contains("aws eks"), "{message}");
        // A rebuilt cluster fails the same way before the year is up.
        assert!(message.contains("rebuilt cluster"), "{message}");
    }

    #[test]
    fn a_credential_plugin_context_is_sent_to_the_provider_login() {
        let message = rejected(AuthKind::Exec);
        assert!(message.contains("aws eks update-kubeconfig"), "{message}");
        assert!(!message.contains("kubeadm"), "{message}");
    }

    #[test]
    fn every_hint_names_the_context_and_the_code() {
        for auth in [
            AuthKind::Exec,
            AuthKind::Token,
            AuthKind::ClientCertificate,
            AuthKind::Basic,
            AuthKind::Unknown,
        ] {
            let message = rejected(auth);
            assert!(message.starts_with("cannot connect to `ctx`"), "{message}");
            assert!(message.contains("401"), "{message}");
        }
    }

    #[test]
    fn a_403_still_talks_about_rbac_rather_than_credentials() {
        let status = kube::core::Status {
            code: 403,
            ..Default::default()
        };
        let message = match CoreError::client_build(
            "ctx",
            AuthKind::Exec,
            kube::Error::Api(Box::new(status)),
        ) {
            CoreError::Other(message) => message,
            other => panic!("expected a hint, got {other:?}"),
        };
        assert!(message.contains("RBAC"), "{message}");
    }

    #[test]
    fn a_code_with_no_advice_keeps_the_original_error() {
        let status = kube::core::Status {
            code: 500,
            ..Default::default()
        };
        let error =
            CoreError::client_build("ctx", AuthKind::Exec, kube::Error::Api(Box::new(status)));
        // Inventing advice for a server fault would be worse than passing the
        // apiserver's own words through.
        assert!(matches!(error, CoreError::ClientBuild { .. }));
    }
}
