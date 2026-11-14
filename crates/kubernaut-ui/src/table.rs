//! The resource table: a `TableDelegate` over [`RowStore`].
//!
//! The delegate only adapts. What a batch does to the rows, and how they are
//! filtered and sorted, lives in `rows.rs` where it is tested without a window.

use gpui_kit::{
    component::{
        ActiveTheme, Theme,
        checkbox::Checkbox,
        h_flex,
        menu::PopupMenu,
        table::{Column, ColumnSort, TableDelegate, TableState},
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::present::{self, Tone};
use k8s_core::{ColumnSpec, RowHealth, TableSpec, WatchBatch, WatchState};
use k8s_metrics::NodeSummary;

use crate::{
    app::AppView,
    drawer::Target,
    metrics::{bytes, cores},
    row_actions,
    rows::{RowStore, Sort, SortKey, StoredRow},
};

use std::collections::{HashMap, HashSet};

/// What a displayed column shows.
#[derive(Clone, Copy)]
enum Field {
    /// The selection checkbox.
    Check,
    Name,
    /// What a node uses, from the metrics sampler. Only on the node list.
    NodeUsage(NodeMetric),
    Namespace,
    /// Index into the watch's columns.
    Cell(usize),
    Age,
}

impl Field {
    fn sort_key(self) -> Option<SortKey> {
        Some(match self {
            Self::Check | Self::NodeUsage(_) => return None,
            Self::Name => SortKey::Name,
            Self::Namespace => SortKey::Namespace,
            Self::Cell(column) => SortKey::Cell(column),
            Self::Age => SortKey::Age,
        })
    }
}

/// The figures a node row shows beside its object columns.
#[derive(Clone, Copy)]
enum NodeMetric {
    Cpu,
    Memory,
    Pods,
    Disk,
}

/// What a node cell says, and how full it is, for colouring. Figures are
/// against what the node can allocate, which is what pods can actually get.
/// `None` for the ratio when there is nothing honest to compare.
fn node_cell(summary: &NodeSummary, metric: NodeMetric) -> (String, Option<f64>) {
    let ratio = |used: f64, total: f64| (total > 0.0).then(|| used / total);
    let with_percent = |text: String, ratio: Option<f64>| match ratio {
        Some(ratio) => (format!("{text} · {:.0}%", ratio * 100.0), Some(ratio)),
        None => (text, None),
    };
    match metric {
        NodeMetric::Cpu if summary.usage_available => with_percent(
            format!(
                "{} / {}",
                cores(summary.cpu_usage),
                cores(summary.cpu_allocatable)
            ),
            ratio(summary.cpu_usage, summary.cpu_allocatable),
        ),
        NodeMetric::Memory if summary.usage_available => with_percent(
            format!(
                "{} / {}",
                bytes(summary.memory_usage),
                bytes(summary.memory_allocatable)
            ),
            ratio(summary.memory_usage, summary.memory_allocatable),
        ),
        NodeMetric::Pods => with_percent(
            format!("{:.0} / {:.0}", summary.pods_used, summary.pods_allocatable),
            ratio(summary.pods_used, summary.pods_allocatable),
        ),
        NodeMetric::Disk if summary.disk_available => with_percent(
            format!(
                "{} / {}",
                bytes(summary.disk_used),
                bytes(summary.disk_capacity)
            ),
            ratio(summary.disk_used, summary.disk_capacity),
        ),
        // Not measured, which is not the same as zero.
        _ => ("—".to_string(), None),
    }
}

/// Where a row's right-click menu sends its choice, and what the rows are.
pub struct RowContext {
    pub core: std::sync::Arc<k8s_app::AppCore>,
    pub owner: WeakEntity<AppView>,
    pub cluster: SharedString,
    /// `group/version/plural`.
    pub resource: SharedString,
    pub kind: SharedString,
    /// Namespaces to keep rows for when the watch covers more than they do.
    pub scope: Vec<SharedString>,
}

pub struct ResourceDelegate {
    context: RowContext,
    store: RowStore,
    /// Every column the watch reports; cells are aligned with these.
    columns: Vec<ColumnSpec>,
    /// The columns shown: name, namespace when the kind has one, the watch's
    /// priority-0 columns, then age.
    fields: Vec<Field>,
    /// Indices into `columns` of the shown ones, for the filter.
    shown_cells: Vec<usize>,
    sort: Option<Sort>,
    needle: String,
    selected: Option<SharedString>,
    /// Rows ticked for a bulk action, by uid so a sort or a new batch does not
    /// lose them. Only the ones still in view are acted on.
    checked: HashSet<SharedString>,
    /// Live figures per node, by name, when this is the node list.
    node_usage: HashMap<SharedString, NodeSummary>,
    watch_state: WatchState,
}

impl ResourceDelegate {
    pub fn new(context: RowContext) -> Self {
        Self {
            store: RowStore::with_scope(context.scope.clone()),
            context,
            columns: Vec::new(),
            fields: Vec::new(),
            shown_cells: Vec::new(),
            sort: None,
            needle: String::new(),
            selected: None,
            checked: HashSet::new(),
            node_usage: HashMap::new(),
            watch_state: WatchState::Initializing,
        }
    }

    /// Learn the columns, once the watch has reported them.
    pub fn start(&mut self, spec: &TableSpec) {
        self.columns.clone_from(&spec.columns);
        self.shown_cells = spec
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.priority == 0)
            .map(|(at, _)| at)
            .collect();

        self.fields.clear();
        self.fields.reserve(self.shown_cells.len() + 4);
        self.fields.push(Field::Check);
        self.fields.push(Field::Name);
        if spec.namespaced {
            self.fields.push(Field::Namespace);
        }
        let is_nodes = &*self.context.resource == "core/v1/nodes";
        for &at in &self.shown_cells {
            self.fields.push(Field::Cell(at));
            // Right after the status, so what a node uses is on screen with
            // whether it is ready rather than past the version and the OS.
            if is_nodes
                && spec
                    .columns
                    .get(at)
                    .is_some_and(|column| column.name == "Status")
            {
                self.fields.extend(
                    [
                        NodeMetric::Cpu,
                        NodeMetric::Memory,
                        NodeMetric::Pods,
                        NodeMetric::Disk,
                    ]
                    .map(Field::NodeUsage),
                );
            }
        }
        self.fields.push(Field::Age);
    }

    /// Apply a batch from the watch. Returns whether anything on screen moved.
    pub fn apply(&mut self, batch: &WatchBatch) -> bool {
        // The state belongs to the newest batch even when its rows are stale.
        let state_changed = self.watch_state != batch.state;
        self.watch_state.clone_from(&batch.state);
        let rows_changed = self.store.apply(batch, &self.columns, present::now());
        if rows_changed {
            self.rebuild();
        }
        rows_changed || state_changed
    }

    pub fn set_needle(&mut self, needle: &str) {
        if self.needle != needle {
            self.needle.clear();
            self.needle.push_str(needle);
            self.rebuild();
        }
    }

    /// Re-age every row.
    pub fn tick(&mut self) {
        self.store.refresh_ages(present::now());
    }

    pub fn select(&mut self, view_ix: usize) {
        self.selected = self.store.row(view_ix).map(|row| row.uid.clone());
    }

    /// The name and namespace of the row at a position in the current view.
    pub fn target_at(&self, view_ix: usize) -> Option<(SharedString, Option<SharedString>)> {
        let row = self.store.row(view_ix)?;
        let namespace = (!row.namespace.is_empty()).then(|| row.namespace.clone());
        Some((row.name.clone(), namespace))
    }

    /// The rows in view, in order.
    fn visible(&self) -> impl Iterator<Item = &StoredRow> + '_ {
        (0..self.store.len()).filter_map(|view_ix| self.store.row(view_ix))
    }

    /// How many ticked rows are in view. A row a filter hides is not counted,
    /// and is not acted on: a bulk delete must touch only what is on screen.
    pub fn checked_count(&self) -> usize {
        self.visible()
            .filter(|row| self.checked.contains(&row.uid))
            .count()
    }

    fn all_visible_checked(&self) -> bool {
        self.store.len() > 0 && self.visible().all(|row| self.checked.contains(&row.uid))
    }

    fn toggle_checked(&mut self, uid: &SharedString) {
        if !self.checked.remove(uid) {
            self.checked.insert(uid.clone());
        }
    }

    /// Tick every row in view, or untick them when they all are already.
    fn toggle_all(&mut self) {
        if self.all_visible_checked() {
            let visible: Vec<SharedString> = self.visible().map(|row| row.uid.clone()).collect();
            for uid in visible {
                self.checked.remove(&uid);
            }
        } else {
            let visible: Vec<SharedString> = self.visible().map(|row| row.uid.clone()).collect();
            self.checked.extend(visible);
        }
    }

    pub fn clear_checked(&mut self) {
        self.checked.clear();
    }

    /// The ticked rows that are in view, as the objects to act on.
    pub fn checked_targets(&self) -> Vec<(SharedString, Option<SharedString>)> {
        self.visible()
            .filter(|row| self.checked.contains(&row.uid))
            .map(|row| {
                let namespace = (!row.namespace.is_empty()).then(|| row.namespace.clone());
                (row.name.clone(), namespace)
            })
            .collect()
    }

    /// Replace the live figures the node list shows.
    pub fn set_node_usage(&mut self, summaries: Vec<NodeSummary>) {
        self.node_usage = summaries
            .into_iter()
            .map(|summary| (summary.name.clone().into(), summary))
            .collect();
    }

    pub fn watch_state(&self) -> &WatchState {
        &self.watch_state
    }

    pub fn shown(&self) -> usize {
        self.store.len()
    }

    pub fn total(&self) -> usize {
        self.store.total()
    }

    fn rebuild(&mut self) {
        self.store
            .rebuild_view(self.sort, &self.needle, &self.shown_cells);
    }

    fn is_selected(&self, row: &StoredRow) -> bool {
        self.selected.as_ref() == Some(&row.uid)
    }
}

fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Ok => theme.success,
        Tone::Pending => theme.info,
        Tone::Warn => theme.warning,
        Tone::Error => theme.danger,
    }
}

fn health_color(health: RowHealth, theme: &Theme) -> Option<Hsla> {
    match health {
        RowHealth::Pending => Some(theme.info),
        RowHealth::Warning => Some(theme.warning),
        RowHealth::Error => Some(theme.danger),
        // A dot on every healthy row would bury the ones that need a look.
        RowHealth::Ok | RowHealth::Unknown => None,
    }
}

/// A checkbox that does not also select the row it sits in: ticking a row for a
/// bulk action must not open its drawer.
fn contained(id: impl Into<ElementId>, checkbox: Checkbox) -> Stateful<Div> {
    div()
        .id(id)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(checkbox)
}

fn is_numeric(kind: &str) -> bool {
    matches!(kind, "integer" | "number")
}

impl TableDelegate for ResourceDelegate {
    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some((name, namespace)) = self.target_at(row_ix) else {
            return menu;
        };
        let target = Target {
            cluster: self.context.cluster.clone(),
            resource: self.context.resource.clone(),
            kind: self.context.kind.clone(),
            namespace,
            name,
        };

        let owner = self.context.owner.clone();
        let pinned = self.context.core.preferences().is_pinned(&target.pin());
        row_actions::build_menu(
            menu,
            &self.context.resource,
            true,
            pinned,
            move |action, window, cx| {
                owner
                    .update(cx, |app, cx| {
                        app.row_action(action, target.clone(), window, cx);
                    })
                    .ok();
            },
        )
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if !matches!(self.fields.get(col_ix), Some(Field::Check)) {
            return div()
                .w_full()
                .child(self.column(col_ix, cx).name)
                .into_any_element();
        }
        let (state, owner) = (cx.entity().downgrade(), self.context.owner.clone());
        contained(
            "check-all-cell",
            Checkbox::new("check-all")
                .checked(self.all_visible_checked())
                .on_click(move |_, _, cx| {
                    state
                        .update(cx, |table, cx| {
                            table.delegate_mut().toggle_all();
                            cx.notify();
                        })
                        .ok();
                    owner.update(cx, |_, cx| cx.notify()).ok();
                }),
        )
        .into_any_element()
    }

    fn columns_count(&self, _: &App) -> usize {
        self.fields.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.store.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        match self.fields.get(col_ix) {
            Some(Field::Check) => Column::new("check", "").width(px(40.)),
            Some(Field::Name) => Column::new("name", "Name").width(px(280.)).sortable(),
            Some(Field::NodeUsage(metric)) => match metric {
                NodeMetric::Cpu => Column::new("node-cpu", "CPU").width(px(170.)),
                NodeMetric::Memory => Column::new("node-memory", "Memory").width(px(190.)),
                NodeMetric::Pods => Column::new("node-pods", "Pods").width(px(110.)),
                NodeMetric::Disk => Column::new("node-disk", "Disk").width(px(180.)),
            },
            Some(Field::Namespace) => Column::new("namespace", "Namespace")
                .width(px(160.))
                .sortable(),
            Some(Field::Cell(at)) => match self.columns.get(*at) {
                Some(spec) => {
                    let column = Column::new(spec.name.clone(), spec.name.clone())
                        .width(px(140.))
                        .sortable();
                    if is_numeric(&spec.kind) {
                        column.text_right()
                    } else {
                        column
                    }
                }
                None => Column::new("unknown", ""),
            },
            Some(Field::Age) => Column::new("age", "Age").width(px(80.)).sortable(),
            None => Column::new("unknown", ""),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let key = self.fields.get(col_ix).copied().and_then(Field::sort_key);
        self.sort = match (key, sort) {
            (Some(key), ColumnSort::Ascending) => Some(Sort {
                key,
                descending: false,
            }),
            (Some(key), ColumnSort::Descending) => Some(Sort {
                key,
                descending: true,
            }),
            _ => None,
        };
        self.rebuild();
        cx.notify();
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let selected = self
            .store
            .row(row_ix)
            .is_some_and(|row| self.is_selected(row));
        let active = cx.theme().list_active;
        div()
            .id(("row", row_ix))
            .when(selected, move |row| row.bg(active))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (Some(row), Some(field)) = (self.store.row(row_ix), self.fields.get(col_ix)) else {
            return div().into_any_element();
        };

        match field {
            Field::Check => {
                let (uid, checked) = (row.uid.clone(), self.checked.contains(&row.uid));
                let (state, owner) = (cx.entity().downgrade(), self.context.owner.clone());
                contained(
                    ("check-cell", row_ix),
                    Checkbox::new(("check", row_ix))
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            state
                                .update(cx, |table, cx| {
                                    table.delegate_mut().toggle_checked(&uid);
                                    cx.notify();
                                })
                                .ok();
                            owner.update(cx, |_, cx| cx.notify()).ok();
                        }),
                )
                .into_any_element()
            }
            Field::Name => h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .when_some(health_color(row.health, theme), |cell, color| {
                    cell.child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                })
                .child(div().truncate().child(row.name.clone()))
                .when(row.terminating, |cell| {
                    cell.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.warning)
                            .child("terminating"),
                    )
                })
                .into_any_element(),
            Field::NodeUsage(metric) => {
                let (text, ratio) = self.node_usage.get(&row.name).map_or_else(
                    || ("…".to_string(), None),
                    |summary| node_cell(summary, *metric),
                );
                // Past nine tenths a node is about to refuse work, and past three
                // quarters it is worth a look; below that the colour stays quiet.
                let color = match ratio {
                    Some(ratio) if ratio >= 0.9 => Some(theme.danger),
                    Some(ratio) if ratio >= 0.75 => Some(theme.warning),
                    _ => None,
                };
                div()
                    .w_full()
                    .truncate()
                    .when_some(color, |cell, color| cell.text_color(color))
                    .child(text)
                    .into_any_element()
            }
            Field::Namespace => div()
                .w_full()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(row.namespace.clone())
                .into_any_element(),
            Field::Cell(at) => {
                let tone = row.tones.get(*at).copied().flatten();
                div()
                    .w_full()
                    .truncate()
                    .when_some(tone, |cell, tone| cell.text_color(tone_color(tone, theme)))
                    .when_some(row.cells.get(*at), |cell, text| cell.child(text.clone()))
                    .into_any_element()
            }
            Field::Age => div()
                .w_full()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(row.age.clone())
                .into_any_element(),
        }
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        let row = self.store.row(row_ix)?;
        match self.fields.get(col_ix)? {
            Field::Check | Field::NodeUsage(_) => None,
            Field::Name => Some(row.name.clone()),
            Field::Namespace => Some(row.namespace.clone()),
            Field::Cell(at) => row.cells.get(*at).cloned(),
            Field::Age => Some(row.age.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NodeMetric, node_cell};
    use k8s_metrics::NodeSummary;

    fn node() -> NodeSummary {
        NodeSummary {
            name: "n1".into(),
            cpu_usage: 1.0,
            cpu_requests: 2.0,
            cpu_allocatable: 4.0,
            cpu_capacity: 4.0,
            memory_usage: 2.0 * 1024.0 * 1024.0 * 1024.0,
            memory_requests: 0.0,
            memory_allocatable: 8.0 * 1024.0 * 1024.0 * 1024.0,
            memory_capacity: 8.0 * 1024.0 * 1024.0 * 1024.0,
            pods_used: 55.0,
            pods_allocatable: 110.0,
            disk_used: 10.0 * 1024.0 * 1024.0 * 1024.0,
            disk_capacity: 40.0 * 1024.0 * 1024.0 * 1024.0,
            image_disk_used: 0.0,
            image_disk_capacity: 0.0,
            usage_available: true,
            disk_available: true,
            os_image: None,
            kernel_version: None,
            container_runtime: None,
            kubelet_version: None,
            architecture: None,
            operating_system: None,
        }
    }

    #[test]
    fn a_node_cell_says_what_is_used_of_what_can_be_allocated() {
        let node = node();
        assert_eq!(
            node_cell(&node, NodeMetric::Cpu),
            ("1.00 / 4.00 · 25%".to_string(), Some(0.25))
        );
        assert_eq!(
            node_cell(&node, NodeMetric::Memory),
            ("2.0 Gi / 8.0 Gi · 25%".to_string(), Some(0.25))
        );
        assert_eq!(
            node_cell(&node, NodeMetric::Pods),
            ("55 / 110 · 50%".to_string(), Some(0.5))
        );
        assert_eq!(
            node_cell(&node, NodeMetric::Disk),
            ("10.0 Gi / 40.0 Gi · 25%".to_string(), Some(0.25))
        );
    }

    #[test]
    fn what_was_not_measured_is_a_dash_not_a_zero() {
        let mut node = node();
        node.usage_available = false;
        node.disk_available = false;
        assert_eq!(node_cell(&node, NodeMetric::Cpu), ("—".to_string(), None));
        assert_eq!(
            node_cell(&node, NodeMetric::Memory),
            ("—".to_string(), None)
        );
        assert_eq!(node_cell(&node, NodeMetric::Disk), ("—".to_string(), None));
        assert!(
            node_cell(&node, NodeMetric::Pods).1.is_some(),
            "pod slots need no metrics"
        );
    }
}
