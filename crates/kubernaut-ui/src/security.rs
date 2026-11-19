//! Security Center: what is misconfigured, and which images carry known CVEs.
//!
//! Two tabs share one list-and-detail layout. *Configuration* scans workload
//! settings and RBAC and needs nothing installed. *Images* reads the Trivy
//! Operator's reports when the cluster has it, or scans with a local `trivy` on
//! request, because that is slow and should never start on its own.

use std::{ops::Range, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    present::Tone,
    security::{Finding, ScanReport, Scanner, Severity, Vulnerability},
    security_ops,
};
use tokio::runtime::Handle;

use crate::{
    cards::{self, Folds},
    drawer::Target,
    namespaces,
    rows::contains_ignore_case,
    ui::{
        Emphasis, Palette, button, button_icon, chip, grid_cell, grid_header, message, page_title,
        pill, segment, segmented_bar, tone_color,
    },
};

const ROW_HEIGHT: Pixels = px(32.);

pub enum SecurityEvent {
    /// Show the object's YAML and events.
    Open(Target),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Configuration,
    Images,
}

fn tone_of(severity: Severity) -> Option<Tone> {
    match severity {
        Severity::Critical | Severity::High => Some(Tone::Error),
        Severity::Medium => Some(Tone::Warn),
        Severity::Low => Some(Tone::Pending),
        Severity::Info => None,
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "Critical",
        Severity::High => "High",
        Severity::Medium => "Medium",
        Severity::Low => "Low",
        Severity::Info => "Info",
    }
}

/// The object a finding is about, so it can be opened.
struct Subject {
    resource: SharedString,
    kind: SharedString,
    namespace: Option<SharedString>,
    name: SharedString,
}

/// One row of either tab. Built once per scan, so drawing only borrows.
struct Entry {
    severity: Severity,
    id: SharedString,
    title: SharedString,
    subject: SharedString,
    namespace: Option<SharedString>,
    message: SharedString,
    remediation: SharedString,
    fields: Vec<(&'static str, SharedString)>,
    open: Option<Subject>,
    builtin: bool,
}

fn entry_of_finding(finding: Finding) -> Entry {
    let mut fields: Vec<(&'static str, SharedString)> = vec![("Kind", finding.kind.clone().into())];
    if let Some(namespace) = &finding.namespace {
        fields.push(("Namespace", namespace.clone().into()));
    }
    if let Some(container) = &finding.container {
        fields.push(("Container", container.clone().into()));
    }
    fields.push(("Check", finding.id.clone().into()));
    Entry {
        severity: finding.severity,
        id: finding.id.into(),
        title: finding.title.into(),
        subject: format!("{} {}", finding.kind, finding.name).into(),
        namespace: finding.namespace.clone().map(Into::into),
        message: finding.message.into(),
        remediation: finding.remediation.into(),
        fields,
        open: Some(Subject {
            resource: finding.resource.into(),
            kind: finding.kind.into(),
            namespace: finding.namespace.map(Into::into),
            name: finding.name.into(),
        }),
        builtin: finding.builtin,
    }
}

fn entry_of_vulnerability(vulnerability: Vulnerability) -> Entry {
    let remediation = match &vulnerability.fixed_version {
        Some(fixed) => format!("Update {} to {fixed} or later.", vulnerability.package),
        None => "No fixed version has been released yet.".to_string(),
    };
    let mut fields: Vec<(&'static str, SharedString)> = vec![
        ("Package", vulnerability.package.clone().into()),
        ("Installed", vulnerability.installed_version.clone().into()),
        (
            "Fixed in",
            vulnerability
                .fixed_version
                .clone()
                .unwrap_or_else(|| "—".into())
                .into(),
        ),
        ("Image", vulnerability.image.into()),
    ];
    if let Some(workload) = &vulnerability.workload {
        fields.push(("Workload", workload.clone().into()));
    }
    if let Some(namespace) = &vulnerability.namespace {
        fields.push(("Namespace", namespace.clone().into()));
    }
    Entry {
        severity: vulnerability.severity,
        id: vulnerability.id.into(),
        title: vulnerability.title.into(),
        subject: format!(
            "{} {}",
            vulnerability.package, vulnerability.installed_version
        )
        .into(),
        namespace: vulnerability.namespace.map(Into::into),
        message: SharedString::default(),
        remediation: remediation.into(),
        fields,
        open: None,
        builtin: false,
    }
}

/// Indices of the entries a filter keeps. Built-in objects are left out unless
/// asked for: a cluster ships hundreds of broad Roles by design, and listing
/// them buries the one that is not.
fn matching(entries: &[Entry], needle: &str, show_builtin: bool) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            (show_builtin || !entry.builtin)
                && [
                    &*entry.id,
                    &*entry.title,
                    &*entry.subject,
                    entry.namespace.as_deref().unwrap_or_default(),
                    severity_label(entry.severity),
                ]
                .into_iter()
                .any(|field| contains_ignore_case(field, needle))
        })
        .map(|(index, _)| index)
        .collect()
}

/// How many of each severity the shown entries hold, worst first.
fn tally(entries: &[Entry], shown: &[usize]) -> [usize; 3] {
    let mut counts = [0; 3];
    for entry in shown.iter().filter_map(|&index| entries.get(index)) {
        match entry.severity {
            Severity::Critical => counts[0] += 1,
            Severity::High => counts[1] += 1,
            Severity::Medium => counts[2] += 1,
            Severity::Low | Severity::Info => {}
        }
    }
    counts
}

/// What the shown findings add up to, for the card above the list. `None` while
/// there is nothing to say yet: not scanned, or nothing matches.
fn security_status(counts: [usize; 3], shown: usize) -> Option<(Tone, String, String)> {
    let [critical, high, medium] = counts;
    if shown == 0 {
        return None;
    }
    let part = |n: usize, word: &str| (n > 0).then(|| format!("{n} {word}"));
    let serious: Vec<String> = [part(critical, "critical"), part(high, "high")]
        .into_iter()
        .flatten()
        .collect();
    // What is not in the label still counts: medium findings and the total.
    let rest: Vec<String> = [
        if serious.is_empty() {
            None
        } else {
            part(medium, "medium")
        },
        Some(format!("{shown} shown")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let detail = rest.join(" · ");
    Some(if !serious.is_empty() {
        (Tone::Error, serious.join(", "), detail)
    } else if medium > 0 {
        (Tone::Warn, format!("{medium} medium"), detail)
    } else {
        (Tone::Ok, "Nothing serious".to_string(), detail)
    })
}

enum Load {
    /// Nothing asked for yet.
    Idle,
    Loading,
    Ready,
    Failed(SharedString),
}

/// One tab's results.
struct Pane {
    load: Load,
    entries: Vec<Entry>,
    shown: Vec<usize>,
    limitations: Vec<SharedString>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
}

impl Pane {
    fn new() -> Self {
        Self {
            load: Load::Idle,
            entries: Vec::new(),
            shown: Vec::new(),
            limitations: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn fill(&mut self, mut entries: Vec<Entry>, limitations: Vec<String>) {
        entries.sort_by_key(|entry| entry.severity);
        self.entries = entries;
        self.limitations = limitations.into_iter().map(Into::into).collect();
        self.selected = None;
        self.load = Load::Ready;
    }
}

pub struct SecurityView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    scope: Vec<SharedString>,
    tab: Tab,
    config: Pane,
    images: Pane,
    scanner: Option<Scanner>,
    /// Whether an image scan has produced results, to tell "no CVEs" from "not
    /// scanned yet".
    scanned: bool,
    show_builtin: bool,
    busy: Option<&'static str>,
    filter: Entity<InputState>,
    _filter: Subscription,
    folds: Folds,
}

impl EventEmitter<SecurityEvent> for SecurityView {}

impl SecurityView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        scope: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
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
            scope,
            tab: Tab::Configuration,
            config: Pane::new(),
            images: Pane::new(),
            scanner: None,
            scanned: false,
            show_builtin: false,
            busy: None,
            filter,
            _filter: subscription,
            folds: Folds::load(core, runtime),
        };
        view.scan_configuration(cx);
        view
    }

    fn pane(&self) -> &Pane {
        match self.tab {
            Tab::Configuration => &self.config,
            Tab::Images => &self.images,
        }
    }

    fn pane_mut(&mut self) -> &mut Pane {
        match self.tab {
            Tab::Configuration => &mut self.config,
            Tab::Images => &mut self.images,
        }
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        if tab == Tab::Images && matches!(self.images.load, Load::Idle) {
            self.scan_images(false, false, cx);
        }
        self.apply_filter(cx);
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter.read(cx).value();
        let show_builtin = self.show_builtin;
        let pane = self.pane_mut();
        pane.shown = matching(&pane.entries, needle.trim(), show_builtin);
        cx.notify();
    }

    /// The server narrows to one namespace; several are read cluster-wide and
    /// narrowed here.
    fn scope(&self) -> (Option<String>, Vec<SharedString>) {
        namespaces::watch_scope(&self.scope)
    }

    fn scan_configuration(&mut self, cx: &mut Context<Self>) {
        self.config.load = Load::Loading;
        let (namespace, scope) = self.scope();
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self.runtime.spawn(async move {
            let mut report = security_ops::findings(&core, &cluster, namespace.as_deref()).await?;
            report
                .findings
                .retain(|finding| namespaces::admits(&scope, finding.namespace.as_deref()));
            Ok::<_, String>(report)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_configuration(result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_configuration(&mut self, result: Result<ScanReport, String>, cx: &mut Context<Self>) {
        match result {
            Ok(report) => {
                let mut limitations = report.limitations;
                if report.examined == 0 && limitations.is_empty() {
                    limitations.push("Nothing was examined: no workloads or RBAC objects.".into());
                }
                self.config.fill(
                    report.findings.into_iter().map(entry_of_finding).collect(),
                    limitations,
                );
            }
            Err(message) => self.config.load = Load::Failed(message.into()),
        }
        self.apply_filter(cx);
    }

    /// Look for a scanner and, when `scan` is set or the operator already has
    /// the answers, read or run the image scan.
    fn scan_images(&mut self, scan: bool, force: bool, cx: &mut Context<Self>) {
        self.images.load = Load::Loading;
        if scan {
            self.busy = Some("Scanning images — this can take minutes…");
        }
        let (namespace, scope) = self.scope();
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self.runtime.spawn(async move {
            let scanner = security_ops::scanner(&core, &cluster).await?;
            let run = scan || matches!(scanner, Scanner::TrivyOperator { .. });
            if !run {
                return Ok::<_, String>((scanner, None));
            }
            let mut result = security_ops::image_scan(
                &core,
                &cluster,
                namespace.as_deref(),
                security_ops::DEFAULT_IMAGE_LIMIT,
                force,
            )
            .await?;
            result
                .vulnerabilities
                .retain(|v| namespaces::admits(&scope, v.namespace.as_deref()));
            Ok((result.scanner.clone(), Some(result)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_images(result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_images(
        &mut self,
        result: Result<(Scanner, Option<security_ops::ImageScan>), String>,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        match result {
            Ok((scanner, scan)) => {
                self.scanner = Some(scanner);
                match scan {
                    Some(scan) => {
                        self.scanned = true;
                        self.images.fill(
                            scan.vulnerabilities
                                .into_iter()
                                .map(entry_of_vulnerability)
                                .collect(),
                            scan.report.limitations,
                        );
                    }
                    None => self.images.fill(Vec::new(), Vec::new()),
                }
            }
            Err(message) => self.images.load = Load::Failed(message.into()),
        }
        self.apply_filter(cx);
    }

    fn download_database(&mut self, cx: &mut Context<Self>) {
        self.busy = Some("Downloading the vulnerability database…");
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self
            .runtime
            .spawn(async move { security_ops::download_database(&core, &cluster).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(scanner) => this.scanner = Some(scanner),
                    Err(reason) => this.images.load = Load::Failed(reason.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_selected(&self, cx: &mut Context<Self>) {
        let Some(subject) = self
            .pane()
            .selected
            .and_then(|index| self.pane().entries.get(index))
            .and_then(|entry| entry.open.as_ref())
        else {
            return;
        };
        cx.emit(SecurityEvent::Open(Target {
            cluster: self.cluster.clone(),
            resource: subject.resource.clone(),
            kind: subject.kind.clone(),
            namespace: subject.namespace.clone(),
            name: subject.name.clone(),
        }));
    }

    /// What the list shows instead of rows, when there are none to show.
    fn placeholder(&self, palette: Palette) -> Option<(SharedString, Hsla)> {
        let pane = self.pane();
        match &pane.load {
            Load::Failed(error) => return Some((error.clone(), palette.danger)),
            Load::Loading => {
                let text = match self.tab {
                    Tab::Configuration => "Scanning workloads and RBAC…",
                    Tab::Images => "Looking for a scanner…",
                };
                return Some((text.into(), palette.muted));
            }
            Load::Idle => return Some(("".into(), palette.muted)),
            Load::Ready => {}
        }
        if self.tab == Tab::Images {
            match &self.scanner {
                Some(Scanner::None { reason }) => {
                    return Some((reason.clone().into(), palette.muted));
                }
                Some(Scanner::TrivyBinary { database_ready, .. }) if !self.scanned => {
                    let text = if *database_ready {
                        "Scan the images running in this cluster with the local trivy. The most \
                         used come first, and results are kept for a day."
                    } else {
                        "trivy is installed, but its vulnerability database is not. Download it \
                         first: about 110 MiB, 1.2 GB on disk."
                    };
                    return Some((text.into(), palette.muted));
                }
                _ => {}
            }
        }
        if !pane.shown.is_empty() {
            return None;
        }
        let text = if pane.entries.is_empty() {
            match self.tab {
                Tab::Configuration => "No findings.",
                Tab::Images => "No vulnerabilities found in the scanned images.",
            }
        } else {
            "Nothing matches."
        };
        Some((text.into(), palette.muted))
    }

    fn render_detail(&self, entry: &Entry, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, border, muted) = (theme.background, theme.border, theme.muted_foreground);
        let color = tone_of(entry.severity).map_or(muted, |tone| tone_color(tone, theme));
        let openable = entry.open.is_some();
        let open = cx.listener(|this, _, _, cx| this.open_selected(cx));
        let card = |id: &'static str,
                    title: &'static str,
                    rows: Vec<AnyElement>,
                    cx: &mut Context<Self>| {
            let folded = self.folds.is_folded(id);
            let toggle = cx.listener(move |this, _, _, cx| {
                this.folds.toggle(id);
                cx.notify();
            });
            cards::card(id, title, Vec::new(), folded, toggle, rows, cx.theme())
        };
        let text_row = |text: SharedString| div().text_sm().child(text).into_any_element();
        let mut cards_column = v_flex().gap_3();
        if !entry.message.is_empty() {
            cards_column = cards_column.child(card(
                "security-problem",
                "What is wrong",
                vec![text_row(entry.message.clone())],
                cx,
            ));
        }
        cards_column = cards_column.child(card(
            "security-fix",
            "How to fix it",
            vec![text_row(entry.remediation.clone())],
            cx,
        ));
        if !entry.fields.is_empty() {
            let rows = entry
                .fields
                .iter()
                .map(|(label, value)| cards::field("security", label, value, 90., cx.theme()))
                .collect();
            cards_column = cards_column.child(card("security-where", "Where", rows, cx));
        }

        v_flex()
            .w(px(460.))
            .h_full()
            .flex_none()
            .bg(background)
            .border_l_1()
            .border_color(border)
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(div().font_semibold().child(entry.title.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(pill(severity_label(entry.severity), color))
                            .child(div().text_xs().text_color(muted).child(entry.id.clone())),
                    ),
            )
            .child(
                v_flex()
                    .id("security-detail")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(cards_column),
            )
            .when(openable, |column| {
                column.child(
                    h_flex()
                        .flex_none()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            button_icon(
                                "security-open",
                                IconName::FileCode,
                                "Open YAML",
                                true,
                                Emphasis::Plain,
                                cx.theme(),
                            )
                            .on_click(open),
                        ),
                )
            })
    }

    fn render_list(
        &self,
        placeholder: Option<(SharedString, Hsla)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some((text, color)) = placeholder {
            return message(text, color).into_any_element();
        }
        uniform_list(
            "security",
            self.pane().shown.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let theme = cx.theme();
                let (selected, hover, muted, line) = (
                    theme.list_active,
                    theme.secondary,
                    theme.muted_foreground,
                    theme.border,
                );
                let pane = this.pane();
                range
                    .filter_map(|position| {
                        let index = *pane.shown.get(position)?;
                        let entry = pane.entries.get(index)?;
                        let color =
                            tone_of(entry.severity).map(|tone| tone_color(tone, cx.theme()));
                        let is_selected = pane.selected == Some(index);
                        Some(
                            h_flex()
                                .id(("security-row", position))
                                .w_full()
                                .h(ROW_HEIGHT)
                                .text_sm()
                                .cursor_pointer()
                                .border_b_1()
                                .border_color(line)
                                .when(is_selected, |row| row.bg(selected))
                                .hover(|row| row.bg(hover))
                                .child(
                                    grid_cell(cx.theme(), Some(96.), false)
                                        .child(
                                            div()
                                                .size(px(8.))
                                                .flex_none()
                                                .rounded_full()
                                                .bg(color.unwrap_or(muted)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .when_some(color, |c, color| c.text_color(color))
                                                .child(severity_label(entry.severity)),
                                        ),
                                )
                                .child(
                                    grid_cell(cx.theme(), None, false)
                                        .child(div().truncate().child(entry.title.clone())),
                                )
                                .child(
                                    grid_cell(cx.theme(), Some(200.), false).child(
                                        div()
                                            .truncate()
                                            .text_color(muted)
                                            .child(entry.subject.clone()),
                                    ),
                                )
                                .child(
                                    grid_cell(cx.theme(), Some(110.), true).child(
                                        div()
                                            .truncate()
                                            .text_color(muted)
                                            .child(entry.namespace.clone().unwrap_or_default()),
                                    ),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pane_mut().selected = Some(index);
                                    cx.notify();
                                })),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.pane().scroll)
        .flex_1()
        .into_any_element()
    }

    /// The second row: what this tab can do, and what it is using.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let idle = self.busy.is_none();
        let row = h_flex().flex_none().px_4().pb_2().gap_2().items_center();

        match self.tab {
            Tab::Configuration => {
                let hidden = self.config.entries.iter().filter(|e| e.builtin).count();
                let rescan = cx.listener(|this, _, _, cx| this.scan_configuration(cx));
                let toggle = cx.listener(|this, _, _, cx| {
                    this.show_builtin = !this.show_builtin;
                    this.apply_filter(cx);
                });
                row.child(
                    button_icon(
                        "security-rescan",
                        IconName::RefreshCw,
                        "Rescan",
                        true,
                        Emphasis::Plain,
                        cx.theme(),
                    )
                    .on_click(rescan),
                )
                .when(hidden > 0, |row| {
                    row.child(
                        chip(
                            "security-builtin",
                            format!("Built-in objects ({hidden})"),
                            true,
                            self.show_builtin,
                            cx.theme(),
                        )
                        .on_click(toggle),
                    )
                })
                .child(div().text_xs().text_color(muted).child(
                    "Built-in Roles are broad on purpose, so they are hidden unless you ask.",
                ))
                .into_any_element()
            }
            Tab::Images => {
                let (status, action): (SharedString, Option<(&'static str, bool)>) =
                    match &self.scanner {
                        Some(Scanner::TrivyOperator { .. }) => (
                            "Trivy Operator reports from this cluster".into(),
                            Some(("Refresh", false)),
                        ),
                        Some(Scanner::TrivyBinary {
                            version,
                            database_ready: true,
                            ..
                        }) => (
                            format!("trivy {version} on this machine").into(),
                            Some((
                                if self.scanned {
                                    "Scan again"
                                } else {
                                    "Scan images"
                                },
                                false,
                            )),
                        ),
                        Some(Scanner::TrivyBinary { version, .. }) => {
                            (format!("trivy {version} on this machine").into(), None)
                        }
                        _ => (SharedString::default(), None),
                    };
                let scan = cx.listener(|this, _, _, cx| this.scan_images(true, false, cx));
                let force = cx.listener(|this, _, _, cx| this.scan_images(true, true, cx));
                let download = cx.listener(|this, _, _, cx| this.download_database(cx));
                let needs_database = matches!(
                    &self.scanner,
                    Some(Scanner::TrivyBinary {
                        database_ready: false,
                        ..
                    })
                );
                let binary_ready = matches!(
                    &self.scanner,
                    Some(Scanner::TrivyBinary {
                        database_ready: true,
                        ..
                    })
                );
                row.when_some(action, |row, (label, _)| {
                    row.child(
                        button_icon(
                            "security-scan",
                            IconName::ScanSearch,
                            label,
                            idle,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(idle, |b| b.on_click(scan)),
                    )
                })
                .when(binary_ready && self.scanned, |row| {
                    row.child(
                        button(
                            "security-force",
                            "Ignore cache",
                            idle,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(idle, |b| b.on_click(force)),
                    )
                })
                .when(needs_database, |row| {
                    row.child(
                        button_icon(
                            "security-database",
                            IconName::Download,
                            "Download database",
                            idle,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(idle, |b| b.on_click(download)),
                    )
                })
                .child(div().text_xs().text_color(muted).child(status))
                .when_some(self.busy, |row, text| {
                    row.child(div().text_xs().text_color(muted).child(text))
                })
                .into_any_element()
            }
        }
    }
}

impl Render for SecurityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let [critical, high, medium] = tally(&self.pane().entries, &self.pane().shown);
        let placeholder = self.placeholder(palette);
        let detail = self
            .pane()
            .selected
            .and_then(|index| self.pane().entries.get(index))
            .map(|entry| self.render_detail(entry, cx).into_any_element());

        let to_config = cx.listener(|this, _, _, cx| this.select_tab(Tab::Configuration, cx));
        let to_images = cx.listener(|this, _, _, cx| this.select_tab(Tab::Images, cx));
        let tabs = segmented_bar(cx.theme())
            .child(
                segment(
                    "security-tab-config",
                    "Configuration",
                    self.tab == Tab::Configuration,
                    cx.theme(),
                )
                .on_click(to_config),
            )
            .child(
                segment(
                    "security-tab-images",
                    "Images",
                    self.tab == Tab::Images,
                    cx.theme(),
                )
                .on_click(to_images),
            );
        let toolbar = self.render_toolbar(cx).into_any_element();
        let has_rows = placeholder.is_none();
        let list = self.render_list(placeholder, cx);

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
                    .child(
                        h_flex()
                            .h(px(48.))
                            .flex_none()
                            .px_4()
                            .gap_3()
                            .items_center()
                            .child(page_title(IconName::ShieldCheck, "Security", cx.theme()))
                            .child(tabs)
                            .child(div().flex_1())
                            .child(div().w(px(220.)).child(Input::new(&self.filter))),
                    )
                    .child(toolbar)
                    .children(
                        security_status([critical, high, medium], self.pane().shown.len()).map(
                            |(tone, label, detail)| {
                                div().px_4().pb_2().child(cards::status_card(
                                    tone,
                                    label,
                                    detail,
                                    None,
                                    cx.theme(),
                                ))
                            },
                        ),
                    )
                    .children(self.pane().limitations.iter().take(3).map(|text| {
                        div()
                            .px_4()
                            .pb_1()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(text.clone())
                    }))
                    .when(has_rows, |column| {
                        column.child(grid_header(
                            cx.theme(),
                            &[
                                ("Severity", Some(96.)),
                                ("Finding", None),
                                ("Object", Some(200.)),
                                ("Namespace", Some(110.)),
                            ],
                        ))
                    })
                    .child(list),
            )
            .children(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Severity, entry_of_finding, entry_of_vulnerability, matching, security_status, tally,
        tone_of,
    };
    use k8s_app::{
        present::Tone,
        security::{Finding, Source, Vulnerability},
    };

    #[test]
    fn the_status_names_what_is_serious_and_says_nothing_when_nothing_is_shown() {
        assert!(security_status([0, 0, 0], 0).is_none());
        let (tone, label, _) = security_status([2, 3, 9], 40).expect("status");
        assert_eq!((tone, label.as_str()), (Tone::Error, "2 critical, 3 high"));
        let (_, _, detail) = security_status([2, 3, 9], 40).expect("status");
        assert_eq!(detail, "9 medium · 40 shown");
        let (tone, label, _) = security_status([0, 0, 4], 40).expect("status");
        assert_eq!((tone, label.as_str()), (Tone::Warn, "4 medium"));
        let (tone, label, detail) = security_status([0, 0, 0], 12).expect("status");
        assert_eq!((tone, label.as_str()), (Tone::Ok, "Nothing serious"));
        assert_eq!(detail, "12 shown");
    }

    fn finding(name: &str, severity: Severity, builtin: bool) -> Finding {
        Finding {
            id: "PSS-PRIVILEGED".into(),
            title: "Privileged container".into(),
            severity,
            source: Source::Posture,
            kind: "Deployment".into(),
            namespace: Some("shop".into()),
            name: name.into(),
            resource: "apps/v1/deployments".into(),
            container: Some("web".into()),
            message: "runs privileged".into(),
            remediation: "drop it".into(),
            builtin,
        }
    }

    fn vulnerability(fixed: Option<&str>) -> Vulnerability {
        Vulnerability {
            id: "CVE-2024-0001".into(),
            severity: Severity::High,
            package: "openssl".into(),
            installed_version: "3.0.1".into(),
            fixed_version: fixed.map(Into::into),
            title: "Buffer overflow".into(),
            image: "nginx:1.25".into(),
            namespace: Some("shop".into()),
            workload: Some("web".into()),
        }
    }

    #[test]
    fn serious_severities_are_errors_and_info_is_neutral() {
        assert_eq!(tone_of(Severity::Critical), Some(Tone::Error));
        assert_eq!(tone_of(Severity::High), Some(Tone::Error));
        assert_eq!(tone_of(Severity::Medium), Some(Tone::Warn));
        assert_eq!(tone_of(Severity::Info), None);
    }

    #[test]
    fn a_finding_names_its_object_and_can_be_opened() {
        let entry = entry_of_finding(finding("web", Severity::High, false));
        assert_eq!(&*entry.subject, "Deployment web");
        let open = entry.open.as_ref().map(|s| (&*s.resource, &*s.name));
        assert_eq!(open, Some(("apps/v1/deployments", "web")));
        assert!(entry.fields.iter().any(|(label, _)| *label == "Container"));
    }

    #[test]
    fn a_vulnerability_says_what_to_update_to_or_that_nothing_exists() {
        let fixed = entry_of_vulnerability(vulnerability(Some("3.0.9")));
        assert_eq!(&*fixed.remediation, "Update openssl to 3.0.9 or later.");
        assert!(fixed.open.is_none(), "an image CVE has no object to open");

        let unfixed = entry_of_vulnerability(vulnerability(None));
        assert!(unfixed.remediation.contains("No fixed version"));
    }

    #[test]
    fn built_in_objects_are_hidden_until_asked_for() {
        let entries = vec![
            entry_of_finding(finding("mine", Severity::High, false)),
            entry_of_finding(finding("system", Severity::High, true)),
        ];
        assert_eq!(matching(&entries, "", false), [0]);
        assert_eq!(matching(&entries, "", true), [0, 1]);
        assert_eq!(matching(&entries, "SYSTEM", true), [1]);
        assert_eq!(matching(&entries, "critical", true), Vec::<usize>::new());
    }

    #[test]
    fn counts_cover_only_what_is_shown() {
        let entries = vec![
            entry_of_finding(finding("a", Severity::Critical, false)),
            entry_of_finding(finding("b", Severity::Medium, false)),
            entry_of_finding(finding("c", Severity::Low, false)),
        ];
        assert_eq!(tally(&entries, &[0, 1, 2]), [1, 0, 1]);
        assert_eq!(tally(&entries, &[1]), [0, 0, 1]);
    }
}
