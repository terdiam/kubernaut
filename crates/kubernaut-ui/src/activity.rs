//! Activity: what this app changed, and whether it worked.
//!
//! The audit log is written by every change the app makes. This is the one
//! place it can be read, newest first, so "what did I do to that cluster?"
//! has an answer that does not need a terminal.

use std::{ops::Range, sync::Arc, time::Duration};

use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, audit::AuditEntry, present};
use tokio::runtime::Handle;

use crate::{
    rows::contains_ignore_case,
    ui::{Palette, chip_icon, message, page_title, pill},
};

const ROW_HEIGHT: Pixels = px(30.);
/// How many entries to read. A week of normal use is far fewer.
const LIMIT: usize = 1000;
/// Other windows or the app itself may write while this is open.
const POLL: Duration = Duration::from_secs(10);

/// `helmRollback` as `Helm rollback`: the log stores camelCase action names.
fn humanize(action: &str) -> String {
    let mut out = String::with_capacity(action.len() + 4);
    for (index, c) in action.chars().enumerate() {
        if c.is_uppercase() && index > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else if index == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

struct Line {
    age: SharedString,
    cluster: SharedString,
    action: SharedString,
    target: SharedString,
    ok: bool,
    outcome: SharedString,
}

fn line(entry: AuditEntry, now: i64) -> Line {
    let mut age = String::new();
    if let Some(at) = present::parse_timestamp(&entry.timestamp) {
        present::write_age(&mut age, (now - at).max(0));
    }
    let ok = entry.outcome == "ok";
    Line {
        age: age.into(),
        cluster: entry.cluster.into(),
        action: humanize(&entry.action).into(),
        target: entry.target.into(),
        ok,
        outcome: entry.outcome.into(),
    }
}

/// Indices of the lines a filter keeps.
fn matching(lines: &[Line], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            [&*line.cluster, &*line.action, &*line.target, &*line.outcome]
                .into_iter()
                .any(|field| contains_ignore_case(field, needle))
        })
        .map(|(index, _)| index)
        .collect()
}

pub struct ActivityView {
    core: Arc<AppCore>,
    runtime: Handle,
    lines: Vec<Line>,
    shown: Vec<usize>,
    loaded: bool,
    filter: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _filter: Subscription,
}

impl ActivityView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
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
            lines: Vec::new(),
            shown: Vec::new(),
            loaded: false,
            filter,
            scroll: UniformListScrollHandle::new(),
            _filter: subscription,
        };
        view.refresh(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn_blocking(move || core.audit.recent(LIMIT));
        cx.spawn(async move |this, cx| {
            let entries = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                let now = present::now();
                this.lines = entries.into_iter().map(|e| line(e, now)).collect();
                this.loaded = true;
                this.apply_filter(cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter.read(cx).value();
        self.shown = matching(&self.lines, needle.trim());
        cx.notify();
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = Palette::of(cx);
        if !self.loaded {
            return message("Reading the log…", palette.muted).into_any_element();
        }
        if self.lines.is_empty() {
            return message(
                "Nothing has been changed from this app yet. Changes appear here, and are kept for a week.",
                palette.muted,
            )
            .into_any_element();
        }
        if self.shown.is_empty() {
            return message("Nothing matches.", palette.muted).into_any_element();
        }
        uniform_list(
            "activity",
            self.shown.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let theme = cx.theme();
                let (hover, muted, danger, success) = (
                    theme.secondary,
                    theme.muted_foreground,
                    theme.danger,
                    theme.success,
                );
                range
                    .filter_map(|position| {
                        let line = this.lines.get(*this.shown.get(position)?)?;
                        Some(
                            h_flex()
                                .id(("activity-row", position))
                                .w_full()
                                .h(ROW_HEIGHT)
                                .px_4()
                                .gap_3()
                                .items_center()
                                .text_sm()
                                .hover(|row| row.bg(hover))
                                .child(
                                    div()
                                        .w(px(60.))
                                        .flex_none()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(line.age.clone()),
                                )
                                .child(
                                    div()
                                        .w(px(150.))
                                        .flex_none()
                                        .truncate()
                                        .text_color(muted)
                                        .child(line.cluster.clone()),
                                )
                                .child(
                                    div()
                                        .w(px(150.))
                                        .flex_none()
                                        .truncate()
                                        .child(line.action.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(line.target.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(if line.ok { success } else { danger })
                                        .child(if line.ok {
                                            SharedString::from("Done")
                                        } else {
                                            line.outcome.clone()
                                        }),
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

impl Render for ActivityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let failed = self.lines.iter().filter(|line| !line.ok).count();
        let danger = cx.theme().danger;
        let refresh = cx.listener(|this, _, _, cx| this.refresh(cx));
        let list = self.render_list(cx);

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
                    .child(page_title(IconName::Clock, "Activity", cx.theme()))
                    .when(failed > 0, |row| {
                        row.child(pill(format!("{failed} failed"), danger))
                    })
                    .child(div().flex_1())
                    .child(
                        chip_icon(
                            "activity-refresh",
                            IconName::RefreshCw,
                            "Refresh",
                            true,
                            false,
                            cx.theme(),
                        )
                        .on_click(refresh),
                    )
                    .child(div().w(px(220.)).child(Input::new(&self.filter))),
            )
            .child(
                h_flex()
                    .h(px(24.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(palette.border)
                    .text_xs()
                    .text_color(palette.muted)
                    .child(div().w(px(60.)).flex_none().child("When"))
                    .child(div().w(px(150.)).flex_none().child("Cluster"))
                    .child(div().w(px(150.)).flex_none().child("Action"))
                    .child(div().flex_1().child("Target"))
                    .child(div().flex_1().child("Result")),
            )
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::{humanize, line, matching};
    use k8s_app::audit::AuditEntry;

    fn entry(action: &str, outcome: &str) -> AuditEntry {
        AuditEntry {
            timestamp: "1970-01-01T00:01:00Z".into(),
            cluster: "prod".into(),
            action: action.into(),
            target: "shop/web".into(),
            outcome: outcome.into(),
        }
    }

    #[test]
    fn action_names_read_as_words() {
        assert_eq!(humanize("delete"), "Delete");
        assert_eq!(humanize("helmRollback"), "Helm rollback");
        assert_eq!(humanize("portForwardNetwork"), "Port forward network");
        assert_eq!(humanize(""), "");
    }

    #[test]
    fn a_line_knows_its_age_and_whether_it_worked() {
        let ok = line(entry("restart", "ok"), 120);
        assert!(ok.ok);
        assert_eq!(&*ok.age, "1m0s");
        let failed = line(entry("delete", "forbidden"), 120);
        assert!(!failed.ok);
        assert_eq!(&*failed.outcome, "forbidden");
    }

    #[test]
    fn the_filter_matches_cluster_action_target_or_outcome() {
        let lines = vec![
            line(entry("delete", "ok"), 0),
            line(entry("helmRollback", "boom"), 0),
        ];
        assert_eq!(matching(&lines, "rollback"), [1]);
        assert_eq!(matching(&lines, "BOOM"), [1]);
        assert_eq!(matching(&lines, "prod"), [0, 1]);
        assert_eq!(matching(&lines, "nope"), Vec::<usize>::new());
    }
}
