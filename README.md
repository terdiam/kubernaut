# Kubernaut

[![Release](https://img.shields.io/github/v/release/terdiam/kubernaut)](https://github.com/terdiam/kubernaut/releases/latest)

A multi-cluster Kubernetes desktop app. Rust core (`kube-rs`) with a native
[GPUI](https://www.gpui.rs) interface — no webview, no JavaScript — built to stay
usable on clusters with thousands of objects, and to never touch a cluster you
did not explicitly add.

> **Status.** The native interface is the only one now; the Tauri and React
> version has been removed. It does not yet do everything the earlier version
> did — see [Not yet in the native app](#not-yet-in-the-native-app). macOS
> (Apple silicon) is the platform it is developed and run on. Linux builds, is
> tested and starts, checked in a container; Windows is compiled, linted and
> tested by CI but has not been run by a person yet.

## Install

Download `Kubernaut-<version>-macos-arm64.zip` from
[Releases](https://github.com/terdiam/kubernaut/releases) and unzip it. The app
is not signed with a Developer ID (no paid developer account), so macOS warns
before the first run: right-click the app → **Open**, or clear the quarantine
flag:

```bash
xattr -dr com.apple.quarantine /Applications/Kubernaut.app
```

**Linux.** Download `Kubernaut-<version>-linux-<x86_64|aarch64>.tar.gz`, unpack
it and run `./install.sh`. It installs for the current user under `~/.local`
(nothing needs root), adds `kubernaut-ui` to `~/.local/bin` and an entry to the
applications menu. It needs a graphical session (X11 or Wayland) and a Vulkan
driver.

**Windows.** Download `Kubernaut-<version>-windows-x86_64.zip`, unzip it and run
`kubernaut-ui.exe`, which carries the app icon. It is not code-signed, so
SmartScreen warns on the first run: **More info**, then **Run anyway**. This
build has not been run on a real Windows machine yet; please report what you find.

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
  YAML, restart, cordon, drain, delete — only what applies to the kind.
- **Pin** any object from its menu for one-click access under a PINNED heading
  in the sidebar, whatever list or page is open; unpin with the × on the row.
  Pins are kept per cluster.
- Right-clicking a cluster in the rail offers **Info** (server, user, sign-in
  plugin, protection, and anything unusual such as ignored certificate checks),
  **Edit…** (opens Settings at that cluster) and **Connect** or **Disconnect**.
- A command palette (⌘K) across clusters, pages, namespaces and resource kinds.
- Double-clicking empty space in the title bar maximises the window, or restores it.
- On macOS an application menu with **About**, **Check for Updates…** and
  **Settings…** (⌘,), plus Hide and Quit. Check for Updates opens Settings and
  looks for a newer version there. Linux and Windows have no native menu bar, so
  Settings and the update check stay in the window.
- **Overview** of a cluster: CPU, memory and pod gauges and the issues worth
  looking at, read from `metrics.k8s.io`.
- The **node list** shows what each node uses next to its status: CPU and
  memory against what it can allocate, pod slots, and the kubelet's disk, with
  the figure turning amber past 75% and red past 90%. What was not measured
  shows a dash, not a zero.
- A **Topology** page: one namespace, the first one selected in the toolbar
  (it follows the selector, and with none selected keeps what was drawn), drawn
  left to right, ingresses to services
  to workloads to pods to nodes, with columns ordered so lines run mostly
  straight. A service that selects nothing, or a pod that is failing, is red.
  Clicking a card opens its object and draws its connections strongly.
  Cards can be dragged to untangle a picture (the lines follow, and **Reset
  layout** puts them back), the background drags to pan, and small dots run along
  each line from what feeds to what is fed. Clicking a card still opens its object.
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

- **Edit with a form**: the same eight kinds have a Form switch beside the YAML
  of an existing object. It starts from the fields the object has, leaves
  everything the form does not cover exactly as it is (an environment variable
  that reads from a Secret survives an edit of the plain ones), and cannot change
  the name, the namespace or a deployment's selector. It writes into the same
  text, so Preview and Apply are unchanged.
  A Secret's form lists its decoded values and saves them base64-encoded in
  `data`; a value with line breaks or binary content (a certificate, a key) is not
  listed and is kept as it is.
- **Schema check** while you type, in both the editor and Create: the text is
  checked against the OpenAPI schema this cluster publishes for its kind, custom
  resources included, so unknown fields, wrong types, missing required fields and
  values outside an enum are underlined and listed with their path
  (`spec.template.spec.containers[0].imagePullPolisy: unknown field`). A line that
  is not YAML at all is reported where it breaks. Click a problem to jump to its
  line. It advises and never blocks: the server's dry run in Preview still
  decides. Quantities such as `cpu: 1` and int-or-string fields are accepted as
  the apiserver accepts them.
- **Prune** on Create: with Prune on and a label selector (`app=web`), Preview
  also lists objects to delete, and Apply asks once more naming each of them. A
  candidate must be of a kind the manifest contains, in a namespace it uses,
  match the selector, and have been applied by this app (its server-side-apply
  field manager is ours), so objects made by hand or by another tool are never
  touched. Pruning waits until every document has applied, deletes only what the
  preview listed, and is refused on a protected cluster. A kind that has left the
  manifest entirely is not searched, so remove its last objects yourself.
- **Edit** an object's YAML with a server-side dry-run diff before anything is
  applied, explicit field-manager conflicts, and an option to take ownership.
  Apply is enabled only for the exact text that was previewed.
- **Create** from 16 templates, a file, or a built kustomize folder. Every
  document in a multi-document manifest is planned and applied independently
  — one failure does not block the rest — and re-applying an unchanged
  manifest reports "unchanged".
- **Create from a form** for Deployment, Service, ConfigMap, Secret, Ingress,
  PersistentVolumeClaim, CronJob and Namespace: fields instead of YAML, with the
  manifest written into the editor as you fill them in, so what is previewed and
  applied is always the text on screen. References are read from the cluster and
  offered as suggestions (an Ingress's backend service and its ports, the ingress
  class, storage class, image pull secret, service account), a deployment's
  selector and pod labels follow its name, and a blank field is left out so the
  cluster's own default applies. Edit the YAML by hand and the form stops
  overwriting it until you ask.

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

### Draining a node

**Drain…** on a node shows what a drain would do before it does it: the pods
that will be evicted, and what is left alone and why (DaemonSet pods, static
pods, and pods with no controller, which would not be recreated, with a box to
delete those too). On confirmation the node is cordoned and its pods are evicted
through the eviction API, so PodDisruptionBudgets are respected. A pod a budget
refuses is not forced or waited for: the result says so, with the cluster's own
message, and the node stays cordoned for you to drain again. Refused on a
protected cluster and audited.

### Node labels and taints

The **Labels & taints** tab of a node edits both as lists instead of YAML. Added
and changed entries are tinted, Save names every change in a confirmation (and
warns when a `NoExecute` taint would evict running pods), and the save is a
merge patch carrying the `resourceVersion` the lists were read at, so a node that
changed meanwhile is refused instead of overwritten. Keys, values, effects and
duplicate taints are checked before anything is sent. Refused on a protected
cluster and audited.

### Debug and node shells

- **Debug** tab on a pod adds an ephemeral container (busybox) that shares the
  process namespace of the pod's first container, so `ps` shows the app's
  processes. Kubernetes cannot remove an ephemeral container; it stays until
  the pod is recreated, and the tab says so.
- **Node shell** tab on a node starts a privileged pod with host namespaces and
  opens a root shell on the node. The pod is deleted when the drawer closes.
- Both ask first, name the pod or node, are refused on a protected cluster and
  are written to the audit log.

### Local shell

The **Shell** page opens a shell on this machine with `KUBECONFIG` pointing at a
temporary file that holds only the open cluster's context (and its namespace,
when exactly one is selected), so `kubectl` there cannot reach another cluster.
The file is private to you and deleted when you leave the page. Because
`kubectl` can change the cluster, it is refused on a protected cluster and
audited like the other shells.

### Crash log

The **Crash log** page lists what went wrong in earlier runs, newest first, from
files already on this machine: panics the app wrote into its own log (with the
backtrace), and on macOS the reports the system writes for a crash the app could
not catch, such as a segfault or a kill for an invalid code signature. Copy puts
the selected entry on the clipboard for a bug report you have read first, and
Show folder opens where it came from. Nothing is sent anywhere.

### Updates

Settings → Updates shows the running version, checks for a newer release, and
installs it with **Install and restart**. Because the app is not signed with a
Developer ID, the system cannot vouch for an update, so the app does: a release
is installed only if its zip carries an Ed25519 signature that verifies against
the public key compiled into the app, and the unpacked app is the right bundle
at the announced version. A download that fails any check is deleted and the
running app is left as it was. Until a key is set up (below), or for a release
that is not signed, the app only tells you a version exists and links its notes.
macOS only.

#### Releasing signed updates

The release workflow signs each macOS zip with the private key in the repository
secret `UPDATE_SIGNING_KEY` and uploads `Kubernaut-<version>-macos-<arch>.zip.sig`
beside it; the matching public key is `PUBLIC_KEY_HEX` in
`crates/k8s-app/src/updates.rs`. Check a downloaded release the way an installed
copy would:

```bash
cargo run -p k8s-app --example update_tool -- verify Kubernaut-<version>-macos-arm64.zip
```

Version 1.0.0 is the first release to carry the key, so it has to be installed by
hand; from then on installed copies update themselves. The secret cannot be read
back from GitHub: if it is lost, create a new pair with
`update_tool keygen`, replace both halves, and every installed copy has to be
reinstalled by hand once, because it only trusts the key it was built with.

#### Offering a release to old installs

Installs of the old Tauri app (0.1.x) look for `releases/latest/download/latest.json`,
which a native release does not have, so they report "Could not fetch a valid
release JSON". For a release that should be offered to them, run once:

```bash
gh workflow run legacy-updater.yml -f tag=v1.0.0
```

It repacks the macOS zip as `Kubernaut.app.tar.gz`, signs it with the old Tauri
updater key (the `TAURI_SIGNING_PRIVATE_KEY` secret) and publishes `latest.json`
for Apple silicon only: the old Intel build was universal, and Windows and Linux
installs need installers a native release does not ship. An old install that
takes the update keeps its settings, since the data folder is the same.

### Safety and privacy

- Deleting, restarting, Helm changes, reconcile, suspend and silencing each ask
  first, and applying YAML needs a preview. Every change is refused on a
  protected cluster and written to a local audit log: timestamp, cluster,
  target, outcome. The Activity page reads it back, newest first, with the
  failures marked, and it is kept for a week. Nothing leaves the machine.
- **No telemetry.** The app talks only to clusters you added. The one exception
  is the update check: at startup, unless you turn it off in Settings → Updates,
  it asks GitHub for the latest release version. That request carries nothing
  about you or this machine, and it never installs anything by itself.

### Not yet in the native app

The Tauri and React version had these; they are not ported yet.

- Windows is compiled, linted and tested by CI but has not been run by a person.
- In-app install of updates on Linux and Windows. There the update check still
  works and links the release page; installing is macOS only.
- Code signing and notarisation on any platform.

## Build

Needs Rust 1.93 or newer.

On Linux the window system and fonts need their development packages first. On
Debian or Ubuntu:

```bash
sudo apt-get install build-essential pkg-config clang cmake \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libx11-dev \
  libx11-xcb-dev libxcb-xkb-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libfontconfig-dev libfreetype-dev libasound2-dev \
  libvulkan-dev libdbus-1-dev libgbm-dev libdrm-dev libegl1-mesa-dev libssl-dev
```

Windows needs the Visual Studio C++ build tools.

```bash
cargo build --release -p kubernaut-ui     # target/release/kubernaut-ui
scripts/bundle-macos.sh                   # dist/Kubernaut.app and a zip
scripts/bundle-linux.sh                   # dist/Kubernaut-<version>-linux-<arch>.tar.gz
scripts/bundle-windows.ps1                # dist\Kubernaut-<version>-windows-x86_64.zip
```

Each bundle script takes `--sidecars <dir>` (`-Sidecars` on Windows) to also copy
`helm`, `trivy` and `kustomize` in; `scripts/fetch-sidecars.sh <platform> <dir>`
downloads and checksum-verifies them for macOS, Linux or Windows, and `.github/workflows/release.yml` shows how a release
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
