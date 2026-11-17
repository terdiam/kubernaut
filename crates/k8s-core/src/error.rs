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
        } else {
            // The top of a transport error says only "client error (Connect)";
            // what went wrong is in the causes beneath it.
            let chain = error_chain(&source);
            let reason = transport_hint(&chain).unwrap_or_else(|| chain.clone());
            return Self::Other(format!("cannot connect to `{context}`: {reason}"));
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

/// A credential plugin that exited with an error. Its own message went to the
/// console the app does not have, so say what to try instead of echoing the
/// empty output twice.
fn exec_failed_hint(chain: &str) -> String {
    let command = chain
        .split(" failed with status")
        .next()
        .unwrap_or(chain)
        .trim_start_matches("auth error: ");
    format!(
        "the login plugin failed and its message is not shown here: {command}. Run that same \
         command in a terminal to see why. For `kubectl oidc-login`, `unknown command` means \
         the plugin is not installed (`kubectl krew install oidc-login`) or its folder \
         (`%USERPROFILE%\\.krew\\bin` on Windows) is not on PATH"
    )
}

/// An error and everything beneath it, joined, so the cause is not lost behind
/// a one-line summary such as `client error (Connect)`.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// What a transport failure means and what to do about it, from the text of its
/// causes. `None` when it is not one of the failures people hit and can fix.
fn transport_hint(chain: &str) -> Option<String> {
    let lower = chain.to_ascii_lowercase();
    if lower.contains("auth exec command") && lower.contains("failed with status") {
        return Some(exec_failed_hint(chain));
    }
    if lower.contains("invalid peer certificate") {
        return Some(certificate_hint(chain, &lower));
    }
    if lower.contains("connection refused") {
        return Some(
            "the connection was refused: nothing is listening at that address and port, or a \
             firewall turned it away. Check the server URL in the kubeconfig and that the \
             apiserver is running"
                .to_string(),
        );
    }
    if lower.contains("dns error")
        || lower.contains("failed to lookup")
        || lower.contains("name or service not known")
    {
        return Some(
            "the server's host name could not be resolved. Check the URL in the kubeconfig, \
             and the VPN or DNS if it is an internal name"
                .to_string(),
        );
    }
    if lower.contains("timed out") || lower.contains("deadline has elapsed") {
        return Some(
            "no answer from the server: the VPN may be down, or a firewall is dropping the \
             connection"
                .to_string(),
        );
    }
    if lower.contains("unreachable") || lower.contains("no route to host") {
        return Some("there is no route to the server: the VPN may be disconnected".to_string());
    }
    None
}

/// Why the server's certificate was refused. `chain` keeps rustls's own words
/// for the one case, a name mismatch, where they are the useful part.
fn certificate_hint(chain: &str, lower: &str) -> String {
    if lower.contains("unknownissuer") || lower.contains("badsignature") {
        // A regenerated CA often keeps its name (`CN=kubernetes`), so the
        // failure is a bad signature rather than an unknown issuer.
        return "the server's certificate was not issued by the CA in this kubeconfig. The \
                cluster was probably rebuilt, or its CA regenerated, so the stored CA is out \
                of date: import a fresh kubeconfig (on kubeadm, /etc/kubernetes/admin.conf), \
                or edit this cluster and replace its certificate authority"
            .to_string();
    }
    if let Some(at) = lower.find("not valid for name") {
        let detail = chain.get(at..).unwrap_or(chain);
        return format!(
            "the server's certificate does not cover the address in this kubeconfig ({detail}). \
             Connect by a name or IP the certificate lists, or add the address to the \
             apiserver's certificate SANs"
        );
    }
    if lower.contains("notvalidyet") || lower.contains("not valid yet") {
        return "the server's certificate is not valid yet: check this computer's clock"
            .to_string();
    }
    if lower.contains("expired") {
        return "the server's certificate has expired. On kubeadm, `kubeadm certs \
                check-expiration` on a control-plane node shows which"
            .to_string();
    }
    format!("the server's certificate was rejected ({chain})")
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

    /// A transport failure the way kube reports one: a summary, then causes.
    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);

    impl fmt::Display for Layer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1
                .as_deref()
                .map(|e| e as &(dyn std::error::Error + 'static))
        }
    }

    fn transport(summary: &'static str, causes: &[&'static str]) -> CoreError {
        let layers = causes
            .iter()
            .rev()
            .fold(None, |inner, text| Some(Box::new(Layer(text, inner))));
        let error = Layer(summary, layers);
        CoreError::client_build("qb", AuthKind::Exec, kube::Error::Service(Box::new(error)))
    }

    fn message(error: CoreError) -> String {
        match error {
            CoreError::Other(message) => message,
            other => panic!("expected a message, got {other:?}"),
        }
    }

    #[test]
    fn a_regenerated_ca_is_named_as_such_and_not_as_connect() {
        // Seen for real: the cluster was rebuilt, its CA kept the name
        // `kubernetes` but not the key, and the app said only "(Connect)".
        let text = message(transport(
            "client error (Connect)",
            &["invalid peer certificate: BadSignature"],
        ));
        assert!(text.starts_with("cannot connect to `qb`"), "{text}");
        assert!(text.contains("not issued by the CA"), "{text}");
        assert!(text.contains("import a fresh kubeconfig"), "{text}");
        assert!(!text.contains("(Connect)"), "{text}");

        let unknown = message(transport(
            "client error (Connect)",
            &["invalid peer certificate: UnknownIssuer"],
        ));
        assert!(unknown.contains("not issued by the CA"), "{unknown}");
    }

    #[test]
    fn an_address_the_certificate_does_not_cover_says_what_it_does_cover() {
        let text = message(transport(
            "client error (Connect)",
            &[
                r#"invalid peer certificate: certificate not valid for name "10.0.0.9"; certificate is only valid for DnsName("k8s-api.qb.internal")"#,
            ],
        ));
        assert!(text.contains("does not cover the address"), "{text}");
        assert!(
            text.contains(r#"only valid for DnsName("k8s-api.qb.internal")"#),
            "{text}"
        );
    }

    #[test]
    fn dates_and_clocks_are_told_apart() {
        let expired = message(transport(
            "client error (Connect)",
            &["invalid peer certificate: Expired"],
        ));
        assert!(expired.contains("has expired"), "{expired}");
        let early = message(transport(
            "client error (Connect)",
            &["invalid peer certificate: NotValidYet"],
        ));
        assert!(early.contains("clock"), "{early}");
    }

    #[test]
    fn network_failures_point_at_the_network() {
        let refused = message(transport(
            "client error (Connect)",
            &["tcp connect error", "Connection refused (os error 61)"],
        ));
        assert!(refused.contains("refused"), "{refused}");
        let dns = message(transport(
            "client error (Connect)",
            &[
                "dns error",
                "failed to lookup address information: Name or service not known",
            ],
        ));
        assert!(dns.contains("could not be resolved"), "{dns}");
        let timeout = message(transport(
            "client error (Connect)",
            &["tcp connect error", "Connection timed out (os error 110)"],
        ));
        assert!(timeout.contains("VPN"), "{timeout}");
        let route = message(transport(
            "client error (Connect)",
            &["tcp connect error", "No route to host (os error 65)"],
        ));
        assert!(route.contains("no route"), "{route}");
    }

    #[test]
    fn an_unknown_transport_failure_still_shows_its_causes() {
        let text = message(transport(
            "client error (Connect)",
            &["something new", "deep cause"],
        ));
        assert!(
            text.contains("client error (Connect): something new: deep cause"),
            "{text}"
        );
    }

    #[test]
    fn a_failed_login_plugin_points_at_running_it_in_a_terminal() {
        let chain = "auth error: auth exec command '\"kubectl\" \"oidc-login\"' failed with status exit code: 1";
        let hint = transport_hint(chain).unwrap_or_default();
        assert!(hint.contains("krew install oidc-login"), "{hint}");
        assert!(hint.contains("\"kubectl\" \"oidc-login\""), "{hint}");
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
