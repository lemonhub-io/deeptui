//! Deep blue surfaces, ice-blue accents, and quiet, readable chrome.

use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Rgb(10, 16, 29);
pub const PANEL: Color = Color::Rgb(17, 27, 46);
pub const FG: Color = Color::Rgb(220, 230, 245);
pub const MUTED: Color = Color::Rgb(133, 153, 180);
pub const EDGE: Color = Color::Rgb(43, 62, 88);
pub const ACCENT: Color = Color::Rgb(96, 165, 250);
pub const ACCENT_LIGHT: Color = Color::Rgb(165, 207, 255);

pub const C_DIM: Style = Style::new().fg(MUTED);
pub const C_USER: Style = Style::new().fg(FG);
pub const C_USER_MARK: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
pub const C_TOOL: Style = Style::new().fg(ACCENT);
pub const C_TOOL_NAME: Style = Style::new().fg(ACCENT_LIGHT).add_modifier(Modifier::BOLD);
pub const C_ERR: Style = Style::new().fg(Color::Rgb(251, 132, 145));
pub const C_OK: Style = Style::new().fg(Color::Rgb(110, 211, 179));
pub const C_CODE: Style = Style::new().fg(Color::Rgb(184, 205, 230));
pub const C_HEAD: Style = Style::new().fg(ACCENT_LIGHT).add_modifier(Modifier::BOLD);
pub const C_H2: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
pub const C_H3: Style = Style::new().fg(FG).add_modifier(Modifier::BOLD);
pub const C_LINK: Style = Style::new().fg(ACCENT).add_modifier(Modifier::UNDERLINED);
pub const C_ASSIST: Style = Style::new().fg(ACCENT_LIGHT);
pub const C_GUTTER: Style = Style::new().fg(EDGE);
pub const C_ICODE: Style = Style::new().fg(Color::Rgb(189, 211, 255));
pub const C_TITLE: Style = Style::new().fg(ACCENT_LIGHT).add_modifier(Modifier::BOLD);
pub const BAR_BG: Color = PANEL;
pub const C_BAR: Style = Style::new().bg(BAR_BG).fg(MUTED);
pub const C_BAR_HI: Style = Style::new().bg(BAR_BG).fg(ACCENT_LIGHT);
pub const C_BAR_WARN: Style = Style::new().bg(BAR_BG).fg(Color::Rgb(242, 198, 125));
pub const SPIN: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// A slow breath for the active composer border; no state or timer of its own.
pub fn breathe(seconds: f32) -> Color {
    let t = ((seconds * 2.0).sin() + 1.0) * 0.5;
    Color::Rgb(
        (62.0 + 34.0 * t) as u8,
        (112.0 + 53.0 * t) as u8,
        (183.0 + 67.0 * t) as u8,
    )
}
