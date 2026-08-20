// Mirrors the serde representations in `k8s-core` and `src-tauri/src/commands.rs`.
// Rust serialises with `rename_all = "camelCase"`, so field names match 1:1.

export interface ContextEntry {
  name: string;
  cluster: string;
  user: string;
  namespace: string | null;
  server: string | null;
  isCurrent: boolean;
  execCommand: string | null;
  /** Auth plugin referenced by the context is not on PATH. */
  missingExecPlugin: boolean;
}

export type ClusterStatus =
  | { state: "connecting" }
  | { state: "connected"; version: string }
  | { state: "degraded"; reason: string }
  | { state: "unreachable"; reason: string }
  | { state: "disconnected" };

export interface ClusterSummary {
  id: string;
  defaultNamespace: string;
  status: ClusterStatus;
}

export interface ColumnDef {
  name: string;
  jsonPath: string;
  kind: string;
  priority: number;
  description: string | null;
}

export interface ResourceDescriptor {
  key: string;
  group: string;
  version: string;
  kind: string;
  plural: string;
  apiVersion: string;
  namespaced: boolean;
  verbs: string[];
  shortNames: string[];
  isCrd: boolean;
  printerColumns: ColumnDef[];
  watchable: boolean;
  editable: boolean;
  deletable: boolean;
}

export interface ResourceGroup {
  name: string;
  preferredVersion: string;
  resources: ResourceDescriptor[];
}

export interface DiscoveryCache {
  cluster: string;
  groups: ResourceGroup[];
  fetchedAt: string;
  crdMetadataAvailable: boolean;
}

export type RowHealth = "ok" | "pending" | "warning" | "error" | "unknown";

export interface Row {
  uid: string;
  name: string;
  namespace: string | null;
  created: string | null;
  resourceVersion: string | null;
  cells: string[];
  health: RowHealth;
  terminating: boolean;
}

export interface ColumnSpec {
  name: string;
  kind: string;
  priority: number;
  description: string | null;
}

export interface TableSpec {
  columns: ColumnSpec[];
  namespaced: boolean;
}

export type WatchState =
  | { state: "initializing" }
  | { state: "ready" }
  | { state: "error"; message: string };

export interface WatchBatch {
  epoch: number;
  snapshot: boolean;
  upserts: Row[];
  deletes: string[];
  state: WatchState;
}

export interface WatchRequest {
  resource: string;
  namespace: string | null;
  labelSelector: string | null;
  fieldSelector: string | null;
}

export interface WatchHandle {
  subscriptionId: number;
  spec: TableSpec;
  initial: WatchBatch;
}

export interface ObjectPayload {
  yaml: string;
  json: unknown;
}

export interface Diagnostics {
  version: string;
  kubeconfigPaths: string[];
  pathEntries: string[];
  activeWatches: number;
  connectedClusters: string[];
  preferencesPath: string | null;
  logDirectory: string | null;
}

export interface CrashReport {
  file: string;
  excerpt: string;
}

// ---- P1: operations -------------------------------------------------------

export interface ContainerInfo {
  name: string;
  /** `init` | `app` | `ephemeral` */
  role: string;
  image: string;
  ready: boolean;
  restarts: number;
  /** `running` | `waiting` | `terminated` | `unknown` */
  state: string;
  /** Waiting or terminated reason, whichever applies. */
  reason: string | null;
  exitCode: number | null;
  startedAt: string | null;
  finishedAt: string | null;
}

export type LogTarget =
  | { kind: "pod"; namespace: string; name: string }
  | { kind: "workload"; namespace: string; resource: string; name: string };

export interface LogOptions {
  container: string | null;
  follow: boolean;
  tailLines: number | null;
  sinceSeconds: number | null;
  timestamps: boolean;
  previous: boolean;
}

export type LogEvent =
  | { type: "line"; pod: string; container: string; text: string }
  | { type: "dropped"; count: number }
  | { type: "podEnded"; pod: string; reason: string }
  | { type: "podFailed"; pod: string; message: string };

export type TerminalEvent =
  | { type: "output"; data: string }
  | { type: "closed"; status: string }
  | { type: "failed"; message: string }
  | { type: "status"; message: string };

export interface SessionHandle {
  sessionId: number;
}

export interface TerminalDescriptor {
  sessionId: number;
  /** `podExec` | `ephemeral` | `nodeShell` | `localShell` */
  kind: string;
  title: string;
  /** Side effects or elevated privileges worth showing once in the header. */
  warning: string | null;
}

export interface ForwardSpec {
  namespace: string;
  resource: string;
  name: string;
  remotePort: number;
  localPort: number | null;
  exposeOnNetwork: boolean;
}

export interface ForwardStatus {
  id: number;
  cluster: string;
  namespace: string;
  resource: string;
  name: string;
  localAddress: string;
  localPort: number;
  remotePort: number;
  activeConnections: number;
  bytesSent: number;
  bytesReceived: number;
  lastError: string | null;
}

export interface PortOption {
  port: number;
  name: string | null;
  protocol: string;
}

export interface EditRequest {
  resource: string;
  namespace: string | null;
  name: string;
  yaml: string;
  force: boolean;
}

export interface DiffResult {
  unified: string;
  changed: boolean;
  conflicts: FieldConflict[];
}

/** A field an apply would take from the manager that owns it. */
export interface FieldConflict {
  manager: string;
  /** Path as the apiserver reports it; empty when it only named the manager. */
  field: string;
}

/** An apply either happened or was refused over ownership. */
export type ApplyOutcome =
  | { status: "applied"; yaml: string; resourceVersion: string | null }
  | { status: "conflict"; conflicts: FieldConflict[] };

export interface TargetRef {
  resource: string;
  namespace: string | null;
  name: string;
}

export interface DrainReport {
  node: string;
  evicted: string[];
  skipped: { name: string; reason: string }[];
  blocked: { name: string; message: string }[];
}

// ---- P2: metrics ----------------------------------------------------------

export type NodeScope = "all" | "controlPlane" | "workers";

export interface ResourceGauge {
  usage: number;
  requests: number;
  limits: number;
  allocatable: number;
  capacity: number;
  /** False when metrics-server is unavailable — draw "unknown", not zero. */
  usageAvailable: boolean;
}

export interface Issue {
  severity: "warning" | "error";
  kind: string;
  /** `group/version/plural`, so the panel can open the object. */
  resource: string;
  namespace: string | null;
  name: string;
  message: string;
}

export interface NodeCounts {
  total: number;
  ready: number;
  notReady: number;
  unschedulable: number;
}

export interface ClusterOverview {
  scope: NodeScope;
  nodes: NodeCounts;
  cpu: ResourceGauge;
  memory: ResourceGauge;
  pods: ResourceGauge;
  issues: Issue[];
  sampledAt: string;
  metricsAvailable: boolean;
  metricsError: string | null;
}

export interface OverviewPayload {
  overview: ClusterOverview | null;
  ready: boolean;
}

export interface Sample {
  at: number;
  cpuUsage: number;
  cpuRequests: number;
  cpuLimits: number;
  memoryUsage: number;
  memoryRequests: number;
  memoryLimits: number;
  pods: number;
}

export type MetricTarget =
  | { kind: "pod"; namespace: string; name: string }
  | { kind: "node"; name: string }
  | { kind: "namespace"; name: string }
  | { kind: "workload"; namespace: string; resource: string; name: string };

export interface MetricPoint {
  at: number;
  cpu: number;
  memory: number;
  /** Bytes per second. */
  networkRx: number;
  networkTx: number;
  /** Absolute bytes. */
  fsUsed: number;
  volumeUsed: number;
}

export interface ObjectMetrics {
  source: "prometheus" | "metricsServer" | "none";
  points: MetricPoint[];
  /** Network and filesystem series, from the kubelet. Pods and nodes only. */
  ioPoints: MetricPoint[];
  ioNote: string | null;
  cpuRequests: number;
  cpuLimits: number;
  memoryRequests: number;
  memoryLimits: number;
  podCount: number;
  note: string | null;
}

export interface NamespaceUsage {
  namespace: string;
  pods: number;
  cpuUsage: number;
  cpuRequests: number;
  cpuLimits: number;
  memoryUsage: number;
  memoryRequests: number;
  memoryLimits: number;
  /** Some container in the namespace declares no request. */
  hasUnsetRequests: boolean;
}

export interface NodeSummary {
  name: string;
  cpuUsage: number;
  cpuRequests: number;
  cpuAllocatable: number;
  cpuCapacity: number;
  memoryUsage: number;
  memoryRequests: number;
  memoryAllocatable: number;
  memoryCapacity: number;
  podsUsed: number;
  podsAllocatable: number;
  /** Bytes on the filesystem that triggers disk-pressure eviction. */
  diskUsed: number;
  diskCapacity: number;
  imageDiskUsed: number;
  imageDiskCapacity: number;
  /** False when metrics-server did not report this node. */
  usageAvailable: boolean;
  /** False when the kubelet summary could not be read. */
  diskAvailable: boolean;
  osImage: string | null;
  kernelVersion: string | null;
  containerRuntime: string | null;
  kubeletVersion: string | null;
  architecture: string | null;
  operatingSystem: string | null;
}

export interface PrometheusTarget {
  namespace: string;
  service: string;
  port: number;
  discoveredBy: string;
}

export interface MetricsSources {
  prometheus: PrometheusTarget | null;
  checked: boolean;
}

export interface TopologyNode {
  id: string;
  kind: string;
  subKind: string | null;
  name: string;
  namespace: string | null;
  health: string;
  detail: string | null;
  resource: string | null;
}

export interface TopologyEdge {
  from: string;
  to: string;
  kind: string;
}

export interface Topology {
  nodes: TopologyNode[];
  edges: TopologyEdge[];
  truncated: boolean;
  namespaces: string[];
}

export interface EventRow {
  /** `Normal` or `Warning`. */
  kind: string;
  reason: string;
  message: string;
  count: number;
  firstSeen: string | null;
  lastSeen: string | null;
  source: string | null;
  object: string;
}

export interface RelatedRef {
  kind: string;
  name: string;
  namespace: string | null;
  resource: string;
  detail: string | null;
  health: string;
}

export interface Related {
  pods: RelatedRef[];
  services: RelatedRef[];
  ingresses: RelatedRef[];
  controllers: RelatedRef[];
  config: RelatedRef[];
  storage: RelatedRef[];
  policies: RelatedRef[];
  nodes: RelatedRef[];
}

// ---- form lookups ---------------------------------------------------------

/** One choice in a form select, read from the cluster. */
export interface LookupOption {
  /** What goes into the manifest. */
  value: string;
  /** What the reader sees, when it differs from the value. */
  label: string;
  /** One line of context — a type, a provisioner, a phase. */
  detail: string | null;
}

// ---- manifests ------------------------------------------------------------

/** What one document in a manifest would do. */
export interface DocPlan {
  index: number;
  apiVersion: string;
  kind: string;
  name: string;
  namespace: string | null;
  /** `group/version/plural`, once the kind resolved against this cluster. */
  resource: string | null;
  /** `create` | `update` | `unchanged` | `conflict` | `error` */
  action: string;
  unified: string;
  conflicts: FieldConflict[];
  /** Not errors, but they change what the apply means. */
  warnings: string[];
  error: string | null;
}

export interface ManifestPlan {
  docs: DocPlan[];
}

/** What one document actually did. */
export interface DocResult {
  index: number;
  kind: string;
  name: string;
  namespace: string | null;
  /** `created` | `configured` | `unchanged` | `conflict` | `error` */
  status: string;
  conflicts: FieldConflict[];
  error: string | null;
}

// ---- diagnostics ----------------------------------------------------------

/** A next action a finding suggests, that the app can carry out itself. */
export type StepAction =
  | { kind: "logs"; container: string | null; previous: boolean }
  | { kind: "terminal" }
  | { kind: "edit" }
  | { kind: "open"; resource: string; namespace: string | null; name: string };

export interface DiagnosticStep {
  text: string;
  /** Equivalent kubectl invocation, for a terminal or a ticket. */
  command: string | null;
  action: StepAction | null;
}

export interface DiagnosticFinding {
  /** `error` | `warning` | `info` */
  severity: string;
  /** Machine-readable cause, e.g. `CrashLoopBackOff`. */
  code: string;
  title: string;
  explanation: string;
  container: string | null;
  /** Exact text the cluster produced. Never paraphrased. */
  evidence: string[];
  steps: DiagnosticStep[];
}

export interface Diagnosis {
  pod: string;
  namespace: string | null;
  phase: string;
  healthy: boolean;
  summary: string;
  findings: DiagnosticFinding[];
}

export interface DiagnosisReport {
  /** Only the pods with something to report. */
  pods: Diagnosis[];
  examined: number;
  healthy: number;
  truncated: boolean;
}

// ---- P3: Helm -------------------------------------------------------------

export interface HelmRelease {
  name: string;
  namespace: string;
  revision: number;
  status: string;
  chart: string;
  chartVersion: string;
  appVersion: string | null;
  updated: string | null;
  description: string | null;
  pending: boolean;
}

export interface HelmRevision {
  revision: number;
  status: string;
  chartVersion: string;
  appVersion: string | null;
  updated: string | null;
  description: string | null;
}

export interface HelmReleaseDetail {
  release: HelmRelease;
  userValues: string;
  effectiveValues: string;
  manifest: string;
  notes: string;
}

export interface HelmRepository {
  name: string;
  url: string;
}

export interface HelmChart {
  name: string;
  version: string;
  appVersion: string | null;
  description: string | null;
}

export interface HelmInfo {
  path: string;
  version: string;
  bundled: boolean;
}

export interface DocumentChange {
  kind: string;
  name: string;
  /** `added` | `removed` | `modified` */
  change: string;
  /** Only regenerated Secret material differs. */
  generatedOnly: boolean;
}

export interface UpgradeDiff {
  unified: string;
  changed: boolean;
  documents: DocumentChange[];
  /** Every difference is Secret material the chart regenerates each render. */
  generatedOnly: boolean;
}

export interface UpgradeOptions {
  createNamespace: boolean;
  wait: boolean;
  atomic: boolean;
  resetValues: boolean;
  dryRun: boolean;
  timeoutSeconds: number;
}

export interface UpgradeRequest {
  cluster: string;
  namespace: string;
  release: string;
  chart: string;
  version: string | null;
  values: string;
  options: UpgradeOptions;
}

// ---- P4: Security ---------------------------------------------------------

export type Severity = "critical" | "high" | "medium" | "low" | "info";

export interface Finding {
  id: string;
  title: string;
  severity: Severity;
  source: "posture" | "rbac" | "image";
  kind: string;
  namespace: string | null;
  name: string;
  resource: string;
  container: string | null;
  message: string;
  remediation: string;
  /** Object the cluster ships itself; hidden by default. */
  builtin: boolean;
}

export interface SeverityCounts {
  critical: number;
  high: number;
  medium: number;
  low: number;
  info: number;
}

export interface ScanReport {
  findings: Finding[];
  counts: SeverityCounts;
  examined: number;
  builtinHidden: number;
  limitations: string[];
  scannedAt: string;
}

export interface ImageUsage {
  image: string;
  usedBy: string[];
  podCount: number;
}

export type Scanner =
  | { kind: "trivyOperator"; reports: number }
  | { kind: "trivyBinary"; path: string; version: string; databaseReady: boolean }
  | { kind: "none"; reason: string };

export interface Vulnerability {
  id: string;
  severity: Severity;
  package: string;
  installedVersion: string;
  fixedVersion: string | null;
  title: string;
  image: string;
  namespace: string | null;
  workload: string | null;
}

export interface VulnerabilityReport {
  scanner: Scanner;
  vulnerabilities: Vulnerability[];
  report: ScanReport;
}

// ---- P5: preferences ------------------------------------------------------

export type Theme = "system" | "light" | "dark";
export type Language = "english" | "indonesian";

export interface ClusterProfile {
  /** Shown instead of the context name; the context name itself is unchanged. */
  displayName: string | null;
  colour: string | null;
  impersonateUser: string | null;
  impersonateGroups: string[];
  defaultNamespace: string | null;
  acceptInvalidCerts: boolean;
  proxyUrl: string | null;
}

export interface Preferences {
  theme: Theme;
  language: Language;
  /** Extra directories prepended to PATH for kubeconfig exec plugins. */
  extraPathEntries: string[];
  logTailLines: number;
  /** IANA zone name, or `system` to follow the machine. */
  timezone: string;
  /** Show absolute timestamps beside relative ages. */
  showAbsoluteTimes: boolean;
  /** Contexts where destructive actions are refused outright. */
  protectedContexts: string[];
  checkUpdatesOnStartup: boolean;
  clusterProfiles: Record<string, ClusterProfile>;
}

// ---- cluster imports ------------------------------------------------------

export interface ManagedKubeconfig {
  file: string;
  label: string;
  contexts: string[];
}

export interface ImportPreview {
  contexts: string[];
  /** Context names already present; importing unchanged would shadow them. */
  conflicts: string[];
  suggested: Record<string, string>;
}

export interface ImportRequest {
  yaml: string;
  label: string;
  renames: Record<string, string>;
}

// ---- GitOps ---------------------------------------------------------------

export interface GitOpsEntry {
  /** `argocd` | `flux` | `fleet` */
  controller: string;
  kind: string;
  resource: string;
  namespace: string | null;
  name: string;
  source: string | null;
  path: string | null;
  targetRevision: string | null;
  appliedRevision: string | null;
  status: string;
  health: string;
  message: string | null;
  lastSync: string | null;
  suspended: boolean;
  reconcilable: boolean;
}

export interface GitOpsSummary {
  controllers: string[];
  entries: GitOpsEntry[];
  limitations: string[];
}

// ---- sizing recommendations ----------------------------------------------

export type Confidence = "reasonable" | "indicative" | "insufficient";

export interface Recommendation {
  container: string;
  samples: number;
  windowSeconds: number;
  confidence: Confidence;
  cpuP95: number;
  cpuMax: number;
  memoryP95: number;
  memoryMax: number;
  currentCpuRequest: number;
  currentCpuLimit: number;
  currentMemoryRequest: number;
  currentMemoryLimit: number;
  recommendedCpuRequest: number;
  recommendedMemoryRequest: number;
  recommendedMemoryLimit: number;
  /** Null on purpose — see `notes`. */
  recommendedCpuLimit: number | null;
  notes: string[];
}

export interface SizingReport {
  workload: string;
  namespace: string;
  pods: number;
  recommendations: Recommendation[];
  note: string | null;
}
