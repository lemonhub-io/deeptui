//! A list the session asked the user to choose from, over the chat: arrows
//! move, typing narrows it, Enter picks, Esc closes.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use deeptui_core::Choice;

pub(crate) struct Picker {
    pub title: String,
    options: Vec<Choice>,
    /// Typed so far: a row stays while it holds every word.
    pub filter: String,
    /// Index into [`rows`](Picker::rows).
    pub sel: usize,
}

/// What a key did.
#[derive(Debug, PartialEq)]
pub(crate) enum Picked {
    Stay,
    Close,
    /// Run this command line.
    Run(String),
}

impl Picker {
    pub fn new(title: String, options: Vec<Choice>, selected: usize) -> Self {
        Self {
            title,
            sel: selected.min(options.len().saturating_sub(1)),
            options,
            filter: String::new(),
        }
    }

    /// The options the filter lets through, in order.
    pub fn rows(&self) -> Vec<&Choice> {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.options
            .iter()
            .filter(|o| {
                let hay = format!("{} {}", o.label, o.detail).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .collect()
    }

    pub fn key(&mut self, k: &KeyEvent) -> Picked {
        if k.kind == KeyEventKind::Release {
            return Picked::Stay;
        }
        let last = self.rows().len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => return Picked::Close,
            KeyCode::Enter => {
                return self
                    .rows()
                    .get(self.sel)
                    .map_or(Picked::Stay, |c| Picked::Run(c.command.clone()))
            }
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(10),
            KeyCode::PageDown => self.sel = (self.sel + 10).min(last),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = last,
            KeyCode::Backspace => {
                self.filter.pop();
                self.sel = 0;
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.sel = 0;
            }
            _ => {}
        }
        Picked::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(label: &str, detail: &str) -> Choice {
        Choice {
            label: label.into(),
            detail: detail.into(),
            command: format!("/pick {label}"),
        }
    }

    fn press(p: &mut Picker, code: KeyCode) -> Picked {
        p.key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn picker() -> Picker {
        Picker::new(
            "Choose".into(),
            vec![
                choice("DeepSeek", "4 models · key ✓"),
                choice("OpenAI", "51 models · needs a key"),
                choice("OpenRouter", "391 models · needs a key"),
            ],
            1,
        )
    }

    #[test]
    fn arrows_move_within_the_list_and_enter_runs_the_choice() {
        let mut p = picker();
        assert_eq!(p.sel, 1, "starts where it was told");
        press(&mut p, KeyCode::Down);
        press(&mut p, KeyCode::Down);
        assert_eq!(p.sel, 2, "stops at the end");
        press(&mut p, KeyCode::Home);
        press(&mut p, KeyCode::Up);
        assert_eq!(p.sel, 0);
        press(&mut p, KeyCode::End);
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Picked::Run("/pick OpenRouter".into())
        );
        assert_eq!(press(&mut p, KeyCode::Esc), Picked::Close);
        // out of range is clamped; an empty list picks nothing
        assert_eq!(Picker::new("x".into(), vec![choice("a", "")], 9).sel, 0);
        let mut none = Picker::new("x".into(), Vec::new(), 0);
        assert_eq!(press(&mut none, KeyCode::Enter), Picked::Stay);
    }

    #[test]
    fn typing_narrows_by_every_word_in_label_or_detail() {
        let mut p = picker();
        for c in "open key".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        let labels: Vec<&str> = p.rows().iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["OpenAI", "OpenRouter"]);
        assert_eq!(p.sel, 0);
        for c in " 391".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Picked::Run("/pick OpenRouter".into())
        );
        for _ in 0..4 {
            press(&mut p, KeyCode::Backspace);
        }
        assert_eq!(p.filter, "open key");
        // ctrl+letters are not typed
        p.key(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(p.filter, "open key");
    }
}
