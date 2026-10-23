//! One turn — a prompt to the model's final answer — as a pure state machine.
//!
//! The turn performs no IO. It says what it needs next ([`Turn::poll`]) and is
//! told what happened: completion deltas ([`Turn::absorb`]), how the stream
//! ended ([`Turn::settle`]), tool results ([`Turn::resolve`]). What it learns
//! leaves through an outbox of [`Event`]s ([`Turn::events`]). Whoever drives
//! it — the async engine, or a test reading from a script — decides how.
//!
//! ```text
//!         ┌───────────────── resolve() × n ─────────────────┐
//!         ▼                                                 │
//!       Ready ── poll() ──▶ Asking ── settle() ──▶ Running ─┘
//!                              │
//!                              └──── settle() ──▶ Over ── poll() ──▶ Finished
//! ```

use crate::event::{Event, Outcome, Receipt, Usage};
use crate::history;
use crate::msg::{Msg, ToolCall};
use crate::text::one_line;
use std::collections::{BTreeMap, VecDeque};

/// One decoded piece of a streamed completion, provider-neutral.
#[derive(Clone, Debug, PartialEq)]
pub enum Delta {
    Text(String),
    /// A fragment of tool call `idx`: id and name arrive once, args in pieces.
    Call {
        idx: usize,
        id: Option<String>,
        name: Option<String>,
        args: String,
    },
    Usage(Usage),
    /// The provider's finish reason (`stop`, `length`, `tool_calls`, …).
    Finish(String),
    /// An error the provider reported inside an otherwise healthy stream.
    Error(String),
}

/// How the completion stream ended, as the transport saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEnd {
    Complete,
    Cancelled,
    Failed(String),
}

/// What the turn needs from the world next.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    /// Stream a completion of [`Turn::history`]; feed each delta to
    /// [`Turn::absorb`], then [`Turn::settle`].
    Ask,
    /// Run these calls and [`Turn::resolve`] each by its index here.
    Run(Vec<ToolCall>),
    /// The turn is over.
    Finished(Receipt),
}

/// A tool's answer: model-facing text either way.
pub type ToolOutput = Result<String, String>;

/// The reply being streamed: text, tool-call fragments by index, and how it ended.
#[derive(Default)]
struct Reply {
    text: String,
    calls: BTreeMap<usize, (String, String, String)>,
    finish: String,
    error: Option<String>,
}

impl Reply {
    /// Assembled calls in index order; nameless fragments are dropped and a
    /// missing id is made up, unique within the round.
    fn take_calls(&mut self, round: usize) -> Vec<ToolCall> {
        std::mem::take(&mut self.calls)
            .into_iter()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(idx, (id, name, args))| {
                let id = if id.is_empty() {
                    format!("call_{round}_{idx}")
                } else {
                    id
                };
                ToolCall::new(id, name, args)
            })
            .collect()
    }
}

struct Done {
    content: String,
    is_err: bool,
    ms: u64,
}

/// Calls in flight. Results may land in any order; their ends are announced
/// — and committed to the history — in call order, like a reorder buffer.
struct Batch {
    calls: Vec<ToolCall>,
    done: Vec<Option<Done>>,
    /// Ends announced so far: always a prefix of `calls`.
    shown: usize,
}

enum State {
    Ready,
    Asking,
    Running(Batch),
    Over(Receipt),
}

pub struct Turn<'h> {
    history: &'h mut Vec<Msg>,
    budget: usize,
    round: usize,
    state: State,
    reply: Reply,
    usage: Usage,
    outbox: VecDeque<Event>,
}

impl<'h> Turn<'h> {
    /// Open a turn on `history` with a new user prompt. `budget` is the token
    /// estimate above which the history is compacted before a request.
    pub fn begin(history: &'h mut Vec<Msg>, prompt: impl Into<String>, budget: usize) -> Self {
        history.push(Msg::user(prompt));
        Turn {
            history,
            budget,
            round: 0,
            state: State::Ready,
            reply: Reply::default(),
            usage: Usage::default(),
            outbox: VecDeque::new(),
        }
    }

    /// The messages the next request must carry.
    pub fn history(&self) -> &[Msg] {
        self.history
    }

    pub fn poll(&mut self) -> Next {
        match &self.state {
            State::Ready => self.open_round(),
            State::Asking => Next::Ask,
            State::Running(b) => Next::Run(b.calls.clone()),
            State::Over(r) => Next::Finished(r.clone()),
        }
    }

    /// Facts produced since the last drain, oldest first.
    pub fn events(&mut self) -> std::collections::vec_deque::Drain<'_, Event> {
        self.outbox.drain(..)
    }

    fn open_round(&mut self) -> Next {
        self.round += 1;
        self.outbox.push_back(Event::Round(self.round));
        history::repair(self.history);
        if let Some(note) = history::compact(self.history, self.budget) {
            self.outbox.push_back(Event::Compacted(note));
        }
        self.state = State::Asking;
        Next::Ask
    }

    pub fn absorb(&mut self, d: Delta) {
        if !matches!(self.state, State::Asking) {
            return;
        }
        match d {
            Delta::Text(t) => {
                self.reply.text.push_str(&t);
                self.outbox.push_back(Event::Text(t));
            }
            Delta::Call {
                idx,
                id,
                name,
                args,
            } => {
                let (i, n, a) = self.reply.calls.entry(idx).or_default();
                i.push_str(id.as_deref().unwrap_or(""));
                n.push_str(name.as_deref().unwrap_or(""));
                a.push_str(&args);
            }
            Delta::Usage(u) => {
                self.usage += u;
                self.outbox.push_back(Event::Usage(u));
            }
            Delta::Finish(f) => self.reply.finish = f,
            // the first error is the cause; later ones are fallout
            Delta::Error(e) => {
                self.reply.error.get_or_insert(e);
            }
        }
    }

    pub fn settle(&mut self, end: StreamEnd) {
        if !matches!(self.state, State::Asking) {
            return;
        }
        let mut reply = std::mem::take(&mut self.reply);
        // A failed round records nothing: half a reply is not a message.
        let interrupted = match end {
            StreamEnd::Failed(e) => return self.finish(Outcome::Failed(e)),
            _ if reply.error.is_some() => {
                return self.finish(Outcome::Failed(reply.error.unwrap_or_default()))
            }
            StreamEnd::Cancelled => true,
            StreamEnd::Complete => false,
        };
        // On cancel, partial tool calls are untrusted — truncated args, never
        // executed — so only the text survives.
        let calls = if interrupted {
            Vec::new()
        } else {
            reply.take_calls(self.round)
        };
        let content = (!reply.text.is_empty()).then_some(reply.text);
        // Never persist an assistant message with neither content nor
        // tool_calls: the next request would be rejected.
        if content.is_some() || !calls.is_empty() {
            self.history.push(Msg::assistant(content, calls.clone()));
        }
        if interrupted {
            return self.finish(Outcome::Interrupted);
        }
        if calls.is_empty() {
            return self.finish(if reply.finish == "length" {
                Outcome::Failed("hit max output tokens".into())
            } else {
                Outcome::Done
            });
        }
        for c in &calls {
            self.outbox.push_back(Event::ToolStart {
                name: c.function.name.clone(),
                args: one_line(&c.function.arguments, 140),
            });
        }
        self.state = State::Running(Batch {
            done: calls.iter().map(|_| None).collect(),
            calls,
            shown: 0,
        });
    }

    /// The result of call `idx` of the current batch, which took `ms`. The
    /// first answer for a call wins; later ones are ignored.
    pub fn resolve(&mut self, idx: usize, out: ToolOutput, ms: u64) {
        let State::Running(b) = &mut self.state else {
            return;
        };
        match b.done.get_mut(idx) {
            Some(slot) if slot.is_none() => {
                let (content, is_err) = match out {
                    Ok(s) => (s, false),
                    Err(e) => (format!("error: {e}"), true),
                };
                *slot = Some(Done {
                    content,
                    is_err,
                    ms,
                });
            }
            _ => return,
        }
        while let Some(Some(d)) = b.done.get(b.shown) {
            let preview = d.content.lines().next().unwrap_or("(empty)");
            self.outbox.push_back(Event::ToolEnd {
                name: b.calls[b.shown].function.name.clone(),
                ms: d.ms,
                preview: one_line(preview, 90),
                is_err: d.is_err,
            });
            b.shown += 1;
        }
        if b.shown < b.calls.len() {
            return;
        }
        if let State::Running(b) = std::mem::replace(&mut self.state, State::Ready) {
            for (c, d) in b.calls.into_iter().zip(b.done.into_iter().flatten()) {
                self.history.push(Msg::tool(c.id, d.content));
            }
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        let r = Receipt {
            outcome,
            usage: self.usage,
            rounds: self.round,
        };
        self.outbox.push_back(Event::Finished(r.clone()));
        self.state = State::Over(r);
    }
}

#[cfg(test)]
mod tests {
    //! The whole agent loop, driven by hand: no runtime, no sockets, no files.
    use super::*;
    use crate::msg::Role;

    fn text(t: &str) -> Delta {
        Delta::Text(t.into())
    }

    fn call(idx: usize, id: Option<&str>, name: Option<&str>, args: &str) -> Delta {
        Delta::Call {
            idx,
            id: id.map(Into::into),
            name: name.map(Into::into),
            args: args.into(),
        }
    }

    fn usage(input: u64, output: u64, cost: f64) -> Delta {
        Delta::Usage(Usage {
            input,
            output,
            cached: 0,
            cost,
        })
    }

    /// One-line rendering of an event, so sequences compare as Vecs.
    fn fmt(e: &Event) -> String {
        match e {
            Event::Round(n) => format!("round:{n}"),
            Event::Text(t) => format!("text:{t}"),
            Event::ToolStart { name, args } => format!("start:{name}:{args}"),
            Event::ToolEnd {
                name,
                preview,
                is_err,
                ..
            } => format!(
                "end:{name}:{}:{preview}",
                if *is_err { "err" } else { "ok" }
            ),
            Event::Compacted(n) => format!("compacted:{n}"),
            Event::Usage(u) => format!("usage:{}:{}", u.input, u.output),
            Event::Finished(r) => format!("finished:{:?}", r.outcome),
        }
    }

    fn drain(t: &mut Turn) -> Vec<String> {
        t.events().map(|e| fmt(&e)).collect()
    }

    /// Feed one completion's deltas and close the stream.
    fn reply(t: &mut Turn, deltas: Vec<Delta>, end: StreamEnd) {
        assert_eq!(t.poll(), Next::Ask);
        for d in deltas {
            t.absorb(d);
        }
        t.settle(end);
    }

    #[test]
    fn a_plain_answer_finishes_in_one_round() {
        let mut h = vec![Msg::system("sys")];
        let mut t = Turn::begin(&mut h, "hi", 60_000);
        reply(
            &mut t,
            vec![
                text("hel"),
                text("lo"),
                usage(11, 3, 0.5),
                Delta::Finish("stop".into()),
            ],
            StreamEnd::Complete,
        );
        let Next::Finished(r) = t.poll() else {
            panic!("not finished")
        };
        assert_eq!(
            (r.outcome, r.rounds, r.usage.input, r.usage.cost),
            (Outcome::Done, 1, 11, 0.5)
        );
        assert_eq!(
            drain(&mut t),
            [
                "round:1",
                "text:hel",
                "text:lo",
                "usage:11:3",
                "finished:Done"
            ]
        );
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].content.as_deref(), Some("hello"));
    }

    #[test]
    fn tool_calls_assemble_from_fragments_run_and_feed_back() {
        let mut h = vec![Msg::system("sys")];
        let mut t = Turn::begin(&mut h, "go", 60_000);
        reply(
            &mut t,
            vec![
                call(0, Some("c1"), Some("ls"), "{\"pa"),
                call(0, None, None, "th\":\".\"}"),
            ],
            StreamEnd::Complete,
        );
        let Next::Run(calls) = t.poll() else {
            panic!("expected a batch")
        };
        assert_eq!(calls, [ToolCall::new("c1", "ls", "{\"path\":\".\"}")]);
        t.resolve(0, Ok("a.txt\nb.txt".into()), 3);
        // the batch is complete: the next poll opens round 2
        assert_eq!(t.poll(), Next::Ask);
        t.absorb(text("done"));
        t.settle(StreamEnd::Complete);
        assert!(matches!(t.poll(), Next::Finished(r) if r.rounds == 2));
        assert_eq!(
            drain(&mut t),
            [
                "round:1",
                "start:ls:{\"path\":\".\"}",
                "end:ls:ok:a.txt",
                "round:2",
                "text:done",
                "finished:Done",
            ]
        );
        let roles: Vec<Role> = h.iter().map(|m| m.role).collect();
        use Role::*;
        assert_eq!(roles, [System, User, Assistant, Tool, Assistant]);
        assert_eq!(h[3].content.as_deref(), Some("a.txt\nb.txt"));
        assert_eq!(h[3].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn ends_are_announced_and_committed_in_call_order() {
        let mut h = Vec::new();
        let mut t = Turn::begin(&mut h, "go", 60_000);
        reply(
            &mut t,
            vec![
                call(0, Some("a"), Some("grep"), "{}"),
                call(1, Some("b"), Some("glob"), "{}"),
            ],
            StreamEnd::Complete,
        );
        assert!(matches!(t.poll(), Next::Run(c) if c.len() == 2));
        t.events().for_each(drop);

        // the second call lands first: its end waits for the first one
        t.resolve(1, Err("no matches".into()), 1);
        assert!(drain(&mut t).is_empty());
        t.resolve(1, Ok("late duplicate".into()), 1);
        t.resolve(7, Ok("no such call".into()), 1);
        t.resolve(0, Ok("hit".into()), 2);
        assert_eq!(
            drain(&mut t),
            ["end:grep:ok:hit", "end:glob:err:error: no matches"]
        );
        let ids: Vec<_> = h.iter().filter_map(|m| m.tool_call_id.as_deref()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(h[3].content.as_deref(), Some("error: no matches"));
    }

    #[test]
    fn cancel_keeps_the_text_but_drops_untrusted_calls() {
        let mut h = vec![Msg::system("sys")];
        let mut t = Turn::begin(&mut h, "go", 60_000);
        reply(
            &mut t,
            vec![
                text("partial"),
                call(0, Some("c"), Some("bash"), "{\"cmd\":\"rm -"),
            ],
            StreamEnd::Cancelled,
        );
        assert!(matches!(t.poll(), Next::Finished(r) if r.outcome == Outcome::Interrupted));
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].content.as_deref(), Some("partial"));
        assert!(h[2].tool_calls.is_none());
    }

    #[test]
    fn a_cancel_before_any_output_records_nothing() {
        let mut h = vec![Msg::system("sys")];
        let mut t = Turn::begin(&mut h, "go", 60_000);
        reply(&mut t, vec![], StreamEnd::Cancelled);
        assert_eq!(drain(&mut t), ["round:1", "finished:Interrupted"]);
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn failures_end_the_turn_and_record_nothing() {
        for (deltas, end, why) in [
            (
                vec![text("half")],
                StreamEnd::Failed("reset".into()),
                "reset",
            ),
            (
                vec![
                    Delta::Error("rate limited".into()),
                    Delta::Error("again".into()),
                ],
                StreamEnd::Complete,
                "rate limited",
            ),
        ] {
            let mut h = vec![Msg::system("sys")];
            let mut t = Turn::begin(&mut h, "hi", 60_000);
            reply(&mut t, deltas, end);
            assert!(
                matches!(t.poll(), Next::Finished(r) if r.outcome == Outcome::Failed(why.into()))
            );
            assert_eq!(h.len(), 2, "{why}: a failed round left a message");
        }
    }

    #[test]
    fn a_length_stop_fails_but_keeps_the_partial_answer() {
        let mut h = Vec::new();
        let mut t = Turn::begin(&mut h, "essay", 60_000);
        reply(
            &mut t,
            vec![text("truncated"), Delta::Finish("length".into())],
            StreamEnd::Complete,
        );
        let want = Outcome::Failed("hit max output tokens".into());
        assert!(matches!(t.poll(), Next::Finished(r) if r.outcome == want));
        assert_eq!(h[1].content.as_deref(), Some("truncated"));
    }

    #[test]
    fn the_receipt_sums_usage_over_every_round() {
        let mut h = Vec::new();
        let mut t = Turn::begin(&mut h, "go", 60_000);
        reply(
            &mut t,
            vec![call(0, None, Some("ls"), "{}"), usage(10, 1, 0.25)],
            StreamEnd::Complete,
        );
        // a call without an id gets one that cannot collide within the round
        assert!(matches!(t.poll(), Next::Run(c) if c[0].id == "call_1_0"));
        t.resolve(0, Ok(String::new()), 0);
        reply(
            &mut t,
            vec![text("ok"), usage(20, 2, 0.5)],
            StreamEnd::Complete,
        );
        let Next::Finished(r) = t.poll() else {
            panic!("not finished")
        };
        assert_eq!((r.usage.input, r.usage.output, r.usage.cost), (30, 3, 0.75));
        assert_eq!(r.rounds, 2);
        // an empty tool output still answers its call
        assert!(drain(&mut t).contains(&"end:ls:ok:(empty)".to_string()));
    }

    #[test]
    fn stray_inputs_outside_their_state_are_ignored() {
        let mut h = Vec::new();
        let mut t = Turn::begin(&mut h, "go", 60_000);
        // nothing is streaming yet
        t.absorb(text("early"));
        t.settle(StreamEnd::Complete);
        t.resolve(0, Ok("x".into()), 0);
        assert_eq!(t.poll(), Next::Ask);
        assert_eq!(drain(&mut t), ["round:1"]);
        // polling again while asking is idempotent: no second round opens
        assert_eq!(t.poll(), Next::Ask);
        assert!(drain(&mut t).is_empty());
    }

    #[test]
    fn each_round_repairs_and_compacts_the_history() {
        let mut h = vec![Msg::system("sys"), Msg::assistant(None, vec![])];
        for i in 0..20 {
            h.push(Msg::assistant(
                None,
                vec![ToolCall::new(format!("c{i}"), "ls", "{}")],
            ));
            h.push(Msg::tool(format!("c{i}"), "x".repeat(2000)));
        }
        let mut t = Turn::begin(&mut h, "go", 2_000);
        assert_eq!(t.poll(), Next::Ask);
        let evs = drain(&mut t);
        assert!(evs[1].starts_with("compacted:"), "{evs:?}");
        // the empty assistant message is gone before the request is built
        assert!(t
            .history()
            .iter()
            .all(|m| m.content.is_some() || m.tool_calls.is_some()));
    }
}
