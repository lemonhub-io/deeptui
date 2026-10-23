//! The palette. Every style the UI paints with lives here.

use ratatui::style::{Color, Modifier, Style};

// A soft blue stays legible on dark terminals without the glare of pure cyan.
pub const ACCENT: Color = Color::Rgb(96, 165, 250);
pub const ACCENT_LIGHT: Color = Color::Rgb(147, 197, 253);

pub const C_DIM: Style = Style::new().fg(Color::DarkGray);
pub const C_USER: Style = Style::new().fg(ACCENT);
pub const C_USER_MARK: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
pub const C_TOOL: Style = Style::new().fg(ACCENT_LIGHT);
pub const C_TOOL_NAME: Style = Style::new().fg(ACCENT_LIGHT).add_modifier(Modifier::BOLD);
pub const C_ERR: Style = Style::new().fg(Color::Red);
pub const C_OK: Style = Style::new().fg(Color::Green);
pub const C_CODE: Style = Style::new().fg(Color::Gray);
pub const C_HEAD: Style = Style::new()
    .fg(ACCENT)
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::UNDERLINED);
pub const C_H2: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
pub const C_H3: Style = Style::new().add_modifier(Modifier::BOLD);
pub const C_LINK: Style = Style::new().fg(ACCENT).add_modifier(Modifier::UNDERLINED);
pub const C_ASSIST: Style = Style::new().fg(ACCENT_LIGHT);
pub const C_GUTTER: Style = Style::new().fg(Color::Indexed(240));
pub const C_ICODE: Style = Style::new().fg(Color::Yellow);
pub const C_TITLE: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
pub const BAR_BG: Color = Color::Rgb(23, 37, 61);
pub const C_BAR: Style = Style::new().bg(BAR_BG).fg(Color::Gray);
pub const C_BAR_HI: Style = Style::new().bg(BAR_BG).fg(ACCENT_LIGHT);
pub const C_BAR_WARN: Style = Style::new().bg(BAR_BG).fg(Color::Yellow);
pub const SPIN: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
