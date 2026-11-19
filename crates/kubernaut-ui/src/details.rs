//! The Details tab: an object's identity, make-up, owners and conditions,
//! read from the object itself by `k8s_app::details`.

use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{ActiveTheme, Icon, h_flex, tooltip::Tooltip, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    details::{Details, Group, Headline, condition_tone, describe, group_of, pack},
    object_ops::{self, Connected},
    present::{self, Tone},
};
use k8s_core::objects;
use k8s_ops::related::{EventRow, RelatedRef};
use tokio::runtime::Handle;

use crate::{
    diagnose::kind_of,
    drawer::Target,
    row_actions,
    ui::{message, pill, toast_success, tone_color},
};

/// Events listed here before pointing at the Events tab.
const EVENTS_SHOWN: usize = 8;

/// What this tab asks its drawer to do.
pub enum DetailsEvent {
    /// Another object a row points at, to open in place of this one.
    Open(Target),
}

enum State {
    Loading,
    Ready(Box<Details>),
    Failed(SharedString),
}

/// How often an open Details tab reads the object again, so replicas, restarts
/// and conditions follow the cluster instead of freezing at the first read.
const REFRESH: Duration = Duration::from_secs(4);

pub struct DetailsView {
    state: State,
    /// Pods, services, ingress addresses and events, for the kinds that have
    /// pods behind them. Read on a slower beat than the object itself.
    connected: Option<Box<Connected>>,
    target: Target,
    /// Cards the reader has folded, kept across objects in the preferences.
    collapsed: HashSet<String>,
    core: Arc<AppCore>,
    runtime: Handle,
    /// When this was last drawn. A tab nobody is looking at is not drawn, and
    /// stops reading: with eight tabs open, only the one in view asks.
    drawn: Instant,
}

impl EventEmitter<DetailsEvent> for DetailsView {}

impl DetailsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        cx: &mut Context<Self>,
    ) -> Self {
        let (shared, handle) = (Arc::clone(core), runtime.clone());
        let (core, runtime, own) = (Arc::clone(core), runtime.clone(), target.clone());
        let connects = connects(target);
        cx.spawn(async move |this, cx| {
            let mut round = 0_u32;
            loop {
                let (reader, target) = (Arc::clone(&core), own.clone());
                let task = runtime.spawn(async move { fetch(&reader, &target).await });
                let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
                if this.update(cx, |this, cx| this.apply(result, cx)).is_err() {
                    return;
                }
                // Listing services and ingresses is heavier than reading one
                // object, so the connected view is read every other round.
                if connects && round.is_multiple_of(2) {
                    let (reader, target) = (Arc::clone(&core), own.clone());
                    let task =
                        runtime.spawn(async move { fetch_connected(&reader, &target).await });
                    if let Ok(Ok(connected)) = task.await
                        && this
                            .update(cx, |this, cx| this.apply_connected(connected, cx))
                            .is_err()
                    {
                        return;
                    }
                }
                round = round.wrapping_add(1);
                // Wait until the tab is on screen again before the next read.
                loop {
                    cx.background_executor().timer(REFRESH).await;
                    match this.update(cx, |this, _| this.drawn.elapsed() < REFRESH * 2) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(_) => return,
                    }
                }
            }
        })
        .detach();
        Self {
            state: State::Loading,
            connected: None,
            target: target.clone(),
            collapsed: shared.details_folded().into_iter().collect(),
            core: shared,
            runtime: handle,
            drawn: Instant::now(),
        }
    }

    /// Take a reading of what the object is connected to. An unchanged one does
    /// not redraw.
    fn apply_connected(&mut self, connected: Connected, cx: &mut Context<Self>) {
        if self.connected.as_deref() == Some(&connected) {
            return;
        }
        self.connected = Some(Box::new(connected));
        cx.notify();
    }

    /// Open `related` in place of this object.
    fn open(&self, related: &RelatedRef, cx: &mut Context<Self>) {
        cx.emit(DetailsEvent::Open(Target {
            cluster: self.target.cluster.clone(),
            resource: related.resource.clone().into(),
            kind: kind_of(&related.resource).into(),
            namespace: related.namespace.clone().map(Into::into),
            name: related.name.clone().into(),
        }));
    }

    /// Take a read. A later failure keeps the last good reading rather than
    /// replacing what was shown with an error, and an unchanged one does not
    /// redraw.
    fn apply(&mut self, result: Result<Details, String>, cx: &mut Context<Self>) {
        match (result, &self.state) {
            (Ok(new), State::Ready(old)) if **old == new => return,
            (Ok(new), _) => self.state = State::Ready(Box::new(new)),
            (Err(_), State::Ready(_)) => return,
            (Err(reason), _) => self.state = State::Failed(reason.into()),
        }
        cx.notify();
    }

    /// Whether this is a suspended CronJob, when it is a CronJob that has been
    /// read. Used to offer Suspend or Resume, not both.
    pub fn suspended(&self) -> Option<bool> {
        let State::Ready(details) = &self.state else {
            return None;
        };
        details
            .summary
            .iter()
            .find(|(label, _)| label == "Suspended")
            .map(|(_, value)| value == "true")
    }
}

async fn fetch(core: &AppCore, target: &Target) -> Result<Details, String> {
    let handle = core
        .clusters
        .require(&target.cluster)
        .map_err(|err| err.to_string())?;
    let object = objects::get(
        &handle,
        &target.resource,
        target.namespace.as_deref(),
        &target.name,
    )
    .await
    .map_err(|err| err.to_string())?;
    let value = objects::to_value(&object).map_err(|err| err.to_string())?;
    Ok(describe(&value))
}

/// Whether this object has pods, services and events worth listing: the
/// workloads, which are the objects people open to see how a rollout is going.
fn connects(target: &Target) -> bool {
    target.namespace.is_some() && row_actions::is_workload(&target.resource)
}

async fn fetch_connected(core: &AppCore, target: &Target) -> Result<Connected, String> {
    let Some(namespace) = target.namespace.as_deref() else {
        return Ok(Connected::default());
    };
    object_ops::read_connected(
        core,
        &target.cluster,
        &target.resource,
        namespace,
        &target.name,
    )
    .await
}

fn health_tone(health: &str) -> Option<Tone> {
    match health {
        "ok" => Some(Tone::Ok),
        "pending" => Some(Tone::Pending),
        "warning" => Some(Tone::Warn),
        "error" => Some(Tone::Error),
        _ => None,
    }
}

/// The newest `limit` events, newest first, with how long ago each was.
fn newest_events(rows: &[EventRow], limit: usize) -> Vec<(&EventRow, String)> {
    let now = present::now();
    let mut keyed: Vec<(i64, &EventRow)> = rows
        .iter()
        .map(|row| {
            let seen = row
                .last_seen
                .as_deref()
                .or(row.first_seen.as_deref())
                .and_then(present::parse_timestamp);
            (seen.unwrap_or(i64::MIN), row)
        })
        .collect();
    keyed.sort_by_key(|&(seen, _)| std::cmp::Reverse(seen));
    keyed
        .into_iter()
        .take(limit)
        .map(|(seen, row)| {
            let mut age = String::new();
            if seen != i64::MIN {
                present::write_age(&mut age, now - seen);
            }
            (row, age)
        })
        .collect()
}

/// A small count or status next to a card's title.
fn badge(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    div()
        .px_1p5()
        .rounded_full()
        .bg(color.opacity(0.14))
        .text_xs()
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

impl DetailsView {
    /// A bordered card whose header folds it. The rows are separated by thin
    /// lines, so a long list stays readable.
    fn card(
        &self,
        id: &'static str,
        title: &'static str,
        badges: Vec<AnyElement>,
        rows: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> Div {
        let (border, muted, hover) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground, theme.secondary)
        };
        let folded = self.collapsed.contains(id);
        v_flex()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .overflow_hidden()
            .child(
                h_flex()
                    .id(SharedString::from(format!("fold-{id}")))
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .hover(|header| header.bg(hover))
                    .tooltip(move |window, cx| {
                        Tooltip::new(if folded { "Show" } else { "Hide" }).build(window, cx)
                    })
                    .child(
                        Icon::new(if folded {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size_3p5()
                        .text_color(muted),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted)
                            .child(title.to_uppercase()),
                    )
                    .children(badges)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.fold(id);
                        cx.notify();
                    })),
            )
            .when(!folded, |card| {
                card.children(rows.into_iter().map(|row| {
                    div()
                        .border_t_1()
                        .border_color(border)
                        .px_3()
                        .py_1p5()
                        .child(row)
                }))
            })
    }

    /// Fold a card, or open it if it is folded, and remember the choice for the
    /// next object opened. Saving happens off the UI thread.
    fn fold(&mut self, id: &'static str) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_string());
        }
        let mut folded: Vec<String> = self.collapsed.iter().cloned().collect();
        folded.sort();
        let core = Arc::clone(&self.core);
        self.runtime.spawn(async move {
            if let Err(reason) = core.update_preferences(|p| p.details_folded = Some(folded)) {
                tracing::warn!(%reason, "could not save which cards are folded");
            }
        });
    }

    /// `label` and `value`, the value wrapping rather than being cut. A click
    /// copies it: an IP, a UID or an image is usually wanted elsewhere. `label`
    /// is narrower when two fields share a line.
    fn field(
        &self,
        id: &str,
        label: &str,
        value: &str,
        label_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (muted, hover) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.secondary)
        };
        let (copy, name) = (value.to_string(), label.to_string());
        h_flex()
            .id(SharedString::from(format!("{id}-{label}")))
            .gap_3()
            .items_start()
            .text_sm()
            .cursor_pointer()
            .rounded_sm()
            .hover(|row| row.bg(hover))
            .tooltip(|window, cx| Tooltip::new("Click to copy").build(window, cx))
            .child(
                div()
                    .w(px(label_width))
                    .flex_none()
                    .text_color(muted)
                    .child(label.to_string()),
            )
            .child(div().flex_1().min_w_0().child(value.to_string()))
            .on_click(cx.listener(move |_, _, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                toast_success(window, cx, format!("Copied {name}"));
            }))
            .into_any_element()
    }

    /// How the object is doing, at the top: a state, the count it rests on, and
    /// a bar for ready out of wanted.
    fn headline(&self, headline: &Headline, cx: &mut Context<Self>) -> Div {
        let (colour, muted) = {
            let theme = cx.theme();
            (tone_color(headline.tone, theme), theme.muted_foreground)
        };
        v_flex()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(colour.opacity(0.4))
            .bg(colour.opacity(0.06))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().size(px(10.)).rounded_full().bg(colour))
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .text_color(colour)
                            .child(headline.label.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(headline.detail.clone()),
                    ),
            )
            .when_some(
                headline.progress.filter(|&(_, wanted)| wanted > 0),
                |card, (ready, wanted)| {
                    let fraction = (ready as f32 / wanted as f32).clamp(0.0, 1.0);
                    card.child(
                        div()
                            .h(px(6.))
                            .w_full()
                            .rounded_full()
                            .bg(colour.opacity(0.18))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(colour)
                                    .w(relative(fraction)),
                            ),
                    )
                },
            )
    }

    /// The summary rows that belong to `group`.
    fn group_rows(
        &self,
        details: &Details,
        group: Group,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let rows: Vec<&(String, String)> = details
            .summary
            .iter()
            .filter(|(label, _)| group_of(label) == group)
            .collect();
        self.lines(&rows, cx)
    }

    /// Fields as lines: two to a line when both values are short, so a card of
    /// small facts does not run to a screen.
    fn lines(&self, rows: &[&(String, String)], cx: &mut Context<Self>) -> Vec<AnyElement> {
        /// Longest value that still shares a line.
        const SHORT: usize = 22;
        pack(rows, |(_, value)| value.chars().count() <= SHORT)
            .into_iter()
            .map(|line| match line {
                [(a_label, a_value), (b_label, b_value)] => h_flex()
                    .gap_4()
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.field("pair", a_label, a_value, 130., cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.field("pair", b_label, b_value, 104., cx)),
                    )
                    .into_any_element(),
                [(label, value)] => self.field("summary", label, value, 130., cx),
                _ => div().into_any_element(),
            })
            .collect()
    }

    /// Pods, services, ingress addresses and recent events. A row opens the
    /// object it names; an address opens in the browser.
    fn connected_cards(&self, connected: &Connected, cx: &mut Context<Self>) -> Vec<Div> {
        let (muted, link, warning, success, hover) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.info,
                theme.warning,
                theme.success,
                theme.secondary,
            )
        };
        let related = &connected.related;
        let mut cards: Vec<Div> = Vec::new();

        let open_row = |id: String, row: &RelatedRef, cx: &mut Context<Self>| {
            let target = row.clone();
            div()
                .id(SharedString::from(id))
                .cursor_pointer()
                .text_color(link)
                .hover(|name| name.underline())
                .child(row.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.open(&target, cx)))
        };

        if !related.pods.is_empty() {
            let ready = related.pods.iter().filter(|p| p.health == "ok").count();
            let all_ready = ready == related.pods.len();
            let rows = related
                .pods
                .iter()
                .map(|pod| {
                    let tone = health_tone(&pod.health).map(|t| tone_color(t, cx.theme()));
                    h_flex()
                        .gap_3()
                        .items_center()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(open_row(
                            format!("pod-{}", pod.name),
                            pod,
                            cx,
                        )))
                        .when_some(pod.detail.clone(), |line, detail| {
                            line.child(pill(detail, tone.unwrap_or(muted)))
                        })
                        .into_any_element()
                })
                .collect();
            cards.push(self.card(
                "pods",
                "Pods",
                vec![badge(
                    format!("{ready}/{} ready", related.pods.len()),
                    if all_ready { success } else { warning },
                )],
                rows,
                cx,
            ));
        }
        if !related.services.is_empty() {
            let rows = related
                .services
                .iter()
                .map(|service| {
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(open_row(
                            format!("svc-{}", service.name),
                            service,
                            cx,
                        )))
                        .when_some(service.detail.clone(), |line, kind| {
                            line.child(div().text_color(muted).child(kind))
                        })
                        .into_any_element()
                })
                .collect();
            cards.push(self.card(
                "services",
                "Services",
                vec![badge(related.services.len().to_string(), muted)],
                rows,
                cx,
            ));
        }
        if !related.ingresses.is_empty() {
            let mut rows: Vec<AnyElement> = Vec::new();
            for route in &related.routes {
                let url = route.url.clone();
                rows.push(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(
                            h_flex()
                                .id(SharedString::from(format!("route-{}", route.url)))
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .items_center()
                                .cursor_pointer()
                                .text_color(link)
                                .hover(|row| row.underline())
                                .tooltip(|window, cx| {
                                    Tooltip::new("Open in the browser").build(window, cx)
                                })
                                .child(route.url.clone())
                                .child(Icon::new(IconName::ExternalLink).size_3())
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{} → {}", route.ingress, route.service)),
                        )
                        .into_any_element(),
                );
            }
            // An ingress with no host has no address to link; name it.
            let linked: Vec<&str> = related.routes.iter().map(|r| r.ingress.as_str()).collect();
            for ingress in related
                .ingresses
                .iter()
                .filter(|i| !linked.contains(&i.name.as_str()))
            {
                rows.push(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(open_row(
                            format!("ing-{}", ingress.name),
                            ingress,
                            cx,
                        )))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(ingress.detail.clone().unwrap_or_default()),
                        )
                        .into_any_element(),
                );
            }
            cards.push(self.card(
                "ingress",
                "Ingress",
                vec![badge(related.ingresses.len().to_string(), muted)],
                rows,
                cx,
            ));
        }
        if !connected.events.is_empty() {
            let warnings = connected
                .events
                .iter()
                .filter(|e| e.kind == "Warning")
                .count();
            let mut badges = vec![badge(connected.events.len().to_string(), muted)];
            if warnings > 0 {
                badges.push(badge(format!("{warnings} warning"), warning));
            }
            let mut rows: Vec<AnyElement> = newest_events(&connected.events, EVENTS_SHOWN)
                .into_iter()
                .map(|(row, age)| {
                    let is_warning = row.kind == "Warning";
                    v_flex()
                        .text_sm()
                        .pl_2()
                        .border_l_2()
                        .border_color(if is_warning { warning } else { hover })
                        .child(
                            h_flex()
                                .gap_2()
                                .items_baseline()
                                .child(
                                    div()
                                        .font_semibold()
                                        .text_color(if is_warning { warning } else { muted })
                                        .child(row.reason.clone()),
                                )
                                .when(row.count > 1, |line| {
                                    line.child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!("×{}", row.count)),
                                    )
                                })
                                .child(div().text_xs().text_color(muted).child(age)),
                        )
                        .child(div().text_xs().text_color(muted).child(row.message.clone()))
                        .into_any_element()
                })
                .collect();
            let more = connected.events.len().saturating_sub(EVENTS_SHOWN);
            if more > 0 {
                rows.push(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("and {more} older in the Events tab"))
                        .into_any_element(),
                );
            }
            cards.push(self.card("events", "Events", badges, rows, cx));
        }
        cards
    }
}

impl Render for DetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn = Instant::now();
        let (muted, danger, secondary) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger, theme.secondary)
        };
        let State::Ready(details) = &self.state else {
            return match &self.state {
                State::Failed(reason) => message(reason.clone(), danger).into_any_element(),
                _ => message("Loading…", muted).into_any_element(),
            };
        };

        let mut body = v_flex().w_full().p_3().gap_3();
        if let Some(headline) = &details.headline {
            body = body.child(self.headline(headline, cx));
        }

        let mut overview = self.group_rows(details, Group::Overview, cx);
        for owner in &details.owners {
            overview.push(self.field("owner", "Owner", owner, 130., cx));
        }
        body = body.child(self.card("overview", "Overview", Vec::new(), overview, cx));

        for (id, title, group) in [
            ("rollout", "Rollout", Group::Rollout),
            ("scheduling", "Scheduling", Group::Scheduling),
        ] {
            let rows = self.group_rows(details, group, cx);
            if !rows.is_empty() {
                body = body.child(self.card(id, title, Vec::new(), rows, cx));
            }
        }

        if !details.containers.is_empty() {
            let rows = details
                .containers
                .iter()
                .map(|(name, image)| self.field("container", name, image, 130., cx))
                .collect();
            body = body.child(self.card(
                "containers",
                "Containers",
                vec![badge(details.containers.len().to_string(), muted)],
                rows,
                cx,
            ));
        }

        if !details.conditions.is_empty() {
            let failing = details
                .conditions
                .iter()
                .filter(|c| {
                    condition_tone(&c.kind, &c.status).is_some_and(|tone| tone == Tone::Error)
                })
                .count();
            let rows = details
                .conditions
                .iter()
                .map(|condition| {
                    let colour = condition_tone(&condition.kind, &condition.status)
                        .map_or(muted, |tone| tone_color(tone, cx.theme()));
                    v_flex()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(div().w(px(180.)).flex_none().child(condition.kind.clone()))
                                .child(pill(condition.status.clone(), colour))
                                .when(!condition.reason.is_empty(), |line| {
                                    line.child(
                                        div().text_color(muted).child(condition.reason.clone()),
                                    )
                                }),
                        )
                        .when(!condition.message.is_empty(), |item| {
                            item.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(condition.message.clone()),
                            )
                        })
                        .into_any_element()
                })
                .collect();
            let mut badges = vec![badge(details.conditions.len().to_string(), muted)];
            if failing > 0 {
                badges.push(badge(format!("{failing} failing"), danger));
            }
            body = body.child(self.card("conditions", "Conditions", badges, rows, cx));
        }

        if let Some(connected) = self.connected.as_deref() {
            for card in self.connected_cards(connected, cx) {
                body = body.child(card);
            }
        }

        if !details.labels.is_empty() {
            let chips = h_flex()
                .flex_wrap()
                .gap_1()
                .children(details.labels.iter().map(|(key, value)| {
                    let text = format!("{key}={value}");
                    let copy = text.clone();
                    div()
                        .id(SharedString::from(format!("label-{key}")))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(secondary)
                        .text_xs()
                        .cursor_pointer()
                        .hover(|chip| chip.opacity(0.7))
                        .tooltip(|window, cx| Tooltip::new("Click to copy").build(window, cx))
                        .child(text)
                        .on_click(cx.listener(move |_, _, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            toast_success(window, cx, "Copied the label");
                        }))
                }))
                .into_any_element();
            body = body.child(self.card(
                "labels",
                "Labels",
                vec![badge(details.labels.len().to_string(), muted)],
                vec![chips],
                cx,
            ));
        }

        if !details.annotations.is_empty() {
            // Key over value: a key such as `deployment.kubernetes.io/revision`
            // does not fit the label column, and a value can run long.
            let rows = details
                .annotations
                .iter()
                .map(|(key, value)| {
                    v_flex()
                        .text_sm()
                        .child(div().text_color(muted).child(key.clone()))
                        .child(div().pl_2().child(value.clone()))
                        .into_any_element()
                })
                .collect();
            body = body.child(self.card(
                "annotations",
                "Annotations",
                vec![badge(details.annotations.len().to_string(), muted)],
                rows,
                cx,
            ));
        }

        div()
            .id("details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }
}
