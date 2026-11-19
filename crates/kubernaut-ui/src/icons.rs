//! Icon sources: the component library's bundled set plus the few extra
//! Lucide icons this app uses. Only these are embedded, so the binary does not
//! carry the whole catalogue.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString, assets, assets::IconName};

assets::icon_assets!(
    ExtraIcons,
    [
        Activity,
        ArrowDownToLine,
        ArrowLeftRight,
        BellOff,
        Box,
        Bug,
        Cable,
        ChartLine,
        Clock,
        Cog,
        Container,
        Database,
        Download,
        Eraser,
        FileCode,
        FolderTree,
        Gauge,
        GitBranch,
        Hammer,
        HardDrive,
        KeyRound,
        Layers,
        Link,
        ListChecks,
        Lock,
        Network,
        Pin,
        PinOff,
        Puzzle,
        Rewind,
        Route,
        Ruler,
        ScanSearch,
        ScrollText,
        Server,
        ServerCog,
        Shield,
        ShieldAlert,
        ShieldCheck,
        ShipWheel,
        SlidersHorizontal,
        Stethoscope,
        Tag,
        Tags,
        TextCursorInput,
        Timer,
        Upload,
        UserCog,
        Waypoints,
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// The icon for a kind of resource, so a long sidebar can be scanned by shape
/// as well as by name. `None` for a kind with no icon of its own, which the
/// caller draws as a custom resource.
pub fn kind_icon(kind: &str) -> Option<IconName> {
    Some(match kind {
        "Pod" => IconName::Box,
        "Deployment" => IconName::Layers,
        "StatefulSet" | "PersistentVolume" => IconName::Database,
        "DaemonSet" => IconName::Cpu,
        "ReplicaSet" | "ReplicationController" => IconName::Copy,
        "Job" => IconName::Timer,
        "CronJob" | "Lease" => IconName::Clock,
        "HorizontalPodAutoscaler" | "ResourceQuota" => IconName::Gauge,
        "PodDisruptionBudget" => IconName::ShieldAlert,
        "PodTemplate" | "ControllerRevision" => IconName::FileCode,
        "Service" => IconName::Waypoints,
        "Ingress" | "IngressClass" => IconName::Route,
        "Endpoints" | "EndpointSlice" => IconName::Cable,
        "NetworkPolicy" => IconName::Shield,
        "ConfigMap" => IconName::FileText,
        "Secret" => IconName::Lock,
        "LimitRange" => IconName::Ruler,
        "PersistentVolumeClaim" => IconName::HardDrive,
        "StorageClass" | "RuntimeClass" => IconName::Container,
        "ServiceAccount" => IconName::UserCog,
        "Role" | "ClusterRole" => IconName::ShieldCheck,
        "RoleBinding" | "ClusterRoleBinding" => IconName::Link,
        "Node" => IconName::Server,
        "Namespace" => IconName::FolderTree,
        "Event" => IconName::Bell,
        "PriorityClass" => IconName::ListChecks,
        "CustomResourceDefinition" => IconName::Puzzle,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::kind_icon;
    use gpui_kit::assets::IconName;

    #[test]
    fn well_known_kinds_have_their_own_icon_and_others_have_none() {
        assert!(matches!(kind_icon("Deployment"), Some(IconName::Layers)));
        assert!(matches!(kind_icon("Secret"), Some(IconName::Lock)));
        assert!(
            kind_icon("Widget").is_none(),
            "a custom kind is left to the caller"
        );
    }

    #[test]
    fn the_icon_of_every_drawer_tab_is_embedded() {
        // A tab with no icon would be an empty square, and there is no label to
        // fall back on until it is hovered.
        use gpui_kit::AssetSource;
        for icon in [
            IconName::Info,
            IconName::FileCode,
            IconName::Activity,
            IconName::ChartLine,
            IconName::Gauge,
            IconName::Stethoscope,
            IconName::ScrollText,
            IconName::SquareTerminal,
            IconName::Bug,
            IconName::Tags,
            IconName::ServerCog,
            // The log toolbar.
            IconName::Rewind,
            IconName::Clock,
            IconName::ArrowDownToLine,
            IconName::Eraser,
            IconName::TextCursorInput,
            IconName::Palette,
            IconName::Copy,
            IconName::Download,
        ] {
            let path = IconName::path(icon);
            assert!(
                matches!(super::AppAssets.load(&path), Ok(Some(_))),
                "{path}"
            );
        }
    }

    #[test]
    fn the_icons_a_kind_can_show_are_all_embedded() {
        // An icon that is not embedded draws nothing, silently. Loading every
        // one through the app's own source catches a name left out of the list.
        use gpui_kit::AssetSource;
        let kinds = [
            "Pod",
            "Deployment",
            "StatefulSet",
            "DaemonSet",
            "ReplicaSet",
            "Job",
            "CronJob",
            "HorizontalPodAutoscaler",
            "PodDisruptionBudget",
            "PodTemplate",
            "Service",
            "Ingress",
            "Endpoints",
            "NetworkPolicy",
            "ConfigMap",
            "Secret",
            "LimitRange",
            "PersistentVolumeClaim",
            "StorageClass",
            "ServiceAccount",
            "Role",
            "RoleBinding",
            "Node",
            "Namespace",
            "Event",
            "PriorityClass",
            "CustomResourceDefinition",
        ];
        for kind in kinds {
            let Some(icon) = kind_icon(kind) else {
                continue;
            };
            let path = IconName::path(icon);
            assert!(
                matches!(super::AppAssets.load(&path), Ok(Some(_))),
                "{kind}: {path}"
            );
        }
    }
}
