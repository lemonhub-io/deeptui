//! Keyboard handling: input editing, history recall, scrolling, quit keys.

use super::{App, UserCmd};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

/// Wrapped row count of the input at inner width `w`.
pub(super) fn input_rows(input: &str, w: usize) -> usize {
    let w = w.max(1);
    input
        .split('\n')
        .map(|l| {
            let lw: usize = l
                .chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            (lw / w) + 1
        })
        .sum()
}

/// (row, col) in wrapped coordinates of a prefix of `input` (used for cursor).
pub(super) fn cursor_pos(prefix: &str, w: usize) -> (usize, usize) {
    let w = w.max(1);
    let (mut row, mut col) = (0usize, 0usize);
    for c in prefix.chars() {
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += UnicodeWidthChar::width(c).unwrap_or(0);
            if col >= w {
                row += 1;
                col = 0;
            }
        }
    }
    (row, col)
}

fn prev_char(s: &str, i: usize) -> (usize, char) {
    let c = s[..i].chars().next_back().unwrap();
    (i - c.len_utf8(), c)
}

/// Byte index of the start of the word before position `i`-ish cursor `s.len()` context.
fn word_back(s: &str) -> usize {
    let mut i = s.len();
    while i > 0 {
        let (start, c) = prev_char(s, i);
        if !c.is_whitespace() {
            break;
        }
        i = start;
    }
    while i > 0 {
        let (start, c) = prev_char(s, i);
        if c.is_whitespace() {
            break;
        }
        i = start;
    }
    i
}

pub(super) fn on_key(app: &mut App, k: KeyEvent, cmd: &mpsc::Sender<UserCmd>) -> bool {
    if k.kind == KeyEventKind::Release {
        return false;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('c') if ctrl => return true,
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return true,
        KeyCode::Char('l') if ctrl => {
            app.log.clear();
            app.wrapped.clear();
            app.wrap_len = 0;
            app.scroll = 0;
            app.in_fence = false;
            app.stream_dirty = true;
        }
        KeyCode::Esc => {
            if app.running {
                let _ = cmd.try_send(UserCmd::Cancel);
            } else if !app.input.is_empty() {
                app.input.clear();
                app.cur = 0;
            }
        }
        KeyCode::Enter if alt || shift => {
            app.input.insert(app.cur, '\n');
            app.cur += 1;
        }
        KeyCode::Enter => {
            let text = app.input.trim_end().to_string();
            if !text.is_empty() {
                let queued = app.running;
                // send first: if the driver is gone or the queue is full the
                // prompt would be dropped — don't echo it as if it ran
                if cmd.try_send(UserCmd::Prompt(text.clone())).is_ok() {
                    app.push_user(&text, queued);
                    app.hist.push(text);
                    app.hi = app.hist.len();
                    app.draft.clear();
                    app.input.clear();
                    app.cur = 0;
                    app.follow = true;
                } else {
                    app.note("not queued — agent unavailable");
                }
            }
        }
        KeyCode::Backspace => {
            if app.cur > 0 {
                let prev = app.input[..app.cur].chars().next_back().unwrap().len_utf8();
                app.input.replace_range(app.cur - prev..app.cur, "");
                app.cur -= prev;
            }
        }
        KeyCode::Delete => {
            if app.cur < app.input.len() {
                let n = app.input[app.cur..].chars().next().unwrap().len_utf8();
                app.input.replace_range(app.cur..app.cur + n, "");
            }
        }
        KeyCode::Left => {
            if ctrl {
                app.cur = word_back(&app.input[..app.cur]);
            } else if app.cur > 0 {
                app.cur -= app.input[..app.cur].chars().next_back().unwrap().len_utf8();
            }
        }
        KeyCode::Right => {
            if ctrl {
                while app.cur < app.input.len()
                    && !app.input[app.cur..].chars().next().unwrap().is_whitespace()
                {
                    app.cur += app.input[app.cur..].chars().next().unwrap().len_utf8();
                }
                while app.cur < app.input.len()
                    && app.input[app.cur..].chars().next().unwrap().is_whitespace()
                {
                    app.cur += app.input[app.cur..].chars().next().unwrap().len_utf8();
                }
            } else if app.cur < app.input.len() {
                app.cur += app.input[app.cur..].chars().next().unwrap().len_utf8();
            }
        }
        KeyCode::Home => app.cur = 0,
        KeyCode::End => app.cur = app.input.len(),
        KeyCode::Char('a') if ctrl => app.cur = 0,
        KeyCode::Char('e') if ctrl => app.cur = app.input.len(),
        KeyCode::Char('u') if ctrl => {
            app.input.replace_range(..app.cur, "");
            app.cur = 0;
        }
        KeyCode::Char('k') if ctrl => {
            app.input.truncate(app.cur);
        }
        KeyCode::Char('w') if ctrl => {
            let n = word_back(&app.input[..app.cur]);
            app.input.replace_range(n..app.cur, "");
            app.cur = n;
        }
        KeyCode::Up => {
            if app.hi > 0 {
                if app.hi == app.hist.len() {
                    app.draft = app.input.clone();
                }
                app.hi -= 1;
                app.input = app.hist[app.hi].clone();
                app.cur = app.input.len();
            }
        }
        KeyCode::Down => {
            if app.hi < app.hist.len() {
                app.hi += 1;
                app.input = if app.hi == app.hist.len() {
                    app.draft.clone()
                } else {
                    app.hist[app.hi].clone()
                };
                app.cur = app.input.len();
            }
        }
        KeyCode::PageUp => app.scroll_by(-10),
        KeyCode::PageDown => app.scroll_by(10),
        KeyCode::Char(c) if !ctrl => {
            app.input.insert(app.cur, c);
            app.cur += c.len_utf8();
        }
        _ => {}
    }
    false
}
