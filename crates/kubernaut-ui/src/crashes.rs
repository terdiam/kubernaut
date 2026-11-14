//! Crash log: what went wrong in earlier runs, read from files on this
//! machine. A panic comes from the app's own log; a crash the app could not
//! catch comes from the report the operating system wrote for it.
//!
//! Nothing is sent anywhere. Copy puts the text on the clipboard so a bug report
//! is something the person reads first and chooses to paste.

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    crashes::{self, CrashEntry, CrashKind},
    logging, present,
};
use tokio::runtime::Handle;

use crate::ui::{Emphasis, Palette, button_icon, message, page_title, pill, toast_success};

/// One entry as the list shows it. Built once, at load.
struct Row {
    entry: CrashEntry,
    age: SharedString,
}

fn row(entry: CrashEntry, now: i64) -> Row {
    let mut age = String::new();
    if entry.at > 0 {
        present::write_age(&mut age, now - entry.at);
        age.push_str(" ago");
    }
    Row {
        entry,
        age: age.into(),
    }
}

pub struct CrashView {
    rows: Vec<Row>,
    loaded: bool,
    selected: usize,
}

impl CrashView {
    pub fn new(runtime: &Handle, cx: &mut Context<Self>) -> Self {
        let task = runtime.spawn_blocking(crashes::find);
        cx.spawn(async move |this, cx| {
            let entries = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                let now = present::now();
                this.rows = entries.into_iter().map(|e| row(e, now)).collect();
                this.loaded = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            rows: Vec::new(),
            loaded: false,
            selected: 0,
        }
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let text = format!(
            "{}\n{}\n\n{}",
            row.entry.title, row.entry.source, row.entry.detail
        );
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Show the folder the selected entry was read from.
    fn reveal(&self, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let directory = match row.entry.kind {
            CrashKind::Panic => logging::log_directory(),
            CrashKind::System => crashes::reports_directory(),
        };
        if let Some(path) = directory {
            cx.reveal_path(&path);
        }
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, active, hover, danger, warning) = (
            theme.muted_foreground,
            theme.border,
            theme.list_active,
            theme.secondary,
            theme.danger,
            theme.warning,
        );
        v_flex()
            .id("crash-list")
            .w(px(340.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(border)
            .children(self.rows.iter().enumerate().map(|(index, row)| {
                let (label, color) = match row.entry.kind {
                    CrashKind::Panic => ("panic", danger),
                    CrashKind::System => ("crash", warning),
                };
                v_flex()
                    .id(("crash-row", index))
                    .px_3()
                    .py_2()
                    .gap_1()
                    .cursor_pointer()
                    .when(index == self.selected, |column| column.bg(active))
                    .hover(|column| column.bg(hover))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(pill(label, color))
                            .child(div().text_xs().text_color(muted).child(row.age.clone())),
                    )
                    .child(div().text_sm().line_clamp(2).child(row.entry.title.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        cx.notify();
                    }))
            }))
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(self.selected) else {
            return div().into_any_element();
        };
        let (muted, mono) = (
            cx.theme().muted_foreground,
            cx.theme().mono_font_family.clone(),
        );
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(row.entry.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(row.entry.source.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .pt_1()
                            .child(
                                button_icon(
                                    "crash-copy",
                                    IconName::Copy,
                                    "Copy",
                                    true,
                                    Emphasis::Plain,
                                    cx.theme(),
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.copy(cx);
                                        toast_success(window, cx, "Copied");
                                    },
                                )),
                            )
                            .child(
                                button_icon(
                                    "crash-reveal",
                                    IconName::FolderOpen,
                                    "Show folder",
                                    true,
                                    Emphasis::Plain,
                                    cx.theme(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.reveal(cx))),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("crash-detail")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_3()
                    .text_xs()
                    .font_family(mono)
                    .children(
                        row.entry
                            .detail
                            .lines()
                            .map(|line| div().min_h(px(14.)).child(line.to_string())),
                    ),
            )
            .into_any_element()
    }
}

impl Render for CrashView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let body = if !self.loaded {
            message("Reading the logs…", palette.muted).into_any_element()
        } else if self.rows.is_empty() {
            message(
                "No crashes on record. A panic is written to this app's log and a crash the app \
                 could not catch to the system's crash reports; either would appear here.",
                palette.muted,
            )
            .into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_list(cx))
                .child(self.render_detail(cx))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_none()
                    .px_4()
                    .items_center()
                    .child(page_title(IconName::TriangleAlert, "Crash log", cx.theme())),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .border_t_1()
                    .border_color(palette.border)
                    .child(body),
            )
    }
}
