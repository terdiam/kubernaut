//! Sorting the cluster's resource types into the sections people look for.
//!
//! Discovery groups types by API group (`apps`, `batch`, `networking.k8s.io`),
//! which is how the apiserver is organised and not how anyone thinks about a
//! cluster. A person looking for "my pods" or "the ingress" wants Workloads and
//! Network, so the sidebar groups by what a type is for.

use gpui_kit::assets::IconName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Workloads,
    Network,
    Config,
    Storage,
    Access,
    Cluster,
    Custom,
    Other,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::Workloads => "Workloads",
            Self::Network => "Network",
            Self::Config => "Config",
            Self::Storage => "Storage",
            Self::Access => "Access control",
            Self::Cluster => "Cluster",
            Self::Custom => "Custom resources",
            Self::Other => "Other",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Workloads => IconName::Box,
            Self::Network => IconName::Network,
            Self::Config => IconName::SlidersHorizontal,
            Self::Storage => IconName::HardDrive,
            Self::Access => IconName::KeyRound,
            Self::Cluster => IconName::Server,
            Self::Custom => IconName::Puzzle,
            Self::Other => IconName::Folder,
        }
    }

    /// Kinds that belong here, in the order they are listed. Anything not named
    /// sorts after these, alphabetically.
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Self::Workloads => &[
                "Pod",
                "Deployment",
                "StatefulSet",
                "DaemonSet",
                "ReplicaSet",
                "Job",
                "CronJob",
                "ReplicationController",
                "HorizontalPodAutoscaler",
                "PodDisruptionBudget",
                "PodTemplate",
                "ControllerRevision",
            ],
            Self::Network => &[
                "Service",
                "Ingress",
                "Endpoints",
                "EndpointSlice",
                "IngressClass",
                "NetworkPolicy",
            ],
            Self::Config => &[
                "ConfigMap",
                "Secret",
                "ResourceQuota",
                "LimitRange",
                "PriorityClass",
                "RuntimeClass",
            ],
            Self::Storage => &[
                "PersistentVolumeClaim",
                "PersistentVolume",
                "StorageClass",
                "VolumeAttachment",
                "CSIDriver",
                "CSINode",
                "CSIStorageCapacity",
            ],
            Self::Access => &[
                "ServiceAccount",
                "Role",
                "RoleBinding",
                "ClusterRole",
                "ClusterRoleBinding",
                "CertificateSigningRequest",
            ],
            Self::Cluster => &[
                "Node",
                "Namespace",
                "Event",
                "CustomResourceDefinition",
                "APIService",
                "Lease",
                "ComponentStatus",
                "MutatingWebhookConfiguration",
                "ValidatingWebhookConfiguration",
                "ValidatingAdmissionPolicy",
                "ValidatingAdmissionPolicyBinding",
            ],
            Self::Custom | Self::Other => &[],
        }
    }
}

const BUILT_IN: [Category; 6] = [
    Category::Workloads,
    Category::Network,
    Category::Config,
    Category::Storage,
    Category::Access,
    Category::Cluster,
];

/// Where a type is listed, and its position within that section.
///
/// Custom types stay under Custom resources even when they share a name with a
/// built-in kind (several CRDs define a `Node` or a `Service`), so a built-in
/// match is only made for types that do not come from a CRD.
pub fn classify(kind: &str, is_crd: bool) -> (Category, usize) {
    if is_crd {
        return (Category::Custom, usize::MAX);
    }
    for category in BUILT_IN {
        if let Some(rank) = category.kinds().iter().position(|known| *known == kind) {
            return (category, rank);
        }
    }
    (Category::Other, usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Category, classify};

    #[test]
    fn built_in_kinds_land_in_the_section_a_person_would_look_in() {
        assert_eq!(classify("Pod", false).0, Category::Workloads);
        assert_eq!(classify("Ingress", false).0, Category::Network);
        assert_eq!(classify("Secret", false).0, Category::Config);
        assert_eq!(
            classify("PersistentVolumeClaim", false).0,
            Category::Storage
        );
        assert_eq!(classify("ClusterRoleBinding", false).0, Category::Access);
        assert_eq!(classify("Node", false).0, Category::Cluster);
    }

    #[test]
    fn the_most_used_kind_in_a_section_comes_first() {
        let rank = |kind| classify(kind, false).1;
        assert!(rank("Pod") < rank("Deployment"));
        assert!(rank("Deployment") < rank("CronJob"));
        assert!(rank("Service") < rank("NetworkPolicy"));
    }

    #[test]
    fn a_custom_type_is_custom_even_when_it_borrows_a_built_in_name() {
        assert_eq!(classify("Certificate", true).0, Category::Custom);
        assert_eq!(classify("Node", true).0, Category::Custom);
    }

    #[test]
    fn unknown_built_in_kinds_fall_to_other_after_the_known_ones() {
        let (category, rank) = classify("SomethingNew", false);
        assert_eq!(category, Category::Other);
        assert_eq!(rank, usize::MAX);
    }

    #[test]
    fn sections_are_ordered_by_how_often_they_are_opened() {
        assert!(Category::Workloads < Category::Network);
        assert!(Category::Cluster < Category::Custom);
        assert!(Category::Custom < Category::Other);
    }
}
