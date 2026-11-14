//! Helm chart repositories: add, remove, refresh, and search the charts they
//! offer.
//!
//! Repositories live in the user's own helm configuration, so anything done
//! here is visible to the `helm` command line too, and the other way round.

use std::{ops::Range, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, WindowExt,
        button::ButtonVariant,
        h_flex,
        input::{Input, InputEvent, InputState},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, helm_ops};
use k8s_helm::{ChartResult, Repository};
use tokio::runtime::Handle;

use crate::{
    rows::contains_ignore_case,
    ui::{Emphasis, Palette, button_icon, chip, message, toast_success},
    upgrade::{Mode, Subject, UpgradeEvent, UpgradeView},
};

const ROW_HEIGHT: Pixels = px(40.);

/// Repositories almost everyone has heard of, offered when the list is empty.
/// A click fills the form; nothing is added until the user presses Add.
const POPULAR: [(&str, &str); 4] = [
    (
        "prometheus-community",
        "https://prometheus-community.github.io/helm-charts",
    ),
    (
        "ingress-nginx",
        "https://kubernetes.github.io/ingress-nginx",
    ),
    ("jetstack", "https://charts.jetstack.io"),
    ("grafana", "https://grafana.github.io/helm-charts"),
];

/// One chart as the list and the suggestions draw it.
pub struct ChartLine {
    /// `repo/chart`.
    pub name: SharedString,
    pub version: SharedString,
    pub app_version: SharedString,
    pub description: SharedString,
}

/// By name, which groups a repository's charts together.
pub fn chart_lines(results: &[ChartResult]) -> Vec<ChartLine> {
    let mut lines: Vec<ChartLine> = results
        .iter()
        .map(|chart| ChartLine {
            name: chart.name.as_str().into(),
            version: chart.version.as_str().into(),
            app_version: chart.app_version.as_deref().unwrap_or_default().into(),
            description: chart.description.as_deref().unwrap_or_default().into(),
        })
        .collect();
    lines.sort_by(|a, b| a.name.cmp(&b.name));
    lines
}

/// Indices of the charts whose name or description contain the needle.
fn matching(lines: &[ChartLine], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            contains_ignore_case(&line.name, needle)
                || contains_ignore_case(&line.description, needle)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Charts to offer while a chart reference is being typed: names that start
/// with it first, then names that contain it. Nothing for an empty box or for
/// a reference that is already complete.
pub fn suggest(lines: &[ChartLine], typed: &str, limit: usize) -> Vec<usize> {
    let typed = typed.trim();
    if typed.is_empty() || lines.iter().any(|line| *line.name == *typed) {
        return Vec::new();
    }
    let lower = typed.to_lowercase();
    let mut hits: Vec<(bool, usize)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| contains_ignore_case(&line.name, typed))
        .map(|(index, line)| (!line.name.to_lowercase().starts_with(&lower), index))
        .collect();
    hits.sort();
    hits.into_iter()
        .take(limit)
        .map(|(_, index)| index)
        .collect()
}

/// Whether a name and address could be a repository. Helm checks too; this
/// catches the slips before a network round trip does.
pub fn validate_repo(name: &str, url: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Give the repository a name.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("A name can use letters, digits, - _ and . only.");
    }
    let rest = ["https://", "http://", "oci://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme));
    match rest {
        Some(rest) if !rest.is_empty() => Ok(()),
        _ => Err("The address must start with https://, http:// or oci://."),
    }
}

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

/// What the page tells the Helm view that holds it.
pub enum ChartsEvent {
    /// A chart was installed, so the releases list is out of date.
    Installed,
}

/// The install pane for one chart, and the subscription that hears it finish.
struct OpenInstall {
    chart: SharedString,
    view: Entity<UpgradeView>,
    _events: Subscription,
}

pub struct ChartsView {
    core: Arc<AppCore>,
    runtime: Handle,
    /// Where an install goes, and the namespace it starts with.
    cluster: SharedString,
    namespace: SharedString,
    install: Option<OpenInstall>,
    load: Load,
    repos: Vec<Repository>,
    catalog: Vec<ChartLine>,
    shown: Vec<usize>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    search: Entity<InputState>,
    busy: Option<&'static str>,
    /// The last failure, kept on screen: a toast would fade before it is read.
    error: Option<SharedString>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChartsEvent> for ChartsView {}

impl ChartsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        namespace: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://…"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search charts"));

        let mut subscriptions = Vec::with_capacity(3);
        for input in [&name, &url] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ));
        }
        subscriptions.push(cx.subscribe_in(
            &search,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_filter(cx);
                }
            },
        ));

        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            namespace,
            install: None,
            load: Load::Loading,
            repos: Vec::new(),
            catalog: Vec::new(),
            shown: Vec::new(),
            name,
            url,
            search,
            busy: None,
            error: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        view.reload(cx);
        view
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let task = self.runtime.spawn(async move {
            let repos = helm_ops::list_repos(&core).await?;
            let charts = helm_ops::search_charts(&core, "").await?;
            Ok::<_, String>((repos, charts))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_reload(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_reload(
        &mut self,
        result: Result<(Vec<Repository>, Vec<ChartResult>), String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((mut repos, charts)) => {
                repos.sort_by(|a, b| a.name.cmp(&b.name));
                self.repos = repos;
                self.catalog = chart_lines(&charts);
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
        let needle = self.search.read(cx).value();
        self.shown = matching(&self.catalog, needle.trim());
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        let url = self.url.read(cx).value().trim().to_string();
        if let Err(reason) = validate_repo(&name, &url) {
            self.error = Some(reason.into());
            cx.notify();
            return;
        }
        self.busy = Some("Adding the repository and fetching its charts…");
        self.error = None;
        cx.notify();

        let core = Arc::clone(&self.core);
        let (task_name, task_url) = (name.clone(), url);
        let task = self
            .runtime
            .spawn(async move { helm_ops::add_repo(&core, &task_name, &task_url).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        for input in [&this.name, &this.url] {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        toast_success(window, cx, format!("Added {name}"));
                        this.reload(cx);
                    }
                    Err(reason) => this.error = Some(reason.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_remove(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (owner, name) = (owner.clone(), name.clone());
            dialog
                .title(format!("Remove repository {name}?"))
                .description(
                    "It is also removed from the helm command line, which shares this \
                     configuration. Releases already installed from it are not touched.",
                )
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    owner
                        .update(cx, |this, cx| this.remove(name.clone(), window, cx))
                        .ok();
                    true
                })
        });
    }

    fn remove(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let target = name.clone();
        let task = self
            .runtime
            .spawn(async move { helm_ops::remove_repo(&core, &target).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    toast_success(window, cx, format!("Removed {name}"));
                    this.reload(cx);
                }
                Err(reason) => {
                    this.error = Some(reason.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn update_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Refreshing every repository…");
        self.error = None;
        cx.notify();

        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { helm_ops::update_repos(&core).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(_) => {
                        toast_success(window, cx, "Repositories updated");
                        this.reload(cx);
                    }
                    Err(reason) => this.error = Some(reason.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open the install form for a chart.
    fn open_install(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chart) = self.catalog.get(index) else {
            return;
        };
        let subject = Subject {
            mode: Mode::Install,
            cluster: self.cluster.clone(),
            namespace: self.namespace.clone(),
            release: SharedString::default(),
            chart: chart.name.clone(),
            version: SharedString::default(),
        };
        let title = chart.name.clone();
        let (core, runtime) = (&self.core, &self.runtime);
        let view = cx.new(|cx| UpgradeView::new(core, runtime, subject, "", window, cx));
        let events = cx.subscribe(&view, |this, _, event: &UpgradeEvent, cx| match event {
            UpgradeEvent::Upgraded => {
                this.install = None;
                cx.emit(ChartsEvent::Installed);
                cx.notify();
            }
        });
        self.install = Some(OpenInstall {
            chart: title,
            view,
            _events: events,
        });
        cx.notify();
    }

    fn fill(&mut self, name: &str, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.url
            .update(cx, |input, cx| input.set_value(url, window, cx));
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let ready = self.busy.is_none()
            && !self.name.read(cx).value().trim().is_empty()
            && !self.url.read(cx).value().trim().is_empty();
        let add = cx.listener(|this, _, window, cx| this.add(window, cx));

        v_flex()
            .flex_none()
            .px_4()
            .pb_3()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(170.)).child(Input::new(&self.name)))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.url)))
                    .child(
                        button_icon(
                            "add-repo",
                            IconName::Plus,
                            "Add",
                            ready,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(ready, |b| b.on_click(add)),
                    ),
            )
            .when(self.repos.is_empty(), |form| {
                form.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(div().text_xs().text_color(palette.muted).child("Popular:"))
                        .children(POPULAR.iter().enumerate().map(|(index, (name, url))| {
                            let (name, url) = (*name, *url);
                            chip(("popular", index), name, true, false, cx.theme()).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.fill(name, url, window, cx);
                                }),
                            )
                        })),
                )
            })
            .when_some(self.busy, |form, text| {
                form.child(div().text_xs().text_color(palette.muted).child(text))
            })
            .when_some(self.error.clone(), |form, text| {
                form.child(div().text_xs().text_color(palette.danger).child(text))
            })
    }

    fn render_repos(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let hover = cx.theme().secondary_hover;

        v_flex()
            .id("repos")
            .flex_none()
            .max_h(px(170.))
            .overflow_y_scroll()
            .px_4()
            .children(self.repos.iter().enumerate().map(|(index, repo)| {
                let name: SharedString = repo.name.as_str().into();
                let remove_name = name.clone();
                h_flex()
                    .h(px(30.))
                    .gap_3()
                    .items_center()
                    .text_sm()
                    .child(div().w(px(170.)).flex_none().truncate().child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(palette.muted)
                            .child(repo.url.clone()),
                    )
                    .child(
                        div()
                            .id(("remove-repo", index))
                            .size(px(22.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(palette.muted)
                            .hover(|button| button.bg(hover))
                            .tooltip(|window, cx| {
                                Tooltip::new("Remove this repository").build(window, cx)
                            })
                            .child(Icon::new(IconName::Close).size_3p5())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_remove(remove_name.clone(), window, cx);
                            })),
                    )
            }))
    }

    /// The pane an install form lives in: a title, a close button, the form.
    fn render_install(&self, install: &OpenInstall, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, border, muted, hover) = (
            theme.background,
            theme.border,
            theme.muted_foreground,
            theme.secondary_hover,
        );
        v_flex()
            .w(px(560.))
            .h_full()
            .flex_none()
            .bg(background)
            .border_l_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(52.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().child("Install a chart"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(install.chart.clone()),
                            ),
                    )
                    .child(
                        div()
                            .id("close-install")
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(|button| button.bg(hover))
                            .tooltip(|window, cx| Tooltip::new("Close").build(window, cx))
                            .child(Icon::new(IconName::Close).size_4())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.install = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().pt_3().child(install.view.clone()))
    }

    fn render_catalog(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = Palette::of(cx);
        if self.catalog.is_empty() {
            return message(
                "No charts yet. Add a repository above and its charts appear here.",
                palette.muted,
            )
            .into_any_element();
        }
        if self.shown.is_empty() {
            return message("No charts match.", palette.muted).into_any_element();
        }
        uniform_list(
            "charts",
            self.shown.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let theme = cx.theme();
                let (muted, hover) = (theme.muted_foreground, theme.secondary_hover);
                range
                    .filter_map(|position| {
                        let index = *this.shown.get(position)?;
                        let chart = this.catalog.get(index)?;
                        let name = chart.name.clone();
                        let install = cx.listener(move |this, _, window, cx| {
                            this.open_install(index, window, cx);
                        });
                        let copy = cx.listener(move |_, _, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(name.to_string()));
                            toast_success(window, cx, "Copied the chart reference");
                        });
                        Some(
                            h_flex()
                                .id(("chart", position))
                                .w_full()
                                .h(ROW_HEIGHT)
                                .px_4()
                                .gap_3()
                                .items_center()
                                .hover(|row| row.bg(hover))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .items_center()
                                                .text_sm()
                                                .child(
                                                    div().font_semibold().child(chart.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(chart.version.clone()),
                                                )
                                                .when(!chart.app_version.is_empty(), |row| {
                                                    row.child(
                                                        div().text_xs().text_color(muted).child(
                                                            format!("app {}", chart.app_version),
                                                        ),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .truncate()
                                                .child(chart.description.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(("copy-chart", position))
                                        .size(px(24.))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_color(muted)
                                        .hover(|button| button.bg(hover))
                                        .tooltip(|window, cx| {
                                            Tooltip::new("Copy the chart reference")
                                                .build(window, cx)
                                        })
                                        .child(Icon::new(IconName::Copy).size_3p5())
                                        .on_click(copy),
                                )
                                .child(
                                    button_icon(
                                        ("install-chart", position),
                                        IconName::Download,
                                        "Install",
                                        true,
                                        Emphasis::Primary,
                                        cx.theme(),
                                    )
                                    .flex_none()
                                    .on_click(install),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .into_any_element()
    }
}

impl Render for ChartsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        match &self.load {
            Load::Loading => {
                return message("Reading repositories…", palette.muted).into_any_element();
            }
            Load::Failed(error) => {
                return v_flex()
                    .p_4()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.danger)
                            .child(error.clone()),
                    )
                    .child(div().text_xs().text_color(palette.muted).child(
                        "Repositories and charts need the helm command. Releases are listed \
                         without it.",
                    ))
                    .into_any_element();
            }
            Load::Ready => {}
        }

        let update = cx.listener(|this, _, window, cx| this.update_all(window, cx));
        let idle = self.busy.is_none() && !self.repos.is_empty();
        let heading = |text: &'static str| {
            div()
                .px_4()
                .py_1p5()
                .text_xs()
                .font_semibold()
                .text_color(palette.muted)
                .child(text)
        };

        let page = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .child(div().font_semibold().child("Chart repositories"))
                    .child(div().text_sm().text_color(palette.muted).child(
                        match self.repos.len() {
                            1 => "1 repository".to_string(),
                            count => format!("{count} repositories"),
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        button_icon(
                            "update-repos",
                            IconName::RefreshCw,
                            "Update all",
                            idle,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(idle, |b| b.on_click(update)),
                    ),
            )
            .child(self.render_form(cx))
            .child(self.render_repos(cx))
            .child(
                h_flex()
                    .flex_none()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .gap_3()
                    .items_center()
                    .border_t_1()
                    .border_color(palette.border)
                    .child(heading("Charts").px_0())
                    .child(div().text_xs().text_color(palette.muted).child(format!(
                        "{} of {}",
                        self.shown.len(),
                        self.catalog.len()
                    )))
                    .child(div().flex_1())
                    .child(div().w(px(260.)).child(Input::new(&self.search))),
            )
            .child(self.render_catalog(cx));

        // A plain flex row, not `h_flex()`: that centres its children
        // vertically, and both must stretch to the full height.
        div()
            .flex()
            .size_full()
            .child(page)
            .when_some(self.install.as_ref(), |row, install| {
                row.child(self.render_install(install, cx))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{ChartLine, chart_lines, matching, suggest, validate_repo};
    use k8s_helm::ChartResult;

    fn catalog() -> Vec<ChartLine> {
        let chart = |name: &str, description: &str| ChartResult {
            name: name.into(),
            version: "1.0.0".into(),
            app_version: None,
            description: Some(description.into()),
        };
        chart_lines(&[
            chart("grafana/loki", "Log aggregation"),
            chart("grafana/grafana", "Dashboards"),
            chart("jetstack/cert-manager", "Certificates for Kubernetes"),
            chart("ingress-nginx/ingress-nginx", "Ingress controller"),
        ])
    }

    fn names(lines: &[ChartLine], hits: &[usize]) -> Vec<String> {
        hits.iter().map(|&i| lines[i].name.to_string()).collect()
    }

    #[test]
    fn charts_are_listed_by_name_so_a_repository_stays_together() {
        let lines = catalog();
        let all: Vec<_> = lines.iter().map(|l| l.name.to_string()).collect();
        assert_eq!(
            all,
            [
                "grafana/grafana",
                "grafana/loki",
                "ingress-nginx/ingress-nginx",
                "jetstack/cert-manager",
            ]
        );
    }

    #[test]
    fn search_matches_the_name_or_the_description() {
        let lines = catalog();
        assert_eq!(names(&lines, &matching(&lines, "loki")), ["grafana/loki"]);
        assert_eq!(
            names(&lines, &matching(&lines, "certificates")),
            ["jetstack/cert-manager"],
            "the description counts"
        );
        assert_eq!(matching(&lines, "").len(), 4, "empty keeps everything");
    }

    #[test]
    fn suggestions_put_prefixes_first_and_stop_once_the_reference_is_complete() {
        let lines = catalog();
        // "gra" starts two names, and is inside none of the others.
        assert_eq!(
            names(&lines, &suggest(&lines, "gra", 5)),
            ["grafana/grafana", "grafana/loki"]
        );
        // "nginx" is inside a name, not at its start.
        assert_eq!(
            names(&lines, &suggest(&lines, "nginx", 5)),
            ["ingress-nginx/ingress-nginx"]
        );
        assert!(suggest(&lines, "", 5).is_empty());
        assert!(suggest(&lines, "  ", 5).is_empty());
        assert!(
            suggest(&lines, "grafana/loki", 5).is_empty(),
            "already complete"
        );
        assert_eq!(suggest(&lines, "gra", 1).len(), 1, "the limit holds");
    }

    #[test]
    fn a_repository_needs_a_clean_name_and_a_real_address() {
        assert!(validate_repo("grafana", "https://grafana.github.io/helm-charts").is_ok());
        assert!(validate_repo("my_repo.2", "oci://registry.example.com/charts").is_ok());
        assert!(validate_repo("local", "http://127.0.0.1:8099").is_ok());

        assert!(validate_repo("", "https://x").is_err());
        assert!(validate_repo("has space", "https://x").is_err());
        assert!(validate_repo("a/b", "https://x").is_err());
        assert!(validate_repo("ok", "ftp://x").is_err());
        assert!(validate_repo("ok", "https://").is_err(), "a scheme alone");
        assert!(validate_repo("ok", "charts.example.com").is_err());
    }
}
