//! A terminal emulator: bytes from a pty in, a grid of coloured text out.
//!
//! A thin wrapper over `alacritty_terminal` that hands the renderer a [`Frame`]
//! of runs rather than a cell at a time. No window system here, so the escape
//! sequence handling is unit-tested directly.

use std::{cell::RefCell, ops::Range, rc::Rc};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    term::{Config, Term, TermMode, cell::Flags},
    vte::ansi::{Color, CursorShape, NamedColor, Processor},
};

/// Scrollback kept. Much of a long session is output nobody scrolls back to.
const HISTORY: usize = 5_000;

/// A colour as the renderer should draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    /// The terminal's default text colour.
    Foreground,
    /// The terminal's default background.
    Background,
    Rgb(u8, u8, u8),
}

/// Text attributes that change how a run is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub dim: bool,
}

/// A stretch of cells on one line that share colours and attributes.
#[derive(Debug, Clone)]
pub struct Run {
    pub line: usize,
    pub column: usize,
    /// Width in columns; a wide character counts as two.
    pub cells: usize,
    /// Range into [`Frame::arena`].
    pub text: Range<usize>,
    pub fg: Paint,
    pub bg: Paint,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    pub column: usize,
}

/// What to draw. Reused from frame to frame, so a steady terminal allocates
/// nothing per paint.
#[derive(Default)]
pub struct Frame {
    pub arena: String,
    pub runs: Vec<Run>,
    pub cursor: Option<Cursor>,
}

impl Frame {
    pub fn text(&self, run: &Run) -> &str {
        self.arena.get(run.text.clone()).unwrap_or_default()
    }
}

/// Collects what the terminal asks to be written back to the pty, such as the
/// reply to a cursor-position query. Programs such as `vim` and `htop` wait for
/// these and hang without them.
#[derive(Clone, Default)]
struct Replies(Rc<RefCell<Vec<u8>>>);

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.0.borrow_mut().extend_from_slice(text.as_bytes());
        }
    }
}

struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct Emulator {
    term: Term<Replies>,
    parser: Processor,
    replies: Replies,
}

impl Emulator {
    pub fn new(columns: usize, lines: usize) -> Self {
        let replies = Replies::default();
        let config = Config {
            scrolling_history: HISTORY,
            ..Config::default()
        };
        let size = Size {
            columns: columns.max(2),
            lines: lines.max(1),
        };
        Self {
            term: Term::new(config, &size, replies.clone()),
            parser: Processor::new(),
            replies,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Bytes the terminal wants sent back to the program.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.replies.0.borrow_mut())
    }

    pub fn resize(&mut self, columns: usize, lines: usize) {
        let (columns, lines) = (columns.max(2), lines.max(1));
        if (columns, lines) != (self.term.columns(), self.term.screen_lines()) {
            self.term.resize(Size { columns, lines });
        }
    }

    /// Scroll the view back through history (positive) or toward the present.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Cursor keys send `ESC O A` instead of `ESC [ A` while an application
    /// such as `less` or `vim` has asked for it.
    pub fn app_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    /// Whether pasted text should be wrapped so the program can tell it from
    /// typing.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    #[cfg(test)]
    pub fn columns(&self) -> usize {
        self.term.columns()
    }

    #[cfg(test)]
    pub fn lines(&self) -> usize {
        self.term.screen_lines()
    }

    /// Rebuild `frame` from the visible grid.
    pub fn fill(&self, frame: &mut Frame) {
        frame.arena.clear();
        frame.runs.clear();

        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let mut open: Option<Run> = None;

        for indexed in content.display_iter {
            let cell = indexed.cell;
            let flags = cell.flags;
            // The second half of a wide character is already drawn by the first.
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let line = (indexed.point.line.0 + offset).max(0) as usize;
            let column = indexed.point.column.0;

            let (mut fg, mut bg) = (
                resolve(cell.fg, Paint::Foreground),
                resolve(cell.bg, Paint::Background),
            );
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let style = Style {
                bold: flags.intersects(Flags::BOLD),
                italic: flags.intersects(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikeout: flags.intersects(Flags::STRIKEOUT),
                dim: flags.intersects(Flags::DIM),
            };
            let width = if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let ch = if flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };

            let extends = open.as_ref().is_some_and(|run| {
                run.line == line
                    && run.column + run.cells == column
                    && run.fg == fg
                    && run.bg == bg
                    && run.style == style
            });
            if !extends {
                flush(&mut open, frame);
                open = Some(Run {
                    line,
                    column,
                    cells: 0,
                    text: frame.arena.len()..frame.arena.len(),
                    fg,
                    bg,
                    style,
                });
            }
            if let Some(run) = &mut open {
                frame.arena.push(ch);
                run.cells += width;
                run.text.end = frame.arena.len();
            }
        }
        flush(&mut open, frame);

        let cursor = content.cursor;
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR)
            && content.display_offset == 0
            && cursor.shape != CursorShape::Hidden;
        frame.cursor = visible.then(|| Cursor {
            line: cursor.point.line.0.max(0) as usize,
            column: cursor.point.column.0,
        });
    }
}

/// Finish a run: drop the blanks that would draw nothing, and the run itself if
/// that leaves nothing.
///
/// Spaces on the default background with no underline or strikeout are
/// invisible, and a line is padded with them out to the terminal width, so
/// without this every line would carry its full width in text.
fn flush(open: &mut Option<Run>, frame: &mut Frame) {
    let Some(mut run) = open.take() else {
        return;
    };
    let invisible_blanks =
        run.bg == Paint::Background && !run.style.underline && !run.style.strikeout;
    if invisible_blanks {
        let kept = frame
            .arena
            .get(run.text.clone())
            .map_or(0, |text| text.trim_end_matches([' ', '\0']).len());
        // Each trimmed blank was one column wide.
        run.cells -= frame.arena.len() - (run.text.start + kept);
        run.text.end = run.text.start + kept;
        frame.arena.truncate(run.text.end);
        if kept == 0 {
            return;
        }
    }
    frame.runs.push(run);
}

/// The 16 ANSI colours, in the order the escape sequences index them.
const PALETTE: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x31, 0x31),
    (0x0d, 0xbc, 0x79),
    (0xe5, 0xe5, 0x10),
    (0x24, 0x72, 0xc8),
    (0xbc, 0x3f, 0xbc),
    (0x11, 0xa8, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x66, 0x66, 0x66),
    (0xf1, 0x4c, 0x4c),
    (0x23, 0xd1, 0x8b),
    (0xf5, 0xf5, 0x43),
    (0x3b, 0x8e, 0xea),
    (0xd6, 0x70, 0xd6),
    (0x29, 0xb8, 0xdb),
    (0xff, 0xff, 0xff),
];

fn indexed(index: u8) -> Paint {
    let (r, g, b) = match index {
        0..=15 => PALETTE[index as usize],
        // The 6x6x6 colour cube.
        16..=231 => {
            let n = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        // The grey ramp.
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    };
    Paint::Rgb(r, g, b)
}

fn resolve(color: Color, default: Paint) -> Paint {
    match color {
        Color::Spec(rgb) => Paint::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => indexed(index),
        Color::Named(named) => match named {
            NamedColor::Black => indexed(0),
            NamedColor::Red => indexed(1),
            NamedColor::Green => indexed(2),
            NamedColor::Yellow => indexed(3),
            NamedColor::Blue => indexed(4),
            NamedColor::Magenta => indexed(5),
            NamedColor::Cyan => indexed(6),
            NamedColor::White => indexed(7),
            NamedColor::BrightBlack | NamedColor::DimBlack => indexed(8),
            NamedColor::BrightRed | NamedColor::DimRed => indexed(9),
            NamedColor::BrightGreen | NamedColor::DimGreen => indexed(10),
            NamedColor::BrightYellow | NamedColor::DimYellow => indexed(11),
            NamedColor::BrightBlue | NamedColor::DimBlue => indexed(12),
            NamedColor::BrightMagenta | NamedColor::DimMagenta => indexed(13),
            NamedColor::BrightCyan | NamedColor::DimCyan => indexed(14),
            NamedColor::BrightWhite | NamedColor::DimWhite => indexed(15),
            // Foreground, background, cursor and their bright/dim forms.
            _ => default,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Emulator, Frame, Paint};

    fn render(emulator: &Emulator) -> Frame {
        let mut frame = Frame::default();
        emulator.fill(&mut frame);
        frame
    }

    fn lines_of(frame: &Frame) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for run in &frame.runs {
            while lines.len() <= run.line {
                lines.push(String::new());
            }
            let text = frame.text(run);
            let line = &mut lines[run.line];
            while line.chars().count() < run.column {
                line.push(' ');
            }
            line.push_str(text);
        }
        lines
    }

    #[test]
    fn plain_text_lands_on_the_right_lines() {
        let mut emulator = Emulator::new(20, 5);
        emulator.feed(b"hello\r\nworld");
        let frame = render(&emulator);
        assert_eq!(lines_of(&frame), ["hello", "world"]);
        assert_eq!(frame.cursor.map(|c| (c.line, c.column)), Some((1, 5)));
    }

    #[test]
    fn colour_escapes_split_runs_and_set_their_paint() {
        let mut emulator = Emulator::new(20, 3);
        emulator.feed(b"ok \x1b[31mred\x1b[0m done");
        let frame = render(&emulator);

        let red = frame.runs.iter().find(|run| frame.text(run) == "red");
        assert!(matches!(
            red.map(|r| r.fg),
            Some(Paint::Rgb(0xcd, 0x31, 0x31))
        ));
        let plain = frame
            .runs
            .iter()
            .find(|run| frame.text(run).starts_with("ok"));
        assert_eq!(plain.map(|r| r.fg), Some(Paint::Foreground));
    }

    #[test]
    fn truecolor_and_256_colour_escapes_are_resolved() {
        let mut emulator = Emulator::new(30, 3);
        emulator.feed(b"\x1b[38;2;10;20;30mT\x1b[0m\x1b[38;5;196mI\x1b[0m");
        let frame = render(&emulator);
        assert_eq!(frame.runs[0].fg, Paint::Rgb(10, 20, 30));
        assert_eq!(
            frame.runs[1].fg,
            Paint::Rgb(255, 0, 0),
            "index 196 is pure red"
        );
    }

    #[test]
    fn inverse_video_swaps_foreground_and_background() {
        let mut emulator = Emulator::new(10, 2);
        emulator.feed(b"\x1b[7mX");
        let frame = render(&emulator);
        assert_eq!(frame.runs[0].fg, Paint::Background);
        assert_eq!(frame.runs[0].bg, Paint::Foreground);
    }

    #[test]
    fn long_lines_wrap_at_the_terminal_width() {
        let mut emulator = Emulator::new(5, 4);
        emulator.feed(b"abcdefgh");
        assert_eq!(lines_of(&render(&emulator)), ["abcde", "fgh"]);
    }

    #[test]
    fn blank_default_cells_draw_nothing() {
        let mut emulator = Emulator::new(40, 5);
        emulator.feed(b"a");
        assert_eq!(
            render(&emulator).runs.len(),
            1,
            "no runs for the empty rest of the grid"
        );
    }

    #[test]
    fn a_cursor_position_query_is_answered_for_the_program() {
        let mut emulator = Emulator::new(20, 5);
        emulator.feed(b"ab\x1b[6n");
        assert_eq!(emulator.take_replies(), b"\x1b[1;3R");
        assert!(
            emulator.take_replies().is_empty(),
            "a reply is delivered once"
        );
    }

    #[test]
    fn application_cursor_and_bracketed_paste_modes_follow_the_program() {
        let mut emulator = Emulator::new(20, 5);
        assert!(!emulator.app_cursor() && !emulator.bracketed_paste());
        emulator.feed(b"\x1b[?1h\x1b[?2004h");
        assert!(emulator.app_cursor() && emulator.bracketed_paste());
        emulator.feed(b"\x1b[?1l\x1b[?2004l");
        assert!(!emulator.app_cursor() && !emulator.bracketed_paste());
    }

    #[test]
    fn scrolling_back_shows_older_lines_and_hides_the_cursor() {
        let mut emulator = Emulator::new(10, 3);
        for n in 0..10 {
            emulator.feed(format!("line{n}\r\n").as_bytes());
        }
        assert!(lines_of(&render(&emulator)).iter().any(|l| l == "line9"));

        emulator.scroll(5);
        let frame = render(&emulator);
        assert!(lines_of(&frame).iter().all(|l| l != "line9"));
        assert!(
            frame.cursor.is_none(),
            "the cursor belongs to the live screen"
        );

        emulator.scroll(-100);
        assert!(render(&emulator).cursor.is_some());
    }

    #[test]
    fn resizing_changes_the_grid_without_losing_text() {
        let mut emulator = Emulator::new(10, 3);
        emulator.feed(b"keep");
        emulator.resize(30, 8);
        assert_eq!((emulator.columns(), emulator.lines()), (30, 8));
        assert_eq!(lines_of(&render(&emulator))[0], "keep");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let mut emulator = Emulator::new(10, 2);
        emulator.feed("a日b".as_bytes());
        let frame = render(&emulator);
        assert_eq!(frame.runs[0].cells, 4, "a, the wide character twice, and b");
        assert_eq!(frame.text(&frame.runs[0]), "a日b");
    }
}
