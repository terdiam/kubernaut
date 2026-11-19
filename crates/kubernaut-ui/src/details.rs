//! The Details tab: an object's identity, make-up, owners and conditions,
//! read from the object itself by `k8s_app::details`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{
    base::StyledExt,
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    details::{Details, condition_tone, describe},
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
    ui::{message, pill, tone_color},
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

fn section(title: &'static str, muted: Hsla) -> Div {
    div()
        .mt_3()
        .mb_1()
        .text_xs()
        .font_semibold()
        .text_color(muted)
        .child(title.to_uppercase())
}

/// `label` over `value`, the value wrapping rather than being cut: an
/// annotation or a message is what someone opened this tab to read.
fn row(label: impl Into<SharedString>, value: impl Into<SharedString>, muted: Hsla) -> Div {
    h_flex()
        .gap_3()
        .items_start()
        .py_0p5()
        .text_sm()
        .child(
            div()
                .w(px(130.))
                .flex_none()
                .text_color(muted)
                .child(label.into()),
        )
        .child(div().flex_1().min_w_0().child(value.into()))
}

impl DetailsView {
    /// Pods, services, ingress addresses and recent events, after the object's
    /// own fields. A row opens the object it names; an address opens in the
    /// browser.
    fn render_connected(
        &self,
        mut body: Div,
        connected: &Connected,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (muted, link, warning) = (theme.muted_foreground, theme.info, theme.warning);
        let related = &connected.related;

        let named = |id: String, row: &RelatedRef, cx: &mut Context<Self>| {
            let target = row.clone();
            div()
                .id(SharedString::from(id))
                .cursor_pointer()
                .text_color(link)
                .child(row.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.open(&target, cx)))
        };

        if !related.pods.is_empty() {
            body = body
                .child(section("Pods", muted))
                .children(related.pods.iter().map(|pod| {
                    let tone = health_tone(&pod.health).map(|t| tone_color(t, cx.theme()));
                    h_flex()
                        .gap_3()
                        .items_center()
                        .py_0p5()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(named(
                            format!("pod-{}", pod.name),
                            pod,
                            cx,
                        )))
                        .when_some(pod.detail.clone(), |line, detail| {
                            line.child(pill(detail, tone.unwrap_or(muted)))
                        })
                }));
        }
        if !related.services.is_empty() {
            body = body
                .child(section("Services", muted))
                .children(related.services.iter().map(|service| {
                    h_flex()
                        .gap_3()
                        .py_0p5()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(named(
                            format!("svc-{}", service.name),
                            service,
                            cx,
                        )))
                        .when_some(service.detail.clone(), |line, kind| {
                            line.child(div().text_color(muted).child(kind))
                        })
                }));
        }
        if !related.ingresses.is_empty() {
            body = body.child(section("Ingress", muted));
            for route in &related.routes {
                let url = route.url.clone();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .py_0p5()
                        .text_sm()
                        .child(
                            div()
                                .id(SharedString::from(format!("route-{}", route.url)))
                                .flex_1()
                                .min_w_0()
                                .cursor_pointer()
                                .text_color(link)
                                .child(route.url.clone())
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .child(
                            div()
                                .text_color(muted)
                                .child(format!("{} → {}", route.ingress, route.service)),
                        ),
                );
            }
            // An ingress with no host has no address to link; name it.
            let linked: Vec<&str> = related.routes.iter().map(|r| r.ingress.as_str()).collect();
            for ingress in related
                .ingresses
                .iter()
                .filter(|i| !linked.contains(&i.name.as_str()))
            {
                body = body.child(
                    h_flex()
                        .gap_3()
                        .py_0p5()
                        .text_sm()
                        .child(div().flex_1().min_w_0().child(named(
                            format!("ing-{}", ingress.name),
                            ingress,
                            cx,
                        )))
                        .child(
                            div()
                                .text_color(muted)
                                .child(ingress.detail.clone().unwrap_or_default()),
                        ),
                );
            }
        }
        if !connected.events.is_empty() {
            body = body.child(section("Events", muted));
            for (row, age) in newest_events(&connected.events, EVENTS_SHOWN) {
                let colour = if row.kind == "Warning" {
                    warning
                } else {
                    muted
                };
                body = body.child(
                    v_flex()
                        .py_0p5()
                        .text_sm()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_baseline()
                                .child(div().text_color(colour).child(row.reason.clone()))
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
                        .child(div().text_xs().text_color(muted).child(row.message.clone())),
                );
            }
            let more = connected.events.len().saturating_sub(EVENTS_SHOWN);
            if more > 0 {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("and {more} older in the Events tab")),
                );
            }
        }
        body
    }
}

impl Render for DetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn = Instant::now();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let details = match &self.state {
            State::Loading => return message("Loading…", muted).into_any_element(),
            State::Failed(reason) => {
                return message(reason.clone(), theme.danger).into_any_element();
            }
            State::Ready(details) => details,
        };

        let mut body = v_flex().w_full().p_3().gap_0();
        for (label, value) in &details.summary {
            body = body.child(row(label.clone(), value.clone(), muted));
        }
        if !details.owners.is_empty() {
            body = body.child(section("Owners", muted)).children(
                details
                    .owners
                    .iter()
                    .map(|owner| div().text_sm().child(owner.clone())),
            );
        }
        if !details.containers.is_empty() {
            body = body.child(section("Containers", muted)).children(
                details
                    .containers
                    .iter()
                    .map(|(name, image)| row(name.clone(), image.clone(), muted)),
            );
        }
        if !details.conditions.is_empty() {
            body =
                body.child(section("Conditions", muted))
                    .children(details.conditions.iter().map(|condition| {
                        let colour = condition_tone(&condition.kind, &condition.status)
                            .map_or(muted, |tone| tone_color(tone, theme));
                        v_flex()
                            .py_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .text_sm()
                                    .child(
                                        div().w(px(200.)).flex_none().child(condition.kind.clone()),
                                    )
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
                                        .pl(px(208.))
                                        .text_xs()
                                        .text_color(muted)
                                        .child(condition.message.clone()),
                                )
                            })
                    }));
        }
        if let Some(connected) = self.connected.as_deref() {
            body = self.render_connected(body, connected, cx);
        }
        let theme = cx.theme();
        if !details.labels.is_empty() {
            body =
                body.child(section("Labels", muted))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .children(details.labels.iter().map(|(key, value)| {
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .bg(theme.secondary)
                                    .text_xs()
                                    .child(format!("{key}={value}"))
                            })),
                    );
        }
        if !details.annotations.is_empty() {
            // Key over value: a key such as `deployment.kubernetes.io/revision`
            // does not fit the label column, and a value can run long.
            body =
                body.child(section("Annotations", muted))
                    .children(details.annotations.iter().map(|(key, value)| {
                        v_flex()
                            .py_0p5()
                            .text_sm()
                            .child(div().text_color(muted).child(key.clone()))
                            .child(div().pl_2().child(value.clone()))
                    }));
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
