//! The chat feed: committed scrollback, the live assistant stream, the wrap
//! cache over both, and the viewport into them.

use crate::md::{mark_line, md_block};
use crate::styled::{sline, wrap_sline, SLine};
use crate::theme::{C_DIM, C_USER, C_USER_MARK};

pub(crate) struct Feed {
    pub log: Vec<SLine>,
    /// `log` wrapped at `wrap_w`; only lines past `wrap_len` are new.
    pub wrapped: Vec<SLine>,
    pub wrap_w: usize,
    pub wrap_len: usize,
    /// Assistant text still streaming, re-rendered whenever it grows.
    pub stream: String,
    pub stream_wrapped: Vec<SLine>,
    pub stream_dirty: bool,
    /// Inside an unclosed code fence at the end of `log`.
    pub in_fence: bool,
    pub scroll: usize,
    /// Pinned to the newest line.
    pub follow: bool,
}

impl Feed {
    pub fn new() -> Self {
        Self {
            log: vec![],
            wrapped: vec![],
            wrap_w: 0,
            wrap_len: 0,
            stream: String::new(),
            stream_wrapped: vec![],
            stream_dirty: false,
            in_fence: false,
            scroll: 0,
            follow: true,
        }
    }

    pub fn push(&mut self, l: SLine) {
        self.log.push(l);
    }

    pub fn note(&mut self, s: impl Into<String>) {
        self.log.push(sline(format!("  {}", s.into()), C_DIM));
    }

    pub fn push_user(&mut self, text: &str, queued: bool) {
        for (i, l) in text.lines().enumerate() {
            let pfx = if i == 0 { "❯ " } else { "  " };
            let mut l2: SLine = vec![(pfx.into(), C_USER_MARK), (l.to_string(), C_USER)];
            if i == 0 && queued {
                l2.push((" (queued)".into(), C_DIM));
            }
            self.log.push(l2);
        }
        self.log.push(Vec::new());
    }

    pub fn stream_text(&mut self, t: &str) {
        self.stream.push_str(t);
        self.stream_dirty = true;
    }

    /// Move the streamed assistant text into the scrollback with light
    /// markdown styling (headings, fenced blocks with a gutter bar, inline
    /// code/bold/italic/links, bullets and ordered lists, quotes, rules and
    /// boxed pipe tables).
    pub fn flush_stream(&mut self) {
        if self.stream.is_empty() {
            return;
        }
        let w = self.wrap_w.max(20);
        let mut first = true;
        for (mut l, is_content) in md_block(&self.stream, &mut self.in_fence, w) {
            if first && is_content {
                mark_line(&mut l);
                first = false;
            }
            self.log.push(l);
        }
        self.log.push(Vec::new());
        self.stream.clear();
        self.stream_wrapped.clear();
        self.stream_dirty = false;
    }

    /// A message ended: flush it, and close any fence it left open — the
    /// fence state is per message, so it must not style the next as code.
    pub fn end_message(&mut self) {
        self.flush_stream();
        self.in_fence = false;
    }

    pub fn ensure_wrap(&mut self, w: usize) {
        if self.wrap_w != w {
            self.wrapped.clear();
            self.wrap_len = 0;
            self.wrap_w = w;
            // the live stream is wrapped too: rebuild it at the new width
            self.stream_dirty = true;
        }
        while self.wrap_len < self.log.len() {
            let ws = wrap_sline(&self.log[self.wrap_len], w);
            self.wrapped.extend(ws);
            self.wrap_len += 1;
        }
    }

    pub fn rebuild_stream(&mut self) {
        self.stream_wrapped.clear();
        self.stream_dirty = false;
        // an empty stream must add no phantom line: it would inflate the
        // scroll total and push the first content line off-screen
        if self.stream.is_empty() {
            return;
        }
        let mut fence = self.in_fence;
        let w = self.wrap_w.max(20);
        let mut first = true;
        for (mut sl, is_content) in md_block(&self.stream, &mut fence, w) {
            if first && is_content {
                mark_line(&mut sl);
                first = false;
            }
            self.stream_wrapped.extend(wrap_sline(&sl, self.wrap_w));
        }
    }

    /// Wrapped rows of scrollback plus stream.
    pub fn rows(&self) -> usize {
        self.wrapped.len() + self.stream_wrapped.len()
    }

    pub fn scroll_by(&mut self, d: isize) {
        self.follow = false;
        self.scroll = self.scroll.saturating_add_signed(d);
    }

    /// Re-pin to the tail once a scroll has reached it in a `view`-row window.
    pub fn refollow(&mut self, view: usize) {
        if self.scroll + view >= self.rows() {
            self.follow = true;
        }
    }

    /// Ctrl-L: forget everything shown so far.
    pub fn clear(&mut self) {
        self.log.clear();
        self.wrapped.clear();
        self.wrap_len = 0;
        self.scroll = 0;
        self.in_fence = false;
        self.stream_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn text_of(l: &SLine) -> String {
        l.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn empty_stream_adds_no_phantom_line() {
        // after a flush, rebuild_stream("") must leave stream_wrapped empty —
        // a phantom line inflates the scroll total and eats the top row
        let mut f = Feed::new();
        f.stream = "## Plan\n\ntext".into();
        f.flush_stream();
        f.rebuild_stream();
        assert!(f.stream_wrapped.is_empty());
        assert!(!f.stream_dirty);
    }

    #[test]
    fn an_unclosed_fence_ends_with_its_message() {
        let mut f = Feed::new();
        f.stream = "```rust\nlet x = 1;".into();
        f.flush_stream();
        assert!(f.in_fence);
        f.end_message();
        assert!(!f.in_fence);
        f.stream_text("plain answer");
        f.flush_stream();
        let last = f
            .log
            .iter()
            .rev()
            .map(text_of)
            .find(|t| !t.trim().is_empty())
            .unwrap();
        assert_eq!(last, "● plain answer");
    }

    #[test]
    fn push_user_marks_the_first_line_and_ends_with_a_blank() {
        let mut f = Feed::new();
        f.push_user("first\nsecond", false);
        assert_eq!(f.log.len(), 3);
        assert_eq!(text_of(&f.log[0]), "❯ first");
        assert_eq!(text_of(&f.log[1]), "  second");
        assert!(f.log[2].is_empty());
        f.push_user("deferred", true);
        assert_eq!(text_of(&f.log[3]), "❯ deferred (queued)");
    }

    #[test]
    fn note_is_dim_and_indented() {
        let mut f = Feed::new();
        f.note("hint");
        assert_eq!(f.log.len(), 1);
        assert_eq!(text_of(&f.log[0]), "  hint");
        assert_eq!(f.log[0][0].1, C_DIM);
    }

    #[test]
    fn ensure_wrap_is_incremental_and_rebuilds_on_width_change() {
        let mut f = Feed::new();
        f.push(sline("hello", Style::default()));
        f.ensure_wrap(40);
        assert_eq!((f.wrap_w, f.wrap_len, f.wrapped.len()), (40, 1, 1));

        // a new line is appended without redoing the earlier one
        f.push(sline("more", Style::default()));
        f.ensure_wrap(40);
        assert_eq!((f.wrap_len, f.wrapped.len()), (2, 2));

        // narrowing re-wraps everything and flags the live stream as dirty
        f.stream_dirty = false;
        f.ensure_wrap(4);
        assert_eq!(f.wrap_w, 4);
        assert_eq!(f.wrap_len, f.log.len());
        assert_eq!(f.wrapped.len(), 3); // "hello" splits into two rows
        assert!(f.stream_dirty);
    }

    #[test]
    fn scroll_by_detaches_follow_and_saturates_at_zero() {
        let mut f = Feed::new();
        assert!(f.follow);
        f.scroll_by(5);
        assert_eq!(f.scroll, 5);
        assert!(!f.follow);
        f.scroll_by(-2);
        assert_eq!(f.scroll, 3);
        f.scroll_by(-100);
        assert_eq!(f.scroll, 0);
        // reaching the tail again re-pins it
        f.refollow(10);
        assert!(f.follow, "an empty feed is all tail");
    }

    #[test]
    fn clear_forgets_the_scrollback() {
        let mut f = Feed::new();
        f.push(sline("hello", Style::default()));
        f.wrapped.push(Vec::new());
        f.wrap_len = 1;
        f.scroll = 5;
        f.in_fence = true;
        f.clear();
        assert!(f.log.is_empty() && f.wrapped.is_empty());
        assert_eq!((f.wrap_len, f.scroll), (0, 0));
        assert!(!f.in_fence && f.stream_dirty);
    }
}
