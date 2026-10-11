use crate::config::Config;
use crate::types::{FnCall, Msg, ToolCall};
use crate::{api, ctx, repomap, tools};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum AgentEvent {
    Round(usize),
    Token(String),
    ToolStart {
        name: String,
        args: String,
    },
    ToolEnd {
        name: String,
        ms: u64,
        preview: String,
        is_err: bool,
    },
    Compacted(String),
    Usage {
        input: u64,
        output: u64,
        cached: u64,
    },
    Done {
        interrupted: bool,
    },
    Err(String),
}

const SYSTEM: &str = "\
You are ragent, an autonomous coding agent in a terminal. Solve the task with tools, then stop.

Rules:
- Prefer edit_file (search/replace) over write_file for existing files.
- Batch independent read/grep/glob calls in one reply.
- Never restate file contents to the user; cite paths and line numbers.
- Keep final answers under 8 lines unless asked for more.
- After changing code, run the relevant build/test command once.
- Don't guess paths: ls/glob/grep first. Don't stop until the task is done.";

pub fn system_prompt(root: &std::path::Path, use_map: bool) -> String {
    let mut s = SYSTEM.to_string();
    s.push_str(&format!("\nProject root: {}", root.display()));
    if use_map {
        let map = repomap::build(root, 5000);
        if !map.is_empty() {
            s.push_str("\n\n## Project map\n");
            s.push_str(&map);
        }
    }
    for f in ["AGENTS.md", "CLAUDE.md", ".ragent/rules.md"] {
        let p = root.join(f);
        if let Ok(c) = std::fs::read_to_string(&p) {
            let c: String = c.chars().take(4000).collect();
            s.push_str(&format!("\n\n## {f}\n{c}"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{sse, Stub, TmpDir};

    fn cfg_for(stub: &Stub) -> Config {
        Config {
            api_key: "test-key".into(),
            base_url: stub.base_url(),
            model: "test-model".into(),
            context_budget: 60_000,
            max_output_tokens: 64,
            bash_timeout_ms: 5_000,
            price_in: 0.1,
            price_out: 0.2,
            price_cached: 0.01,
            ds_peak: false,
            use_map: false,
            update_check: false,
        }
    }

    /// One-line rendering of an event, so sequences can be compared as Vecs.
    fn fmt(e: &AgentEvent) -> String {
        match e {
            AgentEvent::Round(n) => format!("round:{n}"),
            AgentEvent::Token(t) => format!("token:{t}"),
            AgentEvent::ToolStart { name, args } => format!("start:{name}:{args}"),
            AgentEvent::ToolEnd {
                name,
                preview,
                is_err,
                ..
            } => format!(
                "end:{name}:{}:{preview}",
                if *is_err { "err" } else { "ok" }
            ),
            AgentEvent::Compacted(n) => format!("compacted:{n}"),
            AgentEvent::Usage {
                input,
                output,
                cached,
            } => format!("usage:{input}:{output}:{cached}"),
            AgentEvent::Done { interrupted } => format!("done:{interrupted}"),
            AgentEvent::Err(e) => format!("err:{e}"),
        }
    }

    async fn drain(mut rx: mpsc::Receiver<AgentEvent>) -> Vec<String> {
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
    fn text_round(text: &str, finish: &str) -> String {
        serde_json::json!({
            "choices": [{"delta": {"content": text}, "finish_reason": finish}]
        })
        .to_string()
    }

    /// A mid-stream text delta (no finish reason yet).
    fn text_chunk(text: &str) -> String {
        serde_json::json!({
            "choices": [{"delta": {"content": text}, "finish_reason": null}]
        })
        .to_string()
    }

    /// Run one turn against `stub` and return the events plus final history.
    async fn turn(stub: &Stub, root: &std::path::Path, prompt: &str) -> (Vec<String>, Vec<Msg>) {
        let (tx, rx) = mpsc::channel(128);
        let mut msgs = vec![Msg::system("sys")];
        run(
            prompt.to_string(),
            &mut msgs,
            Arc::new(cfg_for(stub)),
            Arc::new(root.to_path_buf()),
            reqwest::Client::builder().build().unwrap(),
            tx,
            CancellationToken::new(),
        )
        .await;
        (drain(rx).await, msgs)
    }

    #[test]
    fn prompt_names_the_root_and_the_rules() {
        let t = TmpDir::new("prompt");
        let p = system_prompt(t.path(), false);
        assert!(p.starts_with("You are ragent"));
        assert!(p.contains(&format!("Project root: {}", t.path().display())));
        assert!(p.contains("Prefer edit_file"));
        assert!(!p.contains("## Project map"));
    }

    #[test]
    fn project_map_is_optional_and_contains_paths() {
        let t = TmpDir::new("prompt-map");
        t.write("src/lib.rs", "pub fn exported() {}\n");
        let with = system_prompt(t.path(), true);
        assert!(with.contains("## Project map"), "{with}");
        assert!(with.contains("src/lib.rs"));
        assert!(with.contains("    pub fn exported() {}"), "{with}");
        // dropping the map loses the repo skeleton but keeps the rules
        let without = system_prompt(t.path(), false);
        assert!(!without.contains("## Project map"));
        assert!(without.contains("Prefer edit_file"));
        assert!(!without.contains("src/lib.rs"));
    }

    #[test]
    fn instruction_files_are_appended_in_order() {
        let t = TmpDir::new("prompt-files");
        t.write("AGENTS.md", "agents rules");
        t.write("CLAUDE.md", "claude rules");
        t.write(".ragent/rules.md", "local rules");
        let p = system_prompt(t.path(), false);
        let a = p
            .find("## AGENTS.md\nagents rules")
            .expect("AGENTS.md missing");
        let c = p
            .find("## CLAUDE.md\nclaude rules")
            .expect("CLAUDE.md missing");
        let r = p
            .find("## .ragent/rules.md\nlocal rules")
            .expect(".ragent/rules.md missing");
        assert!(a < c && c < r, "wrong order");
    }

    #[test]
    fn instruction_files_are_capped_and_missing_ones_skipped() {
        let t = TmpDir::new("prompt-cap");
        let p = system_prompt(t.path(), true);
        assert!(!p.contains("## AGENTS.md"));
        assert!(!p.contains("## CLAUDE.md"));
        assert!(!p.contains("## .ragent/rules.md"));

        // a directory named AGENTS.md must not blow up the prompt builder
        std::fs::create_dir_all(t.join("AGENTS.md")).unwrap();
        assert!(!system_prompt(t.path(), false).contains("## AGENTS.md"));
        std::fs::remove_dir_all(t.join("AGENTS.md")).unwrap();

        t.write("AGENTS.md", &"x".repeat(5000));
        let p = system_prompt(t.path(), false);
        let body = p.split("## AGENTS.md\n").nth(1).unwrap();
        assert_eq!(body.chars().count(), 4000);
        assert_eq!(body, "x".repeat(4000));
    }

    #[tokio::test]
    async fn run_executes_a_tool_then_answers_over_two_rounds() {
        let t = TmpDir::new("agent-two-rounds");
        t.write("a.txt", "hello");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[
                &text_chunk("listing"),
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
                "token:listing",
                "usage:11:3:4",
                "start:ls:{}",
                "end:ls:ok:a.txt (5 B)",
                "round:2",
                "token:all done",
                "done:false",
            ]
        );

        // history: system, user, assistant+call, tool result, final answer
        assert_eq!(msgs.len(), 5, "{msgs:#?}");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content.as_deref(), Some("do it"));
        let calls = msgs[2].tool_calls.as_ref().expect("tool_calls dropped");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].id.as_str(), calls[0].function.name.as_str()),
            ("call_1", "ls")
        );
        assert_eq!(msgs[2].content.as_deref(), Some("listing"));
        assert_eq!(msgs[3].role, "tool");
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
    async fn run_feeds_tool_errors_back_and_recovers() {
        let t = TmpDir::new("agent-tool-err");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&tool_round("call_1", "nope", "{}"), "[DONE]"]));
        stub.enqueue(200, sse(&[&text_round("sorry", "stop"), "[DONE]"]));

        let (evs, msgs) = turn(&stub, t.path(), "go").await;
        assert!(
            evs.contains(&"end:nope:err:error: unknown tool: nope".to_string()),
            "{evs:?}"
        );
        assert_eq!(evs.last().unwrap(), "done:false");
        assert_eq!(
            msgs[3].content.as_deref(),
            Some("error: unknown tool: nope")
        );
        assert!(stub.requests()[1].contains("unknown tool: nope"));
    }

    #[tokio::test]
    async fn run_runs_a_parallel_batch_in_call_order() {
        let t = TmpDir::new("agent-parallel");
        t.write("a.txt", "aaa");
        t.write("b.txt", "bbb");
        let stub = Stub::new();
        let batch = serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [
                    {"index": 0, "id": "call_a", "function": {
                        "name": "read_file", "arguments": r#"{"path":"a.txt"}"#}},
                    {"index": 1, "id": "call_b", "function": {
                        "name": "read_file", "arguments": r#"{"path":"b.txt"}"#}},
                ]},
                "finish_reason": "tool_calls",
            }]
        })
        .to_string();
        stub.enqueue(200, sse(&[&batch, "[DONE]"]));
        stub.enqueue(200, sse(&[&text_round("both read", "stop"), "[DONE]"]));

        let (evs, msgs) = turn(&stub, t.path(), "read both").await;
        assert!(
            evs.contains(&r#"start:read_file:{"path":"a.txt"}"#.to_string()),
            "{evs:?}"
        );
        assert_eq!(evs.iter().filter(|e| e.starts_with("start:")).count(), 2);
        assert_eq!(evs.iter().filter(|e| e.starts_with("end:")).count(), 2);

        // every call gets exactly one result, in call order
        let calls = msgs[2].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 2);
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
    async fn run_surfaces_mid_stream_api_errors_and_keeps_history_valid() {
        let t = TmpDir::new("agent-api-err");
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[r#"{"error":{"message":"rate limited"}}"#, "[DONE]"]),
        );
        let (evs, msgs) = turn(&stub, t.path(), "hi").await;
        assert_eq!(evs, ["round:1", "err:rate limited"]);
        // nothing was recorded: no empty assistant message, no second request
        assert_eq!(msgs.len(), 2);
        assert_eq!(stub.requests().len(), 1);
    }

    #[tokio::test]
    async fn run_stops_without_a_request_when_already_cancelled() {
        let t = TmpDir::new("agent-cancel");
        let stub = Stub::new();
        let (tx, rx) = mpsc::channel(64);
        let mut msgs = vec![Msg::system("sys")];
        let cancel = CancellationToken::new();
        cancel.cancel();
        run(
            "hi".into(),
            &mut msgs,
            Arc::new(cfg_for(&stub)),
            Arc::new(t.path().to_path_buf()),
            reqwest::Client::builder().build().unwrap(),
            tx,
            cancel,
        )
        .await;
        assert_eq!(drain(rx).await, ["round:1", "done:true"]);
        assert_eq!(msgs.len(), 2);
        assert!(stub.requests().is_empty());
    }

    #[tokio::test]
    async fn run_reports_a_length_finish_once() {
        let t = TmpDir::new("agent-length");
        let stub = Stub::new();
        stub.enqueue(200, sse(&[&text_round("truncated", "length"), "[DONE]"]));
        let (evs, msgs) = turn(&stub, t.path(), "write an essay").await;
        assert_eq!(
            evs,
            [
                "round:1",
                "token:truncated",
                "err:hit max output tokens",
                "done:false"
            ]
        );
        // the partial answer is kept so the next turn has context
        assert_eq!(msgs[2].content.as_deref(), Some("truncated"));
    }
}

#[derive(Default)]
struct TcBuild {
    id: String,
    name: String,
    args: String,
}

struct ExecResult {
    idx: usize,
    name: String,
    res: Result<String, String>,
    ms: u64,
}

/// Run one agent turn: prompt → streaming completions + tool calls until the
/// model stops or limits hit. History accumulates in `msgs` across calls.
pub async fn run(
    prompt: String,
    msgs: &mut Vec<Msg>,
    cfg: Arc<Config>,
    root: Arc<PathBuf>,
    http: reqwest::Client,
    tx: mpsc::Sender<AgentEvent>,
    cancel: CancellationToken,
) {
    msgs.push(Msg::user(prompt));
    let specs = tools::specs();
    let mut round = 0usize;

    loop {
        round += 1;
        let _ = tx.send(AgentEvent::Round(round)).await;
        ctx::repair(msgs);
        if let Some(note) = ctx::compact(msgs, cfg.context_budget) {
            let _ = tx.send(AgentEvent::Compacted(note)).await;
        }

        // --- stream one completion ---
        let (stx, mut srx) = mpsc::channel::<api::Ev>(256);
        let mut text = String::new();
        let mut builders: BTreeMap<usize, TcBuild> = BTreeMap::new();
        let mut finish = String::new();

        let stream_fut = api::stream_chat(&http, &cfg, msgs, &specs, stx, cancel.clone());
        let consume_fut = async {
            while let Some(ev) = srx.recv().await {
                match ev {
                    api::Ev::Text(t) => {
                        text.push_str(&t);
                        let _ = tx.send(AgentEvent::Token(t)).await;
                    }
                    api::Ev::TcDelta {
                        idx,
                        id,
                        name,
                        args,
                    } => {
                        let b = builders.entry(idx).or_default();
                        if let Some(i) = id {
                            b.id.push_str(&i);
                        }
                        if let Some(n) = name {
                            b.name.push_str(&n);
                        }
                        b.args.push_str(&args);
                    }
                    api::Ev::Usage {
                        input,
                        output,
                        cached,
                    } => {
                        let _ = tx
                            .send(AgentEvent::Usage {
                                input,
                                output,
                                cached,
                            })
                            .await;
                    }
                    api::Ev::Finish(f) => {
                        // "[DONE]" is a transport sentinel, not a reason: it must
                        // not hide a real finish_reason (or an "api error: …") that
                        // arrived earlier in the same stream.
                        if finish.is_empty() || f != "done" {
                            finish = f;
                        }
                    }
                }
            }
        };
        let (res, ()) = tokio::join!(stream_fut, consume_fut);

        if let Err(e) = res {
            let _ = tx.send(AgentEvent::Err(format!("{e:#}"))).await;
            return;
        }
        // providers signal mid-stream failures via an "error" object that the
        // parser folds into Finish — surface it instead of recording a
        // successful empty turn
        if let Some(e) = finish.strip_prefix("api error:") {
            let _ = tx.send(AgentEvent::Err(e.trim().to_string())).await;
            return;
        }

        let calls: Vec<ToolCall> = builders
            .into_values()
            .filter(|b| !b.name.is_empty())
            .map(|b| ToolCall {
                id: if b.id.is_empty() {
                    format!("call_{round}")
                } else {
                    b.id
                },
                kind: "function".into(),
                function: FnCall {
                    name: b.name,
                    arguments: b.args,
                },
            })
            .collect();

        let content = if text.is_empty() {
            None
        } else {
            Some(text.clone())
        };
        let interrupted = cancel.is_cancelled();
        // Never persist an assistant message with neither content nor
        // tool_calls (the next request would 400). On cancel, partial tool
        // calls are untrusted — truncated args, never executed — so only the
        // text survives.
        let record_calls = if interrupted {
            Vec::new()
        } else {
            calls.clone()
        };
        if content.is_some() || !record_calls.is_empty() {
            msgs.push(Msg::assistant(content, record_calls));
        }

        if calls.is_empty() || interrupted {
            if finish == "length" && !interrupted {
                let _ = tx
                    .send(AgentEvent::Err("hit max output tokens".into()))
                    .await;
            }
            let _ = tx.send(AgentEvent::Done { interrupted }).await;
            return;
        }

        // --- dispatch tool calls ---
        for c in &calls {
            let _ = tx
                .send(AgentEvent::ToolStart {
                    name: c.function.name.clone(),
                    args: crate::util::one_line(&c.function.arguments, 140),
                })
                .await;
        }

        let mut results: Vec<ExecResult> = Vec::with_capacity(calls.len());
        if calls.len() > 1 && calls.iter().all(|c| tools::is_parallel(&c.function.name)) {
            // parallel batch: independent read-only calls run concurrently
            let mut set = tokio::task::JoinSet::new();
            for (i, c) in calls.iter().enumerate() {
                let (name, argstr, root) = (
                    c.function.name.clone(),
                    c.function.arguments.clone(),
                    root.clone(),
                );
                let timeout = cfg.bash_timeout_ms;
                set.spawn(async move {
                    let t = Instant::now();
                    let args: Value = serde_json::from_str(&argstr)
                        .unwrap_or_else(|_| Value::Object(Default::default()));
                    let res = tools::exec(&name, &args, root, timeout).await;
                    ExecResult {
                        idx: i,
                        name,
                        res,
                        ms: t.elapsed().as_millis() as u64,
                    }
                });
            }
            while let Some(r) = set.join_next().await {
                match r {
                    Ok(x) => results.push(x),
                    Err(e) => {
                        let _ = tx.send(AgentEvent::Err(format!("tool task: {e}"))).await;
                    }
                }
            }
            // a panicked task produced no result — synthesize errors so every
            // tool_call gets a response and the history stays API-valid
            for (i, c) in calls.iter().enumerate() {
                if !results.iter().any(|r| r.idx == i) {
                    results.push(ExecResult {
                        idx: i,
                        name: c.function.name.clone(),
                        res: Err("tool task failed".into()),
                        ms: 0,
                    });
                }
            }
            results.sort_by_key(|r| r.idx);
        } else {
            for (i, c) in calls.iter().enumerate() {
                if cancel.is_cancelled() {
                    results.push(ExecResult {
                        idx: i,
                        name: c.function.name.clone(),
                        res: Err("cancelled".into()),
                        ms: 0,
                    });
                    continue;
                }
                let t = Instant::now();
                let args: Value = serde_json::from_str(&c.function.arguments)
                    .unwrap_or_else(|_| Value::Object(Default::default()));
                let res =
                    tools::exec(&c.function.name, &args, root.clone(), cfg.bash_timeout_ms).await;
                results.push(ExecResult {
                    idx: i,
                    name: c.function.name.clone(),
                    res,
                    ms: t.elapsed().as_millis() as u64,
                });
            }
        }

        for r in &results {
            let (content, is_err) = match &r.res {
                Ok(s) => (s.clone(), false),
                Err(e) => (format!("error: {e}"), true),
            };
            let preview = content.lines().next().unwrap_or("(empty)");
            let _ = tx
                .send(AgentEvent::ToolEnd {
                    name: r.name.clone(),
                    ms: r.ms,
                    preview: crate::util::one_line(preview, 90),
                    is_err,
                })
                .await;
        }
        for (c, r) in calls.iter().zip(results.iter()) {
            let content = match &r.res {
                Ok(s) => s.clone(),
                Err(e) => format!("error: {e}"),
            };
            msgs.push(Msg::tool(c.id.clone(), content));
        }
    }
}
