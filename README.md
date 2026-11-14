# Kubernaut

[![Release](https://img.shields.io/github/v/release/terdiam/kubernaut)](https://github.com/terdiam/kubernaut/releases/latest)

A multi-cluster Kubernetes desktop app. Rust core (`kube-rs`) with a native
[GPUI](https://www.gpui.rs) interface — no webview, no JavaScript — built to stay
usable on clusters with thousands of objects, and to never touch a cluster you
did not explicitly add.

> **Status.** The native interface is the only one now; the Tauri and React
> version has been removed. It does not yet cover everything under
> [Features](#features). Known gaps: automatic updates, field-based create
> forms (YAML create works), and pruning on apply. Only macOS (Apple silicon) is
> built and checked; Linux and Windows builds are not produced.

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

### Multi-cluster

- Contexts are added explicitly, from a kubeconfig file, a paste, or picked
  out of `~/.kube/config` — the app never reads that file on its own.
  Colliding context names must be renamed before import, and the underlying
  cluster/user names are qualified per import so two kubeadm clusters (both
  named `kubernetes` / `kubernetes-admin` by default) never merge into one
  credential.
- Lazy, health-probed connections (connected / degraded / unreachable), with
  login-shell `PATH` recovery so `exec` credential plugins (`aws`, `gcloud`,
  `az`, `kubelogin`) resolve even when the app is launched from Finder.
- Per-cluster display name, accent colour, impersonation, default namespace,
  proxy and TLS settings. **Protected contexts** refuse every destructive
  action outright — no dialog, no override — enforced in the command layer,
  not just the UI.

### Resource browser

- Full API discovery including CRDs, with `additionalPrinterColumns` honoured
  — a custom resource gets a real table with no app changes.
  Virtualised, sortable, resizable, with live status colouring from one shared
  vocabulary (`CrashLoopBackOff` reads red everywhere, including in CRD
  columns).
- Shared, ref-counted watches batched into deltas for the UI, so many open
  tabs on the same resource cost one watch, not several.
- A `kubernetes.io/tls` Secret's own certificate read straight off the row:
  domains from its Subject Alternative Name and when it expires, with the row
  turning amber inside its last two weeks and red once past due — no `openssl
  x509 -noout -text` required.
- **Row selection** — checkbox column with select-all and shift-click ranges,
  a bulk bar for delete / restart / export, and a fuzzy command palette (⌘K)
  across clusters, resource types and live objects.
- **Pin** any object for one-click access from the sidebar, independent of
  whatever table or filter happens to be open.

### Create, edit and import

- **Create** — a floating **+** button on every list creates that kind, as a
  form or as YAML; switching between them keeps the draft. The form walks
  through the kind's sections one at a time — a step rail up top, Back/Next
  below — rather than one long page. 16 starting templates plus a generic
  skeleton for any kind, including CRDs. Reference fields — image pull
  secrets, volumes, the governing Service on a StatefulSet, ingress backend
  service and port, ingress class, storage class, service account, node,
  priority class, HPA scale target — are read from the cluster and offered as
  selects, not typed by hand.
- **Import YAML** applies a file as it stands: server-owned fields
  (`resourceVersion`, `managedFields`, `status`, …) are stripped so an
  exported object can be re-applied, `ownerReferences` are flagged rather than
  silently dropped, and each document in a multi-document file is planned and
  applied independently — one failure does not block the rest.
- **YAML editing** in Monaco, validated against *this cluster's* own OpenAPI
  schema, with a real `dryRun=All` diff before anything is applied and
  explicit field-manager conflict reporting (with an option to take
  ownership).
- **Form editing** for common kinds over the same server-side apply path —
  only the fields that changed are sent, so an edit never claims ownership of
  the rest of the object. Secrets decode for editing and re-encode on save,
  through a multi-line field for anything that is (a certificate, a key, a
  config file) — a single-line one would silently strip its newlines the
  moment it was touched.
- **Node labels and taints** through the same Form tab — key/value/effect
  rows, no hand-edited YAML for a scheduling rule.
- **Bulk export** — selected rows, or everything a filter leaves visible, as a
  zip through the OS save dialog: one YAML file per object, grouped
  `<namespace>/<kind>/<name>.yaml`.

### Diagnose

A tab that reads a pod's status the way a human would, and quotes the cluster
rather than guessing: `CrashLoopBackOff`, `ImagePullFailed` (tag vs. credential
vs. network, told apart), `CreateContainerConfigError` (missing object vs.
missing key inside an object that exists), `Unschedulable` (parsed against
what the pod actually requests), `Evicted`, `OOMKilled`, node pressure, a
stuck `Terminating` finalizer, and more — each with the exact evidence and a
next step that opens straight into logs, a shell, or the editor.

### Logs and terminal

- Log streaming with a drop-oldest ring and an explicit "N lines dropped"
  marker, multi-pod tail that follows a workload's pods through a rollout, and
  a clear explanation (with whatever forensics survive) when the log file
  itself is already gone from the node.
- Four terminal modes behind one session type: container shell, ephemeral
  debug container (for images with no shell), node shell (a removable
  privileged debug pod), and a local kubectl shell pinned to a temporary
  single-context kubeconfig — it cannot reach another cluster by changing
  `--context`.
- Port forwarding bound to loopback by default, re-resolving the target pod
  per connection so a rollout does not kill the forward.

### Metrics and topology

- Cluster overview: CPU/memory/pod gauges against usage, requests, limits and
  capacity, an hour of history, and a live issues panel.
- Namespace heatmap (usage vs. declared requests), per-object metrics with
  request/limit reference lines, and node-level CPU/memory/disk/pod usage.
- Topology graph — Ingress → Service → Workload → Pod → Node — with dangling
  selectors and missing backends highlighted.
- Sizing recommendations per container from observed usage, with the sample
  count and confidence attached; under 8 samples nothing is suggested at all.
- Reads `metrics.k8s.io`, and auto-discovers a Prometheus-compatible endpoint
  (Thanos, VictoriaMetrics, Mimir all answer the same API) queried through the
  apiserver proxy — no port-forward, no extra credentials.
- A namespace's own `ResourceQuota`/`LimitRange` on its own Quota tab — hard
  limits against current usage, and the defaults new pods get.

### Helm

- Releases read straight from the cluster's `helm.sh/release.v1` Secrets, so
  every release shows up — including ones installed by CI, Flux, Rancher or a
  colleague.
- Values, rendered manifest, notes and full revision history. Install /
  upgrade / rollback / uninstall with a real pre-upgrade diff (what the
  `helm-diff` plugin does, without the plugin), and regenerated Secret values
  (`genSelfSignedCert`, `randAlphaNum`) flagged instead of shown as noise on
  every diff.

### GitOps

Argo CD, Flux and Fleet in one list — repository, applied commit, and why an
entry is unhealthy — with reconcile/suspend working through the controllers'
own annotations, so neither CLI is required.

### Security Center

- Workload posture checks (privileged, host namespaces, host-path mounts,
  added capabilities, unpinned images, missing limits, …) read straight from
  each object's spec — no scanner, no extra permissions, one finding per
  Deployment rather than one per replica.
- RBAC analysis: wildcard grants, escalation verbs, cluster-wide Secret reads,
  `pods/exec`, bindings to unauthenticated subjects — with the roles the
  cluster ships itself marked and hidden by default.
- Image vulnerabilities from Trivy Operator when installed, or the bundled
  `trivy` CLI otherwise.

### Safety and privacy

- Destructive actions — delete, scale to zero, drain, rollback, uninstall —
  each behind a typed confirmation (the object's name for one, the count of
  the set for many), and refused outright on a protected context.
- **No telemetry.** The only outbound request the app makes on its own is
  checking GitHub for a newer release on startup (a Settings toggle turns it
  off) — everything else it talks to is a cluster you added yourself. Local
  crash logs only, kept for 7 days, surfaced in Settings.
- A local audit log of every destructive action taken from the app —
  timestamp, cluster, target, outcome — kept for 7 days, viewable in
  Settings. Nothing leaves the machine.
- Signed auto-updates; checking happens on its own, but installing is always
  an explicit click, so an editor with unsaved YAML is never restarted
  underneath you.

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
cargo run -p kubernaut-ui
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Log level is controlled by `KUBERNAUT_LOG` (e.g. `KUBERNAUT_LOG=debug,kube=info`).

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
