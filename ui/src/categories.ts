/**
 * Sidebar grouping.
 *
 * API groups are how Kubernetes organises types; they are not how people look
 * for them. Nobody thinks "I need something from `discovery.k8s.io`" — they
 * think "network". These categories mirror the mental model Lens/Rancher users
 * already have, and anything unmapped still shows up under its own API group so
 * no resource becomes unreachable.
 */

import type { IconName } from "./components/Icon";
import type { ResourceDescriptor } from "./types";

export interface Category {
  id: string;
  label: string;
  icon: IconName;
  /** Rendered as a single row rather than a collapsible group. */
  standalone?: boolean;
  /** Which sidebar section the category belongs to. */
  section: "cluster" | "resources";
}

/**
 * Order matters: this is the order the sidebar renders. Cluster-wide entries
 * come first because they are what people check on arriving, then the resource
 * categories in roughly the order a request travels.
 */
export const CATEGORIES: Category[] = [
  { id: "nodes", label: "Nodes", icon: "node", standalone: true, section: "cluster" },
  { id: "namespaces", label: "Namespaces", icon: "namespace", standalone: true, section: "cluster" },
  { id: "events", label: "Events", icon: "events", standalone: true, section: "cluster" },
  { id: "workloads", label: "Workloads", icon: "workloads", section: "resources" },
  { id: "config", label: "Config", icon: "config", section: "resources" },
  { id: "network", label: "Network", icon: "network", section: "resources" },
  { id: "storage", label: "Storage", icon: "storage", section: "resources" },
  { id: "access", label: "Access Control", icon: "access", section: "resources" },
  { id: "custom", label: "Custom Resources", icon: "custom", section: "resources" },
  { id: "other", label: "Other", icon: "other", section: "resources" },
];

/** `group/Kind` → category. Empty group is core/v1. */
const ASSIGNMENTS: Record<string, string> = {
  "/Node": "nodes",
  "/Namespace": "namespaces",
  "/Event": "events",
  "events.k8s.io/Event": "events",

  "/Pod": "workloads",
  "/PodTemplate": "workloads",
  "/ReplicationController": "workloads",
  "apps/Deployment": "workloads",
  "apps/DaemonSet": "workloads",
  "apps/StatefulSet": "workloads",
  "apps/ReplicaSet": "workloads",
  "apps/ControllerRevision": "workloads",
  "batch/Job": "workloads",
  "batch/CronJob": "workloads",

  "/ConfigMap": "config",
  "/Secret": "config",
  "/ResourceQuota": "config",
  "/LimitRange": "config",
  "autoscaling/HorizontalPodAutoscaler": "config",
  "autoscaling.k8s.io/VerticalPodAutoscaler": "config",
  "policy/PodDisruptionBudget": "config",
  "scheduling.k8s.io/PriorityClass": "config",
  "node.k8s.io/RuntimeClass": "config",
  "coordination.k8s.io/Lease": "config",
  "admissionregistration.k8s.io/MutatingWebhookConfiguration": "config",
  "admissionregistration.k8s.io/ValidatingWebhookConfiguration": "config",
  "admissionregistration.k8s.io/ValidatingAdmissionPolicy": "config",
  "admissionregistration.k8s.io/ValidatingAdmissionPolicyBinding": "config",
  "apiextensions.k8s.io/CustomResourceDefinition": "config",
  "apiregistration.k8s.io/APIService": "config",
  "flowcontrol.apiserver.k8s.io/FlowSchema": "config",
  "flowcontrol.apiserver.k8s.io/PriorityLevelConfiguration": "config",

  "/Service": "network",
  "/Endpoints": "network",
  "discovery.k8s.io/EndpointSlice": "network",
  "networking.k8s.io/Ingress": "network",
  "networking.k8s.io/IngressClass": "network",
  "networking.k8s.io/NetworkPolicy": "network",
  "policy.networking.k8s.io/AdminNetworkPolicy": "network",
  "policy.networking.k8s.io/BaselineAdminNetworkPolicy": "network",
  "gateway.networking.k8s.io/Gateway": "network",
  "gateway.networking.k8s.io/HTTPRoute": "network",
  "gateway.networking.k8s.io/GatewayClass": "network",
  "networking.k8s.io/IPAddress": "network",
  "networking.k8s.io/ServiceCIDR": "network",

  "/PersistentVolume": "storage",
  "/PersistentVolumeClaim": "storage",
  "storage.k8s.io/StorageClass": "storage",
  "storage.k8s.io/VolumeAttachment": "storage",
  "storage.k8s.io/CSIDriver": "storage",
  "storage.k8s.io/CSINode": "storage",
  "storage.k8s.io/CSIStorageCapacity": "storage",
  "snapshot.storage.k8s.io/VolumeSnapshot": "storage",
  "snapshot.storage.k8s.io/VolumeSnapshotClass": "storage",
  "snapshot.storage.k8s.io/VolumeSnapshotContent": "storage",

  "/ServiceAccount": "access",
  "rbac.authorization.k8s.io/Role": "access",
  "rbac.authorization.k8s.io/RoleBinding": "access",
  "rbac.authorization.k8s.io/ClusterRole": "access",
  "rbac.authorization.k8s.io/ClusterRoleBinding": "access",
  "certificates.k8s.io/CertificateSigningRequest": "access",
  "authentication.k8s.io/TokenReview": "access",
  "authorization.k8s.io/SelfSubjectAccessReview": "access",
};

/** Preferred order inside Workloads; everything else sorts alphabetically. */
const WORKLOAD_ORDER = [
  "Pod",
  "Deployment",
  "DaemonSet",
  "StatefulSet",
  "ReplicaSet",
  "ReplicationController",
  "Job",
  "CronJob",
];

export function categoryOf(resource: ResourceDescriptor): string {
  const assigned = ASSIGNMENTS[`${resource.group}/${resource.kind}`];
  if (assigned) return assigned;
  if (resource.isCrd) return "custom";
  return "other";
}

export function sortWithinCategory(
  categoryId: string,
  a: ResourceDescriptor,
  b: ResourceDescriptor,
): number {
  if (categoryId === "workloads") {
    const rank = (kind: string) => {
      const index = WORKLOAD_ORDER.indexOf(kind);
      return index === -1 ? WORKLOAD_ORDER.length : index;
    };
    const delta = rank(a.kind) - rank(b.kind);
    if (delta !== 0) return delta;
  }
  return a.kind.localeCompare(b.kind);
}
