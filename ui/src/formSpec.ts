/**
 * Form layouts for the kinds people edit by hand.
 *
 * The cluster's OpenAPI schema says what a field *is*; it does not say which
 * fields matter or how to group them. These layouts supply that editorial
 * judgement for common kinds. Anything without a layout still gets the generic
 * metadata section plus the YAML tab, so no kind is unreachable.
 */

export type Field =
  | { kind: "text"; path: string; label: string; help?: string; placeholder?: string }
  | { kind: "number"; path: string; label: string; help?: string; min?: number }
  | { kind: "boolean"; path: string; label: string; help?: string }
  | { kind: "select"; path: string; label: string; options: string[]; help?: string }
  | { kind: "textarea"; path: string; label: string; help?: string }
  | { kind: "keyValue"; path: string; label: string; help?: string; masked?: boolean }
  | { kind: "stringList"; path: string; label: string; help?: string }
  | { kind: "containers"; path: string; label: string; help?: string }
  | { kind: "servicePorts"; path: string; label: string; help?: string }
  /**
   * A reference to another object, offered from the cluster.
   *
   * `dependsOn` names the draft path whose value narrows the list — an Ingress
   * backend port depends on the Service already chosen. `allowCustom` keeps
   * free text available for something that does not exist yet.
   */
  | {
      kind: "lookup";
      path: string;
      label: string;
      source: LookupSource;
      dependsOn?: string;
      allowCustom?: boolean;
      help?: string;
      placeholder?: string;
    }
  /** A list of `{ name }` references, as `imagePullSecrets` is shaped. */
  | { kind: "refList"; path: string; label: string; source: LookupSource; help?: string }
  /** Host/path routing, with the backend Service and port chosen from the cluster. */
  | { kind: "ingressRules"; path: string; label: string; help?: string }
  /** Pod volumes backed by a claim, chosen from the claims that exist. */
  | { kind: "volumes"; path: string; label: string; help?: string };

/** Where a reference field's options come from. Matches the Rust lookup. */
export type LookupSource =
  | "secrets"
  | "dockerConfigSecrets"
  | "configMaps"
  | "serviceAccounts"
  | "persistentVolumeClaims"
  | "services"
  | "servicePorts"
  | "ingressClasses"
  | "storageClasses"
  | "priorityClasses"
  | "nodes"
  | "workloads";

export interface Section {
  title: string;
  description?: string;
  fields: Field[];
}

const METADATA: Section = {
  title: "Metadata",
  fields: [
    { kind: "keyValue", path: "metadata.labels", label: "Labels" },
    {
      kind: "keyValue",
      path: "metadata.annotations",
      label: "Annotations",
      help: "Controllers store state here; changing an unfamiliar annotation can have side effects.",
    },
  ],
};

/** Pod template fields shared by every workload kind. */
function podTemplate(prefix: string): Section[] {
  return [
    {
      title: "Containers",
      fields: [{ kind: "containers", path: `${prefix}.spec.containers`, label: "Containers" }],
    },
    {
      title: "Pod",
      fields: [
        {
          kind: "lookup",
          path: `${prefix}.spec.serviceAccountName`,
          label: "Service account",
          source: "serviceAccounts",
          allowCustom: true,
        },
        {
          kind: "lookup",
          path: `${prefix}.spec.nodeSelector.kubernetes\\.io/hostname`,
          label: "Pin to node",
          source: "nodes",
          allowCustom: true,
          help: "Pins every replica to one node. Useful for debugging, rarely for production.",
        },
        {
          kind: "lookup",
          path: `${prefix}.spec.priorityClassName`,
          label: "Priority class",
          source: "priorityClasses",
          allowCustom: true,
        },
        {
          kind: "select",
          path: `${prefix}.spec.restartPolicy`,
          label: "Restart policy",
          options: ["Always", "OnFailure", "Never"],
        },
        {
          kind: "number",
          path: `${prefix}.spec.terminationGracePeriodSeconds`,
          label: "Termination grace period (s)",
          min: 0,
        },
        {
          kind: "boolean",
          path: `${prefix}.spec.hostNetwork`,
          label: "Host network",
          help: "Shares the node's network namespace. Rarely correct, and it bypasses NetworkPolicy.",
        },
        { kind: "keyValue", path: `${prefix}.metadata.labels`, label: "Pod labels" },
      ],
    },
    {
      title: "Registry & storage",
      fields: [
        {
          kind: "refList",
          path: `${prefix}.spec.imagePullSecrets`,
          label: "Image pull secrets",
          source: "dockerConfigSecrets",
          help: "Only Secrets of type kubernetes.io/dockerconfigjson can authenticate a pull, and they must live in this namespace.",
        },
        {
          kind: "volumes",
          path: `${prefix}.spec.volumes`,
          label: "Volumes",
          help: "Claims are listed from this namespace; a claim you are about to create can be typed in.",
        },
      ],
    },
  ];
}

const DEPLOYMENT: Section[] = [
  {
    title: "Scale & rollout",
    fields: [
      { kind: "number", path: "spec.replicas", label: "Replicas", min: 0 },
      {
        kind: "select",
        path: "spec.strategy.type",
        label: "Strategy",
        options: ["RollingUpdate", "Recreate"],
      },
      {
        kind: "text",
        path: "spec.strategy.rollingUpdate.maxSurge",
        label: "Max surge",
        placeholder: "25%",
      },
      {
        kind: "text",
        path: "spec.strategy.rollingUpdate.maxUnavailable",
        label: "Max unavailable",
        placeholder: "25%",
      },
      {
        kind: "number",
        path: "spec.revisionHistoryLimit",
        label: "Revision history",
        min: 0,
      },
      {
        kind: "number",
        path: "spec.minReadySeconds",
        label: "Min ready (s)",
        min: 0,
        help: "How long a new pod must stay ready before the rollout continues.",
      },
    ],
  },
  ...podTemplate("spec.template"),
  METADATA,
];

const STATEFULSET: Section[] = [
  {
    title: "Scale & rollout",
    fields: [
      { kind: "number", path: "spec.replicas", label: "Replicas", min: 0 },
      { kind: "text", path: "spec.serviceName", label: "Governing service" },
      {
        kind: "select",
        path: "spec.podManagementPolicy",
        label: "Pod management",
        options: ["OrderedReady", "Parallel"],
      },
      {
        kind: "select",
        path: "spec.updateStrategy.type",
        label: "Update strategy",
        options: ["RollingUpdate", "OnDelete"],
      },
    ],
  },
  ...podTemplate("spec.template"),
  METADATA,
];

const DAEMONSET: Section[] = [
  {
    title: "Rollout",
    fields: [
      {
        kind: "select",
        path: "spec.updateStrategy.type",
        label: "Update strategy",
        options: ["RollingUpdate", "OnDelete"],
      },
      {
        kind: "text",
        path: "spec.updateStrategy.rollingUpdate.maxUnavailable",
        label: "Max unavailable",
        placeholder: "1",
      },
    ],
  },
  ...podTemplate("spec.template"),
  METADATA,
];

const CRONJOB: Section[] = [
  {
    title: "Schedule",
    fields: [
      { kind: "text", path: "spec.schedule", label: "Cron schedule", placeholder: "*/5 * * * *" },
      { kind: "text", path: "spec.timeZone", label: "Time zone", placeholder: "Etc/UTC" },
      {
        kind: "boolean",
        path: "spec.suspend",
        label: "Suspended",
        help: "Stops new runs. Running jobs are unaffected.",
      },
      {
        kind: "select",
        path: "spec.concurrencyPolicy",
        label: "Concurrency",
        options: ["Allow", "Forbid", "Replace"],
      },
      { kind: "number", path: "spec.startingDeadlineSeconds", label: "Starting deadline (s)", min: 0 },
      { kind: "number", path: "spec.successfulJobsHistoryLimit", label: "Keep successful", min: 0 },
      { kind: "number", path: "spec.failedJobsHistoryLimit", label: "Keep failed", min: 0 },
    ],
  },
  {
    title: "Job",
    fields: [
      { kind: "number", path: "spec.jobTemplate.spec.backoffLimit", label: "Backoff limit", min: 0 },
      {
        kind: "number",
        path: "spec.jobTemplate.spec.activeDeadlineSeconds",
        label: "Active deadline (s)",
        min: 0,
      },
      { kind: "number", path: "spec.jobTemplate.spec.parallelism", label: "Parallelism", min: 0 },
    ],
  },
  ...podTemplate("spec.jobTemplate.spec.template"),
  METADATA,
];

const JOB: Section[] = [
  {
    title: "Execution",
    fields: [
      { kind: "number", path: "spec.completions", label: "Completions", min: 0 },
      { kind: "number", path: "spec.parallelism", label: "Parallelism", min: 0 },
      { kind: "number", path: "spec.backoffLimit", label: "Backoff limit", min: 0 },
      { kind: "number", path: "spec.activeDeadlineSeconds", label: "Active deadline (s)", min: 0 },
      { kind: "number", path: "spec.ttlSecondsAfterFinished", label: "TTL after finish (s)", min: 0 },
    ],
  },
  ...podTemplate("spec.template"),
  METADATA,
];

const SERVICE: Section[] = [
  {
    title: "Service",
    fields: [
      {
        kind: "select",
        path: "spec.type",
        label: "Type",
        options: ["ClusterIP", "NodePort", "LoadBalancer", "ExternalName"],
      },
      { kind: "servicePorts", path: "spec.ports", label: "Ports" },
      { kind: "keyValue", path: "spec.selector", label: "Pod selector" },
      {
        kind: "select",
        path: "spec.sessionAffinity",
        label: "Session affinity",
        options: ["None", "ClientIP"],
      },
      { kind: "text", path: "spec.externalName", label: "External name", help: "ExternalName type only." },
      {
        kind: "select",
        path: "spec.internalTrafficPolicy",
        label: "Internal traffic policy",
        options: ["Cluster", "Local"],
      },
    ],
  },
  METADATA,
];

const INGRESS: Section[] = [
  {
    title: "Routing",
    fields: [
      {
        kind: "lookup",
        path: "spec.ingressClassName",
        label: "Ingress class",
        source: "ingressClasses",
        allowCustom: true,
        help: "Which controller serves this Ingress. A class no controller claims leaves the rules inert.",
      },
      { kind: "ingressRules", path: "spec.rules", label: "Rules" },
      { kind: "textarea", path: "spec.tls", label: "TLS (JSON)" },
    ],
  },
  METADATA,
];

const CONFIGMAP: Section[] = [
  {
    title: "Data",
    fields: [
      { kind: "keyValue", path: "data", label: "Entries" },
      {
        kind: "boolean",
        path: "immutable",
        label: "Immutable",
        help: "Cannot be undone: an immutable ConfigMap must be recreated to change.",
      },
    ],
  },
  METADATA,
];

const SECRET: Section[] = [
  {
    title: "Data",
    description:
      "Values are shown decoded and re-encoded on save. They are masked until revealed.",
    fields: [
      { kind: "text", path: "type", label: "Type", placeholder: "Opaque" },
      { kind: "keyValue", path: "stringData", label: "Entries", masked: true },
    ],
  },
  METADATA,
];

const PVC: Section[] = [
  {
    title: "Claim",
    description: "Most fields are immutable after creation; only capacity can usually grow.",
    fields: [
      { kind: "text", path: "spec.resources.requests.storage", label: "Capacity", placeholder: "10Gi" },
      {
        kind: "lookup",
        path: "spec.storageClassName",
        label: "Storage class",
        source: "storageClasses",
        allowCustom: true,
        help: "Leave empty to take the cluster's default class.",
      },
      { kind: "stringList", path: "spec.accessModes", label: "Access modes" },
      {
        kind: "select",
        path: "spec.volumeMode",
        label: "Volume mode",
        options: ["Filesystem", "Block"],
      },
    ],
  },
  METADATA,
];

const HPA: Section[] = [
  {
    title: "Autoscaling",
    fields: [
      {
        kind: "select",
        path: "spec.scaleTargetRef.kind",
        label: "Target kind",
        options: ["Deployment", "StatefulSet", "ReplicaSet"],
      },
      {
        kind: "lookup",
        path: "spec.scaleTargetRef.name",
        label: "Target name",
        source: "workloads",
        dependsOn: "spec.scaleTargetRef.kind",
        allowCustom: true,
      },
      { kind: "number", path: "spec.minReplicas", label: "Min replicas", min: 0 },
      { kind: "number", path: "spec.maxReplicas", label: "Max replicas", min: 1 },
      { kind: "textarea", path: "spec.metrics", label: "Metrics (JSON)" },
    ],
  },
  METADATA,
];

const NAMESPACE: Section[] = [METADATA];

const SERVICE_ACCOUNT: Section[] = [
  {
    title: "Service account",
    fields: [
      {
        kind: "boolean",
        path: "automountServiceAccountToken",
        label: "Automount API token",
        help: "Leaving this on gives every pod using this account a cluster credential.",
      },
    ],
  },
  METADATA,
];

const LAYOUTS: Record<string, Section[]> = {
  "apps/Deployment": DEPLOYMENT,
  "apps/StatefulSet": STATEFULSET,
  "apps/DaemonSet": DAEMONSET,
  "apps/ReplicaSet": DEPLOYMENT,
  "batch/CronJob": CRONJOB,
  "batch/Job": JOB,
  "/Service": SERVICE,
  "networking.k8s.io/Ingress": INGRESS,
  "/ConfigMap": CONFIGMAP,
  "/Secret": SECRET,
  "/PersistentVolumeClaim": PVC,
  "autoscaling/HorizontalPodAutoscaler": HPA,
  "/Namespace": NAMESPACE,
  "/ServiceAccount": SERVICE_ACCOUNT,
};

/** Layout for a kind, or `null` when only the YAML editor applies. */
export function formSections(group: string, kind: string): Section[] | null {
  return LAYOUTS[`${group}/${kind}`] ?? null;
}
