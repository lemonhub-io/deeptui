//! A conversation as an actor. It owns the history; frontends hold a
//! [`Remote`] to ask, cancel and steer it, and read the event stream it
//! returns. Both frontends — the TUI and headless mode — drive the very same
//! actor.

use crate::{Engine, Outlet};
use deeptui_core::history::{self, msg_tokens, total_tokens};
use deeptui_core::text::{fmt_tokens, one_line};
use deeptui_core::{Ask, Event, Msg, Projection, Role, Tariff};
use deeptui_provider::Provider;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What a frontend can say to a session. All but `Cancel` run in arrival
/// order, after the turn in flight.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Run this prompt.
    Ask(String),
    /// Interrupt the running turn, if any.
    Cancel,
    /// Start a new conversation; the current one stays on the shelf.
    Clear,
    /// Compact the history now, however far under budget it is.
    Compact,
    /// Forget the last turn — the conversation, not the files it touched.
    Undo,
    /// Forget the last turn and run its prompt again.
    Retry,
    /// List the shelved conversations, or continue the `n`th (from 1,
    /// newest first).
    Resume(Option<usize>),
    /// Say what fills the context.
    Context,
    /// `/model` and its arguments, for the [`Switchboard`].
    Model(String),
}

/// The frontend's handle on a session. Dropping every handle ends it.
#[derive(Clone)]
pub struct Remote(mpsc::Sender<Command>);

impl Remote {
    /// A handle plus the raw inbox it feeds — for driving a frontend
    /// without a real session behind it.
    pub fn channel(capacity: usize) -> (Remote, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Remote(tx), rx)
    }

    /// False when the session is gone or its inbox is full: the command was
    /// not taken.
    pub fn send(&self, c: Command) -> bool {
        self.0.try_send(c).is_ok()
    }

    /// Queue a prompt.
    pub fn ask(&self, prompt: impl Into<String>) -> bool {
        self.send(Command::Ask(prompt.into()))
    }

    pub fn cancel(&self) -> bool {
        self.send(Command::Cancel)
    }
}

/// Where a session keeps its conversations between runs.
pub trait Shelf: Send {
    /// The whole history, after every turn and every rewind.
    fn save(&mut self, history: &[Msg]);
    /// What follows is a new conversation; the current one stays shelved.
    fn fresh(&mut self);
    /// The other shelved conversations, newest first.
    fn list(&self) -> Vec<Shelved>;
    /// Make the `i`th of [`list`](Shelf::list) current and return its
    /// messages, system prompt aside.
    fn open(&mut self, i: usize) -> Option<Vec<Msg>>;
}

/// One entry of a shelf's index.
#[derive(Clone, Debug, PartialEq)]
pub struct Shelved {
    /// Seconds since it was last saved.
    pub age: u64,
    pub messages: usize,
    /// Its first prompt.
    pub title: String,
}

/// What `/model` acts on. The composition root knows where models come
/// from; the session only swaps in the one it is handed.
pub trait Switchboard: Send {
    /// Carry out `/model <args>`.
    fn handle<'a>(&'a mut self, args: &'a str) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>>;
}

/// What `/model` answers.
#[derive(Default)]
pub struct Reply {
    pub text: String,
    /// The model later turns go to.
    pub switch: Option<Switch>,
    /// A question for the user, asked after the text.
    pub ask: Option<Ask>,
}

impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn ask(ask: Ask) -> Self {
        Self {
            ask: Some(ask),
            ..Self::default()
        }
    }
}

pub struct Switch {
    pub name: String,
    pub provider: Provider,
    pub tariff: Tariff,
    pub context_budget: usize,
}

/// Turns `/undo` can step back through.
const UNDO_DEPTH: usize = 20;
const UNSAVED: &str = "conversations are not saved here (save_session = false)";

pub struct Session {
    engine: Engine,
    history: Vec<Msg>,
    taps: Vec<Box<dyn Projection + Send>>,
    shelf: Option<Box<dyn Shelf>>,
    board: Option<Box<dyn Switchboard>>,
}

impl Session {
    /// A session over `history` — usually just the system prompt.
    pub fn new(engine: Engine, history: Vec<Msg>) -> Self {
        Self {
            engine,
            history,
            taps: Vec::new(),
            shelf: None,
            board: None,
        }
    }

    /// Let `/model` switch models through `board`.
    pub fn switchboard(mut self, board: impl Switchboard + 'static) -> Self {
        self.board = Some(Box::new(board));
        self
    }

    /// Keep the conversation on `shelf`: saved each time a turn ends —
    /// before the session takes its next command or notices that its
    /// frontend has left — and resumable from it.
    pub fn shelf(mut self, shelf: impl Shelf + 'static) -> Self {
        self.shelf = Some(Box::new(shelf));
        self
    }

    /// Fold every event into `p` as it happens, whoever is watching.
    pub fn tap(mut self, p: impl Projection + Send + 'static) -> Self {
        self.taps.push(Box::new(p));
        self
    }

    /// Start the actor on the current runtime.
    pub fn spawn(self) -> (Remote, mpsc::Receiver<Event>) {
        let (remote, inbox) = Remote::channel(32);
        let (tx, events) = mpsc::channel(512);
        let mut out = Outlet::new(tx);
        out.taps = self.taps;
        let actor = Actor {
            engine: self.engine,
            history: self.history,
            out,
            shelf: self.shelf,
            board: self.board,
            past: Vec::new(),
        };
        tokio::spawn(actor.run(inbox));
        (remote, events)
    }
}

struct Actor {
    engine: Engine,
    history: Vec<Msg>,
    out: Outlet,
    shelf: Option<Box<dyn Shelf>>,
    board: Option<Box<dyn Switchboard>>,
    /// The history as it stood before each recent turn, with that turn's
    /// prompt: what undo and retry go back to.
    past: Vec<(Vec<Msg>, String)>,
}

impl Actor {
    /// Commands run one at a time in arrival order; `Cancel` stops the
    /// running turn and lets the queue move on. When the frontend goes away
    /// the running turn is cancelled and the queue dropped — tokens nobody
    /// sees are waste.
    async fn run(mut self, mut inbox: mpsc::Receiver<Command>) {
        let mut queue: VecDeque<Command> = VecDeque::new();
        let mut open = true;
        loop {
            let c = match queue.pop_front() {
                Some(c) => c,
                None if !open => return,
                None => match inbox.recv().await {
                    Some(c) => c,
                    None => return,
                },
            };
            let Some(prompt) = self.command(c).await else {
                continue;
            };
            self.past.push((self.history.clone(), prompt.clone()));
            if self.past.len() > UNDO_DEPTH {
                self.past.remove(0);
            }
            let cancel = CancellationToken::new();
            {
                let turn = self
                    .engine
                    .turn(&mut self.history, prompt, &cancel, &mut self.out);
                tokio::pin!(turn);
                loop {
                    tokio::select! {
                        _ = &mut turn => break,
                        c = inbox.recv(), if open => match c {
                            Some(Command::Cancel) => cancel.cancel(),
                            Some(c) => queue.push_back(c),
                            None => {
                                open = false;
                                queue.clear();
                                cancel.cancel();
                            }
                        },
                    }
                }
            }
            self.save();
        }
    }

    /// Carry out `c`. A prompt to run comes back.
    async fn command(&mut self, c: Command) -> Option<String> {
        match c {
            Command::Ask(p) => return Some(p),
            Command::Cancel => {}
            Command::Retry => match self.past.pop() {
                Some((h, p)) => {
                    self.rewind(h).await;
                    self.notice(format!("↻ again: {}", one_line(&p, 80))).await;
                    return Some(p);
                }
                None => self.notice("nothing to retry").await,
            },
            Command::Undo => match self.past.pop() {
                Some((h, p)) => {
                    self.rewind(h).await;
                    self.save();
                    let note = format!("↶ forgot the last turn: {}", one_line(&p, 80));
                    self.notice(note).await;
                }
                None => self.notice("nothing to undo").await,
            },
            Command::Clear => {
                let had = self.history.iter().any(|m| m.role != Role::System);
                if let Some(s) = &mut self.shelf {
                    s.fresh();
                }
                self.past.clear();
                self.rewind(self.system()).await;
                let note = if had && self.shelf.is_some() {
                    "new conversation — /resume 1 goes back to the last one"
                } else {
                    "new conversation"
                };
                self.notice(note).await;
            }
            Command::Compact => self.compact().await,
            Command::Context => {
                let report = self.context();
                self.notice(report).await;
            }
            Command::Resume(None) => {
                let index = self.index();
                self.notice(index).await;
            }
            Command::Resume(Some(n)) => self.resume(n).await,
            Command::Model(args) => self.model(&args).await,
        }
        None
    }

    async fn model(&mut self, args: &str) {
        let reply = match &mut self.board {
            Some(b) => b.handle(args).await,
            None => Reply::text("the model is fixed in this session"),
        };
        if let Some(s) = reply.switch {
            let priced = s.tariff.is_known();
            self.engine.switch(s.provider, s.tariff, s.context_budget);
            let ev = Event::Model {
                name: s.name,
                budget: s.context_budget,
                priced,
            };
            self.out.emit(ev).await;
        }
        if !reply.text.is_empty() {
            self.notice(reply.text).await;
        }
        if let Some(ask) = reply.ask {
            self.out.emit(Event::Ask(ask)).await;
        }
    }

    fn save(&mut self) {
        if let Some(s) = &mut self.shelf {
            s.save(&self.history);
        }
    }

    async fn notice(&mut self, s: impl Into<String>) {
        self.out.emit(Event::Notice(s.into())).await;
    }

    /// Just the system prompt.
    fn system(&self) -> Vec<Msg> {
        self.history
            .iter()
            .filter(|m| m.role == Role::System)
            .cloned()
            .collect()
    }

    /// Swap the history for `h` and show the frontends what it holds now.
    async fn rewind(&mut self, h: Vec<Msg>) {
        self.history = h;
        let shown = self
            .history
            .iter()
            .filter(|m| m.role != Role::System)
            .cloned()
            .collect();
        self.out.emit(Event::Reset(shown)).await;
    }

    async fn compact(&mut self) {
        let before = total_tokens(&self.history);
        let note = history::compact(&mut self.history, 0);
        let after = total_tokens(&self.history);
        match note {
            Some(note) if after < before => {
                self.save();
                let note = format!(
                    "{note} · ~{} → ~{} tokens",
                    fmt_tokens(before as u64),
                    fmt_tokens(after as u64)
                );
                self.out.emit(Event::Compacted(note)).await;
            }
            _ => {
                let note = format!(
                    "nothing to compact yet (~{} tokens)",
                    fmt_tokens(before as u64)
                );
                self.notice(note).await;
            }
        }
    }

    fn context(&self) -> String {
        let (mut system, mut tools, mut talk) = (0, 0, 0);
        for m in &self.history {
            let t = msg_tokens(m);
            match m.role {
                Role::System => system += t,
                Role::Tool => tools += t,
                _ => talk += t,
            }
        }
        let total = system + tools + talk;
        let budget = self.engine.budget();
        let k = |n: usize| fmt_tokens(n as u64);
        format!(
            "context ~{} of {} tokens ({}%) in {} messages\n\
             system {} · tool output {} · conversation {} — compacts itself past {}",
            k(total),
            k(budget),
            total * 100 / budget.max(1),
            self.history.len(),
            k(system),
            k(tools),
            k(talk),
            k(budget),
        )
    }

    fn index(&self) -> String {
        let Some(shelf) = &self.shelf else {
            return UNSAVED.into();
        };
        let list = shelf.list();
        if list.is_empty() {
            return "no earlier conversation here".into();
        }
        let mut s = String::from("earlier conversations here — /resume <n> continues one:");
        for (i, c) in list.iter().enumerate() {
            s += &format!(
                "\n{:>3}  {:>8}  {:>4} msgs  {}",
                i + 1,
                ago(c.age),
                c.messages,
                one_line(&c.title, 60)
            );
        }
        s
    }

    async fn resume(&mut self, n: usize) {
        let found = match &mut self.shelf {
            None => Err(UNSAVED.to_string()),
            Some(shelf) => {
                let list = shelf.list();
                match n.checked_sub(1).filter(|&i| i < list.len()) {
                    None if list.is_empty() => Err("no earlier conversation here".into()),
                    None => Err(format!(
                        "no conversation #{n} — /resume lists {}",
                        list.len()
                    )),
                    Some(i) => shelf
                        .open(i)
                        .map(|msgs| (msgs, list[i].age))
                        .ok_or_else(|| format!("conversation #{n} could not be read")),
                }
            }
        };
        match found {
            Err(e) => self.notice(e).await,
            Ok((msgs, age)) => {
                let n = msgs.len();
                let mut h = self.system();
                h.extend(msgs);
                self.past.clear();
                self.rewind(h).await;
                let note = format!(
                    "↺ continuing the conversation from {} ({n} messages)",
                    ago(age)
                );
                self.notice(note).await;
            }
        }
    }
}

/// `90` → `1m ago`: the coarsest unit that is not zero.
fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{engine, fmt, text_round};
    use deeptui_core::ToolCall;
    use deeptui_testkit::{sse, Stub, TmpDir};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    async fn next(rx: &mut mpsc::Receiver<Event>) -> Event {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("session stalled")
            .expect("session died")
    }

    #[tokio::test]
    async fn queued_prompts_run_in_order_and_taps_see_them_all() {
        let t = TmpDir::new("session");
        let stub = Stub::new();
        for text in ["first answer", "second answer", "third answer"] {
            stub.enqueue(200, sse(&[&text_round(text, "stop"), "[DONE]"]));
        }
        let finished = Arc::new(Mutex::new(Vec::new()));
        let log = finished.clone();
        let (remote, mut events) = Session::new(engine(&stub, t.path()), Vec::new())
            .tap(move |e: &Event| {
                if let Event::Finished(_) = e {
                    log.lock().unwrap().push(fmt(e));
                }
            })
            .spawn();

        // all three are queued before the first one can possibly finish
        assert!(remote.ask("p1") && remote.ask("p2") && remote.ask("p3"));
        let mut dones = 0;
        while dones < 3 {
            if let Event::Finished(_) = next(&mut events).await {
                dones += 1;
            }
        }
        assert_eq!(*finished.lock().unwrap(), ["done", "done", "done"]);

        let reqs = stub.requests();
        assert_eq!(reqs.len(), 3, "one request per prompt");
        assert!(
            reqs[0].contains("p1") && !reqs[0].contains("p2"),
            "{}",
            reqs[0]
        );
        // the history carries over: the second request replays the first turn
        assert!(reqs[1].contains("first answer") && reqs[1].contains("p2"));
        assert!(reqs[2].contains("p3"));
    }

    /// A shelf in memory: the lengths it saved, how often a new
    /// conversation began, and the older conversations it offers.
    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Shelf3>>);
    type Shelf3 = (Vec<usize>, usize, Vec<Vec<Msg>>);

    impl Shelf for Fake {
        fn save(&mut self, h: &[Msg]) {
            self.0.lock().unwrap().0.push(h.len());
        }
        fn fresh(&mut self) {
            self.0.lock().unwrap().1 += 1;
        }
        fn list(&self) -> Vec<Shelved> {
            let s = self.0.lock().unwrap();
            s.2.iter()
                .map(|m| Shelved {
                    age: 7_200,
                    messages: m.len(),
                    title: m[0].content.clone().unwrap(),
                })
                .collect()
        }
        fn open(&mut self, i: usize) -> Option<Vec<Msg>> {
            self.0.lock().unwrap().2.get(i).cloned()
        }
    }

    async fn until_finished(rx: &mut mpsc::Receiver<Event>) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            let e = next(rx).await;
            seen.push(fmt(&e));
            if let Event::Finished(_) = e {
                return seen;
            }
        }
    }

    #[tokio::test]
    async fn the_shelf_sees_each_finished_history_before_the_session_ends() {
        let t = TmpDir::new("session-keep");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&text_round("one", "stop"), "[DONE]"]));
        stub.enqueue(200, sse(&[&text_round("two", "stop"), "[DONE]"]));
        let shelf = Fake::default();
        let (remote, mut events) = Session::new(engine(&stub, t.path()), vec![Msg::system("s")])
            .shelf(shelf.clone())
            .spawn();
        assert!(remote.ask("a") && remote.ask("b"));
        until_finished(&mut events).await;
        until_finished(&mut events).await;
        // a frontend that waits for the stream to close after the last
        // receipt knows the last history was handed over
        drop(remote);
        let closed = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
        assert!(matches!(closed, Ok(None)));
        assert_eq!(shelf.0.lock().unwrap().0, [3, 5]);
    }

    #[tokio::test]
    async fn undo_retry_and_clear_rewind_the_conversation_in_order() {
        let t = TmpDir::new("session-rewind");
        let stub = Stub::new();
        for text in ["one", "two", "three"] {
            stub.enqueue(200, sse(&[&text_round(text, "stop"), "[DONE]"]));
        }
        let shelf = Fake::default();
        let (remote, mut events) = Session::new(engine(&stub, t.path()), vec![Msg::system("s")])
            .shelf(shelf.clone())
            .spawn();
        // queued behind the turns, the rewinds still run in order
        for c in [
            Command::Ask("a".into()),
            Command::Ask("b".into()),
            Command::Undo,
            Command::Retry,
        ] {
            assert!(remote.send(c));
        }
        until_finished(&mut events).await;
        until_finished(&mut events).await;
        assert_eq!(fmt(&next(&mut events).await), "reset:2");
        assert_eq!(
            fmt(&next(&mut events).await),
            "notice:↶ forgot the last turn: b"
        );
        assert_eq!(fmt(&next(&mut events).await), "reset:0");
        assert_eq!(fmt(&next(&mut events).await), "notice:↻ again: a");
        let retried = until_finished(&mut events).await;
        assert!(retried.contains(&"text:three".to_string()), "{retried:?}");
        // the retry went out without the turns it rewound
        let req = stub.requests().pop().unwrap();
        assert!(req.contains("\"a\"") && !req.contains("\"one\"") && !req.contains("\"b\""));

        assert!(remote.send(Command::Clear) && remote.send(Command::Undo));
        assert_eq!(fmt(&next(&mut events).await), "reset:0");
        assert_eq!(
            fmt(&next(&mut events).await),
            "notice:new conversation — /resume 1 goes back to the last one"
        );
        assert_eq!(fmt(&next(&mut events).await), "notice:nothing to undo");
        let s = shelf.0.lock().unwrap();
        // a, b, the undo, the retried a
        assert_eq!((s.0.as_slice(), s.1), ([3, 5, 3, 3].as_slice(), 1));
    }

    #[tokio::test]
    async fn resume_context_and_compact_answer_without_the_model() {
        let t = TmpDir::new("session-resume");
        let stub = Stub::new();
        let shelf = Fake::default();
        shelf.0.lock().unwrap().2 = vec![vec![
            Msg::user("old task"),
            Msg::assistant(Some("old answer".into()), vec![]),
        ]];
        let (remote, mut events) = Session::new(engine(&stub, t.path()), vec![Msg::system("s")])
            .shelf(shelf)
            .spawn();
        for c in [
            Command::Resume(None),
            Command::Resume(Some(2)),
            Command::Resume(Some(1)),
            Command::Context,
            Command::Compact,
        ] {
            assert!(remote.send(c));
        }
        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(fmt(&next(&mut events).await));
        }
        assert!(
            seen[0].contains("\n  1    2h ago     2 msgs  old task"),
            "{}",
            seen[0]
        );
        assert_eq!(seen[1], "notice:no conversation #2 — /resume lists 1");
        assert_eq!(seen[2], "reset:2");
        assert_eq!(
            seen[3],
            "notice:↺ continuing the conversation from 2h ago (2 messages)"
        );
        assert!(seen[4].starts_with("notice:context ~"), "{}", seen[4]);
        assert!(seen[4].contains("in 3 messages"), "{}", seen[4]);
        assert!(
            seen[5].starts_with("notice:nothing to compact yet"),
            "{}",
            seen[5]
        );
        assert!(stub.requests().is_empty());

        // no shelf: nothing to resume from
        let (remote, mut events) = Session::new(engine(&stub, t.path()), Vec::new()).spawn();
        assert!(remote.send(Command::Resume(Some(1))));
        assert_eq!(fmt(&next(&mut events).await), format!("notice:{UNSAVED}"));
    }

    #[tokio::test]
    async fn compact_on_demand_shrinks_a_history_under_budget() {
        let t = TmpDir::new("session-compact");
        let stub = Stub::new();
        let mut h = vec![Msg::system("s"), Msg::user("task")];
        for i in 0..10 {
            let id = format!("c{i}");
            h.push(Msg::assistant(
                None,
                vec![ToolCall::new(&id, "read_file", "{}")],
            ));
            h.push(Msg::tool(&id, "x".repeat(1_000)));
        }
        let (remote, mut events) = Session::new(engine(&stub, t.path()), h).spawn();
        assert!(remote.send(Command::Compact));
        let note = fmt(&next(&mut events).await);
        assert!(note.starts_with("compacted:compacted: elided"), "{note}");
        assert!(note.contains(" → ~"), "{note}");
    }

    /// Answers with its arguments, and hands over its switch once.
    struct Board(Option<Switch>);

    impl Switchboard for Board {
        fn handle<'a>(
            &'a mut self,
            args: &'a str,
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                Reply {
                    text: format!("got {args:?}"),
                    switch: self.0.take(),
                    ask: Some(Ask::Secret {
                        title: "key".into(),
                        command: "/model key x".into(),
                    }),
                }
            })
        }
    }

    #[tokio::test]
    async fn a_switch_sends_later_turns_to_the_new_model() {
        let t = TmpDir::new("session-model");
        let (old, new) = (Stub::new(), Stub::new());
        new.enqueue(200, sse(&[&text_round("from new", "stop"), "[DONE]"]));
        let provider = Provider::new(deeptui_provider::Endpoint {
            base_url: new.base_url(),
            api_key: "k2".into(),
            model: "other-model".into(),
            max_output_tokens: 64,
        })
        .unwrap();
        let switch = Switch {
            name: "other-model".into(),
            provider,
            tariff: Tariff::default(),
            context_budget: 9_000,
        };
        let (remote, mut events) = Session::new(engine(&old, t.path()), Vec::new())
            .switchboard(Board(Some(switch)))
            .spawn();
        for c in [
            Command::Model("2".into()),
            Command::Ask("hi".into()),
            Command::Model(String::new()),
            Command::Context,
        ] {
            assert!(remote.send(c));
        }
        assert_eq!(fmt(&next(&mut events).await), "model:other-model:9000");
        assert_eq!(fmt(&next(&mut events).await), "notice:got \"2\"");
        assert_eq!(fmt(&next(&mut events).await), "secret:key:/model key x");
        let seen = until_finished(&mut events).await;
        assert!(seen.contains(&"text:from new".to_string()), "{seen:?}");
        assert!(old.requests().is_empty());
        assert!(new.requests()[0].contains("\"other-model\""));
        // nothing more to switch to: just the answer
        assert_eq!(fmt(&next(&mut events).await), "notice:got \"\"");
        next(&mut events).await;
        let context = fmt(&next(&mut events).await);
        assert!(context.contains("of 9.0k tokens"), "{context}");

        // no switchboard: the model stays
        let (remote, mut events) = Session::new(engine(&old, t.path()), Vec::new()).spawn();
        assert!(remote.send(Command::Model(String::new())));
        assert_eq!(
            fmt(&next(&mut events).await),
            "notice:the model is fixed in this session"
        );
    }

    #[test]
    fn ages_read_in_their_coarsest_unit() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(90), "1m ago");
        assert_eq!(ago(7_200), "2h ago");
        assert_eq!(ago(3 * 86_400 + 5), "3d ago");
    }

    #[tokio::test]
    async fn an_idle_cancel_is_harmless_and_dropping_the_remote_ends_the_session() {
        let t = TmpDir::new("session-end");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&text_round("hi", "stop"), "[DONE]"]));
        let (remote, mut events) = Session::new(engine(&stub, t.path()), Vec::new()).spawn();
        assert!(remote.cancel());
        assert!(remote.ask("hello"));
        assert_eq!(fmt(&next(&mut events).await), "round:1");
        while !matches!(next(&mut events).await, Event::Finished(_)) {}
        drop(remote);
        let closed = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
        assert!(
            matches!(closed, Ok(None)),
            "the actor outlived its frontend"
        );
    }

    #[test]
    fn a_full_inbox_refuses_instead_of_blocking() {
        let (remote, mut inbox) = Remote::channel(1);
        assert!(remote.ask("one"));
        assert!(!remote.ask("two"), "the inbox was full");
        assert_eq!(inbox.try_recv().unwrap(), Command::Ask("one".into()));
        drop(inbox);
        assert!(!remote.cancel(), "the session is gone");
    }
}
