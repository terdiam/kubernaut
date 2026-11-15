//! The Details tab: an object's identity, make-up, owners and conditions,
//! read from the object itself by `k8s_app::details`.

use std::sync::Arc;

use gpui_kit::{
    base::StyledExt,
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    details::{Details, condition_tone, describe},
};
use k8s_core::objects;
use tokio::runtime::Handle;

use crate::{
    drawer::Target,
    ui::{message, pill, tone_color},
};

enum State {
    Loading,
    Ready(Box<Details>),
    Failed(SharedString),
}

pub struct DetailsView {
    state: State,
}

impl DetailsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        cx: &mut Context<Self>,
    ) -> Self {
        let (core, target) = (Arc::clone(core), target.clone());
        let task = runtime.spawn(async move { fetch(&core, &target).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(details) => State::Ready(Box::new(details)),
                    Err(reason) => State::Failed(reason.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            state: State::Loading,
        }
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

impl Render for DetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                                        div().w(px(130.)).flex_none().child(condition.kind.clone()),
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
                                        .pl(px(138.))
                                        .text_xs()
                                        .text_color(muted)
                                        .child(condition.message.clone()),
                                )
                            })
                    }));
        }
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
            body = body.child(section("Annotations", muted)).children(
                details
                    .annotations
                    .iter()
                    .map(|(key, value)| row(key.clone(), value.clone(), muted)),
            );
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
