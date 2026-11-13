//! The app's look: macOS system colours, the system font, and rounded
//! controls, in light and dark.
//!
//! Colours are set on top of the component library's theme after every mode
//! change, so the whole tree (tables, inputs, menus) picks them up without any
//! per-view styling.

use gpui_kit::{component::Theme, component::ThemeMode, *};

/// A fixed palette for one appearance. Every colour is spelled out so the look
/// does not drift when the component library's defaults change.
struct Palette {
    window: u32,
    content: u32,
    sidebar: u32,
    popover: u32,
    text: u32,
    text_muted: u32,
    /// An `RRGGBBAA` overlay used for borders and subtle fills.
    line: u32,
    fill: u32,
    fill_hover: u32,
    fill_active: u32,
    accent: u32,
    accent_hover: u32,
    accent_active: u32,
    green: u32,
    orange: u32,
    red: u32,
    blue: u32,
}

const DARK: Palette = Palette {
    window: 0x1c1c1e,
    content: 0x1e1e20,
    sidebar: 0x262629,
    popover: 0x2c2c2e,
    text: 0xf5f5f7,
    text_muted: 0x9a9aa1,
    line: 0xffffff1c,
    fill: 0xffffff12,
    fill_hover: 0xffffff1f,
    fill_active: 0xffffff2a,
    accent: 0x0a84ff,
    accent_hover: 0x409cff,
    accent_active: 0x0064d1,
    green: 0x30d158,
    orange: 0xff9f0a,
    red: 0xff453a,
    blue: 0x64d2ff,
};

const LIGHT: Palette = Palette {
    window: 0xf5f5f7,
    content: 0xffffff,
    sidebar: 0xebebf0,
    popover: 0xffffff,
    text: 0x1d1d1f,
    text_muted: 0x6e6e73,
    line: 0x00000019,
    fill: 0x0000000d,
    fill_hover: 0x00000017,
    fill_active: 0x00000022,
    accent: 0x007aff,
    accent_hover: 0x2e8fff,
    accent_active: 0x0062cc,
    green: 0x28a745,
    orange: 0xe08600,
    red: 0xe5342b,
    blue: 0x32a8d8,
};

/// An opaque `RRGGBB` colour.
fn colour(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// A translucent `RRGGBBAA` colour. Kept apart from [`colour`] because a literal
/// such as `0x00000019` cannot be told from the opaque `0x000019` by value.
fn overlay(hex: u32) -> Hsla {
    rgba(hex).into()
}

/// Follow the system appearance, then lay the palette over it.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    Theme::sync_system_appearance(window, cx);
    restyle(cx);
}

fn restyle(cx: &mut App) {
    // The mode is read after `sync`, so this edit sees the final value.
    Theme::update(cx, |theme| {
        let p = if matches!(theme.mode, ThemeMode::Dark) {
            &DARK
        } else {
            &LIGHT
        };
        let (c, t) = (colour, overlay);

        theme.font_size = px(14.);
        theme.mono_font_size = px(12.);
        theme.radius = px(8.);
        theme.radius_lg = px(12.);

        theme.background = c(p.content);
        theme.foreground = c(p.text);
        theme.muted = t(p.fill);
        theme.muted_foreground = c(p.text_muted);
        theme.border = t(p.line);
        theme.input = t(p.line);
        theme.ring = c(p.accent);
        theme.caret = c(p.accent);
        theme.selection = c(p.accent).opacity(0.3);
        theme.overlay = t(0x00000066);
        theme.window_border = t(p.line);

        theme.title_bar = c(p.window);
        theme.title_bar_border = t(p.line);
        theme.status_bar = c(p.window);
        theme.status_bar_border = t(p.line);

        theme.sidebar = c(p.sidebar);
        theme.sidebar_foreground = c(p.text);
        theme.sidebar_border = t(p.line);
        theme.sidebar_accent = t(p.fill_hover);
        theme.sidebar_accent_foreground = c(p.text);
        theme.sidebar_primary = c(p.accent);
        theme.sidebar_primary_foreground = c(0xffffff);

        theme.popover = c(p.popover);
        theme.popover_foreground = c(p.text);

        theme.primary = c(p.accent);
        theme.primary_hover = c(p.accent_hover);
        theme.primary_active = c(p.accent_active);
        theme.primary_foreground = c(0xffffff);
        theme.accent = t(p.fill_hover);
        theme.accent_foreground = c(p.text);
        theme.link = c(p.accent);
        theme.link_hover = c(p.accent_hover);
        theme.link_active = c(p.accent_active);

        theme.secondary = t(p.fill);
        theme.secondary_hover = t(p.fill_hover);
        theme.secondary_active = t(p.fill_active);
        theme.secondary_foreground = c(p.text);

        theme.success = c(p.green);
        theme.success_foreground = c(0xffffff);
        theme.warning = c(p.orange);
        theme.warning_foreground = c(0xffffff);
        theme.danger = c(p.red);
        theme.danger_foreground = c(0xffffff);
        theme.info = c(p.blue);
        theme.info_foreground = c(0xffffff);

        // `Theme` has its own `list` (settings), which hides the colour.
        theme.colors.list = c(p.content);
        theme.list_even = c(p.content);
        theme.list_head = c(p.content);
        theme.list_hover = t(p.fill);
        theme.list_active = c(p.accent).opacity(0.22);
        theme.list_active_border = c(p.accent);

        theme.table = c(p.content);
        theme.table_even = t(p.fill).opacity(0.4);
        theme.table_head = c(p.content);
        theme.table_head_foreground = c(p.text_muted);
        theme.table_hover = t(p.fill);
        theme.table_active = c(p.accent).opacity(0.22);
        theme.table_active_border = c(p.accent);
        theme.table_row_border = t(p.line).opacity(0.5);

        theme.tab_bar = c(p.content);
        theme.tab_bar_segmented = t(p.fill);
        theme.scrollbar_thumb = c(p.text_muted).opacity(0.45);
        theme.scrollbar_thumb_hover = c(p.text_muted).opacity(0.7);
    });
}

#[cfg(test)]
mod tests {
    use super::{colour, overlay};

    #[test]
    fn opaque_and_translucent_literals_keep_their_alpha() {
        assert!((colour(0x007aff).a - 1.0).abs() < f32::EPSILON);
        let translucent = overlay(0xffffff1c);
        assert!(translucent.a < 0.2 && translucent.a > 0.0);
        // Black with alpha: the value is small, but it is still translucent.
        let black = overlay(0x00000019);
        assert!(black.a < 0.2 && black.a > 0.0);
    }
}
