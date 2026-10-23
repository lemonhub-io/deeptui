//! A conversation as an actor. It owns the history; frontends hold a
//! [`Remote`] to ask and cancel, and read the event stream it returns. Both
//! frontends — the TUI and headless mode — drive the very same actor.

use crate::{Engine, Outlet};
use deeptui_core::{Event, Msg, Projection};
use std::collections::VecDeque;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What a frontend can say to a session.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Run this prompt once every earlier one has finished.
    Ask(String),
    /// Interrupt the running turn, if any.
    Cancel,
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

    /// Queue a prompt. False when the session is gone or its inbox is full:
    /// the prompt was not taken.
    pub fn ask(&self, prompt: impl Into<String>) -> bool {
        self.0.try_send(Command::Ask(prompt.into())).is_ok()
    }

    pub fn cancel(&self) -> bool {
        self.0.try_send(Command::Cancel).is_ok()
    }
}

pub struct Session {
    engine: Engine,
    history: Vec<Msg>,
    taps: Vec<Box<dyn Projection + Send>>,
}

impl Session {
    /// A session over `history` — usually just the system prompt.
    pub fn new(engine: Engine, history: Vec<Msg>) -> Self {
        Self {
            engine,
            history,
            taps: Vec::new(),
        }
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
        let mut outlet = Outlet::new(tx);
        outlet.taps = self.taps;
        tokio::spawn(actor(self.engine, self.history, outlet, inbox));
        (remote, events)
    }
}

/// Prompts run one at a time in arrival order; `Cancel` stops the running
/// one and lets the queue move on. When the frontend goes away the running
/// turn is cancelled and the queue dropped — tokens nobody sees are waste.
async fn actor(
    engine: Engine,
    mut history: Vec<Msg>,
    mut out: Outlet,
    mut inbox: mpsc::Receiver<Command>,
) {
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut open = true;
    loop {
        let prompt = match queue.pop_front() {
            Some(p) => p,
            None if !open => return,
            None => match inbox.recv().await {
                Some(Command::Ask(p)) => p,
                Some(Command::Cancel) => continue,
                None => return,
            },
        };
        let cancel = CancellationToken::new();
        let turn = engine.turn(&mut history, prompt, &cancel, &mut out);
        tokio::pin!(turn);
        loop {
            tokio::select! {
                _ = &mut turn => break,
                c = inbox.recv(), if open => match c {
                    Some(Command::Ask(p)) => queue.push_back(p),
                    Some(Command::Cancel) => cancel.cancel(),
                    None => {
                        open = false;
                        queue.clear();
                        cancel.cancel();
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{engine, fmt, text_round};
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
