use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
    sync::Arc,
    time::Duration,
};

use futures::{StreamExt, channel::mpsc};
use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, WindowExt,
        button::ButtonVariant,
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt, PopupMenuItem},
        table::{DataTable, TableEvent, TableState},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore, cluster_ops, metrics_ops, object_ops,
    preferences::{PinnedObject, Preferences, arrange, reordered, set_pinned},
    updates,
};
use k8s_core::{
    ClusterStatus, ContextEntry, DiscoveryCache, ResourceDescriptor, TableSpec, WatchBatch,
    WatchRequest, WatchState, watch::SubscriptionId,
};
use k8s_ops::actions::TargetRef;
use tokio::{runtime::Handle, sync::broadcast::error::RecvError, task::JoinHandle};

use crate::{
    activity::ActivityView,
    add_cluster::{AddClusterEvent, AddClusterView},
    alerts::{AlertsEvent, AlertsView},
    categories::{self, Category},
    cluster_info,
    crashes::CrashView,
    create::CreateView,
    drain,
    drawer::{DrawerEvent, DrawerView, Tab as DrawerTab, Target},
    gitops::{GitOpsEvent, GitOpsView},
    helm::HelmView,
    icons::kind_icon,
    namespaces,
    overview::{OverviewEvent, OverviewView},
    palette::{Command, Item, PaletteEvent, PaletteView},
    ports::PortsView,
    row_actions::{self, RowAction},
    security::{SecurityEvent, SecurityView},
    settings::{SettingsEvent, SettingsView},
    table::{ResourceDelegate, RowContext},
    terminal::{Request, TerminalView},
    theme,
    topology::{TopologyEvent, TopologyView},
    ui::{Emphasis, button_icon, chip, pill, toast_error, toast_success},
    update_progress,
    usage::{UsageEvent, UsageView},
    window_controls::{leaves_room_for_traffic_lights, window_controls},
    yaml::fetch_yaml,
};

const ROW_HEIGHT: Pixels = px(28.);

/// Namespace pills shown in the toolbar before the rest fold into a count.
const MAX_PILLS: usize = 3;

/// How often the node list reads node usage. The sampler measures every fifteen
/// seconds, so asking sooner only repeats the last answer.
const NODE_USAGE_POLL: std::time::Duration = std::time::Duration::from_secs(5);

actions!(
    kubernaut,
    [
        TogglePalette,
        About,
        CheckForUpdates,
        ShowSettings,
        QuitApp,
        HideApp,
        HideOtherApps,
        ShowAllApps,
    ]
);

/// Empty space in the title bar. Double-clicking it maximises the window, as it
/// does on a native title bar; the bar is drawn by the app, so that is done here.
///
/// Off macOS this is also what drags the window. GPUI answers the system's
/// hit-test with the first window-control area under the pointer, parents before
/// children, so the drag area must not contain the minimise, maximise and close
/// buttons: marking the whole bar as the drag area made every click on them read
/// as a drag, and they did nothing.
fn titlebar_gap(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .h_full()
        .when(!cfg!(target_os = "macos"), |gap| {
            gap.window_control_area(WindowControlArea::Drag)
        })
        .when(cfg!(target_os = "linux"), |gap| {
            gap.on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 1 {
                    window.start_window_move();
                }
            })
        })
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.zoom_window();
            }
        })
}

/// The key context the app's shortcuts are bound under.
pub const KEY_CONTEXT: &str = "Kubernaut";

/// How often ages are re-read. Rows older than an hour change text once a
/// minute, so most ticks allocate nothing.
const AGE_TICK: Duration = Duration::from_secs(5);

/// A cluster tile. Built once: nothing here changes while the app runs, so the
/// render path never touches `Preferences` or allocates for a tile.
struct Tile {
    /// The context name, which is the cluster id everywhere else.
    name: SharedString,
    display: SharedString,
    initials: SharedString,
    accent: Option<Hsla>,
}

/// A tile being dragged to a new place in the rail, carried by its context name.
#[derive(Clone)]
struct DraggedTile {
    name: SharedString,
    initials: SharedString,
}

impl Render for DraggedTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_xl()
            .border_1()
            .border_color(theme.primary)
            .bg(theme.secondary)
            .opacity(0.9)
            .text_sm()
            .font_semibold()
            .child(self.initials.clone())
    }
}

/// A sidebar section header being dragged, carried by its label.
#[derive(Clone)]
struct DraggedSection(SharedString);

/// A resource row being dragged: its key, and the kind shown while it moves.
#[derive(Clone)]
struct DraggedResource {
    key: SharedString,
    kind: SharedString,
}

/// The pill shown under the pointer while a sidebar row is dragged.
fn drag_pill(label: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px_2()
        .h(ROW_HEIGHT)
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(theme.primary)
        .bg(theme.secondary)
        .opacity(0.9)
        .text_sm()
        .child(label)
}

impl Render for DraggedSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_pill(self.0.clone(), cx)
    }
}

impl Render for DraggedResource {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_pill(self.kind.clone(), cx)
    }
}

/// The saved order of the sidebar. Borrowed, so building rows copies nothing.
#[derive(Clone, Copy, Default)]
struct SidebarOrder<'a> {
    sections: &'a [String],
    resources: &'a [String],
}

/// Tiles grouped under a workspace divider. The grouping is purely visual:
/// connecting ignores it.
struct RailGroup {
    /// `None` when no cluster has a workspace, so no divider is drawn at all.
    label: Option<SharedString>,
    tiles: Vec<usize>,
}

/// What the UI knows about one cluster it has opened.
struct ClusterState {
    status: ClusterStatus,
    /// Rebuilt when `status` changes rather than formatted every frame.
    status_label: SharedString,
    discovery: Option<Arc<DiscoveryCache>>,
    error: Option<SharedString>,
    /// The namespaces the cluster is narrowed to, sorted; empty shows every one.
    scope: Vec<SharedString>,
    /// What the picker offers. Read once on connect.
    namespaces: Vec<SharedString>,
}

impl ClusterState {
    fn new(status: ClusterStatus) -> Self {
        Self {
            status_label: status_label(&status),
            status,
            discovery: None,
            error: None,
            scope: Vec::new(),
            namespaces: Vec::new(),
        }
    }

    fn set_status(&mut self, status: ClusterStatus) {
        self.status_label = status_label(&status);
        self.status = status;
    }
}

/// One line of the resource sidebar, flattened so it can be virtualised.
enum SidebarRow {
    Group {
        /// The section label, which is also the collapse key.
        name: SharedString,
        count: SharedString,
        icon: IconName,
        open: bool,
    },
    Resource {
        key: SharedString,
        kind: SharedString,
    },
}

/// What the watch task tells the UI.
enum WatchEvent {
    Started {
        spec: TableSpec,
        initial: WatchBatch,
    },
    Batch(Arc<WatchBatch>),
    Failed(String),
}

/// The table for the selected resource, with the watch feeding it.
struct OpenTable {
    /// Batches from an earlier selection carry an older generation and are
    /// dropped, so a slow watch cannot paint rows from the previous resource.
    generation: u64,
    /// `group/version/plural`, which the drawer needs to fetch the object.
    resource: SharedString,
    title: SharedString,
    state: Entity<TableState<ResourceDelegate>>,
    task: JoinHandle<()>,
    /// Reads node usage for the node list; dropped with the table.
    _usage: Option<Task<()>>,
    /// Rebuilt when rows change, not formatted every frame.
    count_label: SharedString,
    status: Option<SharedString>,
    empty_label: SharedString,
    error: Option<SharedString>,
    _events: Subscription,
}

impl Drop for OpenTable {
    /// Stopping the task drops its [`Unsubscribe`] guard, which releases the
    /// watch.
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// An operation on one object that changes the cluster.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Restart,
    /// `true` to cordon, `false` to uncordon.
    Cordon(bool),
    /// Start a CronJob's Job now.
    Trigger,
    /// `true` to suspend a CronJob, `false` to resume it.
    Suspend(bool),
}

impl Op {
    /// The toast text for success.
    fn done(self, target: &Target) -> String {
        match self {
            Self::Delete => format!("Deleted {} {}", target.kind, target.name),
            Self::Restart => format!("Restarting {}", target.name),
            Self::Cordon(true) => format!("Cordoned {}", target.name),
            Self::Cordon(false) => format!("Uncordoned {}", target.name),
            Self::Trigger => format!("Started a Job from {}", target.name),
            Self::Suspend(true) => format!("Suspended {}", target.name),
            Self::Suspend(false) => format!("Resumed {}", target.name),
        }
    }

    /// The toast title for failure; the reason follows as its body.
    fn failed(self, target: &Target) -> String {
        match self {
            Self::Delete => format!("Could not delete {}", target.name),
            Self::Restart => format!("Could not restart {}", target.name),
            Self::Cordon(true) => format!("Could not cordon {}", target.name),
            Self::Cordon(false) => format!("Could not uncordon {}", target.name),
            Self::Trigger => format!("Could not run {}", target.name),
            Self::Suspend(true) => format!("Could not suspend {}", target.name),
            Self::Suspend(false) => format!("Could not resume {}", target.name),
        }
    }
}

fn target_ref(target: &Target) -> TargetRef {
    TargetRef {
        resource: target.resource.to_string(),
        namespace: target.namespace.as_ref().map(ToString::to_string),
        name: target.name.to_string(),
    }
}

/// What the bar over a ticked selection does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BulkOp {
    Delete,
    Restart,
}

/// Names to show in a confirmation: the first few, then how many more.
fn name_list(names: &[SharedString]) -> String {
    const SHOWN: usize = 6;
    let mut text = names
        .iter()
        .take(SHOWN)
        .map(|name| name.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        text.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    text
}

/// The command palette and the subscription that hears its choice.
struct OpenPalette {
    view: Entity<PaletteView>,
    _events: Subscription,
}

/// The GitOps page and the subscription that opens objects from it.
struct OpenGitOps {
    view: Entity<GitOpsView>,
    _events: Subscription,
}

/// The settings page and the subscription that rebuilds the rail when a
/// cluster's name, colour or group changes.
struct OpenSettings {
    view: Entity<SettingsView>,
    _events: Subscription,
}

/// The add-cluster page and the subscription that rebuilds the rail.
struct OpenAddCluster {
    view: Entity<AddClusterView>,
    _events: Subscription,
}

/// The topology graph and the subscription that opens an object from it.
struct OpenTopology {
    view: Entity<TopologyView>,
    _events: Subscription,
}

/// The usage heatmap and the subscription that opens a namespace from it.
struct OpenUsage {
    view: Entity<UsageView>,
    _events: Subscription,
}

/// The Alerts page and the subscription that opens objects from it.
struct OpenAlerts {
    view: Entity<AlertsView>,
    _events: Subscription,
}

/// The Security Center and the subscription that opens objects from it.
struct OpenSecurity {
    view: Entity<SecurityView>,
    _events: Subscription,
}

/// The overview page and the subscription that opens objects from it.
struct OpenOverview {
    view: Entity<OverviewView>,
    _events: Subscription,
}

/// A drawer tab: the object it shows, and the subscription that closes it.
struct OpenDrawer {
    view: Entity<DrawerView>,
    target: Target,
    _close: Subscription,
}

/// Objects kept open at once. Each holds its own streams, so the oldest
/// background tab makes room for a new one instead of the count growing
/// without limit.
const MAX_DRAWERS: usize = 8;

pub struct AppView {
    core: Arc<AppCore>,
    /// The tokio runtime the Kubernetes client lives on. GPUI drives its own
    /// executor, but `kube`/`hyper` need tokio's reactor, so work is spawned
    /// here and the `JoinHandle` awaited from a GPUI task (a join handle is an
    /// ordinary future on any executor).
    runtime: Handle,
    tiles: Vec<Tile>,
    rail: Vec<RailGroup>,
    clusters: HashMap<SharedString, ClusterState>,
    /// Index into `tiles`.
    active: Option<usize>,
    selected: Option<SharedString>,
    collapsed: HashSet<SharedString>,
    /// Saved order of the sidebar's sections and, within them, its resources.
    section_order: Vec<String>,
    resource_order: Vec<String>,
    /// Derived from the active cluster's discovery, the filter and
    /// `collapsed`; rebuilt by `refresh_rows` when one of those changes, never
    /// per frame.
    rows: Vec<SidebarRow>,
    list_scroll: UniformListScrollHandle,
    filter: Entity<InputState>,
    table: Option<OpenTable>,
    /// Open objects, as tabs beside the list. One is shown, `active_drawer`.
    drawers: Vec<OpenDrawer>,
    active_drawer: usize,
    /// The Helm releases view, shown instead of a resource table.
    helm: Option<Entity<HelmView>>,
    /// What the GitOps controllers are syncing.
    gitops: Option<OpenGitOps>,
    /// Misconfigurations and image vulnerabilities.
    security: Option<OpenSecurity>,
    /// What Alertmanager is firing.
    alerts: Option<OpenAlerts>,
    /// The page for creating resources from a manifest.
    create: Option<Entity<CreateView>>,
    /// The port forwards of the open cluster.
    ports: Option<Entity<PortsView>>,
    /// How a namespace's ingresses, services, workloads and pods connect.
    topology: Option<OpenTopology>,
    /// Every namespace's use against its requests.
    usage: Option<OpenUsage>,
    /// A shell on this machine pinned to the open cluster.
    shell: Option<Entity<TerminalView>>,
    /// What went wrong in earlier runs, from files on this machine.
    crashes: Option<Entity<CrashView>>,
    /// What the app changed, across every cluster.
    activity: Option<Entity<ActivityView>>,
    /// The page for adding and removing clusters, which belongs to none.
    add_cluster: Option<OpenAddCluster>,
    /// The settings page, which belongs to no cluster.
    settings: Option<OpenSettings>,
    /// The cluster overview, the page a cluster opens on.
    overview: Option<OpenOverview>,
    palette: Option<OpenPalette>,
    /// Objects pinned to the sidebar, across every cluster. Kept here so a
    /// frame does not read the preferences.
    pins: Vec<PinnedObject>,
    /// Keeps keyboard shortcuts working when no control has focus.
    focus: FocusHandle,
    next_generation: u64,
    table_filter: Entity<InputState>,
    watch_tx: mpsc::UnboundedSender<(u64, WatchEvent)>,
    status_tx: mpsc::UnboundedSender<(SharedString, ClusterStatus)>,
    /// Namespace selections to save, in the order they were made. One task
    /// writes them, so a slow write can never land after a newer one.
    persist_tx: mpsc::UnboundedSender<(SharedString, Vec<String>)>,
    _subscriptions: Vec<Subscription>,
}

impl AppView {
    pub fn new(
        core: Arc<AppCore>,
        runtime: Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = core.preferences();
        let (tiles, rail) = build_tiles(&core.clusters.contexts(), &preferences);
        let (section_order, resource_order) =
            (preferences.section_order, preferences.resource_order);
        let pins = core.preferences().pinned;

        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter resources"));
        let subscription =
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_rows(cx);
                }
            });

        let table_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter rows"));
        let table_filter_subscription = cx.subscribe_in(
            &table_filter,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_table_filter(cx);
                }
            },
        );

        let (watch_tx, mut watch_rx) = mpsc::unbounded::<(u64, WatchEvent)>();
        cx.spawn(async move |this, cx| {
            while let Some((generation, event)) = watch_rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_watch_event(generation, event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Status changes arrive from tokio tasks; this loop applies them on the
        // UI side so a VPN drop shows up without polling.
        let (status_tx, mut status_rx) = mpsc::unbounded::<(SharedString, ClusterStatus)>();
        cx.spawn(async move |this, cx| {
            while let Some((cluster, status)) = status_rx.next().await {
                if this
                    .update(cx, |this, cx| this.set_status(&cluster, status, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let (persist_tx, mut persist_rx) = mpsc::unbounded::<(SharedString, Vec<String>)>();
        let persist_core = Arc::clone(&core);
        runtime.spawn(async move {
            while let Some((cluster, namespaces)) = persist_rx.next().await {
                if let Err(err) = persist_core.remember_namespaces(&cluster, &namespaces) {
                    tracing::warn!(%cluster, %err, "could not save the namespace selection");
                }
            }
        });

        // The one thing the app asks the network for unprompted, and only when
        // the person has left it on. A failure is silent: being offline is normal.
        let preferences = core.preferences();
        if preferences.check_updates_on_startup {
            let install_here = preferences.auto_install_updates;
            let runtime = runtime.clone();
            let task = runtime.spawn_blocking(|| updates::check(env!("CARGO_PKG_VERSION")));
            cx.spawn_in(window, async move |_, cx| {
                let Ok(Ok(Some(release))) = task.await else {
                    return;
                };
                // Nothing is in progress yet, so this is the one moment a restart
                // costs nothing. It takes the same signature and version checks as
                // Settings → Updates, and an install that cannot replace itself
                // (a .deb, a build run from `target/`) only gets the notice.
                if install_here && release.installable() && updates::current_install().is_some() {
                    update_progress::install(&runtime, release, cx).await;
                    return;
                }
                cx.update(|window, cx| {
                    toast_success(
                        window,
                        cx,
                        format!(
                            "Kubernaut {} is available. See Settings → Updates.",
                            release.version
                        ),
                    );
                })
                .ok();
            })
            .detach();
        }

        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        Self {
            core,
            runtime,
            tiles,
            rail,
            clusters: HashMap::new(),
            active: None,
            selected: None,
            collapsed: HashSet::new(),
            section_order,
            resource_order,
            rows: Vec::new(),
            list_scroll: UniformListScrollHandle::new(),
            filter,
            table: None,
            drawers: Vec::new(),
            active_drawer: 0,
            helm: None,
            gitops: None,
            security: None,
            alerts: None,
            create: None,
            ports: None,
            topology: None,
            usage: None,
            shell: None,
            activity: None,
            crashes: None,
            add_cluster: None,
            settings: None,
            overview: None,
            palette: None,
            pins,
            focus,
            next_generation: 0,
            table_filter,
            watch_tx,
            status_tx,
            persist_tx,
            _subscriptions: vec![
                subscription,
                table_filter_subscription,
                // Light and dark follow the system, live.
                cx.observe_window_appearance(window, |_, window, cx| {
                    theme::apply(Some(window), cx);
                }),
            ],
        }
    }

    /// Close whatever page is showing, so the next one starts clean. Dropping
    /// a table aborts its watch.
    fn clear_pages(&mut self) {
        self.selected = None;
        self.table = None;
        self.drawers.clear();
        self.helm = None;
        self.gitops = None;
        self.security = None;
        self.alerts = None;
        self.create = None;
        self.ports = None;
        self.topology = None;
        self.usage = None;
        self.shell = None;
        self.activity = None;
        self.crashes = None;
        self.add_cluster = None;
        self.settings = None;
        self.overview = None;
    }

    fn active_state(&self) -> Option<&ClusterState> {
        let tile = self.tiles.get(self.active?)?;
        self.clusters.get(&tile.name)
    }

    fn set_status(
        &mut self,
        cluster: &SharedString,
        status: ClusterStatus,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.clusters.get_mut(cluster) {
            state.set_status(status);
            cx.notify();
        }
    }

    /// Rebuild the sidebar rows. Call after the filter, a collapse, the active
    /// cluster or its discovery changes.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter.read(cx).value().trim().to_lowercase();
        self.rows = match self.active_state().and_then(|s| s.discovery.as_deref()) {
            Some(discovery) => build_rows(discovery, &needle, &self.collapsed, self.order()),
            None => Vec::new(),
        };
        cx.notify();
    }

    fn order(&self) -> SidebarOrder<'_> {
        SidebarOrder {
            sections: &self.section_order,
            resources: &self.resource_order,
        }
    }

    /// Put the dragged section in the slot of the one it was dropped on.
    fn move_section(&mut self, moved: &str, target: &str, cx: &mut Context<Self>) {
        if self.is_filtering(cx) {
            return;
        }
        let shown = section_names(&self.rows);
        if let Some(next) = reordered(&self.section_order, shown, moved, target) {
            self.section_order.clone_from(&next);
            self.save_order(move |p| p.section_order = next, cx);
        }
    }

    /// Reorder a resource within its section. Dropping on another section's
    /// row does nothing: a kind belongs to one section.
    fn move_resource(&mut self, moved: &str, target: &str, cx: &mut Context<Self>) {
        if self.is_filtering(cx) {
            return;
        }
        let shown = section_resources(&self.rows, target);
        if let Some(next) = reordered(&self.resource_order, shown, moved, target) {
            self.resource_order.clone_from(&next);
            self.save_order(move |p| p.resource_order = next, cx);
        }
    }

    /// A filtered list shows some rows only, so dragging in it would be a guess.
    fn is_filtering(&self, cx: &App) -> bool {
        !self.filter.read(cx).value().trim().is_empty()
    }

    fn save_order(
        &mut self,
        edit: impl FnOnce(&mut Preferences) + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { core.update_preferences(edit) });
        cx.spawn(async move |_, _| {
            if let Ok(Err(reason)) = task.await {
                tracing::warn!(%reason, "could not save the sidebar order");
            }
        })
        .detach();
        self.refresh_rows(cx);
    }

    fn toggle_group(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        if !self.collapsed.remove(name) {
            self.collapsed.insert(name.clone());
        }
        self.refresh_rows(cx);
    }

    /// Open a cluster tile. Connecting is lazy and explicit, as in the web UI:
    /// nothing is reachable until the user picks it.
    fn select_cluster(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.get(index) else {
            return;
        };
        let name = tile.name.clone();
        self.active = Some(index);
        self.clear_pages();
        if self.clusters.contains_key(&name) {
            self.refresh_rows(cx);
            self.open_overview(window, cx);
            return;
        }
        self.clusters
            .insert(name.clone(), ClusterState::new(ClusterStatus::Connecting));
        self.refresh_rows(cx);

        let core = Arc::clone(&self.core);
        let status_tx = self.status_tx.clone();
        let context = name.clone();
        let task = self.runtime.spawn(async move {
            let handle = core.connect(&context).await.map_err(|e| e.to_string())?;

            let mut rx = handle.watch_status();
            let cancel = handle.cancel_token();
            let id = SharedString::from(handle.id.clone());
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        changed = rx.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            let status = rx.borrow_and_update().clone();
                            if status_tx.unbounded_send((id.clone(), status)).is_err() {
                                return;
                            }
                        }
                    }
                }
            });

            let discovery = match handle.discovery() {
                Some(cache) => cache,
                None => handle
                    .refresh_discovery()
                    .await
                    .map_err(|e| e.to_string())?,
            };
            // Falls back to the context's own namespace when listing is
            // forbidden, so a restricted user still gets a working picker.
            let namespaces = k8s_core::objects::list_namespaces(&handle).await;
            Ok::<_, String>((handle.status(), discovery, namespaces))
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_connect(&name, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_connect(
        &mut self,
        name: &SharedString,
        result: Result<(ClusterStatus, Arc<DiscoveryCache>, Vec<String>), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.clusters.get_mut(name) {
            match result {
                Ok((status, discovery, namespaces)) => {
                    state.set_status(status);
                    state.discovery = Some(discovery);
                    state.namespaces = namespaces.into_iter().map(Into::into).collect();
                    // Back to where the user left this cluster, minus any
                    // namespace that no longer exists.
                    state.scope = namespaces::restore(
                        self.core.preferences().saved_namespaces(name),
                        &state.namespaces,
                    );
                }
                Err(reason) => {
                    state.set_status(ClusterStatus::Unreachable {
                        reason: reason.clone(),
                    });
                    state.error = Some(reason.into());
                }
            }
        }
        self.refresh_rows(cx);
        // A cluster opens on its overview, unless the user has already moved on.
        let is_active = self
            .active
            .and_then(|index| self.tiles.get(index))
            .is_some_and(|tile| tile.name == *name);
        let idle = self.table.is_none()
            && self.helm.is_none()
            && self.gitops.is_none()
            && self.security.is_none()
            && self.alerts.is_none()
            && self.create.is_none()
            && self.ports.is_none()
            && self.topology.is_none()
            && self.usage.is_none()
            && self.shell.is_none()
            && self.activity.is_none()
            && self.crashes.is_none()
            && self.add_cluster.is_none()
            && self.settings.is_none()
            && self.overview.is_none();
        let connected = self
            .clusters
            .get(name)
            .is_some_and(|state| state.discovery.is_some());
        if is_active && idle && connected {
            self.open_overview(window, cx);
        }
    }

    fn disconnect(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.get(index) else {
            return;
        };
        let name = tile.name.clone();
        self.clusters.remove(&name);
        if self.active == Some(index) {
            self.active = None;
            self.clear_pages();
        }
        self.refresh_rows(cx);

        let core = Arc::clone(&self.core);
        self.runtime
            .spawn(async move { core.disconnect(&name).await });
    }

    /// Open the table for a resource: subscribe to its watch and show rows as
    /// batches arrive.
    fn open_resource(&mut self, key: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_pages();
        self.selected = Some(key.clone());

        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let Some(discovery) = self.active_state().and_then(|s| s.discovery.clone()) else {
            return;
        };
        let Some(descriptor) = discovery.get(key) else {
            return;
        };
        let title = SharedString::from(descriptor.kind.clone());
        let custom_columns = self
            .core
            .preferences()
            .custom_columns
            .remove(&format!("{}/{}", descriptor.group, descriptor.kind))
            .unwrap_or_default();
        let (namespace, client_scope) = self
            .active_state()
            .map(|state| namespaces::watch_scope(&state.scope))
            .unwrap_or_default();
        let request = WatchRequest {
            resource: key.to_string(),
            namespace,
            label_selector: None,
            field_selector: None,
            custom_columns,
        };

        let generation = self.next_generation;
        self.next_generation += 1;

        let context = RowContext {
            core: Arc::clone(&self.core),
            scope: client_scope,
            owner: cx.entity().downgrade(),
            cluster: cluster.clone(),
            resource: key.clone(),
            kind: title.clone(),
        };
        let state = cx.new(|cx| {
            TableState::new(ResourceDelegate::new(context), window, cx)
                .sortable(true)
                .col_resizable(true)
        });
        let events = cx.subscribe_in(
            &state,
            window,
            |this, state, event: &TableEvent, window, cx| {
                if let TableEvent::SelectRow(view_ix) = event {
                    let view_ix = *view_ix;
                    state.update(cx, |table, cx| {
                        table.delegate_mut().select(view_ix);
                        cx.notify();
                    });
                    this.open_drawer(view_ix, window, cx);
                }
            },
        );
        self.table_filter
            .update(cx, |input, cx| input.set_value("", window, cx));

        let task = self.runtime.spawn(forward_watch(
            Arc::clone(&self.core),
            cluster.clone(),
            request,
            generation,
            self.watch_tx.clone(),
        ));
        // The node list also shows what each node uses, which comes from the
        // sampler rather than the watch.
        let usage = (&**key == "core/v1/nodes").then(|| {
            let (core, cluster, state) =
                (Arc::clone(&self.core), cluster.clone(), state.downgrade());
            cx.spawn(async move |_, cx| {
                loop {
                    if let Ok(summaries) = metrics_ops::node_summaries(&core, &cluster) {
                        let alive = state.update(cx, |table, cx| {
                            table.delegate_mut().set_node_usage(summaries);
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                    cx.background_executor().timer(NODE_USAGE_POLL).await;
                }
            })
        });
        self.table = Some(OpenTable {
            _usage: usage,
            generation,
            resource: key.clone(),
            empty_label: format!("No {title} found.").into(),
            title,
            state,
            task,
            count_label: SharedString::default(),
            status: Some("Loading…".into()),
            error: None,
            _events: events,
        });
        cx.notify();
    }

    /// Open the command palette, or close it if it is open.
    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        let items = self.palette_items();
        self.open_palette(
            items,
            "Search resources, clusters and pages",
            "↩ open",
            window,
            cx,
        );
    }

    /// Show the palette over `items`, replacing one that is already open.
    fn open_palette(
        &mut self,
        items: Vec<Item>,
        placeholder: &'static str,
        footer: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| PaletteView::new(items, placeholder, footer, window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &PaletteEvent, window, cx| {
                // Ticking a namespace keeps the picker open, so several can be
                // chosen in one go; everything else ends the palette.
                let keeps_open = matches!(event, PaletteEvent::Choose(Command::ToggleNamespace(_)));
                if !keeps_open {
                    this.close_palette(window, cx);
                }
                if let PaletteEvent::Choose(command) = event {
                    this.run_command(command, window, cx);
                }
            },
        );
        self.palette = Some(OpenPalette {
            view,
            _events: events,
        });
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Everything the palette can jump to: pages and clusters first, then
    /// resource types by section.
    fn palette_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let discovery = self.active_state().and_then(|s| s.discovery.as_deref());
        if discovery.is_some() {
            items.push(Item {
                label: "Overview".into(),
                hint: "Page".into(),
                icon: IconName::LayoutDashboard,
                command: Command::Overview,
            });
            items.push(Item {
                label: "GitOps".into(),
                hint: "Page".into(),
                icon: IconName::GitBranch,
                command: Command::GitOps,
            });
            items.push(Item {
                label: "Security".into(),
                hint: "Page".into(),
                icon: IconName::ShieldCheck,
                command: Command::Security,
            });
            items.push(Item {
                label: "Alerts".into(),
                hint: "Page".into(),
                icon: IconName::Bell,
                command: Command::Alerts,
            });
            items.push(Item {
                label: "Topology".into(),
                hint: "Page".into(),
                icon: IconName::Network,
                command: Command::Topology,
            });
            items.push(Item {
                label: "Usage".into(),
                hint: "Page".into(),
                icon: IconName::Gauge,
                command: Command::Usage,
            });
            items.push(Item {
                label: "Local shell".into(),
                hint: "Page".into(),
                icon: IconName::SquareTerminal,
                command: Command::Shell,
            });
            items.push(Item {
                label: "Create resource".into(),
                hint: "Page".into(),
                icon: IconName::Plus,
                command: Command::Create,
            });
            items.push(Item {
                label: "Port forwards".into(),
                hint: "Page".into(),
                icon: IconName::ArrowLeftRight,
                command: Command::Ports,
            });
            items.push(Item {
                label: "Helm releases".into(),
                hint: "Page".into(),
                icon: IconName::ShipWheel,
                command: Command::Helm,
            });
        }
        items.extend(self.tiles.iter().enumerate().map(|(index, tile)| Item {
            label: tile.display.clone(),
            hint: "Switch cluster".into(),
            icon: IconName::Globe,
            command: Command::Cluster(index),
        }));
        if let Some(discovery) = discovery {
            for group in visible_groups(discovery, "", &HashSet::new(), self.order()) {
                let hint: SharedString = group.category.label().into();
                items.extend(group.resources.into_iter().map(|resource| Item {
                    label: resource.kind.clone().into(),
                    hint: hint.clone(),
                    icon: kind_icon(&resource.kind).unwrap_or_else(|| group.category.icon()),
                    command: Command::Resource(resource.key.clone().into()),
                }));
            }
        }
        items.push(Item {
            label: "Activity".into(),
            hint: "Page".into(),
            icon: IconName::Clock,
            command: Command::Activity,
        });
        items.push(Item {
            label: "Crash log".into(),
            hint: "Page".into(),
            icon: IconName::TriangleAlert,
            command: Command::Crashes,
        });
        items.push(Item {
            label: "Add cluster".into(),
            hint: "Page".into(),
            icon: IconName::Plus,
            command: Command::AddCluster,
        });
        items.push(Item {
            label: "Check for updates".into(),
            hint: "Action".into(),
            icon: IconName::ArrowDownToLine,
            command: Command::CheckUpdates,
        });
        items.push(Item {
            label: "Settings".into(),
            hint: "Page".into(),
            icon: IconName::Settings,
            command: Command::Settings,
        });
        items
    }

    fn run_command(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::Overview => self.open_overview(window, cx),
            Command::Helm => self.open_helm(window, cx),
            Command::GitOps => self.open_gitops(window, cx),
            Command::Security => self.open_security(window, cx),
            Command::Alerts => self.open_alerts(window, cx),
            Command::Settings => self.open_settings(window, cx),
            Command::CheckUpdates => self.check_for_updates(window, cx),
            Command::AddCluster => self.open_add_cluster(window, cx),
            Command::Activity => self.open_activity(window, cx),
            Command::Crashes => self.open_crashes(cx),
            Command::Usage => self.open_usage(window, cx),
            Command::Shell => self.open_shell(window, cx),
            Command::Topology => self.open_topology(window, cx),
            Command::Ports => self.open_ports(None, window, cx),
            Command::Create => self.open_create(window, cx),
            Command::Cluster(index) => self.select_cluster(*index, window, cx),
            Command::Resource(key) => self.open_resource(key, window, cx),
            Command::ToggleNamespace(namespace) => {
                self.change_scope(window, cx, |scope| namespaces::toggle(scope, namespace));
            }
            Command::ClearNamespaces => self.change_scope(window, cx, Vec::clear),
        }
    }

    fn open_namespace_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.active_state() else {
            return;
        };
        let items = namespaces::items(&state.namespaces, &state.scope);
        self.open_palette(items, "Filter namespaces", "↩ tick", window, cx);
    }

    /// Change the namespace selection and reload what is showing, so the page
    /// never disagrees with the toolbar. An open picker is refreshed in place.
    fn change_scope(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Vec<SharedString>),
    ) {
        let Some(name) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let Some(state) = self.clusters.get_mut(&name) else {
            return;
        };
        let before = state.scope.clone();
        edit(&mut state.scope);
        if state.scope == before {
            return;
        }
        let saved: Vec<String> = state.scope.iter().map(ToString::to_string).collect();
        let scope_now = state.scope.clone();
        let items = self
            .palette
            .is_some()
            .then(|| namespaces::items(&state.namespaces, &state.scope));
        self.persist_tx.unbounded_send((name, saved)).ok();

        if let (Some(palette), Some(items)) = (&self.palette, items) {
            palette
                .view
                .update(cx, |palette, cx| palette.set_items(items, cx));
        }
        if let Some(resource) = self.table.as_ref().map(|table| table.resource.clone()) {
            self.open_resource(&resource, window, cx);
        } else if self.helm.is_some() {
            self.open_helm(window, cx);
        } else if self.gitops.is_some() {
            self.open_gitops(window, cx);
        } else if self.security.is_some() {
            self.open_security(window, cx);
        } else if self.alerts.is_some() {
            self.open_alerts(window, cx);
        } else if self.usage.is_some() {
            self.open_usage(window, cx);
        } else if let Some(topology) = &self.topology {
            // Redraw in place: it keeps its scroll and has one namespace to show.
            topology
                .view
                .update(cx, |view, cx| view.follow(&scope_now, cx));
        }
        cx.notify();
    }

    /// Show the overview of the active cluster.
    fn open_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| OverviewView::new(core, runtime, cluster, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &OverviewEvent, window, cx| match event {
                OverviewEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
            },
        );
        self.overview = Some(OpenOverview {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Show what the GitOps controllers of the active cluster are syncing.
    fn open_gitops(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let scope = self
            .active_state()
            .map(|state| state.scope.clone())
            .unwrap_or_default();
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| GitOpsView::new(core, runtime, cluster, scope, window, cx));
        let events =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &GitOpsEvent, window, cx| match event {
                    GitOpsEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
                },
            );
        self.gitops = Some(OpenGitOps {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Show the page for creating resources in the active cluster.
    fn open_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        // Start in the namespace the user is looking at, else the cluster's
        // configured default.
        let namespace = self
            .active_state()
            .and_then(|state| match state.scope.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            })
            .or_else(|| {
                self.core
                    .preferences()
                    .profile(&cluster)
                    .default_namespace
                    .map(Into::into)
            })
            // Kubernetes' own default, so a namespaced manifest with no
            // namespace of its own still has somewhere to go.
            .or_else(|| Some("default".into()));
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        self.create = Some(
            cx.new(|cx| CreateView::new(core, runtime, cluster, namespace.as_ref(), window, cx)),
        );
        cx.notify();
    }

    /// Show the port forwards of the active cluster, optionally with the form
    /// open for a target.
    fn open_ports(&mut self, target: Option<Target>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        self.ports = Some(cx.new(|cx| PortsView::new(core, runtime, cluster, target, window, cx)));
        cx.notify();
    }

    /// Show how a namespace's objects connect.
    fn open_topology(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let (known, preferred) = self
            .active_state()
            .map(|state| (state.namespaces.clone(), state.scope.first().cloned()))
            .unwrap_or_default();
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| TopologyView::new(core, runtime, cluster, known, preferred, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &TopologyEvent, window, cx| match event {
                TopologyEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
            },
        );
        self.topology = Some(OpenTopology {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Show every namespace's use against what it requested.
    fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let scope = self
            .active_state()
            .map(|state| state.scope.clone())
            .unwrap_or_default();
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| UsageView::new(core, runtime, cluster, scope, cx));
        let events =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &UsageEvent, window, cx| match event {
                    UsageEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
                },
            );
        self.usage = Some(OpenUsage {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Open a shell on this machine whose `kubectl` reaches only the active
    /// cluster, in the one namespace selected if exactly one is.
    fn open_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let namespace = self
            .active_state()
            .and_then(|state| match state.scope.as_slice() {
                [only] => Some(only.to_string()),
                _ => None,
            });
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx
            .new(|cx| TerminalView::new(core, runtime, cluster, Request::Local { namespace }, cx));
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.shell = Some(view);
        cx.notify();
    }

    /// Show what went wrong in earlier runs. It reads local files, so no cluster
    /// is needed.
    fn open_crashes(&mut self, cx: &mut Context<Self>) {
        self.clear_pages();
        let runtime = &self.runtime;
        self.crashes = Some(cx.new(|cx| CrashView::new(runtime, cx)));
        cx.notify();
    }

    /// Show what the app has changed. It reads a local log, so no cluster is
    /// needed.
    fn open_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        self.activity = Some(cx.new(|cx| ActivityView::new(core, runtime, window, cx)));
        cx.notify();
    }

    /// Show the page for adding and removing clusters.
    fn open_add_cluster(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| AddClusterView::new(core, runtime, window, cx));
        let events =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &AddClusterEvent, _, cx| match event {
                    AddClusterEvent::Changed => this.refresh_clusters(cx),
                },
            );
        self.add_cluster = Some(OpenAddCluster {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Rebuild the rail after clusters were added or removed. The cluster being
    /// viewed stays selected when it still exists; one that is gone takes its
    /// state with it.
    fn refresh_clusters(&mut self, cx: &mut Context<Self>) {
        let active = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone());
        let (tiles, rail) = build_tiles(&self.core.clusters.contexts(), &self.core.preferences());
        self.clusters
            .retain(|name, _| tiles.iter().any(|tile| tile.name == *name));
        self.tiles = tiles;
        self.rail = rail;
        self.active = active.and_then(|name| self.tiles.iter().position(|tile| tile.name == name));
        if self.active.is_none() {
            // Keep the page that is open; only the cluster pages belong to it.
            let (add, settings, activity, crashes) = (
                self.add_cluster.take(),
                self.settings.take(),
                self.activity.take(),
                self.crashes.take(),
            );
            self.clear_pages();
            self.add_cluster = add;
            self.settings = settings;
            self.activity = activity;
            self.crashes = crashes;
            self.refresh_rows(cx);
        }
        cx.notify();
    }

    /// Put the dragged cluster in the slot of the one it was dropped on, and
    /// remember that order.
    fn move_cluster(&mut self, moved: &str, target: &str, cx: &mut Context<Self>) {
        let current: Vec<String> = self
            .tiles
            .iter()
            .map(|tile| tile.name.to_string())
            .collect();
        let (moved, target) = (moved.to_string(), target.to_string());
        let core = Arc::clone(&self.core);
        let task = self.runtime.spawn(async move {
            core.update_preferences(|p| p.reorder_clusters(current, &moved, &target))
        });
        cx.spawn(async move |this, cx| {
            let saved = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                if let Err(reason) = saved {
                    tracing::warn!(%reason, "could not save the cluster order");
                }
                this.refresh_clusters(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Ask before removing an imported kubeconfig, naming every cluster in it.
    fn confirm_remove_cluster(
        &mut self,
        file: k8s_app::clusters::ManagedKubeconfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (owner, path) = (owner.clone(), file.file.clone());
            dialog
                .title(format!("Remove {}?", file.contexts.join(", ")))
                .description(
                    "It disappears from the app and its connection closes. Nothing changes \
                     in the cluster itself, and your own kubeconfig is not touched.",
                )
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let path = path.clone();
                    owner
                        .update(cx, |app, cx| app.remove_cluster(path, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn remove_cluster(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { cluster_ops::remove(&core, &file).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => toast_success(window, cx, "Removed"),
                    Err(reason) => toast_error(window, cx, "Could not remove it", reason),
                }
                this.refresh_clusters(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Say how a cluster is connected: where, as whom, and anything unusual.
    fn show_cluster_info(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.get(index) else {
            return;
        };
        let (name, display) = (tile.name.clone(), tile.display.clone());
        let entry = self
            .core
            .clusters
            .contexts()
            .into_iter()
            .find(|context| context.name == *name);
        let state = self.clusters.get(&name);
        let status = state.map_or_else(
            || "Not connected".to_string(),
            |state| state.status_label.to_string(),
        );
        let preferences = self.core.preferences();
        let rows = cluster_info::rows(
            entry.as_ref(),
            &status,
            state.map(|state| state.namespaces.len()),
            &preferences.profile(&name),
            preferences.is_protected(&name),
        );
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            dialog
                .title(display.clone())
                .description(v_flex().gap_1().children(rows.iter().map(|(label, value)| {
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            div()
                                .w(px(130.))
                                .flex_none()
                                .text_color(muted)
                                .child(*label),
                        )
                        .child(div().flex_1().min_w_0().child(value.clone()))
                })))
                .ok_text("Close")
        });
    }

    /// Open Settings at this cluster's card.
    fn edit_cluster(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.tiles.get(index).map(|tile| tile.name.clone()) else {
            return;
        };
        self.open_settings(window, cx);
        if let Some(settings) = &self.settings {
            settings
                .view
                .update(cx, |settings, cx| settings.show_cluster(&name, window, cx));
        }
    }

    /// Show the settings. They belong to the app, so no cluster is needed.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| SettingsView::new(core, runtime, window, cx));
        let events =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &SettingsEvent, _, cx| match event {
                    SettingsEvent::ClustersChanged => this.refresh_clusters(cx),
                },
            );
        self.settings = Some(OpenSettings {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Open Settings and look for a newer version there, where the result and the
    /// button that installs it are.
    pub fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        toast_success(window, cx, "Looking for a newer version…");
        let runtime = self.runtime.clone();
        let task = runtime.spawn_blocking(|| updates::check(env!("CARGO_PKG_VERSION")));
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            cx.update(|window, cx| match result {
                Ok(Some(release)) => update_progress::open_available(window, cx, runtime, release),
                Ok(None) => toast_success(
                    window,
                    cx,
                    format!(
                        "Kubernaut {} is the latest version.",
                        env!("CARGO_PKG_VERSION")
                    ),
                ),
                Err(reason) => toast_error(window, cx, "Could not check for updates", reason),
            })
            .ok();
        })
        .detach();
    }

    /// Say what this is and which version.
    pub fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            dialog
                .title(format!("Kubernaut {}", env!("CARGO_PKG_VERSION")))
                .description(
                    v_flex()
                        .gap_1()
                        .child("A multi-cluster Kubernetes desktop app, written in Rust with GPUI.")
                        .child(div().text_color(muted).child(format!(
                            "{} {} · Apache License 2.0",
                            std::env::consts::OS,
                            std::env::consts::ARCH
                        )))
                        .child(
                            div()
                                .text_color(muted)
                                .child("No telemetry; the update check is the only request it makes on its own."),
                        ),
                )
                .ok_text("Close")
                .show_cancel(true)
                .cancel_text("GitHub")
                .on_cancel(|_, _, cx| {
                    cx.open_url("https://github.com/terdiam/kubernaut");
                    true
                })
        });
    }

    /// Show the alerts of the active cluster.
    fn open_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let scope = self
            .active_state()
            .map(|state| state.scope.clone())
            .unwrap_or_default();
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| AlertsView::new(core, runtime, cluster, scope, window, cx));
        let events =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &AlertsEvent, window, cx| match event {
                    AlertsEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
                },
            );
        self.alerts = Some(OpenAlerts {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Show the Security Center of the active cluster.
    fn open_security(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let scope = self
            .active_state()
            .map(|state| state.scope.clone())
            .unwrap_or_default();
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| SecurityView::new(core, runtime, cluster, scope, window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SecurityEvent, window, cx| match event {
                SecurityEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
            },
        );
        self.security = Some(OpenSecurity {
            view,
            _events: events,
        });
        cx.notify();
    }

    /// Show the Helm releases of the active cluster.
    fn open_helm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let scope = self
            .active_state()
            .map(|state| state.scope.clone())
            .unwrap_or_default();
        self.clear_pages();
        let (core, runtime) = (&self.core, &self.runtime);
        self.helm = Some(cx.new(|cx| HelmView::new(core, runtime, cluster, scope, window, cx)));
        cx.notify();
    }

    /// Show the object behind a table row.
    fn open_drawer(&mut self, view_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = &self.table else {
            return;
        };
        let Some((name, namespace)) = table.state.read(cx).delegate().target_at(view_ix) else {
            return;
        };
        let Some(cluster) = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())
        else {
            return;
        };
        let target = Target {
            cluster,
            resource: table.resource.clone(),
            kind: table.title.clone(),
            namespace,
            name,
        };
        self.show_drawer(target, window, cx);
    }

    /// Open an object in the drawer: its tab if it is already open, otherwise a
    /// new one beside the others.
    fn show_drawer(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .drawers
            .iter()
            .position(|open| open.view.read(cx).is_for(&target));
        if let Some(index) = existing {
            self.active_drawer = index;
            cx.notify();
            return;
        }
        let subject = target.clone();
        let view =
            cx.new(|cx| DrawerView::new(&self.core, &self.runtime, target.clone(), window, cx));
        let id = view.entity_id();
        let close = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &DrawerEvent, window, cx| match event {
                DrawerEvent::Close => this.close_drawer(id, cx),
                DrawerEvent::Row(action) => this.row_action(*action, subject.clone(), window, cx),
                DrawerEvent::Open(target) => this.show_drawer(target.clone(), window, cx),
            },
        );
        if self.drawers.len() >= MAX_DRAWERS {
            // The oldest tab that is not the one being read.
            let drop = tab_to_evict(self.active_drawer);
            self.drawers.remove(drop);
            self.active_drawer =
                active_after_close(self.active_drawer, drop, self.drawers.len() + 1);
        }
        self.drawers.push(OpenDrawer {
            view,
            target,
            _close: close,
        });
        self.active_drawer = self.drawers.len() - 1;
        cx.notify();
    }

    /// Close one drawer tab and show a neighbour.
    fn close_drawer(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(index) = self
            .drawers
            .iter()
            .position(|open| open.view.entity_id() == id)
        else {
            return;
        };
        let before = self.drawers.len();
        self.drawers.remove(index);
        self.active_drawer = active_after_close(self.active_drawer, index, before);
        cx.notify();
    }

    /// The drawer column: its tabs when there is more than one, and the open
    /// object under them.
    fn render_drawer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = self.drawers.get(self.active_drawer)?;
        let theme = cx.theme();
        let (background, border, muted, selected, hover) = (
            theme.background,
            theme.border,
            theme.muted_foreground,
            theme.secondary,
            theme.secondary_hover,
        );
        let strip = (self.drawers.len() > 1).then(|| {
            h_flex()
                .id("drawer-tabs")
                .flex_none()
                .h(px(32.))
                .px_1()
                .gap_1()
                .items_center()
                .overflow_x_scroll()
                .border_b_1()
                .border_color(border)
                .children(self.drawers.iter().enumerate().map(|(index, open)| {
                    let id = open.view.entity_id();
                    let is_active = index == self.active_drawer;
                    let name: SharedString = open.target.name.clone();
                    h_flex()
                        .id(("drawer-tab", index))
                        .flex_none()
                        .max_w(px(170.))
                        .h(px(24.))
                        .pl_2()
                        .pr_1()
                        .gap_1()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .when(is_active, |tab| tab.bg(selected).font_semibold())
                        .when(!is_active, |tab| {
                            tab.text_color(muted).hover(|t| t.bg(hover))
                        })
                        .child(div().min_w_0().truncate().child(name))
                        .child(
                            div()
                                .id(("drawer-tab-close", index))
                                .size(px(16.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .hover(|close| close.bg(border))
                                .child(Icon::new(IconName::Close).size_3())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_drawer(id, cx);
                                })),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_drawer = index;
                            cx.notify();
                        }))
                }))
        });
        Some(
            v_flex()
                .w(px(560.))
                .h_full()
                .flex_none()
                .bg(background)
                .border_l_1()
                .border_color(border)
                .children(strip)
                .child(active.view.clone())
                .into_any_element(),
        )
    }

    /// Do what a row's context menu entry asks.
    pub fn row_action(
        &mut self,
        action: RowAction,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            RowAction::Open => self.show_drawer(target, window, cx),
            RowAction::Logs => self.open_drawer_tab(target, DrawerTab::Logs, window, cx),
            RowAction::Shell => self.open_drawer_tab(target, DrawerTab::Terminal, window, cx),
            RowAction::Forward => self.open_ports(Some(target), window, cx),
            RowAction::Pin => self.set_pin(&target, true, window, cx),
            RowAction::Unpin => self.set_pin(&target, false, window, cx),
            RowAction::CopyName => {
                cx.write_to_clipboard(ClipboardItem::new_string(target.name.to_string()));
                toast_success(window, cx, format!("Copied {}", target.name));
            }
            RowAction::CopyYaml => self.copy_yaml(target, window, cx),
            RowAction::Cordon => self.run_op(Op::Cordon(true), target, window, cx),
            RowAction::Uncordon => self.run_op(Op::Cordon(false), target, window, cx),
            RowAction::Drain => self.drain_node(target, window, cx),
            RowAction::Scale => self.scale_workload(target, window, cx),
            RowAction::Trigger => self.confirm_op(
                Op::Trigger,
                target,
                "Run",
                "A Job is created now from its template, besides the ones the schedule makes.",
                window,
                cx,
            ),
            RowAction::Suspend => self.run_op(Op::Suspend(true), target, window, cx),
            RowAction::Resume => self.run_op(Op::Suspend(false), target, window, cx),
            RowAction::Restart => self.confirm_op(
                Op::Restart,
                target,
                "Restart",
                "Its pods are replaced one by one, as with kubectl rollout restart.",
                window,
                cx,
            ),
            RowAction::Delete => self.confirm_op(
                Op::Delete,
                target,
                "Delete",
                "This cannot be undone.",
                window,
                cx,
            ),
        }
    }

    /// Scale a workload: read its replica count, ask for a new one, set it.
    fn scale_workload(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(reason) = self.core.ensure_writable(&target.cluster) {
            toast_error(window, cx, format!("Cannot scale {}", target.name), reason);
            return;
        }
        let core = Arc::clone(&self.core);
        let (cluster, reference) = (target.cluster.to_string(), target_ref(&target));
        let task = self
            .runtime
            .spawn(async move { object_ops::current_scale(&core, &cluster, reference).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(current) => this.ask_replicas(target, current, window, cx),
                Err(reason) => toast_error(
                    window,
                    cx,
                    format!("Cannot read the scale of {}", target.name),
                    reason,
                ),
            })
            .ok();
        })
        .detach();
    }

    fn ask_replicas(
        &mut self,
        target: Target,
        current: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Replicas");
            state.set_value(current.to_string(), window, cx);
            state
        });
        let owner = cx.entity().downgrade();
        let title: SharedString = format!("Scale {} {}", target.kind, target.name).into();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (owner, target, input) = (owner.clone(), target.clone(), input.clone());
            dialog
                .title(title.clone())
                .description(
                    v_flex()
                        .gap_2()
                        .child(format!("It has {current} replicas now."))
                        .child(Input::new(&input)),
                )
                .ok_text("Scale")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let text = input.read(cx).value().to_string();
                    match object_ops::parse_replicas(&text) {
                        Ok(replicas) => {
                            owner
                                .update(cx, |app, cx| {
                                    app.apply_scale(target.clone(), replicas, window, cx)
                                })
                                .ok();
                            true
                        }
                        Err(reason) => {
                            toast_error(window, cx, "Not a valid replica count", reason);
                            false
                        }
                    }
                })
        });
    }

    fn apply_scale(
        &mut self,
        target: Target,
        replicas: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let (cluster, reference) = (target.cluster.to_string(), target_ref(&target));
        let task = self
            .runtime
            .spawn(async move { object_ops::scale(&core, &cluster, reference, replicas).await });
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            cx.update(|window, cx| match result {
                Ok(()) => {
                    toast_success(window, cx, format!("Scaled {} to {replicas}", target.name))
                }
                Err(reason) => toast_error(
                    window,
                    cx,
                    format!("Could not scale {}", target.name),
                    reason,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Drain a node: look at what it would do, ask, then do it.
    fn drain_node(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        // Refuse early on a protected cluster, before a question that cannot be
        // acted on.
        if let Err(reason) = self.core.ensure_writable(&target.cluster) {
            toast_error(window, cx, format!("Cannot drain {}", target.name), reason);
            return;
        }
        let core = Arc::clone(&self.core);
        let (cluster, node) = (target.cluster.to_string(), target.name.to_string());
        let task = self
            .runtime
            .spawn(async move { object_ops::drain_preview(&core, &cluster, &node, false).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(preview) => this.confirm_drain(target, &preview, window, cx),
                Err(reason) => toast_error(
                    window,
                    cx,
                    format!("Could not look at {}", target.name),
                    reason,
                ),
            })
            .ok();
        })
        .detach();
    }

    fn confirm_drain(
        &mut self,
        target: Target,
        preview: &k8s_ops::actions::DrainReport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        let node = target.name.clone();
        drain::open_confirm(window, cx, &node, preview, move |standalone, window, cx| {
            owner
                .update(cx, |app, cx| {
                    app.run_drain(target.clone(), standalone, window, cx);
                })
                .ok();
        });
    }

    fn run_drain(
        &mut self,
        target: Target,
        delete_standalone_pods: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        toast_success(window, cx, format!("Draining {}…", target.name));
        let core = Arc::clone(&self.core);
        let (cluster, node) = (target.cluster.to_string(), target.name.to_string());
        let task = self.runtime.spawn(async move {
            object_ops::drain(&core, &cluster, &node, delete_standalone_pods).await
        });
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            cx.update(|window, cx| match result {
                Ok(report) => drain::open_result(window, cx, &target.name, &report),
                Err(reason) => toast_error(
                    window,
                    cx,
                    format!("Could not drain {}", target.name),
                    reason,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Pin an object to the sidebar, or take it off. The list on screen changes
    /// at once; the file follows.
    fn set_pin(
        &mut self,
        target: &Target,
        pinned: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pin = target.pin();
        set_pinned(&mut self.pins, pin.clone(), pinned);
        let core = Arc::clone(&self.core);
        let task = self.runtime.spawn(async move {
            core.update_preferences(|preferences| set_pinned(&mut preferences.pinned, pin, pinned))
        });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(reason) = task.await.unwrap_or_else(|err| Err(err.to_string())) {
                cx.update(|window, cx| toast_error(window, cx, "Could not save the pin", reason))
                    .ok();
            }
        })
        .detach();
        toast_success(
            window,
            cx,
            if pinned {
                format!("Pinned {}", target.name)
            } else {
                format!("Unpinned {}", target.name)
            },
        );
        cx.notify();
    }

    /// Open a pinned object. The drawer shows beside a list or an overview, so
    /// from a page that has none, go to the overview first.
    fn open_pinned(&mut self, pin: &PinnedObject, window: &mut Window, cx: &mut Context<Self>) {
        let target = Target {
            cluster: pin.cluster.clone().into(),
            resource: pin.resource_key.clone().into(),
            kind: pin
                .kind
                .clone()
                .unwrap_or_else(|| crate::diagnose::kind_of(&pin.resource_key))
                .into(),
            namespace: pin.namespace.clone().map(Into::into),
            name: pin.name.clone().into(),
        };
        let without_drawer = self.helm.is_some()
            || self.create.is_some()
            || self.ports.is_some()
            || self.activity.is_some()
            || self.crashes.is_some()
            || self.add_cluster.is_some()
            || self.settings.is_some();
        if without_drawer {
            self.open_overview(window, cx);
        }
        self.show_drawer(target, window, cx);
    }

    /// The pins of the open cluster, under the pages.
    fn render_pins(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cluster = self
            .active
            .and_then(|index| self.tiles.get(index))
            .map(|tile| tile.name.clone())?;
        let mine: Vec<(usize, &PinnedObject)> = self
            .pins
            .iter()
            .enumerate()
            .filter(|(_, pin)| *pin.cluster == *cluster)
            .collect();
        if mine.is_empty() {
            return None;
        }
        let (muted, hover) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.sidebar_accent)
        };
        let mut rows = v_flex().gap_0p5();
        for (index, pin) in mine {
            let icon = pin
                .kind
                .as_deref()
                .and_then(kind_icon)
                .unwrap_or(IconName::Pin);
            let open_pin = pin.clone();
            let unpin = Target {
                cluster: pin.cluster.clone().into(),
                resource: pin.resource_key.clone().into(),
                kind: pin.kind.clone().unwrap_or_default().into(),
                namespace: pin.namespace.clone().map(Into::into),
                name: pin.name.clone().into(),
            };
            rows = rows.child(
                h_flex()
                    .id(("pin", index))
                    .group("pin-row")
                    .h(px(26.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .hover(|row| row.bg(hover))
                    .child(Icon::new(icon).size_3p5().text_color(muted).flex_none())
                    .child(div().flex_1().min_w_0().truncate().child(pin.name.clone()))
                    .child(
                        div()
                            .max_w(px(70.))
                            .flex_none()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(pin.namespace.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .id(("unpin", index))
                            .flex_none()
                            .invisible()
                            .group_hover("pin-row", |button| button.visible())
                            .tooltip(|window, cx| Tooltip::new("Unpin").build(window, cx))
                            .child(Icon::new(IconName::Close).size_3().text_color(muted))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_pin(&unpin, false, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_pinned(&open_pin, window, cx);
                    })),
            );
        }
        Some(
            v_flex()
                .px_2()
                .pt_2()
                .gap_1()
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .font_semibold()
                        .text_color(muted)
                        .child("PINNED"),
                )
                .child(
                    div()
                        .id("pins")
                        .max_h(px(150.))
                        .overflow_y_scroll()
                        .child(rows),
                )
                .into_any_element(),
        )
    }

    fn open_drawer_tab(
        &mut self,
        target: Target,
        tab: DrawerTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_drawer(target, window, cx);
        if let Some(drawer) = self.drawers.get(self.active_drawer) {
            drawer
                .view
                .update(cx, |drawer, cx| drawer.show(tab, window, cx));
        }
    }

    fn copy_yaml(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let task = self
            .runtime
            .spawn(fetch_yaml(Arc::clone(&self.core), target.clone()));
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            cx.update(|window, cx| match result {
                Ok(yaml) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(yaml));
                    toast_success(window, cx, format!("Copied the YAML of {}", target.name));
                }
                Err(reason) => toast_error(window, cx, "Could not copy the YAML", reason),
            })
            .ok();
        })
        .detach();
    }

    /// Ask before an operation that changes the cluster. The dialog names the
    /// object, and the button repeats the verb, so it cannot be mistaken for
    /// another prompt.
    fn confirm_op(
        &mut self,
        op: Op,
        target: Target,
        verb: &'static str,
        detail: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        let question: SharedString = format!("{verb} {} {}?", target.kind, target.name).into();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (owner, target) = (owner.clone(), target.clone());
            let dialog = dialog
                .title(question.clone())
                .description(detail)
                .ok_text(verb)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    owner
                        .update(cx, |app, cx| app.run_op(op, target.clone(), window, cx))
                        .ok();
                    true
                });
            // A delete is red, so it is recognisable before the label is read.
            if op == Op::Delete {
                dialog.ok_variant(ButtonVariant::Danger)
            } else {
                dialog
            }
        });
    }

    /// The ticked rows of the open table, as the objects to act on.
    fn checked_targets(&self, cx: &App) -> Vec<Target> {
        let (Some(table), Some(cluster)) = (
            &self.table,
            self.active
                .and_then(|index| self.tiles.get(index))
                .map(|tile| tile.name.clone()),
        ) else {
            return Vec::new();
        };
        table
            .state
            .read(cx)
            .delegate()
            .checked_targets()
            .into_iter()
            .map(|(name, namespace)| Target {
                cluster: cluster.clone(),
                resource: table.resource.clone(),
                kind: table.title.clone(),
                namespace,
                name,
            })
            .collect()
    }

    fn clear_checked(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = &self.table {
            table.state.update(cx, |table, cx| {
                table.delegate_mut().clear_checked();
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Ask before changing a set of objects. The dialog says how many and
    /// names the first few, so the count cannot be read past.
    fn confirm_bulk(&mut self, op: BulkOp, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.checked_targets(cx);
        let Some(first) = targets.first() else {
            return;
        };
        let (verb, detail) = match op {
            BulkOp::Delete => ("Delete", "This cannot be undone."),
            BulkOp::Restart => (
                "Restart",
                "Their pods are replaced one by one, as with kubectl rollout restart.",
            ),
        };
        let count = targets.len();
        let question: SharedString = if count == 1 {
            format!("{verb} 1 {}?", first.kind).into()
        } else {
            format!("{verb} {count} {} objects?", first.kind).into()
        };
        let names: Vec<SharedString> = targets.iter().map(|t| t.name.clone()).collect();
        let description: SharedString = format!("{}. {detail}", name_list(&names)).into();
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            let dialog = dialog
                .title(question.clone())
                .description(description.clone())
                .ok_text(verb)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    owner
                        .update(cx, |app, cx| app.run_bulk(op, window, cx))
                        .ok();
                    true
                });
            if op == BulkOp::Delete {
                dialog.ok_variant(ButtonVariant::Danger)
            } else {
                dialog
            }
        });
    }

    fn run_bulk(&mut self, op: BulkOp, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.checked_targets(cx);
        let Some(cluster) = targets.first().map(|t| t.cluster.clone()) else {
            return;
        };
        let references: Vec<TargetRef> = targets.iter().map(target_ref).collect();
        let core = Arc::clone(&self.core);
        let task = self.runtime.spawn(async move {
            match op {
                BulkOp::Delete => object_ops::delete_many(&core, &cluster, references).await,
                BulkOp::Restart => object_ops::restart_many(&core, &cluster, references).await,
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_bulk(op, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_bulk(
        &mut self,
        op: BulkOp,
        result: Result<Vec<k8s_ops::actions::BulkOutcome>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (done, past) = match op {
            BulkOp::Delete => ("delete", "Deleted"),
            BulkOp::Restart => ("restart", "Restarted"),
        };
        match result {
            Ok(outcomes) => {
                self.clear_checked(cx);
                let failed: Vec<String> = outcomes
                    .iter()
                    .filter(|outcome| !outcome.ok)
                    .map(|outcome| {
                        format!(
                            "{}: {}",
                            outcome.name,
                            outcome.error.as_deref().unwrap_or("failed")
                        )
                    })
                    .collect();
                if failed.is_empty() {
                    toast_success(window, cx, format!("{past} {}", outcomes.len()));
                } else {
                    let shown: Vec<&str> = failed.iter().take(3).map(String::as_str).collect();
                    let more = failed.len().saturating_sub(shown.len());
                    let mut reason = shown.join("\n");
                    if more > 0 {
                        reason.push_str(&format!("\nand {more} more"));
                    }
                    toast_error(
                        window,
                        cx,
                        format!("Could not {done} {} of {}", failed.len(), outcomes.len()),
                        reason,
                    );
                }
            }
            Err(reason) => toast_error(window, cx, format!("Could not {done}"), reason),
        }
    }

    /// Save the ticked objects as a zip, one YAML file each.
    fn export_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.checked_targets(cx);
        let Some(cluster) = targets.first().map(|t| t.cluster.clone()) else {
            return;
        };
        let directory = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let chosen = cx.prompt_for_new_path(&directory, Some("kubernaut-export.zip"));
        let references: Vec<TargetRef> = targets.iter().map(target_ref).collect();
        let core = Arc::clone(&self.core);
        let runtime = self.runtime.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = chosen.await.ok().and_then(Result::ok).flatten() else {
                return;
            };
            let task = runtime.spawn(async move {
                object_ops::export_archive(&core, &cluster, references, path).await
            });
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(exported) => {
                    this.clear_checked(cx);
                    if exported.failed.is_empty() {
                        toast_success(window, cx, format!("Exported {}", exported.exported));
                    } else {
                        toast_error(
                            window,
                            cx,
                            format!(
                                "Exported {}, {} could not be read",
                                exported.exported,
                                exported.failed.len()
                            ),
                            exported
                                .failed
                                .iter()
                                .take(3)
                                .map(|f| {
                                    format!(
                                        "{}: {}",
                                        f.name,
                                        f.error.as_deref().unwrap_or("failed")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                }
                Err(reason) => toast_error(window, cx, "Could not export", reason),
            })
            .ok();
        })
        .detach();
    }

    /// Run an operation on the runtime and report how it went.
    fn run_op(&mut self, op: Op, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let (cluster, reference) = (target.cluster.clone(), target_ref(&target));
        let task = self.runtime.spawn(async move {
            match op {
                Op::Delete => object_ops::delete(&core, &cluster, reference).await,
                Op::Restart => object_ops::restart(&core, &cluster, reference).await,
                Op::Cordon(cordoned) => {
                    object_ops::set_cordoned(&core, &cluster, &reference.name, cordoned).await
                }
                Op::Trigger => object_ops::trigger_cronjob(&core, &cluster, reference)
                    .await
                    .map(|_| ()),
                Op::Suspend(suspended) => {
                    object_ops::set_suspended(&core, &cluster, reference, suspended).await
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_op(op, &target, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_op(
        &mut self,
        op: Op,
        target: &Target,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                toast_success(window, cx, op.done(target));
                // The row is gone; so is anything the drawer was showing of it.
                if op == Op::Delete {
                    let gone: Vec<EntityId> = self
                        .drawers
                        .iter()
                        .filter(|open| open.view.read(cx).is_for(target))
                        .map(|open| open.view.entity_id())
                        .collect();
                    for id in gone {
                        self.close_drawer(id, cx);
                    }
                }
            }
            Err(reason) => toast_error(window, cx, op.failed(target), reason),
        }
    }

    fn on_watch_event(&mut self, generation: u64, event: WatchEvent, cx: &mut Context<Self>) {
        let Some(table) = self
            .table
            .as_mut()
            .filter(|table| table.generation == generation)
        else {
            return;
        };
        match event {
            WatchEvent::Started { spec, initial } => table.state.update(cx, |table, cx| {
                let delegate = table.delegate_mut();
                delegate.start(&spec);
                delegate.apply(&initial);
                // The columns are known now, so the table must re-read them.
                table.refresh(cx);
            }),
            WatchEvent::Batch(batch) => table.state.update(cx, |table, cx| {
                if table.delegate_mut().apply(&batch) {
                    cx.notify();
                }
            }),
            WatchEvent::Failed(message) => table.error = Some(message.into()),
        }
        self.sync_table(cx);
    }

    /// Refresh the labels derived from the delegate: the row count and the
    /// watch status banner.
    fn sync_table(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        let delegate = table.state.read(cx).delegate();
        let (shown, total) = (delegate.shown(), delegate.total());
        let count = if shown == total {
            format!("{total} rows")
        } else {
            format!("{shown} of {total}")
        };
        if *table.count_label != *count {
            table.count_label = count.into();
        }
        table.status = match delegate.watch_state() {
            WatchState::Ready => None,
            WatchState::Initializing => Some("Loading…".into()),
            WatchState::Error { message } => {
                Some(format!("Reconnecting: {message}. Rows may be out of date.").into())
            }
        };
        cx.notify();
    }

    fn apply_table_filter(&mut self, cx: &mut Context<Self>) {
        let needle = self.table_filter.read(cx).value();
        if let Some(table) = &self.table {
            table.state.update(cx, |table, cx| {
                table.delegate_mut().set_needle(needle.trim());
                cx.notify();
            });
        }
        self.sync_table(cx);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = &self.table {
            table.state.update(cx, |table, cx| {
                table.delegate_mut().tick();
                cx.notify();
            });
        }
    }

    /// The bar over a ticked selection: how many, and what can be done to them.
    fn render_bulk_bar(&self, table: &OpenTable, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = table.state.read(cx).delegate().checked_count();
        if count == 0 {
            return None;
        }
        let (background, border) = {
            let theme = cx.theme();
            (theme.secondary, theme.border)
        };
        let workload = row_actions::is_workload(&table.resource);
        Some(
            h_flex()
                .flex_none()
                .px_4()
                .py_2()
                .gap_2()
                .items_center()
                .bg(background)
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(format!("{count} selected")),
                )
                .child(div().flex_1())
                .when(workload, |bar| {
                    bar.child(
                        button_icon(
                            "bulk-restart",
                            IconName::RotateCw,
                            "Restart",
                            true,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_bulk(BulkOp::Restart, window, cx);
                        })),
                    )
                })
                .child(
                    button_icon(
                        "bulk-export",
                        IconName::Download,
                        "Export",
                        true,
                        Emphasis::Plain,
                        cx.theme(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.export_checked(window, cx))),
                )
                .child(
                    button_icon(
                        "bulk-delete",
                        IconName::Delete,
                        "Delete",
                        true,
                        Emphasis::Danger,
                        cx.theme(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_bulk(BulkOp::Delete, window, cx);
                    })),
                )
                .child(
                    chip("bulk-clear", "Clear", true, false, cx.theme())
                        .on_click(cx.listener(|this, _, _, cx| this.clear_checked(cx))),
                )
                .into_any_element(),
        )
    }

    fn render_table(&self, table: &OpenTable, cx: &mut Context<Self>) -> impl IntoElement {
        let bulk_bar = self.render_bulk_bar(table, cx);
        let theme = cx.theme();
        let delegate = table.state.read(cx).delegate();
        let empty = delegate.shown() == 0 && matches!(delegate.watch_state(), WatchState::Ready);
        let message = if delegate.total() == 0 {
            table.empty_label.clone()
        } else {
            "No matches.".into()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .child(div().font_semibold().child(table.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(table.count_label.clone()),
                    )
                    .child(div().flex_1())
                    .child(div().w(px(260.)).child(Input::new(&self.table_filter))),
            )
            .children(bulk_bar)
            .when_some(table.status.clone(), |column, text| {
                column.child(
                    div()
                        .px_4()
                        .py_1()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(text),
                )
            })
            .when_some(table.error.clone(), |column, text| {
                column.child(
                    div()
                        .px_4()
                        .py_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(text),
                )
            })
            .child(if empty {
                div()
                    .p_4()
                    .text_color(theme.muted_foreground)
                    .child(message)
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(&table.state).stripe(true))
                    .into_any_element()
            })
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Copy the colours: `Hsla` is `Copy`, and holding `theme` would keep `cx`
        // borrowed while the tiles are rendered.
        let (background, border, muted) = {
            let theme = cx.theme();
            (
                theme.title_bar,
                theme.sidebar_border,
                theme.muted_foreground,
            )
        };
        let mut rail = v_flex()
            .id("rail")
            .w(px(64.))
            .h_full()
            .flex_none()
            .py_3()
            .gap_3()
            .items_center()
            .overflow_y_scroll()
            .bg(background)
            .border_r_1()
            .border_color(border);

        for group in &self.rail {
            let mut column = v_flex().w_full().items_center().gap_2();
            if let Some(label) = &group.label {
                column = column.child(div().text_xs().text_color(muted).child(label.clone()));
            }
            for &index in &group.tiles {
                column = column.child(self.render_tile(index, cx));
            }
            rail = rail.child(column);
        }
        let hover = cx.theme().secondary;
        let open = cx.listener(|this, _, window, cx| this.open_settings(window, cx));
        let add = cx.listener(|this, _, window, cx| this.open_add_cluster(window, cx));
        rail.child(div().flex_1())
            .child(
                div()
                    .id("rail-add")
                    .size(px(36.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_color(muted)
                    .when(self.add_cluster.is_some(), |button| button.bg(hover))
                    .hover(|button| button.bg(hover))
                    .child(Icon::new(IconName::Plus).size_4())
                    .on_click(add),
            )
            .child(
                div()
                    .id("rail-settings")
                    .size(px(36.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_color(muted)
                    .when(self.settings.is_some(), |button| button.bg(hover))
                    .hover(|button| button.bg(hover))
                    .child(Icon::new(IconName::Settings).size_4())
                    .on_click(open),
            )
    }

    fn render_tile(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tile = &self.tiles[index];
        let is_active = self.active == Some(index);

        let (dot, state) = match self.clusters.get(&tile.name).map(|s| &s.status) {
            Some(ClusterStatus::Connected { .. }) => (Some(theme.success), "Connected"),
            Some(ClusterStatus::Connecting | ClusterStatus::Degraded { .. }) => {
                (Some(theme.warning), "Connecting")
            }
            Some(ClusterStatus::Unreachable { .. }) => (Some(theme.danger), "Unreachable"),
            _ => (None, "Not connected"),
        };
        // A cluster's own colour shows whether or not it is selected, so a
        // production tile is recognisable at a glance.
        let outline = match (tile.accent, is_active) {
            (Some(accent), _) => accent,
            (None, true) => theme.primary,
            (None, false) => theme.transparent,
        };
        let tint = tile.accent;
        let (primary, idle, hover, ring) = (
            theme.primary,
            theme.secondary,
            theme.secondary_hover,
            theme.title_bar,
        );
        let tip: SharedString = format!("{} · {state}", tile.display).into();
        let owner = cx.entity().downgrade();
        let connected = self.clusters.contains_key(&tile.name);
        let dragged = DraggedTile {
            name: tile.name.clone(),
            initials: tile.initials.clone(),
        };
        let target = tile.name.clone();
        let name = tile.name.clone();

        div()
            .id(("tile", index))
            .relative()
            .w_full()
            .h(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            // The selection mark on the left edge, as in the Dock and Arc.
            .when(is_active, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(12.))
                        .w(px(3.))
                        .h(px(20.))
                        .rounded_r_full()
                        .bg(primary),
                )
            })
            .child(
                div()
                    .size(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_xl()
                    .when(tint.is_some(), |tile| tile.border_2())
                    .when(tint.is_none(), |tile| tile.border_1())
                    .border_color(outline)
                    .bg(match (tint, is_active) {
                        (Some(accent), true) => accent.opacity(0.32),
                        (Some(accent), false) => accent.opacity(0.14),
                        (None, true) => primary.opacity(0.18),
                        (None, false) => idle,
                    })
                    .hover(|tile| tile.bg(hover))
                    .text_sm()
                    .font_semibold()
                    .child(tile.initials.clone()),
            )
            .when_some(dot, |row, dot| {
                row.child(
                    div()
                        .absolute()
                        .bottom(px(3.))
                        .right(px(10.))
                        .size(px(11.))
                        .rounded_full()
                        .border_2()
                        .border_color(ring)
                        .bg(dot),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_cluster(index, window, cx);
            }))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedTile>(move |row, _, _, _| row.bg(hover))
            .on_drop(cx.listener(move |this, dragged: &DraggedTile, _, cx| {
                this.move_cluster(&dragged.name, &target, cx);
            }))
            .context_menu(move |menu, _, _| {
                // Read the folder only now that the menu opens: a tile is drawn
                // on every frame, and this parses the imported kubeconfigs.
                let removable = cluster_ops::managed()
                    .into_iter()
                    .find(|file| file.contexts.iter().any(|c| *c == *name));
                let owner_for =
                    |act: fn(&mut AppView, usize, &mut Window, &mut Context<AppView>)| {
                        let owner = owner.clone();
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            owner.update(cx, |app, cx| act(app, index, window, cx)).ok();
                        }
                    };
                let menu = menu
                    .item(
                        PopupMenuItem::new("Info")
                            .icon(Icon::new(IconName::Info))
                            .on_click(owner_for(AppView::show_cluster_info)),
                    )
                    .item(
                        PopupMenuItem::new("Edit…")
                            .icon(Icon::new(IconName::Settings2))
                            .on_click(owner_for(AppView::edit_cluster)),
                    )
                    .separator();
                let menu = if connected {
                    menu.item(
                        PopupMenuItem::new("Disconnect")
                            .icon(Icon::new(IconName::Close))
                            .on_click(owner_for(|app, index, _, cx| app.disconnect(index, cx))),
                    )
                } else {
                    menu.item(
                        PopupMenuItem::new("Connect")
                            .icon(Icon::new(IconName::Play))
                            .on_click(owner_for(AppView::select_cluster)),
                    )
                };
                // Only a cluster imported into the app can go: the rest live in the
                // user's own kubeconfig, which this app never edits.
                match &removable {
                    Some(file) => {
                        let (owner, file) = (owner.clone(), file.clone());
                        menu.separator().item(
                            PopupMenuItem::new("Remove…")
                                .icon(Icon::new(IconName::Delete))
                                .on_click(move |_: &ClickEvent, window, cx| {
                                    let file = file.clone();
                                    owner
                                        .update(cx, |app, cx| {
                                            app.confirm_remove_cluster(file, window, cx)
                                        })
                                        .ok();
                                }),
                        )
                    }
                    None => menu.separator().item(
                        PopupMenuItem::new("Remove (in your kubeconfig)")
                            .icon(Icon::new(IconName::Delete))
                            .disabled(true),
                    ),
                }
            })
    }

    fn render_row(&self, index: usize, row: &SidebarRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        match row {
            SidebarRow::Group {
                name,
                count,
                icon,
                open,
            } => {
                let key = name.clone();
                let dragged = DraggedSection(name.clone());
                let hover = theme.sidebar_accent;
                let target = name.clone();
                let chevron = if *open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                h_flex()
                    .id(("group", index))
                    .h(ROW_HEIGHT)
                    .mt_1()
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .hover(|row| row.text_color(theme.foreground))
                    .child(Icon::new(chevron).size_3())
                    .child(Icon::new(*icon).size_3p5())
                    .child(div().flex_1().child(name.clone()))
                    .child(div().font_normal().child(count.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&key, cx)))
                    .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                    .drag_over::<DraggedSection>(move |row, _, _, _| row.bg(hover))
                    .on_drop(cx.listener(move |this, dragged: &DraggedSection, _, cx| {
                        this.move_section(&dragged.0, &target, cx);
                    }))
                    .into_any_element()
            }
            SidebarRow::Resource { key, kind } => {
                let is_selected = self.selected.as_ref() == Some(key);
                let key = key.clone();
                let dragged = DraggedResource {
                    key: key.clone(),
                    kind: kind.clone(),
                };
                let target = key.clone();
                let (primary, hover) = (theme.primary, theme.sidebar_accent);
                div()
                    .id(("resource", index))
                    .h(ROW_HEIGHT)
                    .ml_4()
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    // Selection is the accent colour with white text, as in a
                    // Finder or Mail sidebar.
                    .when(is_selected, |row| {
                        row.bg(primary).text_color(theme.primary_foreground)
                    })
                    .when(!is_selected, |row| row.hover(|row| row.bg(hover)))
                    .gap_2()
                    .child(
                        Icon::new(kind_icon(kind).unwrap_or(IconName::Puzzle))
                            .size_3p5()
                            .flex_none(),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(kind.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_resource(&key, window, cx);
                    }))
                    .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                    .drag_over::<DraggedResource>(move |row, _, _, _| row.bg(hover))
                    .on_drop(cx.listener(move |this, dragged: &DraggedResource, _, cx| {
                        this.move_resource(&dragged.key, &target, cx);
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shell = v_flex()
            .w(px(248.))
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border);

        let has_discovery = self
            .active_state()
            .is_some_and(|state| state.discovery.is_some());
        if !has_discovery {
            let text = match self.active {
                Some(_) => "Connecting…",
                None => "Choose a cluster on the left to browse its resources.",
            };
            return shell.child(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text),
            );
        }

        let body = if self.rows.is_empty() {
            div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No resources match.")
                .into_any_element()
        } else {
            uniform_list(
                "resources",
                self.rows.len(),
                // Only the visible range is built, so a cluster with hundreds
                // of CRDs costs the same per frame as one with a dozen.
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| {
                            let row = this.rows.get(index)?;
                            Some(this.render_row(index, row, cx))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .px_2()
            .into_any_element()
        };

        let (primary, hover, on_primary) = (
            theme.primary,
            theme.sidebar_accent,
            theme.primary_foreground,
        );
        // The pages that are not a resource list, pinned above the sections.
        let pinned = |id: &'static str, icon: IconName, label: &'static str, active: bool| {
            h_flex()
                .id(id)
                .h(px(28.))
                .px_2()
                .gap_2()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .when(active, |row| row.bg(primary).text_color(on_primary))
                .when(!active, |row| row.hover(|row| row.bg(hover)))
                .child(Icon::new(icon).size_4())
                .child(label)
        };
        let open_overview = cx.listener(|this, _, window, cx| this.open_overview(window, cx));
        let open_helm = cx.listener(|this, _, window, cx| this.open_helm(window, cx));
        let open_gitops = cx.listener(|this, _, window, cx| this.open_gitops(window, cx));
        let open_topology = cx.listener(|this, _, window, cx| this.open_topology(window, cx));
        let open_usage = cx.listener(|this, _, window, cx| this.open_usage(window, cx));
        let open_shell = cx.listener(|this, _, window, cx| this.open_shell(window, cx));
        let open_activity = cx.listener(|this, _, window, cx| this.open_activity(window, cx));
        let open_crashes = cx.listener(|this, _, _, cx| this.open_crashes(cx));
        let open_create = cx.listener(|this, _, window, cx| this.open_create(window, cx));
        let open_ports = cx.listener(|this, _, window, cx| this.open_ports(None, window, cx));
        let open_alerts = cx.listener(|this, _, window, cx| this.open_alerts(window, cx));
        let open_security = cx.listener(|this, _, window, cx| this.open_security(window, cx));
        let pinned_rows = v_flex()
            .px_2()
            .pt_3()
            .gap_0p5()
            .child(
                pinned(
                    "overview",
                    IconName::LayoutDashboard,
                    "Overview",
                    self.overview.is_some(),
                )
                .on_click(open_overview),
            )
            .child(
                pinned(
                    "gitops",
                    IconName::GitBranch,
                    "GitOps",
                    self.gitops.is_some(),
                )
                .on_click(open_gitops),
            )
            .child(
                pinned(
                    "security",
                    IconName::ShieldCheck,
                    "Security",
                    self.security.is_some(),
                )
                .on_click(open_security),
            )
            .child(
                pinned("alerts", IconName::Bell, "Alerts", self.alerts.is_some())
                    .on_click(open_alerts),
            )
            .child(
                pinned(
                    "topology",
                    IconName::Network,
                    "Topology",
                    self.topology.is_some(),
                )
                .on_click(open_topology),
            )
            .child(
                pinned("usage", IconName::Gauge, "Usage", self.usage.is_some())
                    .on_click(open_usage),
            )
            .child(
                pinned(
                    "shell",
                    IconName::SquareTerminal,
                    "Shell",
                    self.shell.is_some(),
                )
                .on_click(open_shell),
            )
            .child(
                pinned("create", IconName::Plus, "Create", self.create.is_some())
                    .on_click(open_create),
            )
            .child(
                pinned(
                    "ports",
                    IconName::ArrowLeftRight,
                    "Port forwards",
                    self.ports.is_some(),
                )
                .on_click(open_ports),
            )
            .child(
                pinned(
                    "helm",
                    IconName::ShipWheel,
                    "Helm releases",
                    self.helm.is_some(),
                )
                .on_click(open_helm),
            )
            .child(
                pinned(
                    "activity",
                    IconName::Clock,
                    "Activity",
                    self.activity.is_some(),
                )
                .on_click(open_activity),
            )
            .child(
                pinned(
                    "crashes",
                    IconName::TriangleAlert,
                    "Crash log",
                    self.crashes.is_some(),
                )
                .on_click(open_crashes),
            );

        let pins = self.render_pins(cx);
        shell
            .child(pinned_rows)
            .children(pins)
            .child(div().p_2().child(Input::new(&self.filter)))
            .child(body)
    }

    /// The toolbar across the top. It doubles as the window's drag handle, and
    /// leaves room on the left for the traffic lights.
    fn render_titlebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = window_controls(window, cx);
        let theme = cx.theme();
        let bar = h_flex()
            // On macOS the whole bar drags, since nothing on it needs the pointer
            // for anything else. Elsewhere only the gaps do: see `titlebar_gap`.
            .when(cfg!(target_os = "macos"), |bar| {
                bar.window_control_area(WindowControlArea::Drag)
            })
            .h(px(48.))
            .w_full()
            .flex_none()
            // Room for macOS's traffic lights; elsewhere the buttons sit flush
            // against the right edge, so the bar keeps no padding there.
            .pl(if leaves_room_for_traffic_lights() {
                px(84.)
            } else {
                px(16.)
            })
            .pr(if controls.is_some() { px(0.) } else { px(16.) })
            .gap_3()
            .items_center()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border);

        let (muted, field, field_hover) = (
            theme.muted_foreground,
            theme.secondary,
            theme.secondary_hover,
        );
        // The search field sits in the middle of the bar. It looks like a field
        // so that people find it; it opens the palette.
        let search = h_flex()
            .id("search")
            .w(px(320.))
            .h(px(28.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded_lg()
            .cursor_pointer()
            .bg(field)
            .hover(|search| search.bg(field_hover))
            .text_sm()
            .text_color(muted)
            .child(Icon::new(IconName::Search).size_4())
            .child(div().flex_1().child("Search"))
            .child(div().text_xs().child("⌘K"))
            .tooltip(|window, cx| {
                Tooltip::new("Jump to a resource, cluster or page").build(window, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx)));

        let (Some(index), Some(state)) = (self.active, self.active_state()) else {
            return bar
                .child(div().font_semibold().text_color(muted).child("Kubernaut"))
                .child(titlebar_gap("titlebar-gap-a"))
                .child(search)
                .child(titlebar_gap("titlebar-gap-b"))
                .children(controls);
        };
        let tile = &self.tiles[index];
        let tone = match state.status {
            ClusterStatus::Connected { .. } => theme.success,
            ClusterStatus::Connecting | ClusterStatus::Degraded { .. } => theme.warning,
            ClusterStatus::Unreachable { .. } => theme.danger,
            ClusterStatus::Disconnected => theme.muted_foreground,
        };

        let selected = !state.scope.is_empty();
        let (primary, secondary, secondary_hover) =
            (theme.primary, theme.secondary, theme.secondary_hover);
        let namespace_button = h_flex()
            .id("namespace")
            .h(px(24.))
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .bg(secondary)
            .text_color(if selected { theme.foreground } else { muted })
            .hover(|chip| chip.bg(secondary_hover))
            .child(Icon::new(IconName::Folder).size_3p5())
            .child(if selected {
                "Namespaces"
            } else {
                "All namespaces"
            })
            .child(Icon::new(IconName::ChevronDown).size_3())
            .tooltip(|window, cx| Tooltip::new("Choose which namespaces to show").build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_namespace_picker(window, cx);
            }));
        // One removable pill per chosen namespace, so a wrong pick is one click
        // to undo. More than a couple fold into a count that opens the picker.
        let shown_pills = state.scope.iter().take(MAX_PILLS);
        let hidden_pills = state.scope.len().saturating_sub(MAX_PILLS);
        let mut namespace_pills = h_flex().gap_1().items_center();
        for (index, namespace) in shown_pills.enumerate() {
            let target = namespace.clone();
            namespace_pills = namespace_pills.child(
                h_flex()
                    .id(("namespace-pill", index))
                    .h(px(22.))
                    .pl_2()
                    .pr_1()
                    .gap_1()
                    .items_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_xs()
                    .bg(primary.opacity(0.16))
                    .text_color(primary)
                    .hover(|pill| pill.bg(primary.opacity(0.28)))
                    .tooltip(|window, cx| Tooltip::new("Take this namespace out").build(window, cx))
                    .child(namespace.clone())
                    .child(Icon::new(IconName::Close).size_3())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.change_scope(window, cx, |scope| {
                            namespaces::toggle(scope, &target);
                        });
                    })),
            );
        }
        if hidden_pills > 0 {
            namespace_pills = namespace_pills.child(
                div()
                    .id("namespace-more")
                    .px_1p5()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(primary)
                    .child(format!("+{hidden_pills}"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_namespace_picker(window, cx);
                    })),
            );
        }
        if state.scope.len() > 1 {
            namespace_pills = namespace_pills.child(
                div()
                    .id("namespace-clear")
                    .px_1p5()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|clear| clear.text_color(theme.foreground))
                    .tooltip(|window, cx| {
                        Tooltip::new("Show all namespaces again").build(window, cx)
                    })
                    .child("Clear")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.change_scope(window, cx, Vec::clear);
                    })),
            );
        }

        bar.when_some(tile.accent, |bar, accent| {
            bar.child(div().size(px(10.)).flex_none().rounded_full().bg(accent))
        })
        .child(
            div()
                .text_base()
                .font_semibold()
                .child(tile.display.clone()),
        )
        .child(pill(state.status_label.clone(), tone))
        .child(namespace_button)
        .child(namespace_pills)
        .child(titlebar_gap("titlebar-gap-c"))
        .child(search)
        .child(titlebar_gap("titlebar-gap-d"))
        .child(
            chip("disconnect", "Disconnect", true, false, theme)
                .on_click(cx.listener(move |this, _, _, cx| this.disconnect(index, cx))),
        )
        .when(controls.is_some(), |bar| {
            bar.child(div().w(px(8.)).flex_none())
        })
        .children(controls)
    }

    /// One line along the bottom: what is connected, and how much is shown.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (left, tone) = match self.active_state() {
            Some(state) => (state.status_label.clone(), theme.muted_foreground),
            None => ("No cluster selected".into(), theme.muted_foreground),
        };
        let shown = self.table.as_ref().map(|table| {
            match self.active_state().filter(|state| !state.scope.is_empty()) {
                Some(state) => format!(
                    "{} · {} · {}",
                    namespaces::label(&state.scope),
                    table.title,
                    table.count_label
                ),
                None => format!("{} · {}", table.title, table.count_label),
            }
        });

        h_flex()
            .h(px(24.))
            .w_full()
            .flex_none()
            .px_3()
            .gap_3()
            .items_center()
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .text_xs()
            .text_color(tone)
            .child(left)
            .child(div().flex_1())
            .children(shown)
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let main = v_flex().flex_1().h_full().min_w_0();

        if let Some(settings) = &self.settings {
            return main.child(settings.view.clone());
        }
        if let Some(topology) = &self.topology {
            // The drawer opens beside the graph, as it does for a list.
            return main.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(topology.view.clone())
                    .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
            );
        }
        if let Some(usage) = &self.usage {
            // The drawer opens beside the grid, as it does for a list.
            return main.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(usage.view.clone())
                    .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
            );
        }
        if let Some(shell) = &self.shell {
            return main.child(div().flex_1().min_h_0().min_w_0().child(shell.clone()));
        }
        if let Some(activity) = &self.activity {
            return main.child(activity.clone());
        }
        if let Some(crashes) = &self.crashes {
            return main.child(crashes.clone());
        }
        if let Some(add) = &self.add_cluster {
            return main.child(add.view.clone());
        }

        let (Some(_), Some(state)) = (self.active, self.active_state()) else {
            let muted = theme.muted_foreground;
            let none_yet = self.tiles.is_empty();
            let add = cx.listener(|this, _, window, cx| this.open_add_cluster(window, cx));
            let colours = cx.theme();
            return main.child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .items_center()
                    .justify_center()
                    .text_color(muted)
                    .child(if none_yet {
                        "No clusters yet."
                    } else {
                        "Connect a cluster to see its overview."
                    })
                    .when(none_yet, |column| {
                        column.child(
                            button_icon(
                                "empty-add",
                                IconName::Plus,
                                "Add a cluster",
                                true,
                                Emphasis::Primary,
                                colours,
                            )
                            .on_click(add),
                        )
                    }),
            );
        };

        if let Some(error) = &state.error {
            return main.child(div().p_4().text_color(theme.danger).child(error.clone()));
        }
        if let Some(helm) = &self.helm {
            return main.child(helm.clone());
        }
        if let Some(gitops) = &self.gitops {
            // The drawer opens beside the list, as it does for a table.
            return main.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(gitops.view.clone())
                    .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
            );
        }
        if let Some(create) = &self.create {
            return main.child(create.clone());
        }
        if let Some(ports) = &self.ports {
            return main.child(ports.clone());
        }
        if let Some(alerts) = &self.alerts {
            return main.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(alerts.view.clone())
                    .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
            );
        }
        if let Some(security) = &self.security {
            return main.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(security.view.clone())
                    .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
            );
        }
        if let Some(overview) = &self.overview {
            // The overview reads the whole cluster. Say so when a filter is
            // set, rather than leave the numbers to contradict the toolbar.
            let filtered = !state.scope.is_empty();
            return main
                .when(filtered, |page| {
                    page.child(
                        div()
                            .flex_none()
                            .px_4()
                            .py_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .bg(theme.secondary)
                            .child(
                                "The overview covers the whole cluster. Your namespace \
                                 selection applies to resource lists and Helm.",
                            ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .child(overview.view.clone())
                        .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
                );
        }
        match &self.table {
            Some(table) => main.child(
                // A plain flex row, not `h_flex()`: that centres its children
                // vertically, and the table must stretch to the full height.
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.render_table(table, cx))
                    .when_some(self.render_drawer(cx), |row, drawer| row.child(drawer)),
            ),
            None => main.child(
                div()
                    .p_4()
                    .text_color(theme.muted_foreground)
                    .child("Pick a resource from the sidebar."),
            ),
        }
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .relative()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| {
                this.toggle_palette(window, cx);
            }))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_titlebar(window, cx))
            .child(
                // A plain flex row so the three columns stretch to full height.
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rail(cx))
                    .child(self.render_sidebar(cx))
                    .child(self.render_main(cx)),
            )
            .child(self.render_status_bar(cx))
            .when_some(self.palette.as_ref(), |root, palette| {
                root.child(palette.view.clone())
            })
    }
}

/// Releases a watch subscription when the task that holds it ends, including
/// when the task is aborted.
struct Unsubscribe {
    core: Arc<AppCore>,
    id: SubscriptionId,
}

impl Drop for Unsubscribe {
    fn drop(&mut self) {
        // Retiring a watch spawns a task, which needs a runtime. If there is
        // none the process is exiting and there is nothing left to release.
        if Handle::try_current().is_ok() {
            self.core.watches.unsubscribe(self.id);
        }
    }
}

/// Subscribe to a watch and forward what it produces to the UI.
async fn forward_watch(
    core: Arc<AppCore>,
    cluster: SharedString,
    request: WatchRequest,
    generation: u64,
    tx: mpsc::UnboundedSender<(u64, WatchEvent)>,
) {
    let subscription = match core.clusters.require(&cluster) {
        Ok(handle) => core.watches.subscribe(&handle, request).await,
        Err(err) => Err(err),
    };
    let k8s_core::watch::Subscription {
        id,
        spec,
        initial,
        mut receiver,
    } = match subscription {
        Ok(subscription) => subscription,
        Err(err) => {
            let _ = tx.unbounded_send((generation, WatchEvent::Failed(err.to_string())));
            return;
        }
    };
    let _release = Unsubscribe {
        core: Arc::clone(&core),
        id,
    };

    if tx
        .unbounded_send((generation, WatchEvent::Started { spec, initial }))
        .is_err()
    {
        return;
    }
    loop {
        let batch = match receiver.recv().await {
            Ok(batch) => batch,
            // Fell behind the broadcast buffer: take the whole current state
            // instead, which is a snapshot and so replaces what was missed.
            Err(RecvError::Lagged(_)) => match core.watches.resync(id) {
                Some(snapshot) => Arc::new(snapshot),
                None => continue,
            },
            Err(RecvError::Closed) => return,
        };
        if tx
            .unbounded_send((generation, WatchEvent::Batch(batch)))
            .is_err()
        {
            return;
        }
    }
}

fn status_label(status: &ClusterStatus) -> SharedString {
    match status {
        ClusterStatus::Connected { version } => format!("connected · {version}").into(),
        ClusterStatus::Connecting => "connecting…".into(),
        ClusterStatus::Degraded { reason } => format!("degraded · {reason}").into(),
        ClusterStatus::Unreachable { reason } => format!("unreachable · {reason}").into(),
        ClusterStatus::Disconnected => "disconnected".into(),
    }
}

/// Tiles in the user's saved order, and their workspace grouping.
fn build_tiles(
    contexts: &[ContextEntry],
    preferences: &Preferences,
) -> (Vec<Tile>, Vec<RailGroup>) {
    let mut tiles = Vec::with_capacity(contexts.len());
    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();

    let names: Vec<&str> = contexts.iter().map(|c| c.name.as_str()).collect();
    let arranged = arrange(&names, &preferences.cluster_order);
    for (index, context) in arranged.into_iter().map(|i| &contexts[i]).enumerate() {
        let profile = preferences.profile(&context.name);
        let display = profile
            .display_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| context.name.clone());
        let workspace = profile.workspace.filter(|w| !w.trim().is_empty());

        tiles.push(Tile {
            initials: initials(&display).into(),
            display: display.into(),
            name: context.name.clone().into(),
            accent: profile.colour.as_deref().and_then(parse_colour),
        });
        match groups.iter_mut().find(|(w, _)| *w == workspace) {
            Some((_, members)) => members.push(index),
            None => groups.push((workspace, vec![index])),
        }
    }

    let any_workspace = groups.iter().any(|(w, _)| w.is_some());
    let rail = groups
        .into_iter()
        .map(|(workspace, tiles)| RailGroup {
            label: any_workspace.then(|| {
                workspace
                    .as_deref()
                    .unwrap_or("Ungrouped")
                    .to_uppercase()
                    .into()
            }),
            tiles,
        })
        .collect();
    (tiles, rail)
}

/// One section of the sidebar.
struct VisibleGroup<'a> {
    category: Category,
    open: bool,
    resources: Vec<&'a ResourceDescriptor>,
}

/// The sections and resources the sidebar lists for a filter.
///
/// Only watchable types are listed, grouped by what they are for rather than
/// by API group. Sections with no match are dropped, and a non-empty filter
/// opens every section it matches in, since a search that hid its results
/// behind a collapsed header would look like no results.
fn visible_groups<'a>(
    discovery: &'a DiscoveryCache,
    needle: &str,
    collapsed: &HashSet<SharedString>,
    order: SidebarOrder<'_>,
) -> Vec<VisibleGroup<'a>> {
    let mut sections: BTreeMap<Category, Vec<(usize, &ResourceDescriptor)>> = BTreeMap::new();
    // A kind served at several versions (autoscaling v1 and v2) is one list, and
    // so is a built-in kind served by two groups (`Event` in core and in
    // events.k8s.io): both show the same objects. CRDs of the same kind in
    // different groups are different types and stay apart.
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for resource in discovery
        .groups
        .iter()
        .flat_map(|group| &group.resources)
        .filter(|r| r.watchable)
        .filter(|r| seen.insert((if r.is_crd { &r.group } else { "" }, &r.kind)))
        .filter(|r| {
            needle.is_empty()
                || r.kind.to_lowercase().contains(needle)
                || r.plural.to_lowercase().contains(needle)
                || r.short_names.iter().any(|s| s.to_lowercase() == needle)
        })
    {
        let (category, rank) = categories::classify(&resource.kind, resource.is_crd);
        sections.entry(category).or_default().push((rank, resource));
    }

    // Stable sorts: whatever the user has not placed keeps the default order.
    let place = |list: &[String], name: &str| {
        list.iter()
            .position(|entry| entry == name)
            .unwrap_or(usize::MAX)
    };
    let mut sections: Vec<_> = sections.into_iter().collect();
    sections.sort_by_key(|(category, _)| place(order.sections, category.label()));
    sections
        .into_iter()
        .map(|(category, mut resources)| {
            resources.sort_by(|(ra, a), (rb, b)| {
                place(order.resources, &a.key)
                    .cmp(&place(order.resources, &b.key))
                    .then_with(|| ra.cmp(rb))
                    .then_with(|| a.kind.cmp(&b.kind))
            });
            VisibleGroup {
                category,
                open: !needle.is_empty() || !collapsed.contains(category.label()),
                resources: resources.into_iter().map(|(_, r)| r).collect(),
            }
        })
        .collect()
}

fn build_rows(
    discovery: &DiscoveryCache,
    needle: &str,
    collapsed: &HashSet<SharedString>,
    order: SidebarOrder<'_>,
) -> Vec<SidebarRow> {
    let groups = visible_groups(discovery, needle, collapsed, order);
    let capacity = groups
        .iter()
        .map(|g| 1 + if g.open { g.resources.len() } else { 0 })
        .sum();
    let mut rows = Vec::with_capacity(capacity);
    for group in groups {
        rows.push(SidebarRow::Group {
            name: group.category.label().into(),
            count: group.resources.len().to_string().into(),
            icon: group.category.icon(),
            open: group.open,
        });
        if group.open {
            rows.extend(group.resources.into_iter().map(|r| SidebarRow::Resource {
                key: r.key.clone().into(),
                kind: r.kind.clone().into(),
            }));
        }
    }
    rows
}

/// Every section header in the rows, in order.
fn section_names(rows: &[SidebarRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| match row {
            SidebarRow::Group { name, .. } => Some(name.to_string()),
            SidebarRow::Resource { .. } => None,
        })
        .collect()
}

/// The keys of the section `key` is listed in, in order. Empty when it is not
/// in the rows.
fn section_resources(rows: &[SidebarRow], key: &str) -> Vec<String> {
    let Some(at) = rows
        .iter()
        .position(|row| matches!(row, SidebarRow::Resource { key: k, .. } if k == key))
    else {
        return Vec::new();
    };
    let is_resource = |row: &&SidebarRow| matches!(row, SidebarRow::Resource { .. });
    let start = rows[..at].iter().rev().take_while(is_resource).count();
    rows[at - start..]
        .iter()
        .take_while(is_resource)
        .filter_map(|row| match row {
            SidebarRow::Resource { key, .. } => Some(key.to_string()),
            SidebarRow::Group { .. } => None,
        })
        .collect()
}

/// The tab to show once tab `closed` is gone from `len_before` tabs: the same
/// object as before when it was not the one closed, else its neighbour.
fn active_after_close(active: usize, closed: usize, len_before: usize) -> usize {
    let len_after = len_before.saturating_sub(1);
    if closed < active {
        active - 1
    } else {
        active.min(len_after.saturating_sub(1))
    }
}

/// The tab to drop to make room for a new one: the oldest that is not the one
/// being read.
fn tab_to_evict(active: usize) -> usize {
    usize::from(active == 0)
}

/// Two letters for a tile: the first letters of the first and last words, or
/// the first two characters of a single word.
fn initials(name: &str) -> String {
    let mut words = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty());
    let Some(first) = words.next() else {
        return "?".to_string();
    };
    match words.next_back() {
        Some(last) => first
            .chars()
            .take(1)
            .chain(last.chars().take(1))
            .flat_map(char::to_uppercase)
            .collect(),
        None => first.chars().take(2).flat_map(char::to_uppercase).collect(),
    }
}

fn parse_colour(value: &str) -> Option<Hsla> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16)
        .ok()
        .map(|rgb_value| rgb(rgb_value).into())
}

#[cfg(test)]
mod tests {
    // Explicit, not `super::*`: that would glob-import GPUI's `#[test]` macro
    // and shadow Rust's own.
    use super::{
        SidebarOrder, SidebarRow, active_after_close, build_rows, build_tiles, initials,
        parse_colour, section_names, section_resources, tab_to_evict, visible_groups,
    };
    use crate::categories::Category;
    use gpui_kit::SharedString;
    use k8s_app::preferences::{ClusterProfile, Preferences};
    use k8s_core::{ContextEntry, DiscoveryCache};
    use std::collections::HashSet;

    #[test]
    fn initials_use_first_and_last_word() {
        assert_eq!(initials("k3d-kubernaut-test"), "KT");
        assert_eq!(initials("prod"), "PR");
        assert_eq!(initials("a"), "A");
        assert_eq!(initials("---"), "?");
    }

    #[test]
    fn closing_a_tab_keeps_the_one_being_read_or_moves_to_its_neighbour() {
        // [a b c d], reading c (2).
        assert_eq!(active_after_close(2, 0, 4), 1, "left of it: it shifts down");
        assert_eq!(active_after_close(2, 3, 4), 2, "right of it: unchanged");
        assert_eq!(
            active_after_close(2, 2, 4),
            2,
            "itself: the one that follows"
        );
        // The last tab closed while reading it: the one before.
        assert_eq!(active_after_close(3, 3, 4), 2);
        // The only tab.
        assert_eq!(active_after_close(0, 0, 1), 0);
    }

    #[test]
    fn making_room_never_drops_the_tab_being_read() {
        assert_eq!(tab_to_evict(0), 1);
        assert_eq!(tab_to_evict(3), 0);
    }

    #[test]
    fn colours_parse_only_full_hex() {
        assert!(parse_colour("#3b82f6").is_some());
        assert!(parse_colour("3b82f6").is_none());
        assert!(parse_colour("#fff").is_none());
        assert!(parse_colour("#zzzzzz").is_none());
    }

    fn discovery() -> DiscoveryCache {
        let resource = |kind: &str, plural: &str, watchable: bool, is_crd: bool| {
            serde_json::json!({
                "key": format!("x/v1/{plural}"), "group": "", "version": "v1",
                "kind": kind, "plural": plural, "apiVersion": "v1",
                "namespaced": true, "verbs": [], "isCrd": is_crd,
                "watchable": watchable, "editable": true, "deletable": true,
            })
        };
        serde_json::from_value(serde_json::json!({
            "cluster": "test",
            "groups": [
                {"name": "", "preferredVersion": "v1", "resources": [
                    resource("PodTemplate", "podtemplates", true, false),
                    resource("Pod", "pods", true, false),
                    resource("Binding", "bindings", false, false),
                    resource("ConfigMap", "configmaps", true, false),
                ]},
                {"name": "apps", "preferredVersion": "v1", "resources": [
                    resource("Deployment", "deployments", true, false),
                ]},
                {"name": "cert-manager.io", "preferredVersion": "v1", "resources": [
                    resource("Certificate", "certificates", true, true),
                ]},
            ],
            "fetchedAt": "2026-01-01T00:00:00Z",
            "crdMetadataAvailable": true,
        }))
        .unwrap()
    }

    #[test]
    fn a_built_in_kind_served_by_two_groups_is_listed_once() {
        let resource = |group: &str, is_crd: bool| {
            serde_json::json!({
                "key": format!("{group}/v1/events"), "group": group, "version": "v1",
                "kind": "Event", "plural": "events", "apiVersion": "v1",
                "namespaced": true, "verbs": [], "isCrd": is_crd,
                "watchable": true, "editable": true, "deletable": true,
            })
        };
        let cache: DiscoveryCache = serde_json::from_value(serde_json::json!({
            "cluster": "test",
            "groups": [
                {"name": "", "preferredVersion": "v1", "resources": [resource("", false)]},
                {"name": "events.k8s.io", "preferredVersion": "v1",
                 "resources": [resource("events.k8s.io", false)]},
                {"name": "a.example", "preferredVersion": "v1", "resources": [resource("a.example", true)]},
                {"name": "b.example", "preferredVersion": "v1", "resources": [resource("b.example", true)]},
            ],
            "fetchedAt": "2026-01-01T00:00:00Z",
            "crdMetadataAvailable": true,
        }))
        .unwrap();
        let groups = visible_groups(&cache, "", &HashSet::new(), SidebarOrder::default());
        let listed: Vec<(&str, bool)> = groups
            .iter()
            .flat_map(|g| &g.resources)
            .map(|r| (&*r.group, r.is_crd))
            .collect();
        assert_eq!(listed.iter().filter(|(_, crd)| !crd).count(), 1);
        assert_eq!(listed[0].0, "", "the core one is kept");
        assert_eq!(listed.iter().filter(|(_, crd)| *crd).count(), 2);
    }

    #[test]
    fn sidebar_groups_types_by_purpose_not_by_api_group() {
        let cache = discovery();
        let groups = visible_groups(&cache, "", &HashSet::new(), SidebarOrder::default());

        let sections: Vec<_> = groups.iter().map(|g| g.category).collect();
        assert_eq!(
            sections,
            [Category::Workloads, Category::Config, Category::Custom]
        );
        // Pod and Deployment come from different API groups but share a
        // section, most-used first; Binding is not watchable.
        let kinds: Vec<_> = groups[0].resources.iter().map(|r| &*r.kind).collect();
        assert_eq!(kinds, ["Pod", "Deployment", "PodTemplate"]);
    }

    #[test]
    fn filter_matches_kind_or_plural_and_drops_sections_without_a_match() {
        let cache = discovery();
        let groups = visible_groups(&cache, "deploy", &HashSet::new(), SidebarOrder::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].category, Category::Workloads);

        // Plural, not just kind.
        let groups = visible_groups(
            &cache,
            "configmaps",
            &HashSet::new(),
            SidebarOrder::default(),
        );
        assert_eq!(groups[0].resources.len(), 1);
    }

    #[test]
    fn collapsed_sections_stay_closed_until_a_search_matches_inside_them() {
        let cache = discovery();
        let collapsed: HashSet<SharedString> = ["Workloads".into()].into();

        let groups = visible_groups(&cache, "", &collapsed, SidebarOrder::default());
        assert!(!groups[0].open);
        assert!(groups[1].open);

        let groups = visible_groups(&cache, "pod", &collapsed, SidebarOrder::default());
        assert!(groups[0].open, "a search must not hide its own results");
    }

    #[test]
    fn rows_are_flat_and_a_collapsed_section_contributes_only_its_header() {
        let cache = discovery();
        let open = build_rows(&cache, "", &HashSet::new(), SidebarOrder::default());
        // Workloads header + 3, Config header + 1, Custom header + 1.
        assert_eq!(open.len(), 8);

        let collapsed: HashSet<SharedString> = ["Workloads".into()].into();
        let rows = build_rows(&cache, "", &collapsed, SidebarOrder::default());
        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[0], SidebarRow::Group { open: false, .. }));
        assert!(matches!(rows[1], SidebarRow::Group { open: true, .. }));
        assert!(matches!(rows[2], SidebarRow::Resource { .. }));
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_saved_order_moves_sections_and_resources_and_the_rest_keep_their_place() {
        let cache = discovery();
        let sections = strings(&["Custom resources"]);
        let resources = strings(&["x/v1/deployments", "x/v1/pods"]);
        let order = SidebarOrder {
            sections: &sections,
            resources: &resources,
        };
        let groups = visible_groups(&cache, "", &HashSet::new(), order);
        assert_eq!(groups[0].category, Category::Custom);
        assert_eq!(groups[1].category, Category::Workloads);
        assert_eq!(groups[2].category, Category::Config);
        let kinds: Vec<_> = groups[1].resources.iter().map(|r| &*r.kind).collect();
        assert_eq!(kinds, ["Deployment", "Pod", "PodTemplate"]);
    }

    #[test]
    fn the_rows_say_which_names_a_drag_can_reorder() {
        let rows = build_rows(&discovery(), "", &HashSet::new(), SidebarOrder::default());
        assert_eq!(
            section_names(&rows),
            ["Workloads", "Config", "Custom resources"]
        );
        let run = section_resources(&rows, "x/v1/deployments");
        assert_eq!(run.len(), 3, "only the section the row is in: {run:?}");
        assert!(section_resources(&rows, "nothing").is_empty());
    }

    fn context(name: &str) -> ContextEntry {
        ContextEntry {
            name: name.into(),
            cluster: name.into(),
            user: "u".into(),
            namespace: None,
            server: None,
            is_current: false,
            exec_command: None,
            missing_exec_plugin: false,
        }
    }

    #[test]
    fn tiles_group_by_workspace_and_only_draw_dividers_when_one_is_set() {
        let contexts = [context("a"), context("b"), context("c")];

        // No workspace anywhere: a single group with no divider.
        let (_, rail) = build_tiles(&contexts, &Preferences::default());
        assert_eq!(rail.len(), 1);
        assert!(rail[0].label.is_none());

        let mut preferences = Preferences::default();
        preferences.cluster_profiles.insert(
            "b".into(),
            ClusterProfile {
                workspace: Some("dev".into()),
                display_name: Some("Bee Hive".into()),
                ..Default::default()
            },
        );
        let (tiles, rail) = build_tiles(&contexts, &preferences);
        assert_eq!(rail.len(), 2);
        assert_eq!(rail[0].label.as_deref(), Some("UNGROUPED"));
        assert_eq!(rail[0].tiles, [0, 2]);
        assert_eq!(rail[1].label.as_deref(), Some("DEV"));
        assert_eq!(&*tiles[1].display, "Bee Hive");
        assert_eq!(&*tiles[1].initials, "BH");
        assert_eq!(&*tiles[1].name, "b", "the context name stays the id");
    }
}
