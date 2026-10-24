//! Keys to intents — a pure table. What an intent *does* is the app's call.

use crate::editor::Edit;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Action {
    Quit,
    ClearScreen,
    /// Interrupt the run, or drop the draft when idle.
    Escape,
    Submit,
    /// Complete a half-typed slash command.
    Complete,
    Newline,
    Scroll(isize),
    Edit(Edit),
}

/// The intent of `k`. `line_empty` decides whether Ctrl-D means "quit".
pub(crate) fn action(k: &KeyEvent, line_empty: bool) -> Option<Action> {
    if k.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let newline = k
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
    Some(match k.code {
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Char('d') if ctrl && line_empty => Action::Quit,
        KeyCode::Char('l') if ctrl => Action::ClearScreen,
        KeyCode::Char('a') if ctrl => Action::Edit(Edit::Home),
        KeyCode::Char('e') if ctrl => Action::Edit(Edit::End),
        KeyCode::Char('u') if ctrl => Action::Edit(Edit::KillBefore),
        KeyCode::Char('k') if ctrl => Action::Edit(Edit::KillAfter),
        KeyCode::Char('w') if ctrl => Action::Edit(Edit::KillWord),
        // any other ctrl+char is unbound: it never types literal text
        KeyCode::Char(_) if ctrl => return None,
        KeyCode::Char(c) => Action::Edit(Edit::Insert(c)),
        KeyCode::Esc => Action::Escape,
        KeyCode::Enter if newline => Action::Newline,
        KeyCode::Enter => Action::Submit,
        KeyCode::Tab => Action::Complete,
        KeyCode::Backspace => Action::Edit(Edit::Backspace),
        KeyCode::Delete => Action::Edit(Edit::Delete),
        KeyCode::Left if ctrl => Action::Edit(Edit::WordLeft),
        KeyCode::Left => Action::Edit(Edit::Left),
        KeyCode::Right if ctrl => Action::Edit(Edit::WordRight),
        KeyCode::Right => Action::Edit(Edit::Right),
        KeyCode::Home => Action::Edit(Edit::Home),
        KeyCode::End => Action::Edit(Edit::End),
        KeyCode::Up => Action::Edit(Edit::HistPrev),
        KeyCode::Down => Action::Edit(Edit::HistNext),
        KeyCode::PageUp => Action::Scroll(-10),
        KeyCode::PageDown => Action::Scroll(10),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        action(&KeyEvent::new(code, mods), true)
    }

    #[test]
    fn the_table_maps_keys_to_intents() {
        use KeyModifiers as M;
        assert_eq!(k(KeyCode::Char('c'), M::CONTROL), Some(Action::Quit));
        assert_eq!(k(KeyCode::Char('l'), M::CONTROL), Some(Action::ClearScreen));
        assert_eq!(
            k(KeyCode::Char('x'), M::NONE),
            Some(Action::Edit(Edit::Insert('x')))
        );
        assert_eq!(
            k(KeyCode::Char('X'), M::SHIFT),
            Some(Action::Edit(Edit::Insert('X')))
        );
        assert_eq!(k(KeyCode::Enter, M::NONE), Some(Action::Submit));
        assert_eq!(k(KeyCode::Enter, M::ALT), Some(Action::Newline));
        assert_eq!(k(KeyCode::Enter, M::SHIFT), Some(Action::Newline));
        assert_eq!(
            k(KeyCode::Left, M::CONTROL),
            Some(Action::Edit(Edit::WordLeft))
        );
        assert_eq!(k(KeyCode::PageUp, M::NONE), Some(Action::Scroll(-10)));
        assert_eq!(k(KeyCode::Tab, M::NONE), Some(Action::Complete));
    }

    #[test]
    fn ctrl_d_quits_only_on_an_empty_line() {
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(action(&ctrl_d, true), Some(Action::Quit));
        assert_eq!(action(&ctrl_d, false), None);
    }

    #[test]
    fn unbound_keys_and_releases_mean_nothing() {
        use KeyModifiers as M;
        // ctrl+char never types literal text
        assert_eq!(k(KeyCode::Char('b'), M::CONTROL), None);
        for code in [KeyCode::BackTab, KeyCode::Insert, KeyCode::F(12)] {
            assert_eq!(k(code, M::NONE), None, "{code:?}");
        }
        let mut rel = KeyEvent::new(KeyCode::Char('x'), M::NONE);
        rel.kind = KeyEventKind::Release;
        assert_eq!(action(&rel, true), None);
    }
}
