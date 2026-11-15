//! User preferences, persisted between runs.
//!
//! Written to the platform's config directory as JSON. Deliberately small and
//! forgiving: a preferences file that fails to parse must not stop the app
//! starting, so an unreadable or half-written file falls back to defaults
//! rather than propagating an error.

use std::path::PathBuf;

use k8s_core::discovery::ColumnDef;
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
    /// Free-text group shown as a divider in the cluster rail (e.g.
    /// "Production", "Staging"). Purely a display grouping — connecting,
    /// switching and the `⌘N` hotkeys are unaffected.
    pub workspace: Option<String>,

    /// `kubectl --as`
    pub impersonate_user: Option<String>,
    /// `kubectl --as-group`
    pub impersonate_groups: Vec<String>,
    pub default_namespace: Option<String>,
    /// Disables server identity checks. Surfaced with a warning.
    pub accept_invalid_certs: bool,
    pub proxy_url: Option<String>,
}

impl ClusterProfile {
    /// True when nothing is set, so the entry need not be kept.
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.colour.is_none()
            && self.workspace.is_none()
            && self.impersonate_user.is_none()
            && self.impersonate_groups.is_empty()
            && self.default_namespace.is_none()
            && !self.accept_invalid_certs
            && self.proxy_url.is_none()
    }
}

/// A resource pinned for one-click access, independent of the live watch that
/// lists it — identified by name rather than `uid` since a pin can outlive
/// any particular watch session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedObject {
    pub cluster: String,
    pub resource_key: String,
    pub namespace: Option<String>,
    pub name: String,
    pub label: Option<String>,
    /// The kind's name, for the icon and the label. Missing in files saved
    /// before it was recorded.
    #[serde(default)]
    pub kind: Option<String>,
}

impl PinnedObject {
    /// Whether two pins are the same object, whatever else they carry.
    pub fn same_object(&self, other: &Self) -> bool {
        self.cluster == other.cluster
            && self.resource_key == other.resource_key
            && self.namespace == other.namespace
            && self.name == other.name
    }
}

/// Add a pin, or remove it, leaving the others. Pinning what is already pinned
/// changes nothing, so it can be applied to a copy and to the saved list alike.
pub fn set_pinned(list: &mut Vec<PinnedObject>, pin: PinnedObject, pinned: bool) {
    list.retain(|existing| !existing.same_object(&pin));
    if pinned {
        list.push(pin);
    }
}

/// A user-defined row action that opens the local kubectl shell with a
/// templated command already typed in — `{namespace}`/`{name}` are
/// substituted per row. Runs inside the same kubeconfig-pinned shell every
/// other terminal session uses; no separate execution surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomAction {
    pub name: String,
    pub command_template: String,
    /// Resource kind this action applies to (e.g. "Pod"), or `None` for every kind.
    pub applies_to: Option<String>,
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

    /// Check for a new release on startup. On by default — the only outbound
    /// request the app makes on its own — but a single toggle in Settings, and
    /// checking is never installing: nothing downloads without an explicit click.
    pub check_updates_on_startup: bool,

    /// Per-cluster settings, keyed by context name.
    pub cluster_profiles: std::collections::BTreeMap<String, ClusterProfile>,

    /// The namespaces each cluster was last narrowed to, keyed by context
    /// name. A cluster with nothing here shows every namespace.
    pub namespace_scopes: std::collections::BTreeMap<String, Vec<String>>,

    /// Context names in the order the user arranged them in the cluster rail.
    /// Contexts missing from it follow the listed ones in kubeconfig order.
    pub cluster_order: Vec<String>,

    /// Section labels of the resource sidebar in the order the user arranged
    /// them. Sections it does not list follow, in the default order.
    pub section_order: Vec<String>,

    /// Resource keys in the order the user arranged them within their section.
    pub resource_order: Vec<String>,

    /// Resources pinned for one-click access from the sidebar.
    pub pinned: Vec<PinnedObject>,

    /// User-defined table columns, keyed by `"{group}/{kind}"` (empty group
    /// for core/v1, matching `formSpec.ts`'s `LAYOUTS` convention).
    pub custom_columns: std::collections::BTreeMap<String, Vec<ColumnDef>>,
    /// User-defined row actions (Settings → Actions).
    pub custom_actions: Vec<CustomAction>,
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
            check_updates_on_startup: true,
            cluster_profiles: std::collections::BTreeMap::new(),
            namespace_scopes: std::collections::BTreeMap::new(),
            cluster_order: Vec::new(),
            section_order: Vec::new(),
            resource_order: Vec::new(),
            pinned: Vec::new(),
            custom_columns: std::collections::BTreeMap::new(),
            custom_actions: Vec::new(),
        }
    }
}

/// Indexes of `names` in the saved `order`. Names it does not list keep their
/// relative order after the listed ones, so a new cluster lands at the end.
pub fn arrange(names: &[&str], order: &[String]) -> Vec<usize> {
    let mut indexes: Vec<usize> = (0..names.len()).collect();
    indexes.sort_by_key(|&index| {
        order
            .iter()
            .position(|entry| entry == names[index])
            .unwrap_or(usize::MAX)
    });
    indexes
}

/// Move `moved` to the slot `target` occupies, shifting the others. False when
/// either is missing or they are the same, so nothing changed.
pub fn move_to(names: &mut Vec<String>, moved: &str, target: &str) -> bool {
    let from = names.iter().position(|name| name == moved);
    let to = names.iter().position(|name| name == target);
    match (from, to) {
        (Some(from), Some(to)) if from != to => {
            let item = names.remove(from);
            names.insert(to, item);
            true
        }
        _ => false,
    }
}

/// The saved `order` after `moved` takes `target`'s slot among `shown`, the
/// names on screen. Names that are not shown keep their place after them, so a
/// section absent from one cluster is not forgotten. `None` when nothing moved.
pub fn reordered(
    order: &[String],
    mut shown: Vec<String>,
    moved: &str,
    target: &str,
) -> Option<Vec<String>> {
    if !move_to(&mut shown, moved, target) {
        return None;
    }
    let rest: Vec<String> = order
        .iter()
        .filter(|name| !shown.contains(name))
        .cloned()
        .collect();
    shown.extend(rest);
    Some(shown)
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

    /// The namespaces a context was last narrowed to; empty means all.
    pub fn saved_namespaces(&self, context: &str) -> &[String] {
        self.namespace_scopes
            .get(context)
            .map_or(&[], Vec::as_slice)
    }

    /// Remember a context's namespaces. An empty list forgets the entry, so
    /// the file does not fill up with clusters that show everything.
    pub fn remember_namespaces(&mut self, context: &str, namespaces: &[String]) {
        if namespaces.is_empty() {
            self.namespace_scopes.remove(context);
        } else {
            self.namespace_scopes
                .insert(context.to_string(), namespaces.to_vec());
        }
    }

    /// Put `moved` in `target`'s place in the rail. `current` is the order on
    /// screen, which becomes the saved order. A no-op leaves the file untouched.
    pub fn reorder_clusters(&mut self, mut current: Vec<String>, moved: &str, target: &str) {
        if move_to(&mut current, moved, target) {
            self.cluster_order = current;
        }
    }

    /// True when an action against this context should be refused.
    pub fn is_protected(&self, context: &str) -> bool {
        self.protected_contexts
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(context))
    }

    /// Change one context's profile. An entry left with nothing set is dropped,
    /// so the file does not fill up with clusters that have no preferences.
    pub fn update_profile(&mut self, context: &str, edit: impl FnOnce(&mut ClusterProfile)) {
        let mut profile = self.cluster_profiles.remove(context).unwrap_or_default();
        edit(&mut profile);
        if !profile.is_empty() {
            self.cluster_profiles.insert(context.to_string(), profile);
        }
    }

    pub fn is_pinned(&self, pin: &PinnedObject) -> bool {
        self.pinned.iter().any(|existing| existing.same_object(pin))
    }

    /// Mark a context protected or not. Matching ignores case, as
    /// [`Self::is_protected`] does, so un-protecting removes every spelling.
    pub fn set_protected(&mut self, context: &str, protected: bool) {
        self.protected_contexts
            .retain(|entry| !entry.eq_ignore_ascii_case(context));
        if protected {
            self.protected_contexts.push(context.to_string());
        }
    }

    /// The namespace a context starts in. `None` or blank clears it, so the
    /// profile does not keep an empty entry for a cluster with no preference.
    pub fn set_default_namespace(&mut self, context: &str, namespace: Option<&str>) {
        let namespace = namespace.map(str::trim).filter(|value| !value.is_empty());
        match namespace {
            Some(namespace) => {
                self.cluster_profiles
                    .entry(context.to_string())
                    .or_default()
                    .default_namespace = Some(namespace.to_string());
            }
            None => {
                if let Some(profile) = self.cluster_profiles.get_mut(context) {
                    profile.default_namespace = None;
                }
            }
        }
    }

    pub fn path(&self) -> Option<String> {
        config_path().map(|path| path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn saved_order_wins_and_unlisted_names_go_last() {
        let order = strings(&["c", "a"]);
        assert_eq!(arrange(&["a", "b", "c", "d"], &order), vec![2, 0, 1, 3]);
        assert_eq!(arrange(&["a", "b"], &[]), vec![0, 1]);
    }

    #[test]
    fn a_move_takes_the_targets_slot() {
        let mut names = strings(&["a", "b", "c", "d"]);
        assert!(move_to(&mut names, "a", "c"));
        assert_eq!(names, ["b", "c", "a", "d"]);
        assert!(move_to(&mut names, "d", "b"));
        assert_eq!(names, ["d", "b", "c", "a"]);
    }

    #[test]
    fn names_that_are_not_shown_keep_a_place_after_the_shown_ones() {
        let order = strings(&["x", "a", "b"]);
        let next = reordered(&order, strings(&["a", "b", "c"]), "c", "a").unwrap();
        assert_eq!(next, ["c", "a", "b", "x"]);
        assert!(reordered(&order, strings(&["a", "b"]), "a", "a").is_none());
    }

    #[test]
    fn a_move_that_changes_nothing_says_so() {
        let mut names = strings(&["a", "b"]);
        assert!(!move_to(&mut names, "a", "a"));
        assert!(!move_to(&mut names, "a", "zzz"));
        assert!(!move_to(&mut names, "zzz", "a"));
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn an_old_preferences_file_has_no_order() {
        let preferences: Preferences = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(preferences.cluster_order.is_empty());
    }

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
            preferences.check_updates_on_startup,
            "update checks are the one outbound request made without being asked"
        );
    }

    #[test]
    fn namespaces_are_remembered_per_context_and_forgotten_when_empty() {
        let mut preferences = Preferences::default();
        assert!(preferences.saved_namespaces("prod").is_empty());

        preferences.remember_namespaces("prod", &["team-a".into(), "team-b".into()]);
        preferences.remember_namespaces("dev", &["default".into()]);
        assert_eq!(preferences.saved_namespaces("prod"), ["team-a", "team-b"]);
        assert_eq!(preferences.saved_namespaces("dev"), ["default"]);

        preferences.remember_namespaces("prod", &[]);
        assert!(preferences.saved_namespaces("prod").is_empty());
        assert!(!preferences.namespace_scopes.contains_key("prod"));
        assert_eq!(
            preferences.saved_namespaces("dev"),
            ["default"],
            "others stay"
        );
    }

    #[test]
    fn protecting_a_context_is_idempotent_and_undone_in_any_case() {
        let mut preferences = Preferences::default();
        preferences.set_protected("Prod", true);
        preferences.set_protected("prod", true);
        assert_eq!(preferences.protected_contexts, ["prod"]);
        assert!(preferences.is_protected("PROD"));

        preferences.set_protected("Prod", false);
        assert!(preferences.protected_contexts.is_empty());
    }

    fn pin(name: &str) -> PinnedObject {
        PinnedObject {
            cluster: "prod".into(),
            resource_key: "apps/v1/deployments".into(),
            namespace: Some("shop".into()),
            name: name.into(),
            label: None,
            kind: Some("Deployment".into()),
        }
    }

    #[test]
    fn pinning_is_idempotent_and_unpinning_leaves_the_others() {
        let mut list = Vec::new();
        set_pinned(&mut list, pin("web"), true);
        set_pinned(&mut list, pin("web"), true);
        set_pinned(&mut list, pin("api"), true);
        assert_eq!(list.len(), 2);

        set_pinned(&mut list, pin("web"), false);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "api");
        set_pinned(&mut list, pin("web"), false);
        assert_eq!(
            list.len(),
            1,
            "unpinning what is not pinned changes nothing"
        );
    }

    #[test]
    fn a_pin_saved_before_kinds_were_recorded_still_loads() {
        let json = r#"{"pinned":[{"cluster":"c","resourceKey":"core/v1/pods","namespace":"a","name":"p","label":null}]}"#;
        let preferences: Preferences = serde_json::from_str(json).unwrap();
        assert_eq!(preferences.pinned[0].kind, None);
        assert!(preferences.is_pinned(&preferences.pinned[0].clone()));
    }

    #[test]
    fn a_profile_with_nothing_set_is_not_kept() {
        let mut preferences = Preferences::default();
        preferences.update_profile("dev", |profile| profile.colour = Some("#ef4444".into()));
        preferences.update_profile("dev", |profile| profile.display_name = Some("Dev".into()));
        let profile = preferences.profile("dev");
        assert_eq!(profile.colour.as_deref(), Some("#ef4444"));
        assert_eq!(profile.display_name.as_deref(), Some("Dev"));

        preferences.update_profile("dev", |profile| profile.colour = None);
        assert!(
            preferences.cluster_profiles.contains_key("dev"),
            "a name is still set"
        );
        preferences.update_profile("dev", |profile| profile.display_name = None);
        assert!(preferences.cluster_profiles.is_empty());
    }

    #[test]
    fn a_blank_default_namespace_clears_it_without_creating_a_profile() {
        let mut preferences = Preferences::default();
        preferences.set_default_namespace("dev", Some("  "));
        assert!(preferences.cluster_profiles.is_empty());

        preferences.set_default_namespace("dev", Some(" shop "));
        assert_eq!(
            preferences.profile("dev").default_namespace.as_deref(),
            Some("shop")
        );
        preferences.set_default_namespace("dev", None);
        assert_eq!(preferences.profile("dev").default_namespace, None);
    }

    #[test]
    fn a_file_from_before_namespaces_were_saved_still_loads() {
        let preferences: Preferences = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(preferences.namespace_scopes.is_empty());

        let json = serde_json::to_string(&{
            let mut p = Preferences::default();
            p.remember_namespaces("prod", &["a".into()]);
            p
        })
        .unwrap();
        let back: Preferences = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.saved_namespaces("prod"),
            ["a"],
            "survives a round trip"
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
