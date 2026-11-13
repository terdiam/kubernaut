use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
    time::Duration,
};

use futures::{StreamExt, channel::mpsc};
use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        table::{DataTable, TableEvent, TableState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, preferences::Preferences};
use k8s_core::{
    ClusterStatus, ContextEntry, DiscoveryCache, ResourceDescriptor, TableSpec, WatchBatch,
    WatchRequest, WatchState, watch::SubscriptionId,
};
use tokio::{runtime::Handle, sync::broadcast::error::RecvError, task::JoinHandle};

use crate::table::ResourceDelegate;

const ROW_HEIGHT: Pixels = px(28.);

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
}

impl ClusterState {
    fn new(status: ClusterStatus) -> Self {
        Self {
            status_label: status_label(&status),
            status,
            discovery: None,
            error: None,
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
        /// The API group, used as the collapse key.
        name: SharedString,
        label: SharedString,
        count: SharedString,
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
    title: SharedString,
    state: Entity<TableState<ResourceDelegate>>,
    task: JoinHandle<()>,
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
    /// Derived from the active cluster's discovery, the filter and
    /// `collapsed`; rebuilt by `refresh_rows` when one of those changes, never
    /// per frame.
    rows: Vec<SidebarRow>,
    list_scroll: UniformListScrollHandle,
    filter: Entity<InputState>,
    table: Option<OpenTable>,
    next_generation: u64,
    table_filter: Entity<InputState>,
    watch_tx: mpsc::UnboundedSender<(u64, WatchEvent)>,
    status_tx: mpsc::UnboundedSender<(SharedString, ClusterStatus)>,
    _subscriptions: Vec<Subscription>,
}

impl AppView {
    pub fn new(
        core: Arc<AppCore>,
        runtime: Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tiles, rail) = build_tiles(&core.clusters.contexts(), &core.preferences());

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

        Self {
            core,
            runtime,
            tiles,
            rail,
            clusters: HashMap::new(),
            active: None,
            selected: None,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            list_scroll: UniformListScrollHandle::new(),
            filter,
            table: None,
            next_generation: 0,
            table_filter,
            watch_tx,
            status_tx,
            _subscriptions: vec![subscription, table_filter_subscription],
        }
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
            Some(discovery) => build_rows(discovery, &needle, &self.collapsed),
            None => Vec::new(),
        };
        cx.notify();
    }

    fn toggle_group(&mut self, name: &SharedString, cx: &mut Context<Self>) {
        if !self.collapsed.remove(name) {
            self.collapsed.insert(name.clone());
        }
        self.refresh_rows(cx);
    }

    /// Open a cluster tile. Connecting is lazy and explicit, as in the web UI:
    /// nothing is reachable until the user picks it.
    fn select_cluster(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.get(index) else {
            return;
        };
        let name = tile.name.clone();
        self.active = Some(index);
        self.selected = None;
        self.table = None;
        if self.clusters.contains_key(&name) {
            self.refresh_rows(cx);
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
            Ok::<_, String>((handle.status(), discovery))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_connect(&name, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_connect(
        &mut self,
        name: &SharedString,
        result: Result<(ClusterStatus, Arc<DiscoveryCache>), String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.clusters.get_mut(name) {
            match result {
                Ok((status, discovery)) => {
                    state.set_status(status);
                    state.discovery = Some(discovery);
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
    }

    fn disconnect(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.get(index) else {
            return;
        };
        let name = tile.name.clone();
        self.clusters.remove(&name);
        if self.active == Some(index) {
            self.active = None;
            self.selected = None;
            self.table = None;
        }
        self.refresh_rows(cx);

        let core = Arc::clone(&self.core);
        self.runtime
            .spawn(async move { core.disconnect(&name).await });
    }

    /// Open the table for a resource: subscribe to its watch and show rows as
    /// batches arrive.
    fn open_resource(&mut self, key: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        // Replacing the table aborts the previous watch task.
        self.table = None;

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
        let request = WatchRequest {
            resource: key.to_string(),
            namespace: None,
            label_selector: None,
            field_selector: None,
            custom_columns,
        };

        let generation = self.next_generation;
        self.next_generation += 1;

        let state = cx.new(|cx| {
            TableState::new(ResourceDelegate::new(), window, cx)
                .sortable(true)
                .col_resizable(true)
        });
        let events = cx.subscribe(&state, |_, state, event: &TableEvent, cx| {
            if let TableEvent::SelectRow(view_ix) = event {
                let view_ix = *view_ix;
                state.update(cx, |table, cx| {
                    table.delegate_mut().select(view_ix);
                    cx.notify();
                });
            }
        });
        self.table_filter
            .update(cx, |input, cx| input.set_value("", window, cx));

        let task = self.runtime.spawn(forward_watch(
            Arc::clone(&self.core),
            cluster,
            request,
            generation,
            self.watch_tx.clone(),
        ));
        self.table = Some(OpenTable {
            generation,
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

    fn render_table(&self, table: &OpenTable, cx: &mut Context<Self>) -> impl IntoElement {
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
            (theme.sidebar, theme.sidebar_border, theme.muted_foreground)
        };
        let mut rail = v_flex()
            .id("rail")
            .w(px(72.))
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
        rail
    }

    fn render_tile(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tile = &self.tiles[index];
        let is_active = self.active == Some(index);

        let dot = match self.clusters.get(&tile.name).map(|s| &s.status) {
            Some(ClusterStatus::Connected { .. }) => theme.success,
            Some(ClusterStatus::Connecting | ClusterStatus::Degraded { .. }) => theme.warning,
            Some(ClusterStatus::Unreachable { .. }) => theme.danger,
            _ => theme.muted_foreground,
        };
        let border = match (is_active, tile.accent) {
            (true, _) => theme.primary,
            (false, Some(accent)) => accent,
            (false, None) => theme.border,
        };

        div()
            .id(("tile", index))
            .relative()
            .size(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .border_2()
            .border_color(border)
            .bg(if is_active {
                theme.sidebar_accent
            } else {
                theme.secondary
            })
            .cursor_pointer()
            .text_sm()
            .font_semibold()
            .child(tile.initials.clone())
            .child(
                div()
                    .absolute()
                    .bottom(px(2.))
                    .right(px(2.))
                    .size(px(10.))
                    .rounded_full()
                    .bg(dot),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select_cluster(index, cx)))
    }

    fn render_row(&self, index: usize, row: &SidebarRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match row {
            SidebarRow::Group { name, label, count } => {
                let name = name.clone();
                h_flex()
                    .id(("group", index))
                    .h(ROW_HEIGHT)
                    .px_2()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child(label.clone())
                    .child(count.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&name, cx)))
                    .into_any_element()
            }
            SidebarRow::Resource { key, kind } => {
                let is_selected = self.selected.as_ref() == Some(key);
                let key = key.clone();
                div()
                    .id(("resource", index))
                    .h(ROW_HEIGHT)
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .when(is_selected, |row| {
                        row.bg(theme.sidebar_accent).font_semibold()
                    })
                    .hover(|row| row.bg(theme.sidebar_accent))
                    .child(kind.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_resource(&key, window, cx);
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shell = v_flex()
            .w(px(260.))
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border);

        let has_discovery = self
            .active_state()
            .is_some_and(|state| state.discovery.is_some());
        if !has_discovery {
            return shell.child(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Pick a cluster to see its resources."),
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

        shell
            .child(div().p_2().child(Input::new(&self.filter)))
            .child(body)
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let main = v_flex().flex_1().h_full().min_w_0();

        let (Some(index), Some(state)) = (self.active, self.active_state()) else {
            return main.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child("Connect a cluster to see its overview."),
            );
        };
        let tile = &self.tiles[index];

        let tone = match state.status {
            ClusterStatus::Connected { .. } => theme.success,
            ClusterStatus::Connecting | ClusterStatus::Degraded { .. } => theme.warning,
            ClusterStatus::Unreachable { .. } => theme.danger,
            ClusterStatus::Disconnected => theme.muted_foreground,
        };

        let header = h_flex()
            .h(px(52.))
            .flex_none()
            .px_4()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_lg().font_semibold().child(tile.display.clone()))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(tone)
                    .border_1()
                    .border_color(tone)
                    .child(state.status_label.clone()),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("disconnect")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .border_1()
                    .border_color(theme.border)
                    .hover(|b| b.bg(theme.secondary))
                    .child("Disconnect")
                    .on_click(cx.listener(move |this, _, _, cx| this.disconnect(index, cx))),
            );

        if let Some(error) = &state.error {
            return main
                .child(header)
                .child(div().p_4().text_color(theme.danger).child(error.clone()));
        }
        match &self.table {
            Some(table) => main.child(header).child(self.render_table(table, cx)),
            None => main.child(header).child(
                div()
                    .p_4()
                    .text_color(theme.muted_foreground)
                    .child("Pick a resource from the sidebar."),
            ),
        }
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_rail(cx))
            .child(self.render_sidebar(cx))
            .child(self.render_main(cx))
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

/// Tiles and their workspace grouping, in first-appearance order.
fn build_tiles(
    contexts: &[ContextEntry],
    preferences: &Preferences,
) -> (Vec<Tile>, Vec<RailGroup>) {
    let mut tiles = Vec::with_capacity(contexts.len());
    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();

    for (index, context) in contexts.iter().enumerate() {
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

/// One API group as the sidebar shows it.
struct VisibleGroup<'a> {
    name: String,
    open: bool,
    resources: Vec<&'a ResourceDescriptor>,
}

/// The groups and resources the sidebar lists for a filter.
///
/// Only watchable types are listed. Groups with no match are dropped, and a
/// non-empty filter opens every group it matches in, since a search that hid
/// its results behind a collapsed header would look like no results.
fn visible_groups<'a>(
    discovery: &'a DiscoveryCache,
    needle: &str,
    collapsed: &HashSet<SharedString>,
) -> Vec<VisibleGroup<'a>> {
    discovery
        .groups
        .iter()
        .filter_map(|group| {
            let resources: Vec<_> = group
                .resources
                .iter()
                .filter(|r| r.watchable)
                .filter(|r| {
                    needle.is_empty()
                        || r.kind.to_lowercase().contains(needle)
                        || r.plural.to_lowercase().contains(needle)
                })
                .collect();
            if resources.is_empty() {
                return None;
            }
            let name = if group.name.is_empty() {
                "core".to_string()
            } else {
                group.name.clone()
            };
            let open = !needle.is_empty() || !collapsed.contains(name.as_str());
            Some(VisibleGroup {
                name,
                open,
                resources,
            })
        })
        .collect()
}

fn build_rows(
    discovery: &DiscoveryCache,
    needle: &str,
    collapsed: &HashSet<SharedString>,
) -> Vec<SidebarRow> {
    let groups = visible_groups(discovery, needle, collapsed);
    let capacity = groups
        .iter()
        .map(|g| 1 + if g.open { g.resources.len() } else { 0 })
        .sum();
    let mut rows = Vec::with_capacity(capacity);
    for group in groups {
        rows.push(SidebarRow::Group {
            label: group.name.to_uppercase().into(),
            count: group.resources.len().to_string().into(),
            name: group.name.into(),
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
    use super::{SidebarRow, build_rows, build_tiles, initials, parse_colour, visible_groups};
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
    fn colours_parse_only_full_hex() {
        assert!(parse_colour("#3b82f6").is_some());
        assert!(parse_colour("3b82f6").is_none());
        assert!(parse_colour("#fff").is_none());
        assert!(parse_colour("#zzzzzz").is_none());
    }

    fn discovery() -> DiscoveryCache {
        let resource = |kind: &str, plural: &str, watchable: bool| {
            serde_json::json!({
                "key": format!("x/v1/{plural}"), "group": "", "version": "v1",
                "kind": kind, "plural": plural, "apiVersion": "v1",
                "namespaced": true, "verbs": [], "isCrd": false,
                "watchable": watchable, "editable": true, "deletable": true,
            })
        };
        serde_json::from_value(serde_json::json!({
            "cluster": "test",
            "groups": [
                {"name": "", "preferredVersion": "v1", "resources": [
                    resource("Pod", "pods", true),
                    resource("PodTemplate", "podtemplates", true),
                    resource("Binding", "bindings", false),
                ]},
                {"name": "apps", "preferredVersion": "v1", "resources": [
                    resource("Deployment", "deployments", true),
                ]},
            ],
            "fetchedAt": "2026-01-01T00:00:00Z",
            "crdMetadataAvailable": true,
        }))
        .unwrap()
    }

    #[test]
    fn sidebar_lists_watchable_types_and_names_the_empty_group_core() {
        let cache = discovery();
        let groups = visible_groups(&cache, "", &HashSet::new());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "core");
        let kinds: Vec<_> = groups[0].resources.iter().map(|r| &*r.kind).collect();
        assert_eq!(kinds, ["Pod", "PodTemplate"], "Binding is not watchable");
    }

    #[test]
    fn filter_matches_kind_or_plural_and_drops_groups_without_a_match() {
        let cache = discovery();
        let groups = visible_groups(&cache, "deploy", &HashSet::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "apps");

        // Plural, not just kind.
        let groups = visible_groups(&cache, "podtemplates", &HashSet::new());
        assert_eq!(groups[0].resources.len(), 1);
    }

    #[test]
    fn collapsed_groups_stay_closed_until_a_search_matches_inside_them() {
        let cache = discovery();
        let collapsed: HashSet<SharedString> = ["core".into()].into();

        let groups = visible_groups(&cache, "", &collapsed);
        assert!(!groups[0].open);
        assert!(groups[1].open);

        let groups = visible_groups(&cache, "pod", &collapsed);
        assert!(groups[0].open, "a search must not hide its own results");
    }

    #[test]
    fn rows_are_flat_and_a_collapsed_group_contributes_only_its_header() {
        let cache = discovery();
        let open = build_rows(&cache, "", &HashSet::new());
        // core header + Pod + PodTemplate, apps header + Deployment.
        assert_eq!(open.len(), 5);

        let collapsed: HashSet<SharedString> = ["core".into()].into();
        let rows = build_rows(&cache, "", &collapsed);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], SidebarRow::Group { .. }));
        assert!(matches!(rows[1], SidebarRow::Group { .. }));
        assert!(matches!(rows[2], SidebarRow::Resource { .. }));
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
