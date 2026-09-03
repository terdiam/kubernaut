import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  ApplyOutcome,
  ClusterStatus,
  ClusterSummary,
  BulkOutcome,
  ContextEntry,
  Diagnostics,
  DiagnosisReport,
  DocResult,
  DiscoveryCache,
  ContainerInfo,
  DiffResult,
  EventRow,
  DrainReport,
  EditRequest,
  ExportResult,
  ForwardSpec,
  ForwardStatus,
  GitOpsSummary,
  HelmChart,
  HelmInfo,
  HelmRelease,
  HelmReleaseDetail,
  HelmRepository,
  HelmRevision,
  LogEvent,
  LogOptions,
  LogTarget,
  LookupOption,
  MetricsSources,
  MetricTarget,
  NamespaceUsage,
  NodeScope,
  NodeSummary,
  ObjectMetrics,
  ObjectPayload,
  OverviewPayload,
  ImageUsage,
  ClusterProfile,
  CrashReport,
  ImportPreview,
  ImportRequest,
  ManagedKubeconfig,
  ManifestPlan,
  Preferences,
  PortOption,
  Related,
  ScanReport,
  Scanner,
  Sample,
  SizingReport,
  Topology,
  SessionHandle,
  TargetRef,
  TerminalDescriptor,
  UpgradeDiff,
  UpgradeRequest,
  VulnerabilityReport,
  TerminalEvent,
  WatchBatch,
  WatchHandle,
  WatchRequest,
} from "./types";

export const api = {
  listContexts: () => invoke<ContextEntry[]>("list_contexts"),
  reloadKubeconfig: () => invoke<ContextEntry[]>("reload_kubeconfig"),

  connectCluster: (context: string) =>
    invoke<ClusterSummary>("connect_cluster", { context, options: null }),
  disconnectCluster: (cluster: string) =>
    invoke<void>("disconnect_cluster", { cluster }),

  discover: (cluster: string, refresh = false) =>
    invoke<DiscoveryCache>("discover", { cluster, refresh }),
  listNamespaces: (cluster: string) =>
    invoke<string[]>("list_namespaces", { cluster }),

  getObject: (
    cluster: string,
    resource: string,
    namespace: string | null,
    name: string,
  ) =>
    invoke<ObjectPayload>("get_object", {
      reference: { cluster, resource, namespace, name },
      includeManagedFields: false,
    }),

  stopWatch: (subscriptionId: number) =>
    invoke<void>("stop_watch", { subscriptionId }),

  diagnostics: () => invoke<Diagnostics>("diagnostics"),
  getPreferences: () => invoke<Preferences>("get_preferences"),
  lastCrash: () => invoke<CrashReport | null>("last_crash"),

  managedKubeconfigs: () => invoke<ManagedKubeconfig[]>("managed_kubeconfigs"),
  systemKubeconfigContexts: () => invoke<ContextEntry[]>("system_kubeconfig_contexts"),
  importSystemContexts: (contexts: string[], label: string) =>
    invoke<ContextEntry[]>("import_system_contexts", { contexts, label }),
  clusterProfile: (context: string) => invoke<ClusterProfile>("cluster_profile", { context }),
  setClusterProfile: (context: string, profile: ClusterProfile) =>
    invoke<Preferences>("set_cluster_profile", { context, profile }),
  previewKubeconfig: (yaml: string) => invoke<ImportPreview>("preview_kubeconfig", { yaml }),
  readKubeconfigFile: (path: string) => invoke<string>("read_kubeconfig_file", { path }),
  importKubeconfig: (request: ImportRequest) =>
    invoke<ContextEntry[]>("import_kubeconfig", { request }),
  removeKubeconfig: (file: string) => invoke<ContextEntry[]>("remove_kubeconfig", { file }),
  setPreferences: (preferences: Preferences) =>
    invoke<Preferences>("set_preferences", { preferences }),
  resourceSchema: (cluster: string, resource: string) =>
    invoke<{ resource: string; kind: string; schema: unknown }>("resource_schema", {
      cluster,
      resource,
    }),

  // ---- logs
  podContainers: (cluster: string, namespace: string, pod: string) =>
    invoke<ContainerInfo[]>("pod_containers", { cluster, namespace, pod }),
  workloadPods: (
    cluster: string,
    resource: string,
    namespace: string,
    name: string,
  ) => invoke<string[]>("workload_pods", { cluster, resource, namespace, name }),
  logSnapshot: (
    cluster: string,
    namespace: string,
    pod: string,
    options: Partial<LogOptions>,
  ) => invoke<string>("log_snapshot", { cluster, namespace, pod, options }),

  // ---- terminal
  terminalWrite: (sessionId: number, data: string) =>
    invoke<void>("terminal_write", { sessionId, data }),
  terminalResize: (sessionId: number, columns: number, rows: number) =>
    invoke<void>("terminal_resize", { sessionId, columns, rows }),

  // ---- port forwards
  startForward: (cluster: string, spec: ForwardSpec) =>
    invoke<ForwardStatus>("start_forward", { cluster, spec }),
  stopForward: (id: number) => invoke<void>("stop_forward", { id }),
  listForwards: () => invoke<ForwardStatus[]>("list_forwards"),
  targetPorts: (
    cluster: string,
    resource: string,
    namespace: string,
    name: string,
  ) => invoke<PortOption[]>("target_ports", { cluster, resource, namespace, name }),

  // ---- edits
  previewEdit: (cluster: string, request: EditRequest) =>
    invoke<DiffResult>("preview_edit", { cluster, request }),
  applyEdit: (cluster: string, request: EditRequest) =>
    invoke<ApplyOutcome>("apply_edit", { cluster, request }),

  // ---- gitops
  gitopsSurvey: (cluster: string, namespace: string | null) =>
    invoke<GitOpsSummary>("gitops_survey", { cluster, namespace }),
  gitopsReconcile: (
    cluster: string,
    resource: string,
    namespace: string | null,
    name: string,
  ) => invoke<void>("gitops_reconcile", { cluster, resource, namespace, name }),
  gitopsSetSuspended: (
    cluster: string,
    resource: string,
    namespace: string | null,
    name: string,
    suspended: boolean,
  ) => invoke<void>("gitops_set_suspended", { cluster, resource, namespace, name, suspended }),

  // ---- security
  securityScan: (cluster: string, namespace: string | null) =>
    invoke<ScanReport>("security_scan", { cluster, namespace }),
  postureScan: (cluster: string, namespace: string | null) =>
    invoke<ScanReport>("posture_scan", { cluster, namespace }),
  rbacScan: (cluster: string) => invoke<ScanReport>("rbac_scan", { cluster }),
  clusterImages: (cluster: string, namespace: string | null) =>
    invoke<ImageUsage[]>("cluster_images", { cluster, namespace }),
  vulnerabilityScanner: (cluster: string) =>
    invoke<Scanner>("vulnerability_scanner", { cluster }),
  downloadVulnerabilityDatabase: (cluster: string) =>
    invoke<Scanner>("download_vulnerability_database", { cluster }),
  vulnerabilityScan: (cluster: string, namespace: string | null, limit: number) =>
    invoke<VulnerabilityReport>("vulnerability_scan", { cluster, namespace, limit }),

  // ---- helm
  helmInfo: () => invoke<HelmInfo | null>("helm_info"),
  helmReleases: (cluster: string, namespace: string | null) =>
    invoke<HelmRelease[]>("helm_releases", { cluster, namespace }),
  helmHistory: (cluster: string, namespace: string, name: string) =>
    invoke<HelmRevision[]>("helm_history", { cluster, namespace, name }),
  helmReleaseDetail: (
    cluster: string,
    namespace: string,
    name: string,
    revision: number | null,
  ) => invoke<HelmReleaseDetail>("helm_release_detail", { cluster, namespace, name, revision }),
  helmRepos: () => invoke<HelmRepository[]>("helm_repos"),
  helmRepoAdd: (name: string, url: string) => invoke<void>("helm_repo_add", { name, url }),
  helmRepoRemove: (name: string) => invoke<void>("helm_repo_remove", { name }),
  helmRepoUpdate: () => invoke<string>("helm_repo_update"),
  helmSearch: (query: string) => invoke<HelmChart[]>("helm_search", { query }),
  helmChartValues: (chart: string, version: string | null) =>
    invoke<string>("helm_chart_values", { chart, version }),
  helmPreviewUpgrade: (request: UpgradeRequest) =>
    invoke<UpgradeDiff>("helm_preview_upgrade", { request }),
  helmUpgrade: (request: UpgradeRequest) => invoke<string>("helm_upgrade", { request }),
  helmRollback: (
    cluster: string,
    namespace: string,
    release: string,
    revision: number,
    confirmation: string,
  ) => invoke<string>("helm_rollback", { cluster, namespace, release, revision, confirmation }),
  helmUninstall: (
    cluster: string,
    namespace: string,
    release: string,
    confirmation: string,
    keepHistory: boolean,
  ) => invoke<string>("helm_uninstall", { cluster, namespace, release, confirmation, keepHistory }),

  // ---- context
  objectEvents: (cluster: string, namespace: string | null, name: string) =>
    invoke<EventRow[]>("object_events", { cluster, namespace, name }),
  podEvents: (cluster: string, namespace: string, pods: string[]) =>
    invoke<EventRow[]>("pod_events", { cluster, namespace, pods }),
  relatedResources: (
    cluster: string,
    resource: string,
    namespace: string | null,
    name: string,
  ) => invoke<Related>("related_resources", { cluster, resource, namespace, name }),
  diagnose: (cluster: string, resource: string, namespace: string | null, name: string) =>
    invoke<DiagnosisReport>("diagnose_object", { cluster, resource, namespace, name }),
  deleteObjects: (cluster: string, targets: TargetRef[], confirmation: string) =>
    invoke<BulkOutcome[]>("delete_objects", { cluster, targets, confirmation }),
  restartWorkloads: (cluster: string, targets: TargetRef[]) =>
    invoke<BulkOutcome[]>("restart_workloads", { cluster, targets }),
  exportObjects: (cluster: string, targets: TargetRef[]) =>
    invoke<ExportResult>("export_objects", { cluster, targets }),
  exportObjectsToFile: (cluster: string, targets: TargetRef[], path: string) =>
    invoke<ExportResult>("export_objects_to_file", { cluster, targets, path }),
  lookupOptions: (
    cluster: string,
    source: string,
    namespace: string | null,
    param: string | null,
  ) => invoke<LookupOption[]>("lookup_options", { cluster, source, namespace, param }),
  planManifest: (cluster: string, yaml: string, namespace: string | null, force: boolean) =>
    invoke<ManifestPlan>("plan_manifest", { cluster, yaml, namespace, force }),
  applyManifest: (cluster: string, yaml: string, namespace: string | null, force: boolean) =>
    invoke<DocResult[]>("apply_manifest", { cluster, yaml, namespace, force }),

  // ---- metrics
  clusterOverview: (cluster: string, scope: NodeScope) =>
    invoke<OverviewPayload>("cluster_overview", { cluster, scope }),
  overviewHistory: (cluster: string, scope: NodeScope, windowMs: number) =>
    invoke<Sample[]>("overview_history", { cluster, scope, windowMs }),
  nodeSummaries: (cluster: string) => invoke<NodeSummary[]>("node_summaries", { cluster }),
  workloadSizing: (cluster: string, namespace: string, resource: string, name: string) =>
    invoke<SizingReport>("workload_sizing", { cluster, namespace, resource, name }),
  namespaceUsage: (cluster: string) =>
    invoke<NamespaceUsage[]>("namespace_usage", { cluster }),
  objectMetrics: (cluster: string, target: MetricTarget, windowMs: number) =>
    invoke<ObjectMetrics>("object_metrics", { cluster, target, windowMs }),
  metricsSources: (cluster: string) =>
    invoke<MetricsSources>("metrics_sources", { cluster }),
  topology: (cluster: string, namespaces: string[]) =>
    invoke<Topology>("topology", { cluster, namespaces }),

  // ---- actions
  currentScale: (cluster: string, target: TargetRef) =>
    invoke<number>("current_scale", { cluster, target }),
  scaleWorkload: (cluster: string, target: TargetRef, replicas: number) =>
    invoke<number>("scale_workload", { cluster, target, replicas }),
  restartWorkload: (cluster: string, target: TargetRef) =>
    invoke<void>("restart_workload", { cluster, target }),
  setNodeCordoned: (cluster: string, node: string, cordoned: boolean) =>
    invoke<void>("set_node_cordoned", { cluster, node, cordoned }),
  drainNode: (
    cluster: string,
    node: string,
    options: { confirmation: string; deleteStandalonePods: boolean; dryRun: boolean },
  ) => invoke<DrainReport>("drain_node", { cluster, node, options }),
  deleteObject: (
    cluster: string,
    request: TargetRef & {
      confirmation: string;
      propagation: string | null;
      gracePeriodSeconds: number | null;
    },
  ) => invoke<void>("delete_object", { cluster, request }),
  evictPod: (
    cluster: string,
    namespace: string,
    name: string,
    confirmation: string,
  ) => invoke<void>("evict_pod", { cluster, namespace, name, confirmation }),
};

/** Stream logs. Returns the session plus a stop function. */
export async function startLogs(
  cluster: string,
  target: LogTarget,
  options: Partial<LogOptions>,
  onBatch: (events: LogEvent[]) => void,
): Promise<{ sessionId: number; stop: () => void }> {
  const channel = new Channel<LogEvent[]>();
  channel.onmessage = onBatch;
  const handle = await invoke<SessionHandle>("start_logs", {
    cluster,
    target,
    options,
    channel,
  });
  let stopped = false;
  return {
    sessionId: handle.sessionId,
    stop: () => {
      if (stopped) return;
      stopped = true;
      void invoke("stop_logs", { sessionId: handle.sessionId });
    },
  };
}

/** The four ways to open a terminal; all return the same session shape. */
export type TerminalRequest =
  | {
      mode: "podExec";
      namespace: string;
      pod: string;
      container: string | null;
      command: string[];
    }
  | {
      mode: "ephemeral";
      namespace: string;
      pod: string;
      targetContainer: string | null;
      image: string;
      confirmation: string;
    }
  | {
      mode: "nodeShell";
      node: string;
      namespace: string;
      image: string;
      confirmation: string;
    }
  | { mode: "localShell"; namespace: string | null };

const TERMINAL_COMMANDS: Record<TerminalRequest["mode"], string> = {
  podExec: "open_terminal",
  ephemeral: "open_ephemeral_terminal",
  nodeShell: "open_node_shell",
  localShell: "open_local_shell",
};

export async function openTerminal(
  cluster: string,
  request: TerminalRequest,
  size: { columns: number; rows: number },
  onBatch: (events: TerminalEvent[]) => void,
): Promise<{ descriptor: TerminalDescriptor; close: () => void }> {
  const channel = new Channel<TerminalEvent[]>();
  channel.onmessage = onBatch;

  const { mode, ...rest } = request;
  const descriptor = await invoke<TerminalDescriptor>(TERMINAL_COMMANDS[mode], {
    cluster,
    options: { ...rest, ...size },
    channel,
  });

  let closed = false;
  return {
    descriptor,
    close: () => {
      if (closed) return;
      closed = true;
      void invoke("close_terminal", { sessionId: descriptor.sessionId });
    },
  };
}

/**
 * Start a watch. Batches arrive on an IPC channel; the returned function stops
 * the watch and releases the shared subscription on the Rust side.
 */
export async function startWatch(
  cluster: string,
  request: WatchRequest,
  onBatch: (batch: WatchBatch) => void,
): Promise<{ handle: WatchHandle; stop: () => void }> {
  const channel = new Channel<WatchBatch>();
  channel.onmessage = onBatch;

  const handle = await invoke<WatchHandle>("watch_resource", {
    cluster,
    request,
    channel,
  });

  let stopped = false;
  return {
    handle,
    stop: () => {
      if (stopped) return;
      stopped = true;
      void api.stopWatch(handle.subscriptionId);
    },
  };
}

export function onClusterStatus(
  handler: (payload: { cluster: string; status: ClusterStatus }) => void,
) {
  return listen<{ cluster: string; status: ClusterStatus }>(
    "cluster://status",
    (event) => handler(event.payload),
  );
}
