# Kubernaut

[![Release](https://img.shields.io/github/v/release/terdiam/kubernaut)](https://github.com/terdiam/kubernaut/releases/latest)

A multi-cluster Kubernetes desktop app. Rust core (`kube-rs`) with a native
[GPUI](https://www.gpui.rs) interface — no webview, no JavaScript — built to stay
usable on clusters with thousands of objects, and to never touch a cluster you
did not explicitly add.

> **Status.** The native interface is the only one now; the Tauri and React
> version has been removed. It does not yet do everything the earlier version
> did — see [Not yet in the native app](#not-yet-in-the-native-app). Only macOS
> (Apple silicon) is built and checked; Linux and Windows builds are not
> produced.

## Install

Download `Kubernaut-<version>-macos-arm64.zip` from
[Releases](https://github.com/terdiam/kubernaut/releases) and unzip it. The app
is not signed with a Developer ID (no paid developer account), so macOS warns
before the first run: right-click the app → **Open**, or clear the quarantine
flag:

```bash
xattr -dr com.apple.quarantine /Applications/Kubernaut.app
```

Or build from source — see [Build](#build).

## Why

Kubernetes GUIs tend to break in one of two ways: they hold the entire object
list in the frontend and choke on a large cluster, or they read
`~/.kube/config` on launch and put every configured cluster one click away,
production included. Kubernaut keeps all cluster state in Rust — the UI only
ever asks for a page or subscribes to a delta stream — and starts with **no**
clusters connected; every one is added explicitly.

## Features

What the native app does today. The last section lists what the earlier
version had that this one does not yet.

### Clusters

- The app reads kubeconfigs only from its own folder (`clusters/` in its
  application-support directory), so a cluster is there because you added it:
  from a file, a paste, or picked out of `~/.kube/config`, which is read only
  then and never changed. Colliding names are renamed on the way in, and a
  cluster can be removed again from the same page. Nothing connects until you
  click its tile.
- Connections are lazy and health-checked (connected / unreachable), with
  login-shell `PATH` recovery so `exec` credential plugins (`aws`, `gcloud`,
  `az`, `kubelogin`) resolve even when the app is launched from Finder.
- **Protected clusters** (Settings) refuse every change from the app — scale,
  delete, apply, Helm, silence — enforced in the shared write layer, not only
  in the UI. Reading still works.
- A **namespace selector** takes one, several or all namespaces, remembers the
  choice per cluster, and each selected namespace is one click to remove.
- Settings give each cluster a display name, a group (a heading in the rail)
  and one of eight colours, which tints its tile and marks its toolbar, so
  production never looks like staging. They also hold the log length and a
  default namespace per cluster.

### Resource browser

- Full API discovery including CRDs, grouped by purpose in the sidebar, each
  kind with its own icon. Virtualised, live tables with status colouring from
  one shared vocabulary (`CrashLoopBackOff` reads red everywhere).
- Shared, ref-counted watches batched into deltas, so many open views of the
  same resource cost one watch.
- A detail drawer per object: YAML, events, logs and a terminal. For a pod, or
  a workload's pods, a **Diagnose** tab reads the status the way a person would
  and quotes the cluster rather than guessing: `ImagePullBackOff` (DNS, tag or
  credential told apart), a container that exited and with what code,
  `Unschedulable` (against what the pod actually requests), and more. Each
  finding shows the exact evidence and next steps that open the logs, a shell,
  the editor or the node at fault, or give a `kubectl` line to copy.
- **Row selection**: a checkbox column with select-all, and a bar over the
  ticked rows to restart (workloads), export as a zip, or delete. Only rows in
  view are acted on, so a filter cannot hide a row that is about to go; the
  confirmation names the first few; one failure does not stop the rest and each
  is reported.
- A right-click menu per row: open, logs, shell, forward a port, copy name or
  YAML, restart, cordon, delete — only what applies to the kind.
- **Pin** any object from its menu for one-click access under a PINNED heading
  in the sidebar, whatever list or page is open; unpin with the × on the row.
  Pins are kept per cluster.
- A command palette (⌘K) across clusters, pages, namespaces and resource kinds.
- **Overview** of a cluster: CPU, memory and pod gauges and the issues worth
  looking at, read from `metrics.k8s.io`.
- The **node list** shows what each node uses next to its status: CPU and
  memory against what it can allocate, pod slots, and the kubelet's disk, with
  the figure turning amber past 75% and red past 90%. What was not measured
  shows a dash, not a zero.
- A **Topology** page: one namespace drawn left to right, ingresses to services
  to workloads to pods to nodes, with columns ordered so lines run mostly
  straight. A service that selects nothing, or a pod that is failing, is red.
  Clicking a card opens its object and draws its connections strongly.
- A **Usage** page: a heatmap of every namespace, coloured by how much of its
  requested CPU or memory it uses (mostly idle, comfortable, near the request,
  over it, or no request to compare with), busiest first. A tile opens the
  namespace.
- A **Metrics** tab for a pod, node, namespace or workload: CPU and memory over
  the last hour against what is declared, and for a pod or a node network
  traffic in and out and disk (the container layer and volumes), read from the
  kubelet. A Prometheus-compatible endpoint,
  found through the apiserver proxy, gives history at once; with only
  metrics-server the chart fills while the tab is open, and says so. For a
  workload it also suggests requests and limits per container from observed
  usage, with the sample count and confidence attached, and no CPU limit on
  purpose. A namespace has a **Quota** tab: usage against each hard limit, and
  the defaults new pods get.

### Create and edit

- **Edit** an object's YAML with a server-side dry-run diff before anything is
  applied, explicit field-manager conflicts, and an option to take ownership.
  Apply is enabled only for the exact text that was previewed.
- **Create** from 16 templates, a file, or a built kustomize folder. Every
  document in a multi-document manifest is planned and applied independently
  — one failure does not block the rest — and re-applying an unchanged
  manifest reports "unchanged".

### Logs, terminal and port forwarding

- Log streaming with a drop-oldest buffer and an explicit "N lines dropped"
  marker, following a workload's pods through a rollout.
- A container shell in the drawer.
- **Port forwards** from a pod, service or workload, with the target's ports
  suggested, loopback by default (a forward opened to the network is flagged),
  live traffic counters, and the pod re-resolved per connection so a rollout
  does not kill the forward.

### Helm

- Releases read straight from the cluster's `helm.sh/release.v1` Secrets, so
  every release shows up — including ones installed by CI, Flux, Rancher or a
  colleague. Values, rendered manifest, notes and revision history.
- Install from a chart, upgrade, roll back and uninstall. Upgrade and install
  show a real diff first (what `helm-diff` does, without the plugin) and run
  only the exact inputs that were previewed; installing refuses to overwrite an
  existing release.
- Manage chart repositories and search their charts.

### GitOps

Argo CD, Flux and Fleet in one list — source, applied revision, drift from a
pinned commit, and why an entry is unhealthy — with reconcile and suspend /
resume through the controllers' own annotations, so neither CLI is required.

### Security Center

- Workload posture checks (privileged, host namespaces, host-path mounts,
  added capabilities, unpinned images, missing limits, …) read from each
  object's spec — no scanner, no extra permissions, one finding per Deployment
  rather than one per replica.
- RBAC analysis: wildcard grants, escalation verbs, cluster-wide Secret reads,
  `pods/exec`, bindings to unauthenticated subjects — with the roles the
  cluster ships itself marked and hidden by default.
- Image vulnerabilities from Trivy Operator when installed, or the bundled
  `trivy` CLI on request, with results cached for a day.

### Alerts

Firing and silenced alerts from the cluster's Alertmanager, found through the
apiserver proxy with no port-forward. Silence an alert by name for an hour, four
hours or a day, or end a silence early.

### Safety and privacy

- Deleting, restarting, Helm changes, reconcile, suspend and silencing each ask
  first, and applying YAML needs a preview. Every change is refused on a
  protected cluster and written to a local audit log: timestamp, cluster,
  target, outcome. The Activity page reads it back, newest first, with the
  failures marked, and it is kept for a week. Nothing leaves the machine.
- **No telemetry.** The app talks only to clusters you added, and to nothing
  else on its own.

### Not yet in the native app

The Tauri and React version had these; they are not ported yet.

- Field-based create and edit forms (YAML create and edit work), YAML
  validation against the cluster's OpenAPI schema, node label and taint
  editing, and pruning on apply.
- Ephemeral debug-container and node shells, and a local kubectl shell.
- Local crash-log viewing.
- Automatic updates, and builds for Linux and Windows.

## Build

Needs Rust 1.93 or newer.

```bash
cargo build --release -p kubernaut-ui     # target/release/kubernaut-ui
scripts/bundle-macos.sh                   # dist/Kubernaut.app and a zip
```

`scripts/bundle-macos.sh --sidecars <dir>` also copies `helm`, `trivy` and
`kustomize` into the app; `scripts/fetch-sidecars.sh` downloads and
checksum-verifies them, and `.github/workflows/release.yml` shows how a release
uses both. Without them the app uses whichever of the three are on `PATH`.

For the smallest binary, `cargo build --profile release-small -p kubernaut-ui`.

## Develop

```bash
scripts/run-dev.sh      # build and open as a bundle, so the Dock shows the icon
cargo run -p kubernaut-ui
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

A bare `cargo run` starts the executable without a bundle, and macOS draws a
generic icon for a process that has none; `scripts/run-dev.sh` wraps the build
in a small `Kubernaut Dev.app` so the icon shows. Log level is controlled by
`KUBERNAUT_LOG` (e.g. `KUBERNAUT_LOG=debug,kube=info`).

## Layout

| Path | Contents |
| --- | --- |
| `crates/k8s-core` | kubeconfig, connection manager, discovery, watches, row projection |
| `crates/k8s-ops` | logs, exec, port-forward, apply/diff, actions, bulk operations, diagnostics |
| `crates/k8s-metrics` | quantity parsing, aggregation, sampling, Prometheus, Alertmanager, topology |
| `crates/k8s-helm` | release store (Secrets), helm CLI wrapper, upgrade diff |
| `crates/k8s-security` | posture checks, RBAC analysis, vulnerability sources |
| `crates/k8s-app` | state shared by front ends: preferences, audit log, scan cache, write operations |
| `crates/kubernaut-ui` | the native GPUI application |

## Testing against a real cluster

Headless, read-only smoke tests exercise real behaviour without launching the
GUI:

```bash
cargo run -p k8s-core --example smoke -- <context> apps/v1/deployments <namespace>
cargo run -p k8s-core --example import_smoke -- <context>
cargo run -p k8s-ops --example ops_smoke -- <context> <namespace> <deployment>
cargo run -p k8s-ops --example exec_smoke -- <context> <namespace> <pod>
cargo run -p k8s-ops --example diagnose_smoke -- <context> <namespace>
cargo run -p k8s-ops --example lookup_smoke -- <context> <namespace>
cargo run -p k8s-ops --example manifest_smoke -- <context> <namespace>
cargo run -p k8s-ops --example gitops_smoke -- <context>
cargo run -p k8s-ops --example kustomize_smoke -- <overlay-path>
cargo run -p k8s-metrics --example metrics_smoke -- <context>
cargo run -p k8s-metrics --example alertmanager_smoke -- <context>
cargo run -p k8s-metrics --example sizing_smoke -- <context> <namespace> <deployment>
cargo run -p k8s-helm --example helm_smoke -- <context>
cargo run -p k8s-security --example security_smoke -- <context>
```

## License

[Apache License 2.0](LICENSE).
