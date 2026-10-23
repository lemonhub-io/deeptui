//! The input line: a multi-line editor with readline-style motions, prompt
//! history, and the cursor geometry the frame needs to draw it.

use unicode_width::UnicodeWidthChar;

/// One editing operation, already decoupled from whatever key produced it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Edit {
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    /// Cut everything before the cursor.
    KillBefore,
    /// Cut everything after the cursor.
    KillAfter,
    /// Cut the word before the cursor (and the whitespace after it).
    KillWord,
    HistPrev,
    HistNext,
}

pub(crate) struct Editor {
    pub text: String,
    /// Byte offset of the cursor, always on a char boundary.
    pub cur: usize,
    pub hist: Vec<String>,
    /// Position in `hist`; `hist.len()` is the draft being typed.
    pub hi: usize,
    pub draft: String,
    /// First visible row when the text is taller than its box.
    pub scroll: u16,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cur: 0,
            hist: vec![],
            hi: 0,
            draft: String::new(),
            scroll: 0,
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cur, s);
        self.cur += s.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cur = 0;
    }

    /// The prompt to send, if the line holds more than whitespace. Only
    /// trailing whitespace is trimmed: leading indent is meaningful.
    pub fn pending(&self) -> Option<String> {
        let text = self.text.trim_end();
        (!text.is_empty()).then(|| text.to_string())
    }

    /// `text` was sent: remember it and start a fresh line.
    pub fn commit(&mut self, text: String) {
        self.hist.push(text);
        self.hi = self.hist.len();
        self.draft.clear();
        self.clear();
    }

    fn next_char(&self) -> Option<char> {
        self.text[self.cur..].chars().next()
    }

    fn prev_char(&self) -> Option<char> {
        self.text[..self.cur].chars().next_back()
    }

    pub fn apply(&mut self, e: Edit) {
        match e {
            Edit::Insert(c) => {
                self.text.insert(self.cur, c);
                self.cur += c.len_utf8();
            }
            Edit::Backspace => {
                if let Some(c) = self.prev_char() {
                    self.cur -= c.len_utf8();
                    self.text.remove(self.cur);
                }
            }
            Edit::Delete => {
                if self.next_char().is_some() {
                    self.text.remove(self.cur);
                }
            }
            Edit::Left => {
                if let Some(c) = self.prev_char() {
                    self.cur -= c.len_utf8();
                }
            }
            Edit::Right => {
                if let Some(c) = self.next_char() {
                    self.cur += c.len_utf8();
                }
            }
            Edit::WordLeft => self.cur = word_back(&self.text[..self.cur]),
            Edit::WordRight => {
                // the word, then the whitespace after it
                while let Some(c) = self.next_char().filter(|c| !c.is_whitespace()) {
                    self.cur += c.len_utf8();
                }
                while let Some(c) = self.next_char().filter(|c| c.is_whitespace()) {
                    self.cur += c.len_utf8();
                }
            }
            Edit::Home => self.cur = 0,
            Edit::End => self.cur = self.text.len(),
            Edit::KillBefore => {
                self.text.replace_range(..self.cur, "");
                self.cur = 0;
            }
            Edit::KillAfter => self.text.truncate(self.cur),
            Edit::KillWord => {
                let n = word_back(&self.text[..self.cur]);
                self.text.replace_range(n..self.cur, "");
                self.cur = n;
            }
            Edit::HistPrev => {
                if self.hi > 0 {
                    if self.hi == self.hist.len() {
                        self.draft = self.text.clone();
                    }
                    self.hi -= 1;
                    self.text = self.hist[self.hi].clone();
                    self.cur = self.text.len();
                }
            }
            Edit::HistNext => {
                if self.hi < self.hist.len() {
                    self.hi += 1;
                    self.text = if self.hi == self.hist.len() {
                        self.draft.clone()
                    } else {
                        self.hist[self.hi].clone()
                    };
                    self.cur = self.text.len();
                }
            }
        }
    }
}

/// Wrapped row count of the input at inner width `w`.
pub(crate) fn input_rows(input: &str, w: usize) -> usize {
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
pub(crate) fn cursor_pos(prefix: &str, w: usize) -> (usize, usize) {
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

/// Byte offset where the word ending `s` starts, skipping trailing whitespace
/// first — like the shell's Ctrl-W.
fn word_back(s: &str) -> usize {
    let t = s.trim_end();
    t.char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, cur: usize) -> Editor {
        let mut e = Editor::new();
        e.text = text.into();
        e.cur = cur;
        e
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
    fn word_back_skips_trailing_whitespace_then_the_word() {
        assert_eq!(word_back(""), 0);
        assert_eq!(word_back("   "), 0);
        assert_eq!(word_back("abc"), 0);
        assert_eq!(word_back("a b"), 2);
        assert_eq!(word_back("foo bar"), 4);
        assert_eq!(word_back("foo bar   "), 4);
        // byte offsets, so multibyte words land on the right boundary
        assert_eq!(word_back("foo 汉字"), 4);
        assert_eq!(word_back("汉 字"), "汉 ".len());
    }

    #[test]
    fn edits_handle_multibyte_text() {
        let mut e = at("héllo wörld", "héllo wörld".len());
        e.apply(Edit::KillWord);
        assert_eq!((e.text.as_str(), e.cur), ("héllo ", "héllo ".len()));
        e.apply(Edit::Home);
        assert_eq!(e.cur, 0);
        e.apply(Edit::End);
        assert_eq!(e.cur, e.text.len());

        // left walks one char (2 bytes here), backspace eats it
        e.apply(Edit::Left);
        assert_eq!(e.cur, "héllo".len());
        e.apply(Edit::Backspace);
        assert_eq!((e.text.as_str(), e.cur), ("héll ", "héll".len()));
        // delete eats the char *after* the cursor, then nothing is left
        e.apply(Edit::Delete);
        assert_eq!(e.text, "héll");
        e.apply(Edit::Delete);
        assert_eq!(e.text, "héll");
        // and the ends of the line are hard stops
        e.apply(Edit::Right);
        assert_eq!(e.cur, e.text.len());
        e.apply(Edit::Home);
        e.apply(Edit::Backspace);
        e.apply(Edit::Left);
        assert_eq!((e.text.as_str(), e.cur), ("héll", 0));

        let mut e = at("keep drop", 5);
        e.apply(Edit::KillAfter);
        assert_eq!(e.text, "keep ");
        e.apply(Edit::KillBefore);
        assert_eq!((e.text.as_str(), e.cur), ("", 0));
    }

    #[test]
    fn inserts_land_at_the_cursor() {
        let mut e = Editor::new();
        e.apply(Edit::Insert('a'));
        e.apply(Edit::Insert('b'));
        e.apply(Edit::Home);
        e.apply(Edit::Insert('é'));
        assert_eq!((e.text.as_str(), e.cur), ("éab", 2));
        e.insert_str("\n");
        assert_eq!((e.text.as_str(), e.cur), ("é\nab", 3));
    }

    #[test]
    fn word_motions_skip_words_and_their_spacing() {
        let mut e = at("one two three", 13);
        e.apply(Edit::WordLeft);
        assert_eq!(e.cur, 8);
        e.apply(Edit::WordLeft);
        assert_eq!(e.cur, 4);
        e.apply(Edit::WordRight);
        assert_eq!(e.cur, 8); // skips the word *and* its trailing space
        e.apply(Edit::Right);
        assert_eq!(e.cur, 9);

        // ctrl+w eats the whitespace run before the cursor, like the shell
        let mut e = at("   ", 3);
        e.apply(Edit::KillWord);
        assert_eq!((e.text.as_str(), e.cur), ("", 0));
    }

    #[test]
    fn history_recall_preserves_the_draft() {
        let mut e = at("draft", 5);
        e.hist = vec!["one".into(), "two".into()];
        e.hi = 2;

        e.apply(Edit::HistPrev);
        assert_eq!((e.text.as_str(), e.cur, e.hi), ("two", 3, 1));
        e.apply(Edit::HistPrev);
        assert_eq!((e.text.as_str(), e.hi), ("one", 0));
        // already at the oldest entry
        e.apply(Edit::HistPrev);
        assert_eq!((e.text.as_str(), e.hi), ("one", 0));

        e.apply(Edit::HistNext);
        assert_eq!(e.text, "two");
        e.apply(Edit::HistNext);
        assert_eq!((e.text.as_str(), e.hi), ("draft", 2));
        // already at the newest entry
        e.apply(Edit::HistNext);
        assert_eq!(e.text, "draft");
    }

    #[test]
    fn pending_trims_the_tail_only_and_commit_records_it() {
        assert_eq!(at("  do it  ", 0).pending().as_deref(), Some("  do it"));
        assert_eq!(at(" \n\t ", 0).pending(), None);

        let mut e = at("x", 1);
        e.draft = "old draft".into();
        e.commit("x".into());
        assert_eq!((e.text.as_str(), e.cur, e.hi), ("", 0, 1));
        assert_eq!(e.hist, ["x"]);
        assert!(e.draft.is_empty());
    }
}
