//! The Logs tab: a bounded, virtualised tail of a pod or of a workload's pods.

use std::{ops::Range, sync::Arc};

use futures::{StreamExt, channel::mpsc};
use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Editor, EditorState, Input, InputEvent, InputState, Position},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::AppCore;
use k8s_ops::logs::{self, LogEvent, LogOptions, LogTarget};
use tokio::{runtime::Handle, task::JoinHandle};

use crate::{
    drawer::Target,
    log_buffer::{LineKind, LogBuffer},
    ui::{toast_error, toast_success},
};

const ROW_HEIGHT: Pixels = px(20.);
/// Lines kept. About a megabyte of text at typical line lengths.
const CAPACITY: usize = 10_000;
/// A generation no stream ever has, for messages about the pod, not a stream.
const ANY_GENERATION: u64 = u64::MAX;

/// What the stream task tells the view.
enum Message {
    Containers(Vec<SharedString>),
    Batch(Vec<LogEvent>),
    Failed(String),
    Ended,
}

pub struct LogView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    log_target: LogTarget,
    /// Containers of a single pod, for the picker. Empty for a workload.
    containers: Vec<SharedString>,
    container: Option<SharedString>,
    previous: bool,
    timestamps: bool,
    follow: bool,
    buffer: LogBuffer,
    scroll: UniformListScrollHandle,
    /// Messages from an earlier stream carry an older generation and are
    /// dropped, so restarting cannot interleave two streams' lines.
    generation: u64,
    task: Option<JoinHandle<()>>,
    status: Option<SharedString>,
    tx: mpsc::UnboundedSender<(u64, Message)>,
    /// Names the saved file after what is being read.
    name: SharedString,
    filter: Entity<InputState>,
    _filter_events: Subscription,
    /// Read-only text of the lines as they were when Select was pressed, so
    /// words can be marked and copied. A live list cannot hold a selection
    /// while lines keep arriving.
    viewer: Entity<EditorState>,
    selecting: bool,
}

impl Drop for LogView {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl LogView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        log_target: LogTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter lines"));
        let filter_events =
            cx.subscribe_in(&filter, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.buffer.set_filter(&text);
                    cx.notify();
                }
            });
        let viewer = cx.new(|cx| EditorState::new(window, cx).line_number(false));
        let (tx, mut rx) = mpsc::unbounded::<(u64, Message)>();
        cx.spawn(async move |this, cx| {
            while let Some((generation, message)) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_message(generation, message, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let multi_pod = matches!(log_target, LogTarget::Workload { .. });
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster: target.cluster.clone(),
            log_target,
            containers: Vec::new(),
            container: None,
            previous: false,
            timestamps: false,
            follow: true,
            buffer: LogBuffer::new(CAPACITY, multi_pod),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            task: None,
            status: Some("Starting…".into()),
            tx,
            name: target.name.clone(),
            filter,
            _filter_events: filter_events,
            viewer,
            selecting: false,
        };
        view.load_containers();
        view.restart(cx);
        view
    }

    /// For a single pod, learn its containers so the picker can offer them. The
    /// stream does not wait for this: it starts on the pod's default container.
    fn load_containers(&self) {
        let LogTarget::Pod { namespace, name } = &self.log_target else {
            return;
        };
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let (namespace, name) = (namespace.clone(), name.clone());
        let tx = self.tx.clone();
        self.runtime.spawn(async move {
            let Ok(handle) = core.clusters.require(&cluster) else {
                return;
            };
            if let Ok(found) = logs::containers(&handle, &namespace, &name).await {
                let names = found
                    .iter()
                    .filter(|container| container.role != "init")
                    .map(|container| SharedString::from(container.name.as_str()))
                    .collect();
                // The container list belongs to no stream, so it is sent under
                // a generation the handler never compares.
                let _ = tx.unbounded_send((ANY_GENERATION, Message::Containers(names)));
            }
        });
    }

    /// (Re)start the stream with the current options, discarding what was shown.
    fn restart(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation += 1;
        self.buffer.clear();
        self.status = Some("Starting…".into());

        let options = LogOptions {
            container: self.container.as_ref().map(ToString::to_string),
            follow: !self.previous,
            tail_lines: Some(self.core.preferences().log_tail_lines),
            since_seconds: None,
            timestamps: self.timestamps,
            previous: self.previous,
        };
        self.task = Some(self.runtime.spawn(stream_logs(
            Arc::clone(&self.core),
            self.cluster.clone(),
            self.log_target.clone(),
            options,
            self.generation,
            self.tx.clone(),
        )));
        cx.notify();
    }

    fn on_message(&mut self, generation: u64, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Containers(names) => {
                // The stream already runs on the pod's default container, which
                // is the first one listed, so only the highlight needs setting.
                if self.container.is_none() {
                    self.container = names.first().cloned();
                }
                self.containers = names;
            }
            _ if generation != self.generation => return,
            Message::Batch(events) => {
                self.buffer.push_all(&events);
                self.status = None;
                if self.follow && self.buffer.visible_len() > 0 {
                    self.scroll
                        .scroll_to_item(self.buffer.visible_len() - 1, ScrollStrategy::Bottom);
                }
            }
            Message::Failed(message) => self.status = Some(message.into()),
            Message::Ended => {
                if self.buffer.is_empty() {
                    self.status = Some("No log lines.".into());
                }
            }
        }
        cx.notify();
    }

    fn chip(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .border_1()
            .border_color(theme.border)
            .when(active, |chip| chip.bg(theme.secondary).font_semibold())
            .child(label.into())
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut toolbar = h_flex()
            .flex_none()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .flex_wrap();

        for (index, name) in self.containers.iter().enumerate() {
            let selected = self.container.as_ref() == Some(name);
            let name = name.clone();
            toolbar = toolbar.child(
                self.chip(("container", index), name.clone(), selected, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.container = Some(name.clone());
                        this.restart(cx);
                    })),
            );
        }

        let is_pod = matches!(self.log_target, LogTarget::Pod { .. });
        let filtering = self.buffer.is_filtering();
        let counts: SharedString =
            format!("{} of {}", self.buffer.visible_len(), self.buffer.len()).into();
        let muted = cx.theme().muted_foreground;
        toolbar
            .child(div().w(px(180.)).child(Input::new(&self.filter)))
            .when(filtering, |bar| {
                bar.child(div().text_xs().text_color(muted).child(counts))
            })
            .when(is_pod, |bar| {
                bar.child(
                    self.chip("previous", "Previous", self.previous, cx)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.previous = !this.previous;
                            this.restart(cx);
                        })),
                )
            })
            .child(
                self.chip("timestamps", "Timestamps", self.timestamps, cx)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.timestamps = !this.timestamps;
                        this.restart(cx);
                    })),
            )
            .child(
                self.chip("follow", "Follow", self.follow, cx)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.follow = !this.follow;
                        cx.notify();
                    })),
            )
            .child(self.chip("clear", "Clear", false, cx).on_click(cx.listener(
                |this, _, _, cx| {
                    this.buffer.clear();
                    cx.notify();
                },
            )))
            .child(
                self.chip(
                    "select",
                    if self.selecting { "Live" } else { "Select" },
                    self.selecting,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_select(window, cx))),
            )
            .child(
                self.chip("copy", "Copy", false, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.copy_all(window, cx))),
            )
            .child(
                self.chip("save", "Save…", false, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
    }

    /// Freeze what is shown into text that can be selected with the mouse, or
    /// go back to the live list.
    fn toggle_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            let text = self.buffer.to_text();
            let last = u32::try_from(self.buffer.visible_len().saturating_sub(1)).unwrap_or(0);
            self.viewer.update(cx, |viewer, cx| {
                viewer.set_value(text, window, cx);
                // The newest lines are at the bottom, where a tail is read.
                viewer.set_cursor_position(Position::new(last, 0), window, cx);
            });
        }
        self.selecting = !self.selecting;
        cx.notify();
    }

    /// Put the lines on screen (those matching the filter, if there is one) on
    /// the clipboard.
    fn copy_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.buffer.visible_len();
        if count == 0 {
            toast_error(window, cx, "Nothing to copy", "No log lines are shown.");
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.to_text()));
        toast_success(window, cx, format!("Copied {count} lines"));
    }

    /// Write the lines on screen (those matching the filter, if there is one) to
    /// a file the person picks.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.visible_len() == 0 {
            toast_error(window, cx, "Nothing to save", "No log lines are shown.");
            return;
        }
        let text = self.buffer.to_text();
        // Windows has no HOME; without this the dialog opened at the drive root.
        let directory = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let chosen = cx.prompt_for_new_path(&directory, Some(&format!("{}.log", self.name)));
        let runtime = self.runtime.clone();
        cx.spawn_in(window, async move |_, cx| {
            let Some(path) = chosen.await.ok().and_then(Result::ok).flatten() else {
                return;
            };
            let target = path.clone();
            let task = runtime.spawn(async move { tokio::fs::write(target, text).await });
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(Ok(())) => toast_success(window, cx, format!("Saved {}", path.display())),
                Ok(Err(err)) => toast_error(window, cx, "Could not save the log", err.to_string()),
                Err(err) => toast_error(window, cx, "Could not save the log", err.to_string()),
            })
            .ok();
        })
        .detach();
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, mono) = (
            theme.muted_foreground,
            theme.danger,
            theme.mono_font_family.clone(),
        );

        let body = match &self.status {
            _ if self.selecting => div()
                .flex_1()
                .min_h_0()
                .child(
                    Editor::new(&self.viewer)
                        .appearance(false)
                        .readonly(true)
                        .h(relative(1.)),
                )
                .into_any_element(),
            Some(status) if self.buffer.is_empty() => div()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(status.clone())
                .into_any_element(),
            _ => uniform_list(
                "log-lines",
                self.buffer.visible_len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| {
                            let line = this.buffer.visible(index)?;
                            let color = match line.kind {
                                LineKind::Output => None,
                                LineKind::Notice => Some(muted),
                                LineKind::Error => Some(danger),
                            };
                            let copy = match &line.pod {
                                Some(pod) => format!("{pod} {}", line.text),
                                None => line.text.to_string(),
                            };
                            Some(
                                h_flex()
                                    .id(("line", index))
                                    .w_full()
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy.clone(),
                                        ));
                                        toast_success(window, cx, "Copied the line");
                                    }))
                                    .h(ROW_HEIGHT)
                                    .px_3()
                                    .gap_2()
                                    .items_center()
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .when_some(color, |row, color| row.text_color(color))
                                    .when_some(line.pod.clone(), |row, pod| {
                                        row.child(
                                            div()
                                                .flex_none()
                                                .max_w(px(160.))
                                                .truncate()
                                                .text_color(muted)
                                                .child(pod),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(line.text.clone()),
                                    ),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element(),
        };

        v_flex()
            .size_full()
            .child(self.render_toolbar(cx))
            .child(body)
    }
}

/// Run one log stream and forward its batches until it ends or is aborted.
async fn stream_logs(
    core: Arc<AppCore>,
    cluster: SharedString,
    target: LogTarget,
    options: LogOptions,
    generation: u64,
    tx: mpsc::UnboundedSender<(u64, Message)>,
) {
    let session = match core.clusters.require(&cluster) {
        Ok(handle) => core.logs.start(&handle, target, options).await,
        Err(err) => Err(err.into()),
    };
    let session = match session {
        Ok(session) => session,
        Err(err) => {
            let _ = tx.unbounded_send((generation, Message::Failed(err.to_string())));
            return;
        }
    };
    // Dropping the session on return or abort stops its streams.
    while let Some(batch) = session.next_batch().await {
        if tx
            .unbounded_send((generation, Message::Batch(batch)))
            .is_err()
        {
            return;
        }
    }
    let _ = tx.unbounded_send((generation, Message::Ended));
}
