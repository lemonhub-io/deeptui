//! The interpreter. [`deeptui_core::Turn`] decides; the [`Engine`] does: it
//! streams completions from the provider, executes tool calls in effect-
//! derived waves, prices usage by the clock, and pours the turn's events into
//! an [`Outlet`]. The [`Session`] actor owns a conversation between turns.

mod prompt;
mod session;

pub use prompt::system_prompt;
pub use session::{Command, Remote, Session};

use deeptui_core::schedule::waves;
use deeptui_core::{
    Delta, Event, Msg, Next, Projection, Receipt, StreamEnd, Tariff, ToolCall, Turn,
};
use deeptui_provider::Provider;
use deeptui_tools::Toolbox;
use serde_json::Value;
use std::future::Future;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Where a turn's events go. Every tap sees each event first — synchronously,
/// so no projection can miss one — then the channel carries it on.
pub struct Outlet {
    tx: mpsc::Sender<Event>,
    taps: Vec<Box<dyn Projection + Send>>,
}

impl Outlet {
    pub fn new(tx: mpsc::Sender<Event>) -> Self {
        Self {
            tx,
            taps: Vec::new(),
        }
    }

    pub fn tap(mut self, p: impl Projection + Send + 'static) -> Self {
        self.taps.push(Box::new(p));
        self
    }

    async fn flush(&mut self, turn: &mut Turn<'_>) {
        for ev in turn.events() {
            for t in &mut self.taps {
                t.apply(&ev);
            }
            // a frontend that left only stops watching; the turn goes on
            let _ = self.tx.send(ev).await;
        }
    }
}

pub struct Engine {
    provider: Provider,
    toolbox: Toolbox,
    tariff: Tariff,
    budget: usize,
    schemas: Vec<Value>,
}

impl Engine {
    /// `context_budget` is the token estimate above which history is compacted.
    pub fn new(
        provider: Provider,
        toolbox: Toolbox,
        tariff: Tariff,
        context_budget: usize,
    ) -> Self {
        Self {
            schemas: toolbox.schemas(),
            provider,
            toolbox,
            tariff,
            budget: context_budget,
        }
    }

    /// Run one turn on `history` to its end. Cancelling stops the stream or
    /// the remaining tool waves; the turn still closes with a receipt.
    pub async fn turn(
        &self,
        history: &mut Vec<Msg>,
        prompt: String,
        cancel: &CancellationToken,
        out: &mut Outlet,
    ) -> Receipt {
        let mut turn = Turn::begin(history, prompt, self.budget);
        loop {
            let next = turn.poll();
            out.flush(&mut turn).await;
            match next {
                Next::Ask => {
                    let end = self.ask(&mut turn, cancel, out).await;
                    turn.settle(end);
                }
                Next::Run(calls) => self.run(&mut turn, &calls, cancel, out).await,
                Next::Finished(r) => return r,
            }
        }
    }

    async fn ask(
        &self,
        turn: &mut Turn<'_>,
        cancel: &CancellationToken,
        out: &mut Outlet,
    ) -> StreamEnd {
        if cancel.is_cancelled() {
            return StreamEnd::Cancelled;
        }
        let req = self.provider.request(turn.history(), &self.schemas);
        let mut completion = tokio::select! {
            _ = cancel.cancelled() => return StreamEnd::Cancelled,
            r = self.provider.open(&req) => match r {
                Ok(c) => c,
                Err(e) => return StreamEnd::Failed(format!("{e:#}")),
            },
        };
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return StreamEnd::Cancelled,
                d = completion.next() => d,
            };
            match next {
                Ok(Some(d)) => {
                    turn.absorb(self.price(d));
                    out.flush(turn).await;
                }
                Ok(None) => return StreamEnd::Complete,
                Err(e) => return StreamEnd::Failed(format!("{e:#}")),
            }
        }
    }

    /// Usage arrives as bare token counts; it leaves with its price.
    fn price(&self, d: Delta) -> Delta {
        match d {
            Delta::Usage(mut u) => {
                u.cost = self.tariff.cost(&u, unix_now());
                Delta::Usage(u)
            }
            d => d,
        }
    }

    /// Execute a batch wave by wave: reads in a wave run concurrently, waves
    /// run in order, and a cancel answers every call not yet started.
    async fn run(
        &self,
        turn: &mut Turn<'_>,
        calls: &[ToolCall],
        cancel: &CancellationToken,
        out: &mut Outlet,
    ) {
        let effects = calls.iter().map(|c| self.toolbox.effect(&c.function.name));
        for wave in waves(effects) {
            if cancel.is_cancelled() {
                for i in wave {
                    turn.resolve(i, Err("cancelled".into()), 0);
                }
            } else if wave.len() == 1 {
                let f = &calls[wave.start].function;
                let (res, ms) = timed(self.toolbox.call(&f.name, &f.arguments)).await;
                turn.resolve(wave.start, res, ms);
            } else {
                let mut set = JoinSet::new();
                for i in wave.clone() {
                    let (tb, f) = (self.toolbox.clone(), calls[i].function.clone());
                    set.spawn(async move { (i, timed(tb.call(&f.name, &f.arguments)).await) });
                }
                while let Some(joined) = set.join_next().await {
                    if let Ok((i, (res, ms))) = joined {
                        turn.resolve(i, res, ms);
                        out.flush(turn).await;
                    }
                }
                // a task that panicked produced nothing: answer for it, so
                // every call keeps a result and the history stays valid
                for i in wave {
                    turn.resolve(i, Err("tool task failed".into()), 0);
                }
            }
            out.flush(turn).await;
        }
    }
}

async fn timed<T>(f: impl Future<Output = T>) -> (T, u64) {
    let t = Instant::now();
    let v = f.await;
    (v, t.elapsed().as_millis() as u64)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_core::{Outcome, Role};
    use deeptui_provider::Endpoint;
    use deeptui_testkit::{sse, Stub, TmpDir};
    use deeptui_tools::Workspace;
    use std::path::Path;

    pub(crate) fn engine(stub: &Stub, root: &Path) -> Engine {
        let provider = Provider::new(Endpoint {
            api_key: "test-key".into(),
            base_url: stub.base_url(),
            model: "test-model".into(),
            max_output_tokens: 64,
        })
        .unwrap();
        let tariff = Tariff {
            input: 0.1,
            output: 0.2,
            cached: 0.01,
            deepseek_peak: false,
        };
        Engine::new(
            provider,
            Toolbox::new(Workspace::new(root, 5_000)),
            tariff,
            60_000,
        )
    }

    /// One-line rendering of an event, so sequences can be compared as Vecs.
    pub(crate) fn fmt(e: &Event) -> String {
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
            Event::Usage(u) => format!("usage:{}:{}:{}", u.input, u.output, u.cached),
            Event::Finished(r) => match &r.outcome {
                Outcome::Done => "done".into(),
                Outcome::Interrupted => "interrupted".into(),
                Outcome::Failed(e) => format!("failed:{e}"),
            },
        }
    }

    async fn drain(mut rx: mpsc::Receiver<Event>) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(e) = rx.recv().await {
            out.push(fmt(&e));
        }
        out
    }

    /// A completion whose only tool call is `name` with raw JSON `args`.
    fn tool_round(id: &str, name: &str, args: &str) -> String {
        serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": id,
                    "function": {"name": name, "arguments": args},
                }]},
                "finish_reason": "tool_calls",
            }]
        })
        .to_string()
    }

    /// A completion that only emits text.
    pub(crate) fn text_round(text: &str, finish: &str) -> String {
        serde_json::json!({
            "choices": [{"delta": {"content": text}, "finish_reason": finish}]
        })
        .to_string()
    }

    /// A completion with several tool calls, as `(id, name, args)`.
    fn batch(calls: &[(&str, &str, &str)]) -> String {
        let tcs: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (id, name, args))| {
                serde_json::json!({"index": i, "id": id, "function": {"name": name, "arguments": args}})
            })
            .collect();
        serde_json::json!({"choices": [{"delta": {"tool_calls": tcs}, "finish_reason": "tool_calls"}]})
            .to_string()
    }

    /// Run one turn against `stub` and return the events plus final history.
    async fn turn_with(
        stub: &Stub,
        root: &Path,
        prompt: &str,
        cancel: CancellationToken,
    ) -> (Vec<String>, Vec<Msg>) {
        let (tx, rx) = mpsc::channel(128);
        let mut msgs = vec![Msg::system("sys")];
        let mut out = Outlet::new(tx);
        engine(stub, root)
            .turn(&mut msgs, prompt.to_string(), &cancel, &mut out)
            .await;
        drop(out);
        (drain(rx).await, msgs)
    }

    async fn turn(stub: &Stub, root: &Path, prompt: &str) -> (Vec<String>, Vec<Msg>) {
        turn_with(stub, root, prompt, CancellationToken::new()).await
    }

    #[tokio::test]
    async fn a_tool_round_then_an_answer() {
        let t = TmpDir::new("agent-two-rounds");
        t.write("a.txt", "hello");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[
                &serde_json::json!({"choices":[{"delta":{"content":"listing"},"finish_reason":null}]}).to_string(),
                &tool_round("call_1", "ls", "{}"),
                r#"{"usage":{"prompt_tokens":11,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":4}}}"#,
                "[DONE]",
            ]),
        );
        stub.enqueue(200, sse(&[&text_round("all done", "stop"), "[DONE]"]));

        let (evs, msgs) = turn(&stub, t.path(), "do it").await;
        assert_eq!(
            evs,
            [
                "round:1",
                "text:listing",
                "usage:11:3:4",
                "start:ls:{}",
                "end:ls:ok:a.txt (5 B)",
                "round:2",
                "text:all done",
                "done",
            ]
        );

        // history: system, user, assistant+call, tool result, final answer
        assert_eq!(msgs.len(), 5, "{msgs:#?}");
        assert_eq!(msgs[1].role, Role::User);
        assert_eq!(msgs[1].content.as_deref(), Some("do it"));
        let calls = msgs[2].tool_calls.as_ref().expect("tool_calls dropped");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].id.as_str(), calls[0].function.name.as_str()),
            ("call_1", "ls")
        );
        assert_eq!(msgs[2].content.as_deref(), Some("listing"));
        assert_eq!(msgs[3].role, Role::Tool);
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(msgs[3].content.as_deref(), Some("a.txt (5 B)"));
        assert_eq!(msgs[4].content.as_deref(), Some("all done"));
        assert!(msgs[4].tool_calls.is_none());

        // the second request replays the tool result back to the model
        let reqs = stub.requests();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[1].contains("\"role\":\"tool\""), "{}", reqs[1]);
        assert!(reqs[1].contains("a.txt (5 B)"), "{}", reqs[1]);
    }

    #[tokio::test]
    async fn usage_is_priced_at_the_source() {
        let t = TmpDir::new("agent-price");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[
                &text_round("hi", "stop"),
                r#"{"usage":{"prompt_tokens":1000000,"completion_tokens":1000000}}"#,
                "[DONE]",
            ]),
        );
        let (tx, mut rx) = mpsc::channel(16);
        let mut out = Outlet::new(tx);
        let mut msgs = Vec::new();
        let r = engine(&stub, t.path())
            .turn(&mut msgs, "hi".into(), &CancellationToken::new(), &mut out)
            .await;
        // 1M in at 0.1 + 1M out at 0.2 per 1M
        assert!((r.usage.cost - 0.3).abs() < 1e-12, "{r:?}");
        let mut seen = 0.0;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Usage(u) = ev {
                seen += u.cost;
            }
        }
        assert_eq!(seen, r.usage.cost, "events and receipt disagree");
    }

    #[tokio::test]
    async fn tool_errors_are_fed_back_and_the_turn_recovers() {
        let t = TmpDir::new("agent-tool-err");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&tool_round("call_1", "nope", "{}"), "[DONE]"]));
        stub.enqueue(200, sse(&[&text_round("sorry", "stop"), "[DONE]"]));

        let (evs, msgs) = turn(&stub, t.path(), "go").await;
        assert!(
            evs.contains(&"end:nope:err:error: unknown tool: nope".to_string()),
            "{evs:?}"
        );
        assert_eq!(evs.last().unwrap(), "done");
        assert_eq!(
            msgs[3].content.as_deref(),
            Some("error: unknown tool: nope")
        );
        assert!(stub.requests()[1].contains("unknown tool: nope"));
    }

    #[tokio::test]
    async fn a_parallel_batch_answers_in_call_order() {
        let t = TmpDir::new("agent-parallel");
        t.write("a.txt", "aaa");
        t.write("b.txt", "bbb");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[
                &batch(&[
                    ("call_a", "read_file", r#"{"path":"a.txt"}"#),
                    ("call_b", "read_file", r#"{"path":"b.txt"}"#),
                ]),
                "[DONE]",
            ]),
        );
        stub.enqueue(200, sse(&[&text_round("both read", "stop"), "[DONE]"]));

        let (evs, msgs) = turn(&stub, t.path(), "read both").await;
        assert!(
            evs.contains(&r#"start:read_file:{"path":"a.txt"}"#.to_string()),
            "{evs:?}"
        );
        let ends: Vec<&String> = evs.iter().filter(|e| e.starts_with("end:")).collect();
        assert_eq!(
            ends,
            ["end:read_file:ok:   1│aaa", "end:read_file:ok:   1│bbb"]
        );

        // every call gets exactly one result, in call order
        let calls = msgs[2].tool_calls.as_ref().unwrap();
        assert_eq!(
            calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["call_a", "call_b"]
        );
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(msgs[4].tool_call_id.as_deref(), Some("call_b"));
        assert!(msgs[3].content.as_deref().unwrap().contains("aaa"));
        assert!(msgs[4].content.as_deref().unwrap().contains("bbb"));
    }

    #[tokio::test]
    async fn a_write_is_a_barrier_between_reads() {
        let t = TmpDir::new("agent-barrier");
        t.write("a.txt", "old");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[
                &batch(&[
                    ("r1", "read_file", r#"{"path":"a.txt"}"#),
                    ("r2", "ls", "{}"),
                    ("w", "write_file", r#"{"path":"a.txt","content":"new"}"#),
                    ("r3", "read_file", r#"{"path":"a.txt"}"#),
                ]),
                "[DONE]",
            ]),
        );
        stub.enqueue(200, sse(&[&text_round("ok", "stop"), "[DONE]"]));
        let (_, msgs) = turn(&stub, t.path(), "rewrite").await;
        let out = |id: &str| {
            msgs.iter()
                .find(|m| m.tool_call_id.as_deref() == Some(id))
                .and_then(|m| m.content.clone())
                .unwrap()
        };
        // reads before the write see the old file, reads after it the new one
        assert!(out("r1").contains("old"), "{}", out("r1"));
        assert!(out("r3").contains("new"), "{}", out("r3"));
    }

    #[tokio::test]
    async fn a_mid_stream_api_error_fails_the_turn_and_records_nothing() {
        let t = TmpDir::new("agent-api-err");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[r#"{"error":{"message":"rate limited"}}"#, "[DONE]"]),
        );
        let (evs, msgs) = turn(&stub, t.path(), "hi").await;
        assert_eq!(evs, ["round:1", "failed:rate limited"]);
        // nothing was recorded: no empty assistant message, no second request
        assert_eq!(msgs.len(), 2);
        assert_eq!(stub.requests().len(), 1);
    }

    #[tokio::test]
    async fn transport_errors_end_the_turn_with_their_cause() {
        let t = TmpDir::new("agent-http-err");
        let stub = Stub::new();
        stub.enqueue(401, "bad key");
        let (evs, msgs) = turn(&stub, t.path(), "hi").await;
        assert_eq!(evs.len(), 2, "{evs:?}");
        assert!(evs[1].starts_with("failed:HTTP 401"), "{evs:?}");
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn an_already_cancelled_turn_makes_no_request() {
        let t = TmpDir::new("agent-cancel");
        let stub = Stub::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (evs, msgs) = turn_with(&stub, t.path(), "hi", cancel).await;
        assert_eq!(evs, ["round:1", "interrupted"]);
        assert_eq!(msgs.len(), 2);
        assert!(stub.requests().is_empty());
    }

    #[tokio::test]
    async fn a_length_finish_fails_once_and_keeps_the_text() {
        let t = TmpDir::new("agent-length");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&text_round("truncated", "length"), "[DONE]"]));
        let (evs, msgs) = turn(&stub, t.path(), "write an essay").await;
        assert_eq!(
            evs,
            ["round:1", "text:truncated", "failed:hit max output tokens"]
        );
        // the partial answer is kept so the next turn has context
        assert_eq!(msgs[2].content.as_deref(), Some("truncated"));
    }

    #[tokio::test]
    async fn taps_see_every_event_before_the_channel() {
        let t = TmpDir::new("agent-tap");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&text_round("x", "stop"), "[DONE]"]));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let (tx, rx) = mpsc::channel(16);
        let mut out = Outlet::new(tx).tap(move |e: &Event| log.lock().unwrap().push(fmt(e)));
        engine(&stub, t.path())
            .turn(
                &mut Vec::new(),
                "go".into(),
                &CancellationToken::new(),
                &mut out,
            )
            .await;
        drop(out);
        let sent = drain(rx).await;
        assert_eq!(*seen.lock().unwrap(), sent);
        assert_eq!(sent.last().unwrap(), "done");
    }
}
