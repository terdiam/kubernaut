//! The Terminal tab: a shell in a pod, drawn on a custom GPUI element.
//!
//! Output flows pty → tokio task → `Emulator`; keys flow back through a channel
//! to a second task, so keystrokes are written in the order they were typed.

use std::{cell::RefCell, sync::Arc};

use futures::{StreamExt, channel::mpsc};
use gpui_kit::{
    component::{ActiveTheme, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::AppCore;
use k8s_ops::exec::{ExecOptions, TerminalEvent};
use tokio::{runtime::Handle, sync::mpsc as tokio_mpsc, task::JoinHandle};

use crate::{
    drawer::Target,
    emulator::{Emulator, Frame, Paint, Style as RunStyle},
    keys,
};

/// Lines scrolled per wheel notch.
const WHEEL_LINES: i32 = 3;

/// What the UI tells the session task.
enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

/// What the session task tells the UI.
enum Message {
    Events(Vec<TerminalEvent>),
    Failed(String),
    Ended,
}

pub struct TerminalView {
    focus: FocusHandle,
    emulator: Emulator,
    /// Rebuilt on each paint from the emulator; behind a `RefCell` because the
    /// element paints from a shared reference to the view.
    frame: RefCell<Frame>,
    input: tokio_mpsc::UnboundedSender<Input>,
    task: JoinHandle<()>,
    /// The size last sent to the pty, so a resize is sent once, not per paint.
    sent_size: (usize, usize),
    status: Option<SharedString>,
    ended: bool,
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // Dropping the session on abort closes the remote shell.
        self.task.abort();
    }
}

impl TerminalView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        namespace: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded::<Message>();
        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_message(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let (input, input_rx) = tokio_mpsc::unbounded_channel();
        let options = ExecOptions {
            namespace,
            pod: target.name.to_string(),
            container: None,
            command: Vec::new(),
            columns: 80,
            rows: 24,
        };
        let task = runtime.spawn(run_session(
            Arc::clone(core),
            target.cluster.clone(),
            options,
            input_rx,
            tx,
        ));

        Self {
            focus: cx.focus_handle(),
            emulator: Emulator::new(80, 24),
            frame: RefCell::new(Frame::default()),
            input,
            task,
            sent_size: (80, 24),
            status: Some("Connecting…".into()),
            ended: false,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn send(&self, bytes: Vec<u8>) {
        // A closed channel means the session already ended.
        let _ = self.input.send(Input::Bytes(bytes));
    }

    /// Called by the element once it knows how many cells fit.
    fn resize(&mut self, columns: usize, lines: usize) {
        if (columns, lines) == self.sent_size || columns < 2 || lines < 1 {
            return;
        }
        self.sent_size = (columns, lines);
        self.emulator.resize(columns, lines);
        let (Ok(columns), Ok(lines)) = (u16::try_from(columns), u16::try_from(lines)) else {
            return;
        };
        let _ = self.input.send(Input::Resize(columns, lines));
    }

    fn on_message(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Events(events) => {
                for event in events {
                    match event {
                        TerminalEvent::Output { data } => {
                            self.status = None;
                            self.emulator.feed(data.as_bytes());
                            let replies = self.emulator.take_replies();
                            if !replies.is_empty() {
                                self.send(replies);
                            }
                        }
                        TerminalEvent::Status { message } => self.status = Some(message.into()),
                        TerminalEvent::Closed { status } => {
                            self.end(&format!("[process exited: {status}]"));
                        }
                        TerminalEvent::Failed { message } => self.end(&message),
                    }
                }
            }
            Message::Failed(message) => self.end(&message),
            Message::Ended => self.end("[session closed]"),
        }
        cx.notify();
    }

    /// Print a final line in the terminal itself and stop accepting input.
    fn end(&mut self, text: &str) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.status = None;
        self.emulator
            .feed(format!("\r\n\x1b[2m{text}\x1b[0m\r\n").as_bytes());
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform {
            if keystroke.key == "v" {
                self.paste(cx);
            }
            return;
        }
        if self.ended {
            return;
        }
        if let Some(bytes) = keys::encode(keystroke, self.emulator.app_cursor()) {
            // Typing returns the view to the live screen.
            self.emulator.scroll(i32::MIN / 2);
            self.send(bytes.into_owned());
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if self.ended || text.is_empty() {
            return;
        }
        // A terminal expects carriage returns where text has line breaks.
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let mut bytes = Vec::with_capacity(text.len() + 12);
        let bracketed = self.emulator.bracketed_paste();
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        self.send(bytes);
    }

    fn on_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let dy = f32::from(event.delta.pixel_delta(px(1.)).y);
        if dy != 0.0 {
            // Wheel up shows older output.
            let lines = if dy > 0.0 { WHEEL_LINES } else { -WHEEL_LINES };
            self.emulator.scroll(lines);
            cx.notify();
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, muted) = (theme.background, theme.muted_foreground);
        v_flex()
            .size_full()
            .bg(background)
            .when_some(self.status.clone(), |column, status| {
                column.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(muted)
                        .child(status),
                )
            })
            .child(
                div()
                    .id("terminal")
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .track_focus(&self.focus)
                    .key_context("Terminal")
                    .on_key_down(cx.listener(|this, event, _, cx| this.on_key_down(event, cx)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
                    )
                    .on_scroll(cx.listener(|this, event, _, cx| this.on_scroll(event, cx)))
                    .child(TerminalElement { view: cx.entity() }),
            )
    }
}

/// Run one shell session until it ends or is aborted.
async fn run_session(
    core: Arc<AppCore>,
    cluster: SharedString,
    options: ExecOptions,
    mut input: tokio_mpsc::UnboundedReceiver<Input>,
    tx: mpsc::UnboundedSender<Message>,
) {
    let session = match core.clusters.require(&cluster) {
        Ok(handle) => core.terminals.open(&handle, options).await,
        Err(err) => Err(err.into()),
    };
    let session = match session {
        Ok(session) => session,
        Err(err) => {
            let _ = tx.unbounded_send(Message::Failed(err.to_string()));
            return;
        }
    };

    // Writes go through their own task so a slow write never holds up reading
    // output, and keystrokes stay in order.
    let writer = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            while let Some(message) = input.recv().await {
                match message {
                    Input::Bytes(bytes) => {
                        if session.write(bytes).await.is_err() {
                            break;
                        }
                    }
                    Input::Resize(columns, rows) => {
                        let _ = session.resize(columns, rows).await;
                    }
                }
            }
        })
    };

    while let Some(events) = session.next_batch().await {
        if tx.unbounded_send(Message::Events(events)).is_err() {
            break;
        }
    }
    writer.abort();
    let _ = tx.unbounded_send(Message::Ended);
}

/// Measurements taken in `prepaint` and used by `paint`.
struct Layout {
    font: Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
}

struct TerminalElement {
    view: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Layout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = gpui_kit::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Layout {
        let (family, font_size) = {
            let theme = cx.theme();
            (theme.mono_font_family.clone(), theme.mono_font_size)
        };
        let mut font = window.text_style().font();
        font.family = family;

        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .em_advance(font_id, font_size)
            .unwrap_or(font_size * 0.6);
        let line_height = (font_size * 1.35).ceil();

        // How many whole cells fit, which is the size the pty must be told.
        let columns = (bounds.size.width / cell_width).floor() as usize;
        let lines = (bounds.size.height / line_height).floor() as usize;
        self.view.update(cx, |view, _| view.resize(columns, lines));

        Layout {
            font,
            font_size,
            cell_width,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut Layout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (foreground, background) = {
            let theme = cx.theme();
            (theme.foreground, theme.background)
        };
        // Take the frame out of the view so painting can borrow `cx` mutably,
        // and put it back afterwards for the next paint to reuse.
        let (focused, frame) = {
            let view = self.view.read(cx);
            let mut frame = view.frame.take();
            view.emulator.fill(&mut frame);
            (view.focus.is_focused(window), frame)
        };

        let cell = |column: usize, line: usize, cells: usize| Bounds {
            origin: point(
                bounds.origin.x + layout.cell_width * column as f32,
                bounds.origin.y + layout.line_height * line as f32,
            ),
            size: size(layout.cell_width * cells as f32, layout.line_height),
        };
        let paint_of = |paint: Paint, dim: bool| -> Hsla {
            let color = match paint {
                Paint::Foreground => foreground,
                Paint::Background => background,
                Paint::Rgb(r, g, b) => {
                    rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)).into()
                }
            };
            if dim { color.opacity(0.6) } else { color }
        };

        for run in &frame.runs {
            let area = cell(run.column, run.line, run.cells);
            if run.bg != Paint::Background {
                window.paint_quad(fill(area, paint_of(run.bg, false)));
            }

            let text = frame.text(run);
            if text.is_empty() {
                continue;
            }
            let RunStyle {
                bold,
                italic,
                underline,
                strikeout,
                dim,
            } = run.style;
            let color = paint_of(run.fg, dim);
            let mut font = layout.font.clone();
            if bold {
                font.weight = FontWeight::BOLD;
            }
            if italic {
                font.style = FontStyle::Italic;
            }
            let text_run = TextRun {
                len: text.len(),
                font,
                color,
                background_color: None,
                underline: underline.then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: strikeout.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                }),
            };
            // Forcing the cell width keeps wide and box-drawing characters on
            // the grid however the font sizes them.
            let shaped = window.text_system().shape_line(
                SharedString::from(text),
                layout.font_size,
                &[text_run],
                Some(layout.cell_width),
            );
            let _ = shaped.paint(
                area.origin,
                layout.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        if let Some(cursor) = frame.cursor {
            let alpha = if focused { 0.7 } else { 0.3 };
            window.paint_quad(fill(
                cell(cursor.column, cursor.line, 1),
                foreground.opacity(alpha),
            ));
        }
        self.view.read(cx).frame.replace(frame);
    }
}
