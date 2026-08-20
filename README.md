# Kubernaut

Multi-cluster Kubernetes management for macOS, Windows and Linux. Rust core
(`kube-rs`) behind a Tauri v2 shell, React/TypeScript UI.

## Status — P2 (metrics & overview)

**P0 — foundation**

- Multi-cluster connection manager over kubeconfig contexts, lazily connected,
  with a health prober that reports connected / degraded / unreachable.
- Login-shell `PATH` recovery so `exec` credential plugins (`aws`,
  `gke-gcloud-auth-plugin`, `az`, `kubelogin`) resolve when the app is launched
  from Finder or a `.desktop` entry.
- Full API discovery including CRDs, with `additionalPrinterColumns` honoured.
- Shared, ref-counted watches with 100 ms delta batching over a Tauri channel.
- Virtualised resource table with per-kind columns and row health.

**P1 — operations**

- Log streaming with a drop-oldest ring, an explicit "N lines dropped" marker,
  and multi-pod tail that follows a workload's pods across a rollout.
- Four terminal modes, all through one session type: a **container shell**, an
  **ephemeral debug container** for images that ship no shell, a **node shell**
  (privileged `nsenter` pod, removed when the terminal closes), and a **local
  kubectl shell** whose `KUBECONFIG` is a temporary single-context file — a
  shell opened against staging cannot reach production by changing `--context`.
  Live resize and UTF-8-safe chunking throughout.
- Port forwarding bound to loopback by default, resolving the target pod per
  connection so a rollout does not kill the forward.
- YAML editing in Monaco, validated against **this cluster's** OpenAPI schema
  (so CRDs work), with a `dryRun=All` diff preview and field-manager conflict
  reporting before anything is applied.
- Rancher-style form editing for 14 common kinds, over the same server-side
  apply path. Secrets decode for editing and re-encode on save.
- Actions: scale, rollout restart, cordon/uncordon, drain (eviction API, so
  PodDisruptionBudgets are respected), delete and evict — each behind a typed
  confirmation.
- Command palette (⌘K) searching clusters, resource types and live objects.

**P2 — metrics & visualisation**

- Cluster overview: CPU, memory and pod gauges showing usage, requests, limits,
  allocatable and capacity against one shared denominator, an hour of in-memory
  history, and a live issues panel.
- Namespace heatmap: usage against declared requests per namespace, flagging
  both over-request and namespaces that declare no request at all.
- Topology graph: Ingress → Service → Workload → Pod → Node, laid out in fixed
  layers so it does not rearrange between refreshes. Services selecting zero
  pods and ingresses routing to a missing service are highlighted.
- Per-object metrics for a pod, node, namespace or workload, with its requests
  and limits drawn as reference lines.
- Nodes list live CPU, memory, disk and pod usage as bars coloured by pressure,
  plus OS and architecture. CPU and memory come from metrics-server, pod counts
  from the pod set, and disk from each kubelet's summary endpoint — none of it
  is in the node object, and it is merged into the table without the watch and
  the sampler fighting over the same rows.
- Status values are coloured from one shared vocabulary, so `Ready` reads green
  and `CrashLoopBackOff` red across built-in kinds and CRD printer columns
  alike. Compound values take their most severe part, and conditions whose name
  describes a problem invert — `MemoryPressure=True` is not good news.
- Sources: `metrics.k8s.io` for live usage, and a Prometheus-compatible endpoint
  when one is present — discovered automatically and queried through the
  apiserver's service proxy, so there is no port-forward and no second set of
  credentials. Thanos, VictoriaMetrics and Mimir answer the same API.
- Searchable, multi-select namespace filter: selecting several namespaces opens
  one watch per namespace and merges the results.

**P3 — Helm**

- Releases are read from the cluster's own `helm.sh/release.v1` Secrets, so
  listing and inspecting needs no helm binary and shows **every** release —
  including ones installed by CI, Flux, Rancher or a colleague, which a UI that
  only tracked its own installs would miss.
- Values, rendered manifest, notes and full revision history per release. The
  "include chart defaults" view merges defaults with the user's overrides using
  helm's own coalescing rules (maps merge, lists replace).
- Install, upgrade, rollback and uninstall through the helm binary, each behind
  a typed confirmation, with a real diff first: the proposed render compared
  against the manifest helm recorded for the current revision — what the
  `helm-diff` plugin does, without the plugin.
- The diff is summarised per object, and differences confined to Secret values
  are flagged as regenerated material. Charts calling `genSelfSignedCert` or
  `randAlphaNum` mint new values on every render, so a plain text diff always
  reports pending changes — which teaches people to ignore it.
- Repository management and chart search. These write to the user's own helm
  configuration, so repos stay in sync with their CLI.

**P4 — Security Center**

- Workload posture: privileged containers, host namespaces, host-path mounts,
  added capabilities, unpinned images, missing limits and more — read from each
  object's own spec, so it needs no scanner and no extra permissions. Checks run
  per workload rather than per pod, so a Deployment's flaw is reported once
  instead of once per replica.
- RBAC analysis: wildcard grants, escalation verbs, cluster-wide Secret reads,
  `pods/exec`, and bindings to unauthenticated subjects. Roles the cluster
  ships itself are marked and hidden by default — without that distinction the
  hundreds of built-in roles bury the one somebody granted by mistake.
- Image inventory, and vulnerabilities from Trivy Operator when it is installed
  or the local `trivy` CLI otherwise. When neither is present the panel says so
  rather than showing an empty list that reads like "nothing found".
- The first-run database download is a separate, explicit step (~110 MiB to
  fetch, ~1.2 GB on disk). Folding it into the first scan is how that scan times
  out and the whole feature looks broken. Several database repositories are
  tried in turn, because Trivy's default mirror can accept a connection and then
  transfer nothing — which is indistinguishable from a slow link until you try
  somewhere else.
- Findings start at medium severity: a real cluster produces thousands of true
  but unremarkable low-severity notes, and showing them first hides the rest.

**P5 — release engineering**

- Settings, persisted between runs: theme (light / dark / follow the system),
  display time zone, extra `PATH` directories for kubeconfig exec plugins,
  default log tail, and update behaviour. Written atomically, and an unreadable file falls back to
  defaults rather than stopping the app.
- **Protected contexts.** Named contexts refuse every destructive action from
  the app — no dialog, no override. A confirmation stops accidents but not
  habit; after the tenth one people type the name without reading it. Enforced
  in the command layer across all twelve mutating operations, not in the UI.
- Signed auto-updates through `tauri-plugin-updater`, off by default. With the
  setting off the app makes no outbound request of its own — everything it
  talks to is a cluster you chose. Installing is always an explicit click, so
  an editor with unsaved YAML is never restarted underneath you.
- Release workflow builds macOS (both architectures), Windows and Linux, fetches
  and checksum-verifies the helm sidecar, and opens a **draft** release so a
  human sees the artefacts before any installed copy does. See
  [docs/RELEASING.md](docs/RELEASING.md).
- Local logging with panic capture, seven days retained. **No telemetry**: this
  app holds credentials for production clusters, and the only privacy guarantee
  worth trusting is that there is no code to send anything anywhere. Crashes are
  surfaced in Settings so a bug report is a file you read first.

**Beyond the plan**

- **Explicit clusters.** The app does not read `~/.kube/config` on its own. The
  first run is empty, and every cluster is one somebody added — so nothing is
  one click from production because a file happened to be on disk. Adding a
  cluster offers the contexts found in your kubeconfig, a file picker, or pasted
  YAML; whichever you choose, a copy is stored in this app's own configuration
  and your kubeconfig is never written to. Context name collisions must be
  renamed before importing: two contexts with one name give no way to tell which
  cluster a click reaches.
- **Time zone.** Kubernetes records every timestamp in UTC, and reading those
  beside a local clock is how an incident timeline gets misread by a whole
  timezone. Absolute times — object metadata, conditions, chart axes, and the
  prefix Kubernetes puts on log lines — are converted to a chosen zone and shown
  on a 24-hour clock. The YAML tab is deliberately excluded: it shows what the
  cluster stores, and rewriting it would make a copied manifest wrong.
- **Per-cluster settings.** Right-click a tile for connect/disconnect, settings
  and removal. Settings cover a display name and accent colour — making
  production look different is the cheapest guard against acting on the wrong
  cluster — plus impersonation (`--as`, `--as-group`), a default namespace, a
  per-cluster proxy, and TLS verification. Removal forgets the stored
  kubeconfig; the cluster itself is untouched.
- **GitOps.** Argo CD, Flux and Fleet in one list: which repository, which
  commit is applied, and why it is not. Unhealthy entries sort first, and
  reconcile/suspend work through the annotations the controllers already watch,
  so neither CLI is needed.
- **Sizing recommendations.** Request and limit suggestions per container from
  observed usage, with the sample count, observation window and a confidence
  level attached. Below eight samples no numbers are shown at all; a window
  under six hours is labelled as not having seen a daily peak. No CPU limit is
  suggested, and a suggestion that would *lower* a memory limit says outright
  that memory limits are enforced by killing the container.

Not done: Indonesian translation. The chrome could be translated, but Kubernetes
terms (`Deployment`, `CrashLoopBackOff`) must stay as they appear in kubectl and
in every search result, so a half-translated UI was judged worse than none.

## Develop

```bash
npm install
npm run app
```

`npm run app` runs `tauri dev`, which starts Vite and the Rust app together.

Useful checks:

```bash
cargo test --workspace
npm run typecheck
npm test
```

Log level is controlled by `KUBERNAUT_LOG` (e.g. `KUBERNAUT_LOG=debug,kube=info`).

## Layout

| Path | Contents |
| --- | --- |
| `crates/k8s-core` | kubeconfig, connection manager, discovery, watches, row projection |
| `crates/k8s-ops` | logs, exec, port-forward, apply/diff, actions |
| `crates/k8s-metrics` | quantity parsing, aggregation, sampling, Prometheus, topology |
| `crates/k8s-helm` | release store (Secrets), helm CLI wrapper, upgrade diff |
| `crates/k8s-security` | posture checks, RBAC analysis, vulnerability sources |
| `src-tauri` | Tauri app, IPC commands, capabilities |
| `ui` | React frontend |

## Testing against a cluster

```bash
kind create cluster --name kubernaut-dev --config test/kind-3node.yaml
kubectl apply -f test/crd-sample.yaml
```

The CRD sample verifies that a custom type appears in the sidebar without any
app-side changes and that its printer columns drive the table.

Two headless smoke tests exercise a real cluster without launching the GUI.
Both are read-only — the second uses `dryRun=All` for its diff and makes no
writes:

```bash
cargo run -p k8s-core --example smoke -- <context> apps/v1/deployments <namespace>
cargo run -p k8s-core --example import_smoke -- <context>
cargo run -p k8s-ops --example ops_smoke -- <context> <namespace> <deployment>
cargo run -p k8s-metrics --example metrics_smoke -- <context>
cargo run -p k8s-helm --example helm_smoke -- <context>
cargo run -p k8s-security --example security_smoke -- <context>
cargo run -p k8s-ops --example exec_smoke -- <context> <namespace> <pod>
```

The metrics numbers can be cross-checked directly:

```bash
kubectl top nodes
```

### Known gap

The Prometheus path is unit-tested (query encoding, proxy path construction,
metric selection) but has not been exercised against a live Prometheus — the
development cluster has none, and discovery correctly reports that. Treat the
range-query code as unverified until it runs against a real endpoint.
