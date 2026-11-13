//! Turning a key press into the bytes a terminal program expects.
//!
//! Pure functions over GPUI's `Keystroke`, so the table is unit-tested without
//! a window.

use std::borrow::Cow;

use gpui_kit::{Keystroke, Modifiers};

/// The bytes for a key press, or `None` when the key means nothing to the
/// program (a bare modifier, or a command shortcut the app keeps for itself).
///
/// `app_cursor` is the terminal's application-cursor mode: cursor keys send
/// `ESC O A` instead of `ESC [ A` while a program such as `less` has asked.
pub fn encode(keystroke: &Keystroke, app_cursor: bool) -> Option<Cow<'static, [u8]>> {
    let modifiers = keystroke.modifiers;
    // Command is reserved for the app: copy, paste, quit.
    if modifiers.platform {
        return None;
    }
    let key = keystroke.key.as_str();

    if let Some(bytes) = named(key, modifiers, app_cursor) {
        return Some(bytes);
    }
    if modifiers.control {
        return control(key).map(|byte| Cow::Owned(prefixed(modifiers.alt, &[byte])));
    }

    // Ordinary text, including whatever shift and the keyboard layout made of
    // the key.
    let text = keystroke
        .key_char
        .as_deref()
        .or_else(|| (key.chars().count() == 1).then_some(key))?;
    Some(Cow::Owned(prefixed(modifiers.alt, text.as_bytes())))
}

/// Alt sends the key with an escape in front of it.
fn prefixed(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

/// The control character for `ctrl+key`.
fn control(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let (first, rest) = (chars.next()?, chars.next());
    match (first, rest) {
        (letter @ 'a'..='z', None) => Some(letter as u8 - b'a' + 1),
        ('@' | ' ', None) => Some(0),
        ('[', None) => Some(0x1b),
        ('\\', None) => Some(0x1c),
        (']', None) => Some(0x1d),
        ('^', None) => Some(0x1e),
        ('_' | '/', None) => Some(0x1f),
        _ => {
            // `space` arrives by name.
            (key == "space").then_some(0)
        }
    }
}

/// xterm's modifier parameter: 1 plus shift, alt and control.
fn modifier_code(modifiers: Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

fn named(key: &str, modifiers: Modifiers, app_cursor: bool) -> Option<Cow<'static, [u8]>> {
    let plain = modifier_code(modifiers) == 1;

    // Cursor keys and Home/End: `ESC [ A`, `ESC O A` in application mode, and
    // `ESC [ 1 ; m A` when a modifier is held.
    let cursor = |letter: u8| -> Cow<'static, [u8]> {
        if !plain {
            Cow::Owned(
                format!("\x1b[1;{}{}", modifier_code(modifiers), letter as char).into_bytes(),
            )
        } else if app_cursor {
            Cow::Owned(vec![0x1b, b'O', letter])
        } else {
            Cow::Owned(vec![0x1b, b'[', letter])
        }
    };
    // `ESC [ n ~` keys, which take the modifier as a second parameter.
    let tilde = |number: u8| -> Cow<'static, [u8]> {
        if plain {
            Cow::Owned(format!("\x1b[{number}~").into_bytes())
        } else {
            Cow::Owned(format!("\x1b[{number};{}~", modifier_code(modifiers)).into_bytes())
        }
    };

    Some(match key {
        "enter" => Cow::Borrowed(b"\r"),
        "escape" => Cow::Borrowed(b"\x1b"),
        "backspace" => Cow::Borrowed(b"\x7f"),
        "tab" if modifiers.shift => Cow::Borrowed(b"\x1b[Z"),
        "tab" => Cow::Borrowed(b"\t"),
        "space" if !modifiers.control => Cow::Borrowed(b" "),
        "up" => cursor(b'A'),
        "down" => cursor(b'B'),
        "right" => cursor(b'C'),
        "left" => cursor(b'D'),
        "home" => cursor(b'H'),
        "end" => cursor(b'F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => Cow::Borrowed(b"\x1bOP"),
        "f2" => Cow::Borrowed(b"\x1bOQ"),
        "f3" => Cow::Borrowed(b"\x1bOR"),
        "f4" => Cow::Borrowed(b"\x1bOS"),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::encode;
    use gpui_kit::{Keystroke, Modifiers};

    fn press(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn bytes(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
        encode(keystroke, app_cursor).map(|b| b.into_owned())
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Modifiers::default()
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn printable_text_is_sent_as_typed() {
        let a = press("a", Some("a"), Modifiers::default());
        assert_eq!(bytes(&a, false), Some(b"a".to_vec()));
        let shifted = press(
            "a",
            Some("A"),
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(bytes(&shifted, false), Some(b"A".to_vec()));
        let unicode = press("é", Some("é"), Modifiers::default());
        assert_eq!(bytes(&unicode, false), Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn editing_keys_send_their_usual_codes() {
        let key = |name: &str| bytes(&press(name, None, Modifiers::default()), false);
        assert_eq!(key("enter"), Some(b"\r".to_vec()));
        assert_eq!(key("backspace"), Some(vec![0x7f]));
        assert_eq!(key("tab"), Some(b"\t".to_vec()));
        assert_eq!(key("escape"), Some(vec![0x1b]));
        assert_eq!(key("delete"), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key("pageup"), Some(b"\x1b[5~".to_vec()));
        assert_eq!(key("f5"), Some(b"\x1b[15~".to_vec()));
        assert_eq!(key("space"), Some(b" ".to_vec()));
    }

    #[test]
    fn shift_tab_is_back_tab() {
        let tab = press(
            "tab",
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(bytes(&tab, false), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn control_letters_become_control_characters() {
        assert_eq!(bytes(&press("c", Some("c"), ctrl()), false), Some(vec![3]));
        assert_eq!(bytes(&press("d", Some("d"), ctrl()), false), Some(vec![4]));
        assert_eq!(bytes(&press("z", Some("z"), ctrl()), false), Some(vec![26]));
        assert_eq!(
            bytes(&press("[", Some("["), ctrl()), false),
            Some(vec![0x1b])
        );
        assert_eq!(bytes(&press("space", None, ctrl()), false), Some(vec![0]));
    }

    #[test]
    fn alt_prefixes_an_escape() {
        assert_eq!(
            bytes(&press("b", Some("b"), alt()), false),
            Some(b"\x1bb".to_vec())
        );
    }

    #[test]
    fn cursor_keys_follow_application_mode_and_modifiers() {
        let up = press("up", None, Modifiers::default());
        assert_eq!(bytes(&up, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(bytes(&up, true), Some(b"\x1bOA".to_vec()));

        let ctrl_right = press("right", None, ctrl());
        assert_eq!(bytes(&ctrl_right, false), Some(b"\x1b[1;5C".to_vec()));
        let shift_delete = press(
            "delete",
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(bytes(&shift_delete, false), Some(b"\x1b[3;2~".to_vec()));
    }

    #[test]
    fn command_shortcuts_and_bare_modifiers_send_nothing() {
        let cmd_v = press(
            "v",
            Some("v"),
            Modifiers {
                platform: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(bytes(&cmd_v, false), None);
        assert_eq!(
            bytes(&press("shift", None, Modifiers::default()), false),
            None
        );
    }
}
