//! The Quota tab: a namespace's own `ResourceQuota` and `LimitRange` objects.
//!
//! Usage against a hard limit is what stops a deploy, and the defaults are what
//! a pod gets when it says nothing. Neither shows in a chart of what pods use.

use std::{collections::BTreeMap, sync::Arc};

use gpui_kit::{
    base::StyledExt,
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, metrics_ops};
use k8s_metrics::{NamespaceQuotaInfo, quantity};
use tokio::runtime::Handle;

use crate::ui::{Palette, message};

/// How full a quota line is, from its quantity strings. `None` when either is
/// not a quantity or the limit is zero, so a bar is never drawn from nonsense.
pub fn fraction(used: &str, hard: &str) -> Option<f64> {
    let (used, hard) = (quantity::parse(used)?, quantity::parse(hard)?);
    (hard > 0.0).then(|| (used / hard).clamp(0.0, 1.0))
}

/// `cpu 500m, memory 256Mi`, in a stable order.
fn quantities(map: &BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(key, value)| format!("{key} {value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

enum Load {
    Loading,
    Ready(NamespaceQuotaInfo),
    Failed(SharedString),
}

pub struct QuotaView {
    load: Load,
}

impl QuotaView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        namespace: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        let core = Arc::clone(core);
        let task = runtime
            .spawn(async move { metrics_ops::namespace_quota(&core, &cluster, &namespace).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.load = match result {
                    Ok(info) => Load::Ready(info),
                    Err(reason) => Load::Failed(reason.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            load: Load::Loading,
        }
    }
}

impl Render for QuotaView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let (border, secondary, primary, warning) = {
            let theme = cx.theme();
            (theme.border, theme.secondary, theme.primary, theme.warning)
        };
        match &self.load {
            Load::Loading => message("Reading the quota…", palette.muted).into_any_element(),
            Load::Failed(reason) => message(reason.clone(), palette.danger).into_any_element(),
            Load::Ready(info) if info.quotas.is_empty() && info.limit_ranges.is_empty() => message(
                "This namespace has no quota and no limit range.",
                palette.muted,
            )
            .into_any_element(),
            Load::Ready(info) => {
                let mut column = v_flex().p_3().gap_3();
                for quota in &info.quotas {
                    let mut card = v_flex()
                        .p_3()
                        .gap_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .child(div().font_semibold().child(quota.name.clone()));
                    for (resource, hard) in &quota.hard {
                        let used = quota.used.get(resource).map_or("0", String::as_str);
                        let ratio = fraction(used, hard);
                        let nearly_full = ratio.is_some_and(|ratio| ratio >= 0.9);
                        card = card.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .child(div().flex_1().child(resource.clone()))
                                        .child(
                                            div()
                                                .text_color(if nearly_full {
                                                    warning
                                                } else {
                                                    palette.muted
                                                })
                                                .child(format!("{used} / {hard}")),
                                        ),
                                )
                                .when_some(ratio, |line, ratio| {
                                    line.child(
                                        div()
                                            .h(px(4.))
                                            .w_full()
                                            .rounded_full()
                                            .bg(secondary)
                                            .child(
                                                div()
                                                    .h_full()
                                                    .rounded_full()
                                                    .w(relative(ratio as f32))
                                                    .bg(if nearly_full {
                                                        warning
                                                    } else {
                                                        primary
                                                    }),
                                            ),
                                    )
                                }),
                        );
                    }
                    column = column.child(card);
                }
                for item in &info.limit_ranges {
                    let line = |label: &'static str, map: &BTreeMap<String, String>| {
                        (!map.is_empty()).then(|| {
                            h_flex()
                                .gap_3()
                                .text_sm()
                                .child(
                                    div()
                                        .w(px(120.))
                                        .flex_none()
                                        .text_color(palette.muted)
                                        .child(label),
                                )
                                .child(div().flex_1().min_w_0().child(quantities(map)))
                        })
                    };
                    column = column.child(
                        v_flex()
                            .p_3()
                            .gap_1()
                            .rounded_lg()
                            .border_1()
                            .border_color(border)
                            .child(
                                div()
                                    .font_semibold()
                                    .child(format!("Limit range · {}", item.kind)),
                            )
                            .children(line("Default limit", &item.default))
                            .children(line("Default request", &item.default_request))
                            .children(line("Largest allowed", &item.max))
                            .children(line("Smallest allowed", &item.min)),
                    );
                }
                div()
                    .id("quota-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(column)
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fraction, quantities};
    use std::collections::BTreeMap;

    #[test]
    fn a_quota_line_is_a_fraction_only_when_both_sides_are_quantities() {
        assert_eq!(fraction("1", "4"), Some(0.25));
        assert_eq!(fraction("2Gi", "4Gi"), Some(0.5));
        assert_eq!(
            fraction("9", "4"),
            Some(1.0),
            "over the limit is a full bar"
        );
        assert_eq!(fraction("1", "0"), None, "no division by zero");
        assert_eq!(fraction("x", "4"), None);
    }

    #[test]
    fn quantities_read_in_a_stable_order() {
        let map = BTreeMap::from([
            ("memory".to_string(), "256Mi".to_string()),
            ("cpu".to_string(), "500m".to_string()),
        ]);
        assert_eq!(quantities(&map), "cpu 500m, memory 256Mi");
    }
}
