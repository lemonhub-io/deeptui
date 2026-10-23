//! The UI state: three independent parts — the feed, the editor, the meter —
//! composed. Events from the session fold into it ([`Projection`]); keys act
//! on it through the keymap and a [`Remote`].

use crate::editor::Editor;
use crate::feed::Feed;
use crate::keymap::{action, Action};
use crate::meter::Meter;
use crate::styled::{sline, SLine};
use crate::theme::{C_DIM, C_ERR, C_GUTTER, C_OK, C_TOOL, C_TOOL_NAME};
use crossterm::event::KeyEvent;
use deeptui_core::text::fmt_tokens;
use deeptui_core::{Event, Outcome, Projection, Receipt};
use deeptui_engine::Remote;

pub(crate) struct App {
    pub feed: Feed,
    pub editor: Editor,
    pub meter: Meter,
    pub model: String,
    /// Newer release found by the last check (banner line, TUI notice).
    pub update: Option<String>,
    pub agent_dead: bool,
    pub quit: bool,
}

impl App {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            feed: Feed::new(),
            editor: Editor::new(),
            meter: Meter::new(),
            model: model.into(),
            update: None,
            agent_dead: false,
            quit: false,
        }
    }

    pub fn key(&mut self, k: KeyEvent, remote: &Remote) {
        let Some(act) = action(&k, self.editor.text.is_empty()) else {
            return;
        };
        match act {
            Action::Quit => self.quit = true,
            Action::ClearScreen => self.feed.clear(),
            Action::Escape => {
                if self.meter.running {
                    remote.cancel();
                } else {
                    self.editor.clear();
                }
            }
            Action::Submit => self.submit(remote),
            Action::Newline => self.editor.insert_str("\n"),
            Action::Scroll(d) => self.feed.scroll_by(d),
            Action::Edit(e) => self.editor.apply(e),
        }
    }

    fn submit(&mut self, remote: &Remote) {
        let Some(text) = self.editor.pending() else {
            return;
        };
        // send first: if the session is gone or its inbox is full the prompt
        // would be dropped — don't echo it as if it ran
        if remote.ask(text.clone()) {
            self.feed.push_user(&text, self.meter.running);
            self.editor.commit(text);
            self.feed.follow = true;
        } else {
            self.feed.note("not queued — agent unavailable");
        }
    }

    /// The session's event stream closed: don't leave a spinner running
    /// forever — say so once.
    pub fn agent_gone(&mut self) {
        if !self.agent_dead {
            self.agent_dead = true;
            self.meter.halt();
            self.feed.note("agent stopped — Ctrl-C to quit");
        }
    }

    pub fn update_found(&mut self, tag: String) {
        self.feed.note(format!(
            "↑ deeptui {tag} available (you have {}) — run `deeptui --self-update`",
            env!("CARGO_PKG_VERSION")
        ));
        self.update = Some(tag);
    }

    fn receipt(&mut self, r: &Receipt) {
        // the fence state is per-message: an unclosed block must not style
        // the next response as code
        self.feed.end_message();
        match &r.outcome {
            Outcome::Done => self.feed.push(vec![
                ("  ─ ".into(), C_GUTTER),
                ("done".into(), C_OK),
                (
                    format!(
                        " · ${:.4} · ↑{} ↓{} · {:.1}s",
                        r.usage.cost,
                        fmt_tokens(r.usage.input),
                        fmt_tokens(r.usage.output),
                        self.meter.elapsed()
                    ),
                    C_DIM,
                ),
            ]),
            Outcome::Interrupted => self.feed.note("— interrupted (Esc)"),
            Outcome::Failed(e) => self.feed.push(sline(format!("✗ {e}"), C_ERR)),
        }
    }
}

impl Projection for App {
    fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Text(t) => self.feed.stream_text(t),
            Event::ToolStart { name, args } => {
                self.feed.flush_stream();
                let l: SLine = vec![
                    ("  ◆ ".into(), C_TOOL),
                    (name.clone(), C_TOOL_NAME),
                    (format!(" {args}"), C_DIM),
                ];
                self.feed.push(l);
            }
            Event::ToolEnd {
                ms,
                preview,
                is_err,
                ..
            } => {
                let (mark, st) = if *is_err {
                    ("✗", C_ERR)
                } else {
                    ("✓", C_OK)
                };
                self.feed.push(vec![
                    ("    └ ".into(), C_GUTTER),
                    (format!("{mark} {ms}ms"), st),
                    (format!("  {preview}"), C_DIM),
                ]);
            }
            Event::Compacted(n) => {
                self.feed.flush_stream();
                self.feed.note(format!("⛁ {n}"));
            }
            // read before the meter stops the clock
            Event::Finished(r) => self.receipt(r),
            Event::Round(_) | Event::Usage(_) => {}
        }
        self.meter.apply(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use deeptui_core::Usage;
    use deeptui_engine::Command;

    fn text_of(l: &SLine) -> String {
        l.iter().map(|(t, _)| t.as_str()).collect()
    }

    fn texts(app: &App) -> Vec<String> {
        app.feed.log.iter().map(text_of).collect()
    }

    fn press(app: &mut App, code: KeyCode, mods: KeyModifiers, remote: &Remote) {
        app.key(KeyEvent::new(code, mods), remote);
    }

    fn finished(outcome: Outcome, cost: f64, input: u64, output: u64) -> Event {
        Event::Finished(Receipt {
            outcome,
            usage: Usage {
                input,
                output,
                cached: 0,
                cost,
            },
            rounds: 1,
        })
    }

    #[test]
    fn tokens_stream_and_tool_events_flush_them_first() {
        let mut app = App::new("m");
        app.apply(&Event::Text("thinking ".into()));
        app.apply(&Event::Text("out loud".into()));
        assert_eq!(app.feed.stream, "thinking out loud");
        assert!(app.feed.stream_dirty);
        app.apply(&Event::ToolStart {
            name: "bash".into(),
            args: "ls -a".into(),
        });
        // the streamed text landed in the log before the tool line
        assert!(app.feed.stream.is_empty() && !app.feed.stream_dirty);
        let t = texts(&app);
        assert!(t.iter().any(|t| t == "● thinking out loud"), "{t:?}");
        assert_eq!(t.last().unwrap(), "  ◆ bash ls -a");
        // flush_stream ends the block with a blank separator line
        assert!(app.feed.log[app.feed.log.len() - 2].is_empty());

        for (is_err, want) in [(false, "    └ ✓ 12ms  ok"), (true, "    └ ✗ 7ms  boom")] {
            app.apply(&Event::ToolEnd {
                name: "bash".into(),
                ms: if is_err { 7 } else { 12 },
                preview: if is_err { "boom".into() } else { "ok".into() },
                is_err,
            });
            assert_eq!(texts(&app).last().unwrap(), want);
        }

        app.apply(&Event::Compacted("elided 3".into()));
        assert_eq!(texts(&app).last().unwrap(), "  ⛁ elided 3");
    }

    #[test]
    fn a_failed_turn_stops_the_run_and_keeps_the_partial_text() {
        let mut app = App::new("m");
        app.apply(&Event::Round(1));
        app.apply(&Event::Text("half an answer".into()));
        app.apply(&finished(
            Outcome::Failed("connection reset".into()),
            0.0,
            0,
            0,
        ));

        assert!(!app.meter.running && app.meter.started.is_none());
        assert_eq!((app.meter.tally.ok, app.meter.tally.failed()), (0, 1));
        let t = texts(&app);
        assert!(t.iter().any(|t| t == "● half an answer"), "{t:?}");
        assert_eq!(t.last().unwrap(), "✗ connection reset");
        assert_eq!(app.feed.log.last().unwrap()[0].1, C_ERR);
    }

    #[test]
    fn a_finished_turn_prints_its_receipt() {
        let mut app = App::new("m");
        app.apply(&Event::Round(1));
        app.apply(&finished(Outcome::Done, 0.5, 1_000, 250));
        assert_eq!((app.meter.tally.ok, app.meter.tally.failed()), (1, 0));
        assert!(!app.meter.running);
        let last = texts(&app).pop().unwrap();
        assert!(last.contains("done"), "{last}");
        assert!(
            last.contains("$0.5000") && last.contains("↑1.0k ↓250") && last.contains('s'),
            "{last}"
        );
    }

    #[test]
    fn an_interrupted_turn_notes_the_cancel_instead_of_a_receipt() {
        let mut app = App::new("m");
        app.apply(&Event::Round(1));
        app.apply(&finished(Outcome::Interrupted, 3.0, 0, 0));
        assert_eq!((app.meter.tally.ok, app.meter.tally.failed()), (0, 1));
        let last = app.feed.log.last().unwrap();
        assert_eq!(text_of(last), "  — interrupted (Esc)");
        assert_eq!(last[0].1, C_DIM);
    }

    #[test]
    fn an_unclosed_fence_does_not_leak_into_the_next_answer() {
        let mut app = App::new("m");
        app.apply(&Event::Text("```rust\nlet x = 1;".into()));
        app.apply(&finished(Outcome::Done, 0.0, 0, 0));
        assert!(!app.feed.in_fence);
        app.apply(&Event::Text("plain answer".into()));
        app.feed.flush_stream();
        let last = texts(&app)
            .into_iter()
            .rev()
            .find(|t| !t.trim().is_empty() && !t.contains("done"))
            .unwrap();
        assert_eq!(last, "● plain answer");
    }

    #[test]
    fn enter_trims_and_queues_the_prompt_once() {
        let mut a = App::new("m");
        let (remote, mut rx) = Remote::channel(4);
        a.editor.insert_str("  do it  ");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        // only trailing whitespace is trimmed: leading indent is preserved
        assert_eq!(rx.try_recv().unwrap(), Command::Ask("  do it".into()));
        assert_eq!((a.editor.text.as_str(), a.editor.cur), ("", 0));
        assert_eq!(a.editor.hist, ["  do it"]);
        assert!(a.feed.follow);

        // whitespace-only input is discarded silently
        a.editor.insert_str("   ");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(a.editor.text, "   ");
        assert!(rx.try_recv().is_err());

        // alt/shift+enter insert a newline instead of sending
        a.editor.text = "line".into();
        a.editor.cur = 4;
        press(&mut a, KeyCode::Enter, KeyModifiers::ALT, &remote);
        press(&mut a, KeyCode::Enter, KeyModifiers::SHIFT, &remote);
        assert_eq!(a.editor.text, "line\n\n");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queued_prompts_are_echoed_with_a_marker() {
        let mut a = App::new("m");
        a.meter.running = true;
        let (remote, _rx) = Remote::channel(4);
        a.editor.insert_str("multi\nline");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(texts(&a), ["❯ multi (queued)", "  line", ""]);
        // history keeps the raw multi-line text, not the echo
        assert_eq!(a.editor.hist, ["multi\nline"]);
    }

    #[test]
    fn a_full_queue_warns_instead_of_echoing() {
        let mut a = App::new("m");
        let (remote, _rx) = Remote::channel(1);
        assert!(remote.cancel()); // the inbox is now full
        a.editor.insert_str("hi");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(a.editor.text, "hi"); // not cleared, not echoed as a run
        assert!(a.editor.hist.is_empty());
        assert_eq!(
            texts(&a).last().unwrap(),
            "  not queued — agent unavailable"
        );
    }

    #[test]
    fn esc_cancels_while_running_and_clears_otherwise() {
        let mut a = App::new("m");
        let (remote, mut rx) = Remote::channel(4);
        a.meter.running = true;
        press(&mut a, KeyCode::Esc, KeyModifiers::NONE, &remote);
        assert_eq!(rx.try_recv().unwrap(), Command::Cancel);

        // idle: esc just drops the draft
        a.meter.running = false;
        a.editor.insert_str("draft");
        press(&mut a, KeyCode::Esc, KeyModifiers::NONE, &remote);
        assert_eq!((a.editor.text.as_str(), a.editor.cur), ("", 0));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn quit_keys_and_ctrl_l() {
        let (remote, _rx) = Remote::channel(4);
        for (code, line) in [(KeyCode::Char('c'), "x"), (KeyCode::Char('d'), "")] {
            let mut a = App::new("m");
            a.editor.insert_str(line);
            press(&mut a, code, KeyModifiers::CONTROL, &remote);
            assert!(a.quit, "{code:?}");
        }
        // ctrl-d with text on the line is not a quit
        let mut a = App::new("m");
        a.editor.insert_str("x");
        press(&mut a, KeyCode::Char('d'), KeyModifiers::CONTROL, &remote);
        assert!(!a.quit);

        a.feed.note("hello");
        a.feed.scroll = 5;
        press(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL, &remote);
        assert!(a.feed.log.is_empty() && a.feed.scroll == 0);
        assert!(!a.quit);
    }

    #[test]
    fn page_keys_scroll_and_detach_from_the_tail() {
        let mut a = App::new("m");
        let (remote, _rx) = Remote::channel(4);
        a.feed.scroll = 5;
        press(&mut a, KeyCode::PageDown, KeyModifiers::NONE, &remote);
        assert_eq!(a.feed.scroll, 15);
        assert!(!a.feed.follow); // scrolling always detaches from the tail
        press(&mut a, KeyCode::PageUp, KeyModifiers::NONE, &remote);
        assert_eq!(a.feed.scroll, 5);
        a.feed.scroll = 3;
        press(&mut a, KeyCode::PageUp, KeyModifiers::NONE, &remote);
        assert_eq!(a.feed.scroll, 0); // saturates at the top
    }

    #[test]
    fn repeated_enter_keeps_history_and_cursor_in_step() {
        let mut a = App::new("m");
        let (remote, mut rx) = Remote::channel(8);
        for p in ["one", "two"] {
            a.editor.insert_str(p);
            press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        }
        assert_eq!(rx.try_recv().unwrap(), Command::Ask("one".into()));
        assert_eq!(rx.try_recv().unwrap(), Command::Ask("two".into()));
        assert_eq!((a.editor.hist.len(), a.editor.hi), (2, 2));

        // an edited recall is stored verbatim on the next send
        press(&mut a, KeyCode::Up, KeyModifiers::NONE, &remote);
        assert_eq!((a.editor.text.as_str(), a.editor.hi), ("two", 1));
        press(&mut a, KeyCode::Char('!'), KeyModifiers::NONE, &remote);
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(rx.try_recv().unwrap(), Command::Ask("two!".into()));
        assert_eq!(a.editor.hist, ["one", "two", "two!"]);
    }

    #[test]
    fn a_vanished_session_is_reported_once() {
        let mut a = App::new("m");
        a.meter.running = true;
        a.agent_gone();
        a.agent_gone();
        assert!(!a.meter.running);
        assert_eq!(texts(&a), ["  agent stopped — Ctrl-C to quit"]);
    }
}
