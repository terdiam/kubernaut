//! The Helm releases list for one cluster.
//!
//! Releases are read from the cluster's own release Secrets, so listing works
//! with no helm binary installed. Only changing a release needs it.

use std::{ops::Range, sync::Arc};

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, present, present::Tone};
use k8s_helm::{Release, store};
use tokio::runtime::Handle;

use crate::{
    release::{ReleaseEvent, ReleaseView, status_tone},
    rows::contains_ignore_case,
    ui::{Palette, chip, message, tone_color},
};

const ROW_HEIGHT: Pixels = px(28.);
const NAMESPACE_WIDTH: Pixels = px(120.);
const CHART_WIDTH: Pixels = px(150.);
const REVISION_WIDTH: Pixels = px(36.);
const STATUS_WIDTH: Pixels = px(104.);
const AGE_WIDTH: Pixels = px(44.);

/// One release as the list draws it. Labels are built once, at load.
struct ReleaseLine {
    name: SharedString,
    namespace: SharedString,
    /// `chart version`.
    chart: SharedString,
    revision: SharedString,
    status: SharedString,
    tone: Option<Tone>,
    age: SharedString,
}

/// Namespace, then name: the order `helm list -A` uses.
fn release_lines(releases: &[Release], now: i64) -> Vec<ReleaseLine> {
    let mut sorted: Vec<&Release> = releases.iter().collect();
    sorted.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));

    let mut age = String::new();
    sorted
        .into_iter()
        .map(|release| {
            age.clear();
            if let Some(updated) = release
                .updated
                .as_deref()
                .and_then(present::parse_timestamp)
            {
                present::write_age(&mut age, now - updated);
            }
            ReleaseLine {
                name: release.name.as_str().into(),
                namespace: release.namespace.as_str().into(),
                chart: format!("{} {}", release.chart, release.chart_version).into(),
                revision: release.revision.to_string().into(),
                status: release.status.as_str().into(),
                tone: status_tone(&release.status),
                age: age.as_str().into(),
            }
        })
        .collect()
}

/// Indices of the lines a filter keeps, in order.
fn matching(lines: &[ReleaseLine], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            [&line.name, &line.namespace, &line.chart, &line.status]
                .into_iter()
                .any(|field| contains_ignore_case(field, needle))
        })
        .map(|(index, _)| index)
        .collect()
}

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

/// The pane for the selected release, and the subscription that hears it.
struct OpenRelease {
    view: Entity<ReleaseView>,
    /// Identifies the row across refreshes, which can reorder the list.
    namespace: SharedString,
    name: SharedString,
    _events: Subscription,
}

pub struct HelmView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    /// The releases as listed, and the raw values the detail pane starts from.
    releases: Vec<Release>,
    lines: Vec<ReleaseLine>,
    shown: Vec<usize>,
    count_label: SharedString,
    load: Load,
    filter: Entity<InputState>,
    open: Option<OpenRelease>,
    scroll: UniformListScrollHandle,
    _filter: Subscription,
}

impl HelmView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter releases"));
        let subscription =
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_filter(cx);
                }
            });
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            releases: Vec::new(),
            lines: Vec::new(),
            shown: Vec::new(),
            count_label: SharedString::default(),
            load: Load::Loading,
            filter,
            open: None,
            scroll: UniformListScrollHandle::new(),
            _filter: subscription,
        };
        view.refresh(cx);
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self.runtime.spawn(async move {
            let handle = core
                .clusters
                .require(&cluster)
                .map_err(|err| err.to_string())?;
            store::list(&handle, None)
                .await
                .map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_refresh(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_refresh(&mut self, result: Result<Vec<Release>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(releases) => {
                self.lines = release_lines(&releases, present::now());
                self.releases = releases;
                self.load = Load::Ready;
                self.apply_filter(cx);
            }
            Err(message) => {
                self.load = Load::Failed(message.into());
                cx.notify();
            }
        }
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter.read(cx).value();
        self.shown = matching(&self.lines, needle.trim());
        let (shown, total) = (self.shown.len(), self.lines.len());
        self.count_label = if shown == total {
            format!("{total} releases").into()
        } else {
            format!("{shown} of {total}").into()
        };
        cx.notify();
    }

    fn open_release(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.lines.get(line) else {
            return;
        };
        let Some(release) = self
            .releases
            .iter()
            .find(|r| *r.namespace == *line.namespace && *r.name == *line.name)
        else {
            return;
        };
        let (core, runtime, cluster) = (&self.core, &self.runtime, self.cluster.clone());
        let view = cx.new(|cx| ReleaseView::new(core, runtime, cluster, release, window, cx));
        let events = cx.subscribe(&view, |this, _, event: &ReleaseEvent, cx| match event {
            ReleaseEvent::Close => {
                this.open = None;
                cx.notify();
            }
            ReleaseEvent::Changed => this.refresh(cx),
        });
        self.open = Some(OpenRelease {
            view,
            namespace: line.namespace.clone(),
            name: line.name.clone(),
            _events: events,
        });
        cx.notify();
    }

    fn is_open(&self, line: &ReleaseLine) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.namespace == line.namespace && open.name == line.name)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let refresh = cx.listener(|this, _, _, cx| this.refresh(cx));
        // With a release open the pane takes half the width, so the columns the
        // pane repeats (chart) or that matter least (age) give way to the name.
        let compact = self.open.is_some();
        let label = |text: &'static str| div().text_xs().text_color(palette.muted).child(text);

        v_flex()
            .flex_none()
            .child(
                h_flex()
                    .h(px(48.))
                    .px_4()
                    .gap_3()
                    .items_center()
                    .child(div().font_semibold().child("Helm releases"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.muted)
                            .child(self.count_label.clone()),
                    )
                    .child(div().flex_1())
                    .child(chip("refresh", "Refresh", true, false, cx.theme()).on_click(refresh))
                    .child(div().w(px(220.)).child(Input::new(&self.filter))),
            )
            .child(
                h_flex()
                    .h(px(24.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(label("Name").flex_1().min_w_0())
                    .child(label("Namespace").w(NAMESPACE_WIDTH).flex_none())
                    .when(!compact, |row| {
                        row.child(label("Chart").w(CHART_WIDTH).flex_none())
                    })
                    .child(label("Rev").w(REVISION_WIDTH).flex_none())
                    .child(label("Status").w(STATUS_WIDTH).flex_none())
                    .when(!compact, |row| {
                        row.child(label("Age").w(AGE_WIDTH).flex_none())
                    }),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = Palette::of(cx);
        match &self.load {
            Load::Loading => message("Loading releases…", palette.muted).into_any_element(),
            Load::Failed(error) => message(error.clone(), palette.danger).into_any_element(),
            Load::Ready if self.lines.is_empty() => {
                message("No Helm releases in this cluster.", palette.muted).into_any_element()
            }
            Load::Ready if self.shown.is_empty() => {
                message("No matches.", palette.muted).into_any_element()
            }
            Load::Ready => uniform_list(
                "releases",
                self.shown.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let theme = cx.theme();
                    let (selected, hover, muted) =
                        (theme.list_active, theme.secondary, theme.muted_foreground);
                    let compact = this.open.is_some();
                    range
                        .filter_map(|position| {
                            let index = *this.shown.get(position)?;
                            let line = this.lines.get(index)?;
                            let color = line.tone.map(|tone| tone_color(tone, cx.theme()));
                            Some(
                                h_flex()
                                    .id(("release", position))
                                    .w_full()
                                    .h(ROW_HEIGHT)
                                    .px_4()
                                    .gap_2()
                                    .items_center()
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(this.is_open(line), |row| row.bg(selected))
                                    .hover(|row| row.bg(hover))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(line.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .w(NAMESPACE_WIDTH)
                                            .flex_none()
                                            .truncate()
                                            .child(line.namespace.clone()),
                                    )
                                    .when(!compact, |row| {
                                        row.child(
                                            div()
                                                .w(CHART_WIDTH)
                                                .flex_none()
                                                .truncate()
                                                .text_color(muted)
                                                .child(line.chart.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .w(REVISION_WIDTH)
                                            .flex_none()
                                            .child(line.revision.clone()),
                                    )
                                    .child(
                                        div()
                                            .w(STATUS_WIDTH)
                                            .flex_none()
                                            .truncate()
                                            .when_some(color, |cell, color| cell.text_color(color))
                                            .child(line.status.clone()),
                                    )
                                    .when(!compact, |row| {
                                        row.child(
                                            div()
                                                .w(AGE_WIDTH)
                                                .flex_none()
                                                .text_color(muted)
                                                .child(line.age.clone()),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_release(index, window, cx);
                                    })),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element(),
        }
    }
}

impl Render for HelmView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A plain flex row, not `h_flex()`: that centres its children
        // vertically, and both panes must stretch to the full height.
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.render_header(cx))
                    .child(self.render_list(cx)),
            )
            .when_some(self.open.as_ref(), |row, open| row.child(open.view.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::{matching, release_lines};
    use k8s_app::present::Tone;
    use k8s_helm::Release;

    fn release(name: &str, namespace: &str, status: &str) -> Release {
        Release {
            name: name.into(),
            namespace: namespace.into(),
            revision: 2,
            status: status.into(),
            chart: "traefik".into(),
            chart_version: "39.0.0".into(),
            app_version: None,
            updated: Some("1970-01-01T00:01:00Z".into()),
            description: None,
            pending: false,
        }
    }

    #[test]
    fn lines_are_ordered_by_namespace_then_name_with_labels_built_once() {
        let releases = [
            release("b", "kube-system", "deployed"),
            release("z", "apps", "failed"),
            release("a", "kube-system", "deployed"),
        ];
        let lines = release_lines(&releases, 120);

        let order: Vec<_> = lines.iter().map(|l| (&*l.namespace, &*l.name)).collect();
        assert_eq!(
            order,
            [("apps", "z"), ("kube-system", "a"), ("kube-system", "b")]
        );
        assert_eq!(&*lines[0].chart, "traefik 39.0.0");
        assert_eq!(&*lines[0].revision, "2");
        assert_eq!(&*lines[0].age, "1m0s");
        assert_eq!(lines[0].tone, Some(Tone::Error));
    }

    #[test]
    fn the_filter_matches_name_namespace_chart_or_status_ignoring_case() {
        let releases = [
            release("api", "prod", "deployed"),
            release("web", "staging", "failed"),
        ];
        let lines = release_lines(&releases, 0);

        assert_eq!(matching(&lines, ""), [0, 1]);
        assert_eq!(matching(&lines, "API"), [0]);
        assert_eq!(matching(&lines, "stag"), [1]);
        assert_eq!(matching(&lines, "FAILED"), [1]);
        assert_eq!(matching(&lines, "traefik"), [0, 1]);
        assert!(matching(&lines, "nothing").is_empty());
    }
}
