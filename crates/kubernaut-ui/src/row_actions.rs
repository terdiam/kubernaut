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
    CopyName,
    CopyYaml,
    Restart,
    Cordon,
    Uncordon,
    Delete,
}

impl RowAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open details",
            Self::Logs => "View logs",
            Self::Shell => "Open shell",
            Self::CopyName => "Copy name",
            Self::CopyYaml => "Copy YAML",
            Self::Restart => "Restart",
            Self::Cordon => "Cordon",
            Self::Uncordon => "Uncordon",
            Self::Delete => "Delete…",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Open => IconName::Inspector,
            Self::Logs => IconName::FileText,
            Self::Shell => IconName::SquareTerminal,
            Self::CopyName | Self::CopyYaml => IconName::Copy,
            Self::Restart => IconName::RotateCw,
            Self::Cordon => IconName::Pause,
            Self::Uncordon => IconName::Play,
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

/// The menu as groups; the caller draws a separator between groups.
pub fn groups(resource: &str) -> Vec<Vec<RowAction>> {
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

    let mut change = Vec::new();
    if is_workload {
        change.push(RowAction::Restart);
    }
    if is_node {
        change.push(RowAction::Cordon);
        change.push(RowAction::Uncordon);
    }
    // Delete is always last and alone with its kind of action, so it is not
    // hit by accident while reaching for something else.
    change.push(RowAction::Delete);

    vec![view, vec![RowAction::CopyName, RowAction::CopyYaml], change]
}

/// Fill a menu with the entries for a kind, calling `pick` with the choice.
/// `include_open` is false where the object is already open.
pub fn build_menu(
    menu: PopupMenu,
    resource: &str,
    include_open: bool,
    pick: impl Fn(RowAction, &mut Window, &mut App) + Clone + 'static,
) -> PopupMenu {
    let mut menu = menu;
    let mut first = true;
    for group in groups(resource) {
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
        groups(resource).into_iter().flatten().collect()
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
    fn workloads_get_logs_and_restart_but_no_shell() {
        let deployment = flat("apps/v1/deployments");
        assert!(deployment.contains(&RowAction::Logs));
        assert!(deployment.contains(&RowAction::Restart));
        assert!(!deployment.contains(&RowAction::Shell));
    }

    #[test]
    fn nodes_can_be_cordoned_either_way() {
        let node = flat("core/v1/nodes");
        assert!(node.contains(&RowAction::Cordon) && node.contains(&RowAction::Uncordon));
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
