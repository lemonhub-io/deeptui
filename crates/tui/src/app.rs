//! The UI state: three independent parts — the feed, the editor, the meter —
//! composed. Events from the session fold into it ([`Projection`]); keys act
//! on it through the keymap and a [`Remote`].

use crate::editor::{Edit, Editor};
use crate::feed::Feed;
use crate::keymap::{action, Action};
use crate::meter::Meter;
use crate::picker::{Picked, Picker};
use crate::slash::{self, Line, Slash};
use crate::styled::sline;
use crate::theme::{C_DIM, C_ERR, C_GUTTER, C_OK, C_TOOL, C_TOOL_NAME};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use deeptui_core::history::is_aside;
use deeptui_core::text::{fmt_tokens, one_line};
use deeptui_core::{Ask, Event, Msg, Outcome, Projection, Receipt, Role};
use deeptui_engine::{Command, Remote};
use std::collections::HashMap;
use std::path::Path;

pub(crate) struct App {
    pub feed: Feed,
    pub editor: Editor,
    pub meter: Meter,
    pub model: String,
    pub root: String,
    pub context_budget: usize,
    pub motion: bool,
    /// Newer release found by the last check (banner line, TUI notice).
    pub update: Option<String>,
    pub agent_dead: bool,
    pub quit: bool,
    /// The last answer, for `/copy`.
    pub answer: Option<String>,
    /// The current round's text: the answer, if the round turns out last.
    said: String,
    /// Text for the terminal clipboard, written out by the frame loop.
    pub clipboard: Option<String>,
    /// A model with a key is set up: prompts can run.
    pub ready: bool,
    /// A list the session asked to choose from; it takes the keys.
    pub picker: Option<Picker>,
    /// The composer is taking a secret.
    pub secret: Option<Secret>,
}

/// A secret being typed: masked, sent straight to the session, never echoed
/// or kept in history.
pub(crate) struct Secret {
    pub title: String,
    /// Run as `command <secret>`.
    command: String,
    /// The draft it displaced, given back after.
    stash: String,
}

impl App {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            feed: Feed::new(),
            editor: Editor::new(),
            meter: Meter::new(),
            model: model.into(),
            root: String::new(),
            context_budget: 60_000,
            motion: true,
            update: None,
            agent_dead: false,
            quit: false,
            answer: None,
            said: String::new(),
            clipboard: None,
            ready: true,
            picker: None,
            secret: None,
        }
    }

    pub fn key(&mut self, k: KeyEvent, remote: &Remote) {
        if let Some(p) = &mut self.picker {
            if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                self.quit = true;
                return;
            }
            match p.key(&k) {
                Picked::Stay => {}
                Picked::Close => self.picker = None,
                Picked::Run(line) => {
                    self.picker = None;
                    self.line(&line, remote);
                }
            }
            return;
        }
        let Some(act) = action(&k, self.editor.text.is_empty()) else {
            return;
        };
        if self.secret.is_some() {
            match act {
                Action::Submit => return self.give_secret(remote),
                Action::Escape => return self.drop_secret("key entry cancelled"),
                // nothing typed here may be recalled, completed or kept
                Action::Edit(Edit::HistPrev | Edit::HistNext)
                | Action::Complete
                | Action::Newline => return,
                _ => {}
            }
        }
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
            Action::Complete => {
                if let Some(t) = slash::complete(&self.editor.text) {
                    self.editor.clear();
                    self.editor.insert_str(&t);
                }
            }
            Action::Newline => self.editor.insert_str("\n"),
            Action::Scroll(d) => self.feed.scroll_by(d),
            Action::Edit(e) => self.editor.apply(e),
        }
    }

    fn submit(&mut self, remote: &Remote) {
        let Some(text) = self.editor.pending() else {
            return;
        };
        if self.line(&text, remote) {
            self.editor.commit(text);
        }
    }

    /// Carry out one line, typed or picked. True when it was taken — and
    /// so belongs in history.
    fn line(&mut self, text: &str, remote: &Remote) -> bool {
        self.feed.follow = true;
        let session = match slash::parse(text) {
            Line::Bad(why) => {
                self.feed.note(why);
                return false;
            }
            // the prompt stays on the line for when a model is ready
            Line::Prompt(_) if !self.ready => {
                self.feed.note("no model yet: choose a provider first");
                remote.send(Command::Model("setup".into()));
                return false;
            }
            Line::Prompt(p) => Command::Ask(p),
            Line::Command(c) => match c {
                Slash::Clear => Command::Clear,
                Slash::Resume(n) => Command::Resume(n),
                Slash::Compact => Command::Compact,
                Slash::Undo => Command::Undo,
                Slash::Retry => Command::Retry,
                Slash::Context => Command::Context,
                Slash::Model(args) => Command::Model(args),
                Slash::Exit => {
                    self.quit = true;
                    return false;
                }
                Slash::Help | Slash::Cost | Slash::Skills | Slash::Copy => {
                    self.feed.push_user(text, false);
                    self.local(c);
                    return true;
                }
            },
        };
        let echo = match &session {
            Command::Ask(p) => p.clone(),
            _ => text.to_string(),
        };
        // send first: if the session is gone or its inbox is full the prompt
        // would be dropped — don't echo it as if it ran
        if remote.send(session) {
            self.feed.push_user(&echo, self.meter.running);
            true
        } else {
            self.feed.note("not queued — agent unavailable");
            false
        }
    }

    fn give_secret(&mut self, remote: &Remote) {
        let key = self.editor.text.trim().to_string();
        if key.is_empty() {
            return;
        }
        let Some(s) = self.secret.take() else {
            return;
        };
        self.editor.clear();
        self.editor.insert_str(&s.stash);
        // straight to the session: not through the feed, not into history
        let sent = match slash::parse(&format!("{} {key}", s.command)) {
            Line::Command(Slash::Model(args)) => remote.send(Command::Model(args)),
            _ => false,
        };
        self.feed.follow = true;
        self.feed.note(if sent {
            "checking the key…"
        } else {
            "the key could not be sent — agent unavailable"
        });
    }

    fn drop_secret(&mut self, why: &str) {
        if let Some(s) = self.secret.take() {
            self.editor.clear();
            self.editor.insert_str(&s.stash);
            self.feed.note(why);
        }
    }

    /// The commands the UI answers itself, from what it has seen.
    fn local(&mut self, c: Slash) {
        match c {
            Slash::Help => self.help(),
            Slash::Cost => self.cost(),
            Slash::Skills => self.skills(),
            Slash::Copy => match &self.answer {
                Some(a) => {
                    self.feed.note(format!(
                        "copied the last answer ({} chars) — via OSC 52, where the terminal allows it",
                        a.chars().count()
                    ));
                    self.clipboard = Some(a.clone());
                }
                None => self.feed.note("no answer to copy yet"),
            },
            _ => {}
        }
        self.feed.push(Vec::new());
    }

    fn help(&mut self) {
        for s in slash::COMMANDS {
            let names = std::iter::once(&s.name)
                .chain(s.aliases)
                .map(|n| format!("/{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            let left = format!("{names} {}", s.args);
            self.feed.push(vec![
                (format!("  {:<18}", left.trim_end()), C_TOOL_NAME),
                (s.help.into(), C_DIM),
            ]);
        }
        self.feed
            .note("//text sends a prompt that starts with a slash");
        self.feed.note(
            "Tab completes · Esc interrupts · Alt-Enter newline · ↑↓ history · \
             PgUp/PgDn scroll · Ctrl-L clears the screen · Ctrl-C quits",
        );
    }

    fn cost(&mut self) {
        let m = &self.meter;
        let s = &m.session;
        let cached = (s.cached * 100).checked_div(s.input).unwrap_or(0);
        self.feed.note(format!(
            "session ${:.4} · {} tasks ({}✓ {}✗) · ${:.4} per ✓",
            s.cost,
            m.tally.tasks,
            m.tally.ok,
            m.tally.failed(),
            m.per_ok()
        ));
        self.feed.note(format!(
            "tokens ↑{} in ({cached}% from cache) · ↓{} out",
            fmt_tokens(s.input),
            fmt_tokens(s.output)
        ));
        if m.tally.tasks > 0 {
            self.feed.note(format!(
                "last task ${:.4} · ↑{} ↓{}",
                m.task.cost,
                fmt_tokens(m.task.input),
                fmt_tokens(m.task.output)
            ));
        }
    }

    /// `/skills`: what the project and the user offer, and how it loads.
    fn skills(&mut self) {
        let root = Path::new(&self.root);
        let found = deeptui_tools::skill::find_all(root);
        if found.is_empty() {
            self.feed.note(format!(
                "no skills: add {}/<name>/SKILL.md with `name` and `description` frontmatter",
                deeptui_tools::skill::PROJECT_DIRS[0]
            ));
            return;
        }
        for k in &found {
            self.feed.push(vec![
                (format!("  {}", k.name), C_TOOL_NAME),
                (
                    format!(
                        "  {} [{}]",
                        k.description,
                        k.path.strip_prefix(root).unwrap_or(&k.path).display()
                    ),
                    C_DIM,
                ),
            ]);
        }
        self.feed.note(format!(
            "{} skill(s) — the agent loads one when a task matches",
            found.len()
        ));
    }

    fn tool_start(&mut self, name: &str, args: &str) {
        self.feed.push(vec![
            ("  ◆ ".into(), C_TOOL),
            (name.into(), C_TOOL_NAME),
            (format!(" {args}"), C_DIM),
        ]);
    }

    fn tool_end(&mut self, ms: Option<u64>, preview: &str, is_err: bool) {
        let (mark, st) = if is_err {
            ("✗", C_ERR)
        } else {
            ("✓", C_OK)
        };
        let mark = match ms {
            Some(ms) => format!("{mark} {ms}ms"),
            None => mark.into(),
        };
        self.feed.push(vec![
            ("    └ ".into(), C_GUTTER),
            (mark, st),
            (format!("  {preview}"), C_DIM),
        ]);
    }

    /// Draw a conversation the session handed over, the way it streamed —
    /// less the timings and receipts, which history does not keep.
    fn replay(&mut self, msgs: &[Msg]) {
        let results: HashMap<&str, &str> = msgs
            .iter()
            .filter_map(|m| Some((m.tool_call_id.as_deref()?, m.content.as_deref()?)))
            .collect();
        for m in msgs {
            let text = m.content.as_deref().unwrap_or("");
            match m.role {
                Role::User if is_aside(m) => {
                    let head = text.lines().next().unwrap_or("");
                    self.feed.note(one_line(head.trim_matches(['[', ']']), 90));
                }
                Role::User => self.feed.push_user(text, false),
                Role::Assistant => {
                    self.feed.stream_text(text);
                    for c in m.tool_calls.iter().flatten() {
                        self.feed.flush_stream();
                        self.tool_start(&c.function.name, &one_line(&c.function.arguments, 140));
                        let out = results.get(c.id.as_str()).copied().unwrap_or("");
                        let head = out.lines().next().unwrap_or("(empty)");
                        self.tool_end(None, &one_line(head, 90), out.starts_with("error: "));
                    }
                    self.feed.end_message();
                }
                _ => {}
            }
        }
        self.answer = msgs
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && m.tool_calls.is_none())
            .and_then(|m| m.content.clone());
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
            Outcome::Failed(e) => {
                self.feed.push(sline(format!("✗ {e}"), C_ERR));
                if e.starts_with("HTTP 401") || e.starts_with("HTTP 403") {
                    self.feed
                        .note("the key was refused — /model key replaces it");
                }
            }
        }
    }
}

impl Projection for App {
    fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Text(t) => {
                self.said.push_str(t);
                self.feed.stream_text(t);
            }
            Event::ToolStart { name, args } => {
                self.feed.flush_stream();
                self.tool_start(name, args);
            }
            Event::ToolEnd {
                ms,
                preview,
                is_err,
                ..
            } => self.tool_end(Some(*ms), preview, *is_err),
            Event::Compacted(n) => {
                self.feed.flush_stream();
                self.feed.note(format!("⛁ {n}"));
            }
            // read before the meter stops the clock
            Event::Finished(r) => {
                if !self.said.trim().is_empty() {
                    self.answer = Some(std::mem::take(&mut self.said));
                }
                self.receipt(r);
            }
            Event::Notice(n) => {
                self.feed.flush_stream();
                for l in n.lines() {
                    self.feed.note(l);
                }
                self.feed.push(Vec::new());
            }
            Event::Reset(msgs) => {
                self.feed.clear();
                self.replay(msgs);
            }
            Event::Ask(Ask::Choose {
                title,
                options,
                selected,
            }) => {
                self.secret = None;
                self.picker = Some(Picker::new(title.clone(), options.clone(), *selected));
            }
            Event::Ask(Ask::Secret { title, command }) => {
                self.picker = None;
                let stash = match self.secret.take() {
                    // asked again: the draft is still the one from before
                    Some(s) => s.stash,
                    None => std::mem::take(&mut self.editor.text),
                };
                self.editor.clear();
                self.secret = Some(Secret {
                    title: title.clone(),
                    command: command.clone(),
                    stash,
                });
            }
            Event::Model {
                name,
                budget,
                priced,
            } => {
                self.ready = true;
                self.model = name.clone();
                self.context_budget = *budget;
                if !priced {
                    self.feed
                        .note(format!("no price known for {name}: the meter will read $0"));
                }
            }
            Event::Round(_) => self.said.clear(),
            Event::Usage(_) => {}
        }
        self.meter.apply(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styled::SLine;
    use crossterm::event::{KeyCode, KeyModifiers};
    use deeptui_core::{ToolCall, Usage};
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

    #[test]
    fn slash_commands_go_to_the_session_or_are_answered_here() {
        let mut a = App::new("m");
        let (remote, mut rx) = Remote::channel(8);
        let enter = |a: &mut App, text: &str| {
            a.editor.clear();
            a.editor.insert_str(text);
            press(a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        };
        enter(&mut a, "/clear");
        enter(&mut a, "/resume 2");
        enter(&mut a, "/model search qwen coder");
        enter(&mut a, "//etc is a dir");
        assert_eq!(rx.try_recv().unwrap(), Command::Clear);
        assert_eq!(rx.try_recv().unwrap(), Command::Resume(Some(2)));
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::Model("search qwen coder".into())
        );
        assert_eq!(rx.try_recv().unwrap(), Command::Ask("/etc is a dir".into()));
        assert!(texts(&a).contains(&"❯ /resume 2".to_string()));
        assert!(texts(&a).contains(&"❯ /etc is a dir".to_string()));
        assert_eq!(
            a.editor.hist,
            [
                "/clear",
                "/resume 2",
                "/model search qwen coder",
                "//etc is a dir"
            ]
        );

        // a mistyped command stays on the line, with the reason under it
        enter(&mut a, "/fix it");
        assert!(rx.try_recv().is_err());
        assert_eq!(a.editor.text, "/fix it");
        assert!(texts(&a).last().unwrap().contains("unknown command /fix"));

        enter(&mut a, "/help");
        assert!(rx.try_recv().is_err());
        let t = texts(&a);
        assert!(t.iter().any(|l| l.starts_with("  /clear, /new")), "{t:?}");
        assert!(t.iter().any(|l| l.starts_with("  /resume [n]")), "{t:?}");

        enter(&mut a, "/cost");
        assert!(texts(&a)
            .iter()
            .any(|l| l.starts_with("  session $0.0000 · 0 tasks")));
        assert!(!a.quit);
        enter(&mut a, "/exit");
        assert!(a.quit);
    }

    #[test]
    fn the_skills_command_lists_what_the_agent_may_load() {
        let dir = std::env::temp_dir().join(format!("deeptui-app-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".deeptui/skills/pdf")).unwrap();
        std::fs::write(
            dir.join(".deeptui/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: Read PDFs.\n---\nbody",
        )
        .unwrap();
        let mut a = App::new("m");
        a.root = dir.display().to_string();
        let (remote, _rx) = Remote::channel(4);
        a.editor.insert_str("/skills");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);

        let t = texts(&a);
        assert!(
            t.iter()
                .any(|l| l.starts_with("  pdf") && l.contains("Read PDFs.")),
            "{t:?}"
        );
        assert!(t.iter().any(|l| l.contains("skill(s)")), "{t:?}");
        // the body is not printed: the list is what the agent may load
        assert!(!t.iter().any(|l| l.contains("body")), "{t:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tab_completes_a_half_typed_command() {
        let mut a = App::new("m");
        let (remote, _rx) = Remote::channel(4);
        a.editor.insert_str("/comp");
        press(&mut a, KeyCode::Tab, KeyModifiers::NONE, &remote);
        assert_eq!((a.editor.text.as_str(), a.editor.cur), ("/compact", 8));
        // tab elsewhere does nothing
        a.editor.clear();
        a.editor.insert_str("fix it");
        press(&mut a, KeyCode::Tab, KeyModifiers::NONE, &remote);
        assert_eq!(a.editor.text, "fix it");
    }

    #[test]
    fn notices_print_and_a_reset_redraws_the_conversation() {
        let mut a = App::new("m");
        a.feed.note("old screen");
        a.apply(&Event::Notice("one\n  two".into()));
        assert_eq!(texts(&a)[1..], ["  one", "    two", ""]);

        a.apply(&Event::Reset(vec![
            Msg::user("fix it"),
            Msg::assistant(
                Some("looking".into()),
                vec![
                    ToolCall::new("c1", "read_file", r#"{"path":"a.rs"}"#),
                    ToolCall::new("c2", "bash", r#"{"cmd":"false"}"#),
                ],
            ),
            Msg::tool("c1", "fn main() {}\n"),
            Msg::tool("c2", "error: [exit 1]"),
            Msg::user(
                "[context compacted: 4 earlier messages removed to save tokens. What they did:\n…]",
            ),
            Msg::assistant(Some("fixed".into()), vec![]),
        ]));
        let t: Vec<String> = texts(&a).into_iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            t,
            [
                "❯ fix it",
                "● looking",
                r#"  ◆ read_file {"path":"a.rs"}"#,
                "    └ ✓  fn main() {}",
                r#"  ◆ bash {"cmd":"false"}"#,
                "    └ ✗  error: [exit 1]",
                "  context compacted: 4 earlier messages removed to save tokens. What they did:",
                "● fixed",
            ]
        );
        assert_eq!(a.answer.as_deref(), Some("fixed"));

        // a switch renames the header and resets the budget
        a.apply(&Event::Model {
            name: "qwen/qwen3-coder".into(),
            budget: 9_000,
            priced: false,
        });
        assert_eq!(
            (a.model.as_str(), a.context_budget),
            ("qwen/qwen3-coder", 9_000)
        );
        assert!(texts(&a).last().unwrap().contains("no price known"));

        // an empty reset is a clean slate
        a.apply(&Event::Reset(Vec::new()));
        assert!(a.feed.log.is_empty() && a.answer.is_none());
    }

    #[test]
    fn copy_takes_the_last_rounds_text_not_the_thinking_before_tools() {
        let mut a = App::new("m");
        let (remote, _rx) = Remote::channel(4);
        let copy = |a: &mut App| {
            a.editor.insert_str("/copy");
            press(a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        };
        copy(&mut a);
        assert!(a.clipboard.is_none());
        assert!(texts(&a).iter().any(|l| l == "  no answer to copy yet"));

        a.apply(&Event::Round(1));
        a.apply(&Event::Text("let me look".into()));
        a.apply(&Event::Round(2));
        a.apply(&Event::Text("the answer".into()));
        a.apply(&finished(Outcome::Done, 0.0, 0, 0));
        copy(&mut a);
        assert_eq!(a.clipboard.as_deref(), Some("the answer"));
    }

    fn ask(app: &mut App, ask: Ask) {
        app.apply(&Event::Ask(ask));
    }

    #[test]
    fn a_picked_option_runs_as_its_command_line() {
        let mut a = App::new("m");
        let (remote, mut rx) = Remote::channel(4);
        ask(
            &mut a,
            Ask::Choose {
                title: "Choose a provider".into(),
                options: ["deepseek", "openai"]
                    .iter()
                    .map(|id| deeptui_core::Choice {
                        label: id.to_string(),
                        detail: String::new(),
                        command: format!("/model provider {id}"),
                    })
                    .collect(),
                selected: 0,
            },
        );
        a.editor.insert_str("draft");
        // the picker has the keys: the draft is untouched
        press(&mut a, KeyCode::Down, KeyModifiers::NONE, &remote);
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert!(a.picker.is_none());
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::Model("provider openai".into())
        );
        assert_eq!(a.editor.text, "draft");
        assert!(a.editor.hist.is_empty(), "a pick is not typed history");
        assert!(texts(&a).contains(&"❯ /model provider openai".to_string()));

        ask(
            &mut a,
            Ask::Choose {
                title: "t".into(),
                options: Vec::new(),
                selected: 0,
            },
        );
        press(&mut a, KeyCode::Esc, KeyModifiers::NONE, &remote);
        assert!(a.picker.is_none() && rx.try_recv().is_err());
    }

    #[test]
    fn a_secret_goes_to_the_session_and_nowhere_else() {
        let mut a = App::new("m");
        let (remote, mut rx) = Remote::channel(4);
        a.editor.insert_str("half a prompt");
        let secret = || Ask::Secret {
            title: "Paste your DeepSeek API key".into(),
            command: "/model key deepseek".into(),
        };
        ask(&mut a, secret());
        assert_eq!(a.editor.text, "", "the draft steps aside");
        a.editor.insert_str("  sk-very-secret ");
        // no history to recall into it, no completion, no newline
        a.editor.hist.push("old".into());
        press(&mut a, KeyCode::Up, KeyModifiers::NONE, &remote);
        press(&mut a, KeyCode::Tab, KeyModifiers::NONE, &remote);
        assert_eq!(a.editor.text, "  sk-very-secret ");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::Model("key deepseek sk-very-secret".into())
        );
        assert!(a.secret.is_none());
        assert_eq!(a.editor.text, "half a prompt", "the draft is back");
        assert_eq!(a.editor.hist, ["old"]);
        let t = texts(&a);
        assert!(!t.iter().any(|l| l.contains("sk-very")), "{t:?}");
        assert_eq!(t.last().unwrap(), "  checking the key…");

        // asked again after a refusal, then cancelled: the draft survives both
        ask(&mut a, secret());
        ask(&mut a, secret());
        a.editor.insert_str("sk-x");
        press(&mut a, KeyCode::Esc, KeyModifiers::NONE, &remote);
        assert!(a.secret.is_none() && rx.try_recv().is_err());
        assert_eq!(a.editor.text, "half a prompt");
        assert_eq!(texts(&a).last().unwrap(), "  key entry cancelled");
    }

    #[test]
    fn without_a_model_a_prompt_waits_and_setup_opens() {
        let mut a = App::new("m");
        a.ready = false;
        let (remote, mut rx) = Remote::channel(4);
        a.editor.insert_str("fix the bug");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(rx.try_recv().unwrap(), Command::Model("setup".into()));
        assert_eq!(
            a.editor.text, "fix the bug",
            "kept for when a model is ready"
        );
        // commands still work; a switch makes it ready
        a.editor.clear();
        a.editor.insert_str("/model");
        press(&mut a, KeyCode::Enter, KeyModifiers::NONE, &remote);
        assert_eq!(rx.try_recv().unwrap(), Command::Model(String::new()));
        a.apply(&Event::Model {
            name: "deepseek-v4-pro".into(),
            budget: 60_000,
            priced: true,
        });
        assert!(a.ready);

        // a refused key at run time points at the fix
        a.apply(&finished(
            Outcome::Failed("HTTP 401 Unauthorized: bad key".into()),
            0.0,
            0,
            0,
        ));
        assert_eq!(
            texts(&a).last().unwrap(),
            "  the key was refused — /model key replaces it"
        );
    }
}
