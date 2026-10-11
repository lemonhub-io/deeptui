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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sline;
    use ratatui::style::Style;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn send(app: &mut App, code: KeyCode, mods: KeyModifiers, tx: &mpsc::Sender<UserCmd>) -> bool {
        on_key(app, key(code, mods), tx)
    }

    #[test]
    fn input_rows_counts_wrapped_and_explicit_lines() {
        assert_eq!(input_rows("", 10), 1);
        assert_eq!(input_rows("hello", 10), 1);
        assert_eq!(input_rows("0123456789", 10), 2);
        assert_eq!(input_rows("a\nb", 10), 2);
        assert_eq!(input_rows("a\n\nb", 10), 3);
        // wide chars occupy two columns
        assert_eq!(input_rows("汉字汉字汉", 10), 2);
        // a zero width is treated as one column so the maths stays finite
        assert_eq!(input_rows("abc", 0), 4);
    }

    #[test]
    fn cursor_pos_wraps_at_the_column_limit() {
        assert_eq!(cursor_pos("", 10), (0, 0));
        assert_eq!(cursor_pos("abc", 10), (0, 3));
        assert_eq!(cursor_pos("0123456789", 10), (1, 0));
        assert_eq!(cursor_pos("0123456789a", 10), (1, 1));
        assert_eq!(cursor_pos("a\nb", 10), (1, 1));
        assert_eq!(cursor_pos("汉字汉字汉", 10), (1, 0));
        assert_eq!(cursor_pos("abc", 0), (3, 0));
    }

    #[test]
    fn editing_keys_handle_multibyte_input() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(4);
        a.input = "héllo wörld".into();
        a.cur = a.input.len();
        send(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL, &tx);
        assert_eq!(a.input, "héllo ");
        assert_eq!(a.cur, a.input.len());

        send(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL, &tx);
        assert_eq!(a.cur, 0);
        send(&mut a, KeyCode::Char('e'), KeyModifiers::CONTROL, &tx);
        assert_eq!(a.cur, a.input.len());

        // left walks one char (2 bytes here), backspace eats it
        send(&mut a, KeyCode::Left, KeyModifiers::NONE, &tx);
        assert_eq!(a.cur, "héllo".len());
        send(&mut a, KeyCode::Backspace, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "héll ");
        assert_eq!(a.cur, "héll".len());
        // delete eats the char *after* the cursor, then nothing is left
        send(&mut a, KeyCode::Delete, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "héll");
        send(&mut a, KeyCode::Delete, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "héll");

        a.input = "keep drop".into();
        a.cur = 5;
        send(&mut a, KeyCode::Char('k'), KeyModifiers::CONTROL, &tx);
        assert_eq!(a.input, "keep ");
        send(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL, &tx);
        assert_eq!(a.input, "");
    }

    #[test]
    fn printable_keys_insert_at_the_cursor() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(4);
        for c in "ab".chars() {
            send(&mut a, KeyCode::Char(c), KeyModifiers::NONE, &tx);
        }
        send(&mut a, KeyCode::Home, KeyModifiers::NONE, &tx);
        send(&mut a, KeyCode::Char('é'), KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.cur), ("éab", 2));

        // ctrl+char never types literal text
        send(&mut a, KeyCode::Char('b'), KeyModifiers::CONTROL, &tx);
        assert_eq!(a.input, "éab");

        // key releases are ignored entirely
        let mut rel = key(KeyCode::Char('x'), KeyModifiers::NONE);
        rel.kind = KeyEventKind::Release;
        assert!(!on_key(&mut a, rel, &tx));
        assert_eq!(a.input, "éab");
    }

    #[test]
    fn enter_trims_and_queues_the_prompt_once() {
        let mut a = App::new();
        let (tx, mut rx) = mpsc::channel(4);
        a.input = "  do it  ".into();
        a.cur = a.input.len();
        assert!(!send(&mut a, KeyCode::Enter, KeyModifiers::NONE, &tx));
        // only trailing whitespace is trimmed: leading indent is preserved
        assert!(matches!(rx.try_recv(), Ok(UserCmd::Prompt(t)) if t == "  do it"));
        assert_eq!(a.input, "");
        assert_eq!(a.cur, 0);
        assert_eq!(a.hist, ["  do it"]);
        assert_eq!(a.hi, 1);
        assert!(a.follow);

        // whitespace-only input is discarded silently
        a.input = "   ".into();
        send(&mut a, KeyCode::Enter, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "   ");
        assert!(rx.try_recv().is_err());

        // alt/shift+enter insert a newline instead of sending
        a.input = "line".into();
        a.cur = 4;
        send(&mut a, KeyCode::Enter, KeyModifiers::ALT, &tx);
        send(&mut a, KeyCode::Enter, KeyModifiers::SHIFT, &tx);
        assert_eq!(a.input, "line\n\n");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queued_prompts_are_echoed_with_a_marker() {
        let mut a = App::new();
        a.running = true;
        let (tx, _rx) = mpsc::channel(4);
        a.input = "multi\nline".into();
        a.cur = a.input.len();
        send(&mut a, KeyCode::Enter, KeyModifiers::NONE, &tx);
        let text: Vec<String> = a
            .log
            .iter()
            .map(|l| l.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, ["❯ multi (queued)", "  line", ""]);
        // history keeps the raw multi-line text, not the echo
        assert_eq!(a.hist, ["multi\nline"]);
    }

    #[test]
    fn a_full_queue_warns_instead_of_echoing() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(UserCmd::Cancel).unwrap(); // queue is now full
        a.input = "hi".into();
        a.cur = 2;
        send(&mut a, KeyCode::Enter, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "hi"); // not cleared, not echoed as a run
        assert!(a.hist.is_empty());
        let last = a.log.last().unwrap();
        let text: String = last.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "  not queued — agent unavailable");
    }

    #[test]
    fn esc_cancels_while_running_and_clears_otherwise() {
        let mut a = App::new();
        let (tx, mut rx) = mpsc::channel(4);
        a.running = true;
        send(&mut a, KeyCode::Esc, KeyModifiers::NONE, &tx);
        assert!(matches!(rx.try_recv(), Ok(UserCmd::Cancel)));

        // idle: esc just drops the draft
        a.running = false;
        a.input = "draft".into();
        a.cur = 5;
        send(&mut a, KeyCode::Esc, KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.cur), ("", 0));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn quit_keys_and_ctrl_l() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(4);
        assert!(send(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL, &tx));
        assert!(send(&mut a, KeyCode::Char('d'), KeyModifiers::CONTROL, &tx));
        // ctrl-d with text on the line is not a quit
        a.input = "x".into();
        a.cur = 1;
        assert!(!send(
            &mut a,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
            &tx
        ));
        // an unhandled key is harmless
        assert!(!send(&mut a, KeyCode::F(5), KeyModifiers::NONE, &tx));

        a.log.push(sline("hello", Style::default()));
        a.wrapped.push(Vec::new());
        a.wrap_len = 1;
        a.scroll = 5;
        a.in_fence = true;
        assert!(!send(
            &mut a,
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
            &tx
        ));
        assert!(a.log.is_empty() && a.wrapped.is_empty());
        assert_eq!((a.wrap_len, a.scroll), (0, 0));
        assert!(!a.in_fence && a.stream_dirty);
    }

    #[test]
    fn history_recall_preserves_the_draft() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(4);
        a.hist = vec!["one".into(), "two".into()];
        a.hi = 2;
        a.input = "draft".into();
        a.cur = 5;

        send(&mut a, KeyCode::Up, KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.cur, a.hi), ("two", 3, 1));
        send(&mut a, KeyCode::Up, KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.hi), ("one", 0));
        // already at the oldest entry
        send(&mut a, KeyCode::Up, KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.hi), ("one", 0));

        send(&mut a, KeyCode::Down, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "two");
        send(&mut a, KeyCode::Down, KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.hi), ("draft", 2));
        // already at the newest entry
        send(&mut a, KeyCode::Down, KeyModifiers::NONE, &tx);
        assert_eq!(a.input, "draft");
    }

    #[test]
    fn word_movement_and_page_scrolling() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(4);
        a.input = "one two three".into();
        a.cur = a.input.len();
        send(&mut a, KeyCode::Left, KeyModifiers::CONTROL, &tx);
        assert_eq!(a.cur, 8);
        send(&mut a, KeyCode::Left, KeyModifiers::CONTROL, &tx);
        assert_eq!(a.cur, 4);
        send(&mut a, KeyCode::Right, KeyModifiers::CONTROL, &tx);
        assert_eq!(a.cur, 8); // skips the word *and* its trailing space
        send(&mut a, KeyCode::Right, KeyModifiers::NONE, &tx);
        assert_eq!(a.cur, 9);

        a.scroll = 5;
        a.follow = true;
        send(&mut a, KeyCode::PageDown, KeyModifiers::NONE, &tx);
        assert_eq!(a.scroll, 15);
        assert!(!a.follow); // scrolling always detaches from the tail
        send(&mut a, KeyCode::PageUp, KeyModifiers::NONE, &tx);
        assert_eq!(a.scroll, 5);
        a.scroll = 3;
        send(&mut a, KeyCode::PageUp, KeyModifiers::NONE, &tx);
        assert_eq!(a.scroll, 0); // saturates at the top
    }

    #[test]
    fn word_back_skips_trailing_whitespace_then_the_word() {
        assert_eq!(word_back(""), 0);
        assert_eq!(word_back("   "), 0);
        assert_eq!(word_back("abc"), 0);
        assert_eq!(word_back("a b"), 2);
        assert_eq!(word_back("foo bar"), 4);
        assert_eq!(word_back("foo bar   "), 4);
        // byte offsets, so multibyte words land on the right boundary
        assert_eq!(word_back("foo 汉字"), 4);
    }

    #[test]
    fn home_end_and_unhandled_keys_leave_the_line_alone() {
        let mut a = App::new();
        let (tx, _rx) = mpsc::channel(4);
        a.input = "abc".into();
        a.cur = 1;
        send(&mut a, KeyCode::End, KeyModifiers::NONE, &tx);
        assert_eq!(a.cur, 3);
        send(&mut a, KeyCode::Home, KeyModifiers::NONE, &tx);
        assert_eq!(a.cur, 0);

        // keys with no binding change nothing and never quit
        for code in [
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Insert,
            KeyCode::F(12),
        ] {
            assert!(!send(&mut a, code, KeyModifiers::NONE, &tx), "{code:?}");
        }
        assert_eq!((a.input.as_str(), a.cur), ("abc", 0));

        // ctrl+k cuts the tail at the cursor
        send(&mut a, KeyCode::Right, KeyModifiers::NONE, &tx);
        send(&mut a, KeyCode::Char('k'), KeyModifiers::CONTROL, &tx);
        assert_eq!((a.input.as_str(), a.cur), ("a", 1));

        // ctrl+w eats the whitespace run before the cursor, like the shell
        a.input = "   ".into();
        a.cur = 3;
        send(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL, &tx);
        assert_eq!((a.input.as_str(), a.cur), ("", 0));
    }

    #[test]
    fn repeated_enter_keeps_history_and_cursor_in_step() {
        let mut a = App::new();
        let (tx, mut rx) = mpsc::channel(8);
        for p in ["one", "two"] {
            a.input = p.into();
            a.cur = p.len();
            send(&mut a, KeyCode::Enter, KeyModifiers::NONE, &tx);
        }
        assert!(matches!(rx.try_recv(), Ok(UserCmd::Prompt(t)) if t == "one"));
        assert!(matches!(rx.try_recv(), Ok(UserCmd::Prompt(t)) if t == "two"));
        assert_eq!(a.hist, ["one", "two"]);
        assert_eq!(a.hi, 2);
        assert_eq!((a.input.as_str(), a.cur), ("", 0));

        // an edited recall is stored verbatim on the next send
        send(&mut a, KeyCode::Up, KeyModifiers::NONE, &tx);
        assert_eq!((a.input.as_str(), a.hi), ("two", 1));
        send(&mut a, KeyCode::Char('!'), KeyModifiers::NONE, &tx);
        send(&mut a, KeyCode::Enter, KeyModifiers::NONE, &tx);
        assert!(matches!(rx.try_recv(), Ok(UserCmd::Prompt(t)) if t == "two!"));
        assert_eq!(a.hist, ["one", "two", "two!"]);
    }
}
