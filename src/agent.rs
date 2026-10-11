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

    for round in 0..cfg.max_iters {
        let _ = tx.send(AgentEvent::Round(round + 1)).await;
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
                    api::Ev::Finish(f) => finish = f,
                }
            }
        };
        let (res, ()) = tokio::join!(stream_fut, consume_fut);

        if let Err(e) = res {
            let _ = tx.send(AgentEvent::Err(format!("{e:#}"))).await;
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
        msgs.push(Msg::assistant(content, calls.clone()));

        if calls.is_empty() || cancel.is_cancelled() {
            if finish == "length" {
                let _ = tx
                    .send(AgentEvent::Err("hit max output tokens".into()))
                    .await;
            }
            let _ = tx
                .send(AgentEvent::Done {
                    interrupted: cancel.is_cancelled(),
                })
                .await;
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
    let _ = tx
        .send(AgentEvent::Err(format!(
            "hit iteration cap ({})",
            cfg.max_iters
        )))
        .await;
}
