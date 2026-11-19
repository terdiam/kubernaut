//! The body of the window shown while an update installs itself.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    base::StyledExt,
    component::{ActiveTheme, Theme, WindowExt, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::updates::{self, CANCELLED, NoteKind, NoteLine, Progress, Release, Stage, note_lines};
use tokio::runtime::Handle;

use crate::ui::{Emphasis, button, progress_bar, toast_error, toast_success};

pub struct UpdateProgress {
    progress: Arc<Progress>,
}

impl UpdateProgress {
    pub fn new(progress: Arc<Progress>, cx: &mut Context<Self>) -> Self {
        // The worker writes atomics, which nothing observes, so repaint on a
        // timer. The loop ends when the window closes and the view is dropped.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self { progress }
    }
}

impl Render for UpdateProgress {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let fraction = match self.progress.stage() {
            Stage::Downloading => self.progress.fraction(),
            _ => None,
        };
        let cancelling = self.progress.is_cancelled();
        let can_cancel = self.progress.can_cancel() && !cancelling;
        let label = if cancelling {
            "Cancelling…".to_string()
        } else {
            self.progress.describe()
        };
        let cancel = button(
            "update-cancel",
            "Cancel update",
            can_cancel,
            Emphasis::Plain,
            theme,
        )
        .when(can_cancel, |button| {
            let progress = Arc::clone(&self.progress);
            button.on_click(cx.listener(move |_, _, _, cx| {
                progress.cancel();
                cx.notify();
            }))
        });
        v_flex()
            .gap_3()
            .child(progress_bar(fraction, relative(1.), theme))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(cancel),
            )
    }
}

/// Release notes as a short scrolling list: headings, bullets and paragraphs.
pub fn notes_panel(id: &'static str, lines: &[NoteLine], theme: &Theme) -> Stateful<Div> {
    v_flex()
        .id(id)
        .max_h(px(280.))
        .overflow_y_scroll()
        .gap_1p5()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.4))
        .text_sm()
        .children(lines.iter().map(|line| {
            match line.kind {
                NoteKind::Heading => div()
                    .pt_1()
                    .font_semibold()
                    .child(line.text.clone())
                    .into_any_element(),
                NoteKind::Bullet => h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().text_color(theme.muted_foreground).child("•"))
                    .child(div().flex_1().min_w_0().child(line.text.clone()))
                    .into_any_element(),
                NoteKind::Text => div()
                    .text_color(theme.muted_foreground)
                    .child(line.text.clone())
                    .into_any_element(),
            }
        }))
}

/// Install a release behind a window of its own, so a download that takes a
/// while is not a silent wait for a restart. Quits on success so the swap can
/// run; says so in a toast when the person cancels or it fails.
pub async fn install(runtime: &Handle, release: Release, cx: &mut AsyncWindowContext) {
    let version = release.version.clone();
    let progress = Arc::new(Progress::default());
    cx.update(|window, cx| {
        let body = cx.new(|cx| UpdateProgress::new(Arc::clone(&progress), cx));
        let title = format!("Updating Kubernaut to {version}");
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(420.))
                .close_button(false)
                .overlay_closable(false)
                .keyboard(false)
                .child(body.clone())
        });
    })
    .ok();
    let worker = Arc::clone(&progress);
    let outcome = runtime
        .spawn_blocking(move || updates::download_and_install_tracked(&release, &worker))
        .await
        .unwrap_or_else(|err| Err(err.to_string()));
    cx.update(|window, cx| window.close_dialog(cx)).ok();
    match outcome {
        Ok(()) => {
            cx.update(|_, cx| cx.quit()).ok();
        }
        Err(reason) if reason == CANCELLED => {
            cx.update(|window, cx| {
                toast_success(
                    window,
                    cx,
                    format!("Update to {version} cancelled. See Settings → Updates."),
                );
            })
            .ok();
        }
        Err(reason) => {
            cx.update(|window, cx| {
                toast_error(
                    window,
                    cx,
                    format!("Could not install Kubernaut {version}"),
                    format!("{}. See Settings → Updates.", reason.trim_end_matches('.')),
                );
            })
            .ok();
        }
    }
}

/// The window that says a newer version exists, what is in it, and offers to
/// install it. An install this build cannot do itself (a .deb, a build run from
/// `target/`) keeps the notes and the link to the release page.
pub fn open_available(window: &mut Window, cx: &mut App, runtime: Handle, release: Release) {
    let lines = note_lines(&release.notes);
    let installable = release.installable() && updates::current_install().is_some();
    window.open_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let page = release.page_url.clone();
        let mut footer = h_flex().gap_2().justify_end().child(
            button("update-later", "Later", true, Emphasis::Plain, theme)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        );
        if !page.is_empty() {
            footer = footer.child(
                button("update-page", "Release page", true, Emphasis::Plain, theme)
                    .on_click(move |_, _, cx| cx.open_url(&page)),
            );
        }
        if installable {
            let (runtime, release) = (runtime.clone(), release.clone());
            footer = footer.child(
                button(
                    "update-install",
                    "Install and restart",
                    true,
                    Emphasis::Primary,
                    theme,
                )
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let (runtime, release) = (runtime.clone(), release.clone());
                    window
                        .spawn(cx, async move |cx| install(&runtime, release, cx).await)
                        .detach();
                }),
            );
        }
        let body = v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(format!(
                "You have {}. {}",
                env!("CARGO_PKG_VERSION"),
                if installable {
                    "It installs in a moment and Kubernaut restarts."
                } else {
                    "This build cannot install it itself; use the release page."
                }
            )))
            .when(!lines.is_empty(), |body| {
                body.child(notes_panel("update-notes", &lines, theme))
            });
        dialog
            .title(format!("Kubernaut {} is available", release.version))
            .width(px(540.))
            .child(body)
            .footer(footer)
    });
}
