//! What a right-click on a table row offers, decided from the kind alone.
//!
//! Kept free of any window so the rules are tested directly.

use gpui_kit::{
    App, Window,
    assets::IconName,
    component::{
        Icon,
        menu::{PopupMenu, PopupMenuItem},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    Open,
    Logs,
    Shell,
    Forward,
    Pin,
    Unpin,
    CopyName,
    CopyYaml,
    Restart,
    Scale,
    Trigger,
    Suspend,
    Resume,
    Cordon,
    Uncordon,
    Drain,
    Delete,
}

impl RowAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open details",
            Self::Logs => "View logs",
            Self::Shell => "Open shell",
            Self::Forward => "Forward port…",
            Self::Pin => "Pin to sidebar",
            Self::Unpin => "Unpin from sidebar",
            Self::CopyName => "Copy name",
            Self::CopyYaml => "Copy YAML",
            Self::Restart => "Restart",
            Self::Scale => "Scale…",
            Self::Trigger => "Run now",
            Self::Suspend => "Suspend",
            Self::Resume => "Resume",
            Self::Cordon => "Cordon",
            Self::Uncordon => "Uncordon",
            Self::Drain => "Drain…",
            Self::Delete => "Delete…",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Open => IconName::Inspector,
            Self::Logs => IconName::FileText,
            Self::Shell => IconName::SquareTerminal,
            Self::Forward => IconName::ArrowLeftRight,
            Self::Pin => IconName::Pin,
            Self::Unpin => IconName::PinOff,
            Self::CopyName | Self::CopyYaml => IconName::Copy,
            Self::Restart => IconName::RotateCw,
            Self::Scale => IconName::ChevronsUpDown,
            Self::Trigger => IconName::Play,
            Self::Suspend => IconName::Pause,
            Self::Resume => IconName::Play,
            Self::Cordon => IconName::Pause,
            Self::Uncordon => IconName::Play,
            Self::Drain => IconName::Download,
            Self::Delete => IconName::Delete,
        }
    }
}

/// Kinds with pods behind them, whose logs the log view can follow.
const WORKLOADS: [&str; 3] = [
    "apps/v1/deployments",
    "apps/v1/statefulsets",
    "apps/v1/daemonsets",
];

/// Kinds with a scale subresource worth offering. A DaemonSet has none: it runs
/// one pod per node.
const SCALABLE: [&str; 3] = [
    "apps/v1/deployments",
    "apps/v1/statefulsets",
    "apps/v1/replicasets",
];

const CRONJOBS: &str = "batch/v1/cronjobs";

/// Whether a kind is a workload that can be rolled, so a set of them can be
/// restarted together.
pub fn is_workload(resource: &str) -> bool {
    WORKLOADS.contains(&resource)
}

/// The menu as groups; the caller draws a separator between groups.
///
/// `suspended` is a CronJob's state when it is known: only the action that would
/// change something is offered. Unknown offers both.
pub fn groups(resource: &str, pinned: bool, suspended: Option<bool>) -> Vec<Vec<RowAction>> {
    let is_pod = resource == "core/v1/pods";
    let is_workload = WORKLOADS.contains(&resource);
    let is_node = resource == "core/v1/nodes";

    let mut view = vec![RowAction::Open];
    if is_pod || is_workload {
        view.push(RowAction::Logs);
    }
    if is_pod {
        view.push(RowAction::Shell);
    }
    if is_pod || is_workload || resource == "core/v1/services" {
        view.push(RowAction::Forward);
    }

    let mut change = Vec::new();
    if SCALABLE.contains(&resource) {
        change.push(RowAction::Scale);
    }
    if is_workload {
        change.push(RowAction::Restart);
    }
    if resource == CRONJOBS {
        change.push(RowAction::Trigger);
        if suspended != Some(true) {
            change.push(RowAction::Suspend);
        }
        if suspended != Some(false) {
            change.push(RowAction::Resume);
        }
    }
    if is_node {
        change.push(RowAction::Cordon);
        change.push(RowAction::Uncordon);
        change.push(RowAction::Drain);
    }
    // Delete is always last and alone with its kind of action, so it is not
    // hit by accident while reaching for something else.
    change.push(RowAction::Delete);

    vec![
        view,
        vec![
            if pinned {
                RowAction::Unpin
            } else {
                RowAction::Pin
            },
            RowAction::CopyName,
            RowAction::CopyYaml,
        ],
        change,
    ]
}

/// Fill a menu with the entries for a kind, calling `pick` with the choice.
/// `include_open` is false where the object is already open.
pub fn build_menu(
    menu: PopupMenu,
    resource: &str,
    include_open: bool,
    pinned: bool,
    suspended: Option<bool>,
    pick: impl Fn(RowAction, &mut Window, &mut App) + Clone + 'static,
) -> PopupMenu {
    let mut menu = menu;
    let mut first = true;
    for group in groups(resource, pinned, suspended) {
        let group: Vec<RowAction> = group
            .into_iter()
            .filter(|action| include_open || *action != RowAction::Open)
            .collect();
        if group.is_empty() {
            continue;
        }
        if !first {
            menu = menu.separator();
        }
        first = false;
        for action in group {
            let pick = pick.clone();
            menu = menu.item(
                PopupMenuItem::new(action.label())
                    .icon(Icon::new(action.icon()))
                    .on_click(move |_, window, cx| pick(action, window, cx)),
            );
        }
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::{RowAction, groups};

    fn flat(resource: &str) -> Vec<RowAction> {
        groups(resource, false, None)
            .into_iter()
            .flatten()
            .collect()
    }

    #[test]
    fn every_kind_can_be_opened_copied_and_deleted() {
        let all = flat("core/v1/configmaps");
        for action in [
            RowAction::Open,
            RowAction::CopyName,
            RowAction::CopyYaml,
            RowAction::Delete,
        ] {
            assert!(all.contains(&action));
        }
        assert!(!all.contains(&RowAction::Logs));
        assert!(!all.contains(&RowAction::Restart));
    }

    #[test]
    fn pods_get_logs_and_a_shell_but_cannot_be_restarted() {
        let pod = flat("core/v1/pods");
        assert!(pod.contains(&RowAction::Logs) && pod.contains(&RowAction::Shell));
        assert!(!pod.contains(&RowAction::Restart));
    }

    #[test]
    fn a_pinned_object_offers_unpin_instead_of_pin() {
        let flat_of = |pinned| -> Vec<RowAction> {
            groups("core/v1/pods", pinned, None)
                .into_iter()
                .flatten()
                .collect()
        };
        assert!(flat_of(false).contains(&RowAction::Pin));
        assert!(!flat_of(false).contains(&RowAction::Unpin));
        assert!(flat_of(true).contains(&RowAction::Unpin));
        assert!(!flat_of(true).contains(&RowAction::Pin));
    }

    #[test]
    fn only_kinds_with_pods_behind_them_can_be_forwarded() {
        for resource in ["core/v1/pods", "core/v1/services", "apps/v1/deployments"] {
            assert!(flat(resource).contains(&RowAction::Forward), "{resource}");
        }
        for resource in ["core/v1/configmaps", "core/v1/nodes", "apps/v1/replicasets"] {
            assert!(!flat(resource).contains(&RowAction::Forward), "{resource}");
        }
    }

    #[test]
    fn workloads_get_logs_and_restart_but_no_shell() {
        let deployment = flat("apps/v1/deployments");
        assert!(deployment.contains(&RowAction::Logs));
        assert!(deployment.contains(&RowAction::Restart));
        assert!(!deployment.contains(&RowAction::Shell));
    }

    #[test]
    fn only_kinds_with_a_replica_count_can_be_scaled() {
        for resource in [
            "apps/v1/deployments",
            "apps/v1/statefulsets",
            "apps/v1/replicasets",
        ] {
            assert!(flat(resource).contains(&RowAction::Scale), "{resource}");
        }
        for resource in ["apps/v1/daemonsets", "core/v1/pods", "batch/v1/cronjobs"] {
            assert!(!flat(resource).contains(&RowAction::Scale), "{resource}");
        }
    }

    #[test]
    fn a_cronjob_can_run_now_and_be_paused_or_resumed() {
        let cronjob = flat("batch/v1/cronjobs");
        for action in [RowAction::Trigger, RowAction::Suspend, RowAction::Resume] {
            assert!(cronjob.contains(&action));
        }
        assert!(!flat("batch/v1/jobs").contains(&RowAction::Trigger));
    }

    #[test]
    fn a_cronjob_offers_only_the_change_that_would_do_something() {
        let of = |suspended| -> Vec<RowAction> {
            groups("batch/v1/cronjobs", false, suspended)
                .into_iter()
                .flatten()
                .collect()
        };
        assert!(of(Some(false)).contains(&RowAction::Suspend));
        assert!(!of(Some(false)).contains(&RowAction::Resume));
        assert!(of(Some(true)).contains(&RowAction::Resume));
        assert!(!of(Some(true)).contains(&RowAction::Suspend));
        let unknown = of(None);
        assert!(unknown.contains(&RowAction::Suspend) && unknown.contains(&RowAction::Resume));
        assert!(
            of(Some(true)).contains(&RowAction::Trigger),
            "run now stays"
        );
    }

    #[test]
    fn nodes_can_be_cordoned_either_way() {
        let node = flat("core/v1/nodes");
        assert!(node.contains(&RowAction::Cordon) && node.contains(&RowAction::Uncordon));
    }

    #[test]
    fn only_nodes_can_be_drained_and_it_sits_before_delete() {
        let node = flat("core/v1/nodes");
        let drain = node.iter().position(|a| *a == RowAction::Drain);
        let delete = node.iter().position(|a| *a == RowAction::Delete);
        assert!(drain.is_some() && drain < delete);
        assert!(!flat("core/v1/pods").contains(&RowAction::Drain));
    }

    #[test]
    fn delete_is_always_the_last_entry() {
        for resource in [
            "core/v1/pods",
            "apps/v1/deployments",
            "core/v1/nodes",
            "x/v1/y",
        ] {
            assert_eq!(flat(resource).last(), Some(&RowAction::Delete));
        }
    }
}
