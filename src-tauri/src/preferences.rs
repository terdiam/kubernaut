//! User preferences, persisted between runs.
//!
//! Written to the platform's config directory as JSON. Deliberately small and
//! forgiving: a preferences file that fails to parse must not stop the app
//! starting, so an unreadable or half-written file falls back to defaults
//! rather than propagating an error.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Language {
    #[default]
    English,
    Indonesian,
}

/// Per-cluster settings that belong to this app, not to the kubeconfig.
///
/// A display name and colour so a production cluster is distinguishable at a
/// glance, plus the connection options the kubeconfig has no field for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClusterProfile {
    /// Shown instead of the context name. The context name is unchanged, so
    /// kubectl and this app still agree on what to call the cluster.
    pub display_name: Option<String>,
    /// Accent for this cluster's tile. Making production look different is the
    /// cheapest guard against acting on the wrong one.
    pub colour: Option<String>,

    /// `kubectl --as`
    pub impersonate_user: Option<String>,
    /// `kubectl --as-group`
    pub impersonate_groups: Vec<String>,
    pub default_namespace: Option<String>,
    /// Disables server identity checks. Surfaced with a warning.
    pub accept_invalid_certs: bool,
    pub proxy_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub theme: Theme,
    pub language: Language,

    /// Extra directories prepended to `PATH` before kubeconfig exec plugins run.
    ///
    /// The escape hatch for the case the login-shell probe cannot cover: a
    /// custom launcher, a nix profile, or a shell whose startup times out.
    pub extra_path_entries: Vec<PathBuf>,

    /// Lines of history requested when a log view opens.
    pub log_tail_lines: i64,

    /// IANA zone for displaying timestamps, or `system` to follow the machine.
    ///
    /// Kubernetes reports every timestamp in UTC. Showing them raw is a
    /// reliable way to mis-read an incident timeline by seven hours, so they
    /// are converted for display — while the YAML tab keeps the original,
    /// because that is what the cluster actually stores.
    pub timezone: String,

    /// Show absolute timestamps alongside relative ages.
    pub show_absolute_times: bool,

    /// Contexts where destructive actions are refused outright.
    ///
    /// Confirmation dialogs stop accidents, not habit. A name on this list
    /// cannot be scaled, drained, deleted or uninstalled from the app at all.
    pub protected_contexts: Vec<String>,

    /// Check for a new release on startup. Off by default: the app must not
    /// talk to the network until asked.
    pub check_updates_on_startup: bool,

    /// Per-cluster settings, keyed by context name.
    pub cluster_profiles: std::collections::BTreeMap<String, ClusterProfile>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            language: Language::default(),
            extra_path_entries: Vec::new(),
            log_tail_lines: 500,
            timezone: "system".to_string(),
            show_absolute_times: false,
            protected_contexts: Vec::new(),
            check_updates_on_startup: false,
            cluster_profiles: std::collections::BTreeMap::new(),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let directories = directories::ProjectDirs::from("dev", "kubernaut", "Kubernaut")?;
    Some(directories.config_dir().join("preferences.json"))
}

impl Preferences {
    /// Load, falling back to defaults for anything unreadable.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str(&text) {
            Ok(preferences) => preferences,
            Err(err) => {
                // Keep the broken file: it may hold something the user wants
                // back, and silently overwriting it would lose that.
                tracing::warn!(path = %path.display(), %err, "unreadable preferences; using defaults");
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or("no config directory on this platform")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|err| err.to_string())?;

        // Write to a sibling then rename: a crash mid-write must not leave a
        // truncated file that resets every preference on next start.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|err| err.to_string())?;
        std::fs::rename(&temporary, &path).map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn profile(&self, context: &str) -> ClusterProfile {
        self.cluster_profiles
            .get(context)
            .cloned()
            .unwrap_or_default()
    }

    /// True when an action against this context should be refused.
    pub fn is_protected(&self, context: &str) -> bool {
        self.protected_contexts
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(context))
    }

    pub fn path(&self) -> Option<String> {
        config_path().map(|path| path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_do_not_break_loading() {
        // A file written by a newer version must still load.
        let json = r#"{"theme":"dark","somethingNew":42}"#;
        let preferences: Preferences = serde_json::from_str(json).unwrap();
        assert_eq!(preferences.theme, Theme::Dark);
        assert_eq!(
            preferences.log_tail_lines, 500,
            "missing fields take defaults"
        );
    }

    #[test]
    fn missing_file_yields_defaults() {
        let preferences = Preferences::default();
        assert_eq!(preferences.theme, Theme::System);
        assert!(
            !preferences.check_updates_on_startup,
            "no network by default"
        );
    }

    #[test]
    fn protected_contexts_match_case_insensitively() {
        let preferences = Preferences {
            protected_contexts: vec!["Prod-Cluster".into()],
            ..Default::default()
        };
        assert!(preferences.is_protected("prod-cluster"));
        assert!(!preferences.is_protected("staging"));
    }
}
