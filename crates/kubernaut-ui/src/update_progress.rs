//! The body of the window shown while an update installs itself.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::updates::{Progress, Stage};

use crate::ui::{Emphasis, button, progress_bar};

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
