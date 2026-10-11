mod agent;
mod api;
mod config;
mod ctx;
mod repomap;
mod stats;
mod tools;
mod tui;
mod types;
mod util;

use agent::AgentEvent;
use anyhow::{bail, Result};
use clap::Parser;
use config::Config;
use std::io::{IsTerminal, Read, Write};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// ragent — a lean coding agent: extreme cost & speed optimization.
#[derive(Parser)]
#[command(name = "ragent", version, about)]
struct Cli {
    /// Prompt. If given (or stdin is piped), runs headless and exits.
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,
    /// Model name.
    #[arg(short, long)]
    model: Option<String>,
    /// OpenAI-compatible base URL (e.g. https://api.deepseek.com/v1).
    #[arg(long)]
    base_url: Option<String>,
    /// API key.
    #[arg(long)]
    api_key: Option<String>,
    /// Context budget in tokens before compaction.
    #[arg(long)]
    budget: Option<usize>,
    /// Force headless mode.
    #[arg(short = 'p', long)]
    print: bool,
    /// Don't inject the repo map into the system prompt.
    #[arg(long)]
    no_map: bool,
    /// Write an example config file and exit.
    #[arg(long)]
    init: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.init {
        return Config::write_example();
    }
    let cfg = Config::load(
        cli.model.as_deref(),
        cli.base_url.as_deref(),
        cli.api_key.as_deref(),
        cli.budget,
        cli.no_map,
    )?;

    let mut prompt = cli.prompt.join(" ");
    let piped = if std::io::stdin().is_terminal() {
        None
    } else {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).ok();
        Some(s.trim_end().to_string()).filter(|s| !s.is_empty())
    };
    if let Some(p) = piped {
        prompt = if prompt.is_empty() {
            p
        } else {
            format!("{prompt}\n\n{p}")
        };
    }

    if cli.print || !prompt.is_empty() {
        if prompt.is_empty() {
            bail!("empty prompt");
        }
        headless(cfg, prompt).await
    } else {
        tui::run(cfg).await
    }
}

/// Non-interactive run: assistant text → stdout, tool/status lines → stderr.
async fn headless(cfg: Config, prompt: String) -> Result<()> {
    let root = std::env::current_dir()?;
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()?;
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(512);
    let mut msgs = vec![types::Msg::system(agent::system_prompt(&root, cfg.use_map))];
    let cancel = CancellationToken::new();
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            c.cancel();
        });
    }

    let cfg = Arc::new(cfg);
    let t0 = Instant::now();
    let fut = agent::run(
        prompt,
        &mut msgs,
        cfg.clone(),
        Arc::new(root),
        http,
        tx,
        cancel.clone(),
    );
    tokio::pin!(fut);

    let (mut intok, mut outtok, mut cached) = (0u64, 0u64, 0u64);
    let mut out = std::io::stdout();
    let mut code = 0;
    let mut interrupted = false;
    let mut handle = |ev: AgentEvent| match ev {
        AgentEvent::Token(t) => {
            out.write_all(t.as_bytes()).ok();
            out.flush().ok();
        }
        AgentEvent::ToolStart { name, args } => {
            eprintln!("\x1b[2m▸ {name} {args}\x1b[0m");
        }
        AgentEvent::ToolEnd {
            name,
            ms,
            preview,
            is_err,
        } => {
            let m = if is_err { "✗" } else { "✓" };
            eprintln!("\x1b[2m  └ {m} {ms}ms {name} · {preview}\x1b[0m");
        }
        AgentEvent::Compacted(n) => eprintln!("\x1b[2m⛁ {n}\x1b[0m"),
        AgentEvent::Usage {
            input,
            output,
            cached: c,
        } => {
            intok += input;
            outtok += output;
            cached += c;
        }
        AgentEvent::Done { interrupted: i } => {
            interrupted = i;
            if i {
                eprintln!("\x1b[2m— interrupted\x1b[0m");
            }
        }
        AgentEvent::Err(e) => {
            eprintln!("\x1b[31m✗ {e}\x1b[0m");
            code = 1;
        }
        AgentEvent::Round(_) => {}
    };
    loop {
        tokio::select! {
            _ = &mut fut => break,
            ev = rx.recv() => {
                if let Some(e) = ev { handle(e) }
            },
        }
    }
    while let Ok(e) = rx.try_recv() {
        handle(e);
    }
    out.write_all(b"\n").ok();
    let task_cost = cfg.cost(intok, outtok, cached);
    let ok = code == 0 && !interrupted;
    stats::record(&cfg.model, intok, outtok, cached, task_cost, ok);
    let life = stats::load();
    eprintln!(
        "\x1b[2m[task {}] ${:.4} · ↑{} ↓{} ⛁{} · {:.1}s · life ${:.4}/✓ ({}✓ {}✗, ${:.2} tot)\x1b[0m",
        if ok { "ok" } else { "FAIL" },
        task_cost,
        util::fmt_tokens(intok),
        util::fmt_tokens(outtok),
        util::fmt_tokens(cached),
        t0.elapsed().as_secs_f32(),
        life.avg_ok().unwrap_or(0.0),
        life.ok,
        life.tasks - life.ok,
        life.cost,
    );
    std::process::exit(code);
}
