//! Terminal UI: chat scrollback, streaming markdown, input box, status bar.
//!
//! - [`App`] holds all visual state (scrollback + live stream + input).
//! - [`driver`] owns the conversation and forwards [`AgentEvent`]s to the UI.
//! - [`draw`] renders one frame from `App`; [`input`] maps keys to edits and
//!   [`UserCmd`]s; [`md`] turns assistant markdown into styled lines.

mod draw;
mod input;
mod md;

use draw::{banner, render};
use input::on_key;
use md::{mark_line, md_block};

use crate::agent::{self, AgentEvent};
use crate::config::Config;
use crate::types::Msg;
use crate::util::{fmt_tokens, sanitize, sline, wrap_sline, SLine};
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

const C_DIM: Style = Style::new().fg(Color::DarkGray);
const C_USER: Style = Style::new().fg(Color::Cyan);
const C_TOOL: Style = Style::new().fg(Color::Yellow);
const C_ERR: Style = Style::new().fg(Color::Red);
const C_OK: Style = Style::new().fg(Color::Green);
const C_CODE: Style = Style::new().fg(Color::Gray);
const C_HEAD: Style = Style::new()
    .fg(Color::Cyan)
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::UNDERLINED);
const C_H2: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const C_H3: Style = Style::new().add_modifier(Modifier::BOLD);
const C_LINK: Style = Style::new()
    .fg(Color::Cyan)
    .add_modifier(Modifier::UNDERLINED);
const C_ASSIST: Style = Style::new().fg(Color::Green);
const C_GUTTER: Style = Style::new().fg(Color::Indexed(240));
const C_ICODE: Style = Style::new().fg(Color::Yellow);
const C_BAR: Style = Style::new().bg(Color::Indexed(237)).fg(Color::Gray);
const C_BAR_HI: Style = Style::new().bg(Color::Indexed(237)).fg(Color::White);
const SPIN: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
enum UserCmd {
    Prompt(String),
    Cancel,
}

struct App {
    log: Vec<SLine>,
    wrapped: Vec<SLine>,
    wrap_w: usize,
    wrap_len: usize,
    stream: String,
    stream_wrapped: Vec<SLine>,
    stream_dirty: bool,
    in_fence: bool,

    input: String,
    cur: usize,
    hist: Vec<String>,
    hi: usize,
    draft: String,

    scroll: usize,
    follow: bool,
    input_scroll: u16,

    running: bool,
    iter: usize,
    in_tok: u64,
    out_tok: u64,
    cached: u64,
    cost: f64,
    task_cost: f64,
    task_in: u64,
    task_out: u64,
    task_cached: u64,
    ok_cnt: u64,
    fail_cnt: u64,
    started: Option<Instant>,
    model: String,

    agent_dead: bool,
    quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            log: vec![],
            wrapped: vec![],
            wrap_w: 0,
            wrap_len: 0,
            stream: String::new(),
            stream_wrapped: vec![],
            stream_dirty: false,
            in_fence: false,
            input: String::new(),
            cur: 0,
            hist: vec![],
            hi: 0,
            draft: String::new(),
            scroll: 0,
            follow: true,
            input_scroll: 0,
            running: false,
            iter: 0,
            in_tok: 0,
            out_tok: 0,
            cached: 0,
            cost: 0.0,
            task_cost: 0.0,
            task_in: 0,
            task_out: 0,
            task_cached: 0,
            ok_cnt: 0,
            fail_cnt: 0,
            started: None,
            model: String::new(),
            agent_dead: false,
            quit: false,
        }
    }

    fn push(&mut self, l: SLine) {
        self.log.push(l);
    }

    fn note(&mut self, s: impl Into<String>) {
        self.log.push(sline(format!("  {}", s.into()), C_DIM));
    }

    fn push_user(&mut self, text: &str, queued: bool) {
        for (i, l) in text.lines().enumerate() {
            let pfx = if i == 0 { "❯ " } else { "  " };
            let mut l2: SLine = vec![
                (
                    pfx.into(),
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                (l.to_string(), C_USER),
            ];
            if i == 0 && queued {
                l2.push((" (queued)".into(), C_DIM));
            }
            self.log.push(l2);
        }
        self.log.push(Vec::new());
    }

    /// Move the streamed assistant text into the scrollback with light
    /// markdown styling (headings, fenced blocks with a gutter bar, inline
    /// code/bold/italic/links, bullets and ordered lists, quotes, rules and
    /// boxed pipe tables).
    fn flush_stream(&mut self) {
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

    fn ensure_wrap(&mut self, w: usize) {
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

    fn rebuild_stream(&mut self) {
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

    fn scroll_by(&mut self, d: isize) {
        self.follow = false;
        self.scroll = self.scroll.saturating_add_signed(d);
    }
}
/// Long-running task that owns the conversation history.
/// Queues at most one pending prompt; Esc cancels the in-flight run.
async fn driver(
    mut msgs: Vec<Msg>,
    cfg: Arc<Config>,
    root: Arc<PathBuf>,
    http: reqwest::Client,
    ev_tx: mpsc::Sender<AgentEvent>,
    mut inbox: mpsc::Receiver<UserCmd>,
) {
    let mut next: Option<String> = None;
    let mut open = true;
    while open || next.is_some() {
        let prompt = match next.take() {
            Some(p) => p,
            None => loop {
                match inbox.recv().await {
                    Some(UserCmd::Prompt(p)) => break p,
                    Some(UserCmd::Cancel) => {}
                    None => return,
                }
            },
        };
        let cancel = CancellationToken::new();
        let fut = agent::run(
            prompt,
            &mut msgs,
            cfg.clone(),
            root.clone(),
            http.clone(),
            ev_tx.clone(),
            cancel.clone(),
        );
        tokio::pin!(fut);
        let mut fut_done = false;
        while !fut_done {
            tokio::select! {
                _ = &mut fut => fut_done = true,
                c = inbox.recv(), if open => match c {
                    Some(UserCmd::Cancel) => cancel.cancel(),
                    Some(UserCmd::Prompt(p)) => next = Some(p),
                    None => {
                        // UI is gone: cancel the run and drop any queued
                        // prompt — running it would burn tokens nobody sees
                        open = false;
                        next = None;
                        cancel.cancel();
                    }
                },
            }
        }
    }
}

fn apply(app: &mut App, ev: AgentEvent, cfg: &Config) {
    match ev {
        AgentEvent::Token(t) => {
            app.stream.push_str(&t);
            app.stream_dirty = true;
        }
        AgentEvent::Round(n) => {
            app.iter = n;
            if n == 1 {
                app.task_cost = 0.0;
                app.task_in = 0;
                app.task_out = 0;
                app.task_cached = 0;
            }
        }
        AgentEvent::ToolStart { name, args } => {
            app.flush_stream();
            let l: SLine = vec![
                ("  ◆ ".into(), C_TOOL),
                (
                    name,
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                (format!(" {args}"), C_DIM),
            ];
            app.push(l);
        }
        AgentEvent::ToolEnd {
            ms,
            preview,
            is_err,
            ..
        } => {
            let (mark, st) = if is_err {
                ("✗", C_ERR)
            } else {
                ("✓", C_OK)
            };
            app.push(vec![
                ("    └ ".into(), C_GUTTER),
                (format!("{mark} {ms}ms"), st),
                (format!("  {preview}"), C_DIM),
            ]);
        }
        AgentEvent::Compacted(n) => {
            app.flush_stream();
            app.note(format!("⛁ {n}"));
        }
        AgentEvent::Usage {
            input,
            output,
            cached,
        } => {
            app.in_tok += input;
            app.out_tok += output;
            app.cached += cached;
            let c = cfg.cost(input, output, cached);
            app.cost += c;
            app.task_cost += c;
            app.task_in += input;
            app.task_out += output;
            app.task_cached += cached;
        }
        AgentEvent::Done { interrupted } => {
            app.flush_stream();
            // the fence state is per-message: an unclosed block must not
            // style the next response as code
            app.in_fence = false;
            app.running = false;
            let secs = app
                .started
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            app.started = None;
            let ok = !interrupted;
            crate::stats::record(
                &app.model,
                app.task_in,
                app.task_out,
                app.task_cached,
                app.task_cost,
                ok,
            );
            if ok {
                app.ok_cnt += 1;
                app.push(vec![
                    ("  ─ ".into(), C_GUTTER),
                    ("done".into(), C_OK),
                    (
                        format!(
                            " · ${:.4} · ↑{} ↓{} · {:.1}s",
                            app.task_cost,
                            fmt_tokens(app.task_in),
                            fmt_tokens(app.task_out),
                            secs
                        ),
                        C_DIM,
                    ),
                ]);
            } else {
                app.fail_cnt += 1;
                app.note("— interrupted (Esc)");
            }
        }
        AgentEvent::Err(e) => {
            app.flush_stream();
            app.running = false;
            app.started = None;
            crate::stats::record(
                &app.model,
                app.task_in,
                app.task_out,
                app.task_cached,
                app.task_cost,
                false,
            );
            app.fail_cnt += 1;
            app.push(sline(format!("✗ {e}"), C_ERR));
        }
    }
}
pub async fn run(cfg: Config) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let mut term = Terminal::new(CrosstermBackend::new(stdout))?;
    let res = run_ui(&mut term, cfg).await;
    let _ = execute!(
        term.backend_mut(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    disable_raw_mode().ok();
    term.show_cursor().ok();
    res
}

async fn run_ui(term: &mut Terminal<CrosstermBackend<io::Stdout>>, cfg: Config) -> Result<()> {
    let root = std::env::current_dir()?;
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()?;
    let (cmd_tx, cmd_rx) = mpsc::channel::<UserCmd>(32);
    let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(512);

    let msgs = vec![Msg::system(agent::system_prompt(&root, cfg.use_map))];
    tokio::spawn(driver(
        msgs,
        Arc::new(cfg.clone()),
        Arc::new(root.clone()),
        http,
        ev_tx,
        cmd_rx,
    ));

    let mut app = App::new();
    app.model = cfg.model.clone();
    banner(&mut app, &cfg, &root);

    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(300));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            e = keys.next() => {
                match e {
                    Some(Ok(Event::Key(k))) => {
                        if on_key(&mut app, k, &cmd_tx) { app.quit = true; }
                    }
                    Some(Ok(Event::Paste(s))) => {
                        let s = sanitize(&s);
                        app.input.insert_str(app.cur, &s);
                        app.cur += s.len();
                    }
                    Some(Ok(Event::Mouse(m))) => match m.kind {
                        crossterm::event::MouseEventKind::ScrollUp => app.scroll_by(-3),
                        crossterm::event::MouseEventKind::ScrollDown => {
                            app.scroll_by(3);
                            let total = app.wrapped.len() + app.stream_wrapped.len();
                            let h = term.size().map(|s| s.height as usize).unwrap_or(24);
                            if app.scroll + h - 4 >= total { app.follow = true; }
                        }
                        _ => {}
                    },
                    Some(Ok(Event::Resize(_, _))) => {}
                    _ => {}
                }
            }
            a = ev_rx.recv() => {
                match a {
                    Some(ev) => {
                        let first = matches!(ev, AgentEvent::Round(1));
                        if first { app.running = true; app.started = Some(Instant::now()); }
                        apply(&mut app, ev, &cfg);
                        while let Ok(ev2) = ev_rx.try_recv() {
                            apply(&mut app, ev2, &cfg);
                        }
                    }
                    None => {
                        // the driver task died (panic) or closed the channel:
                        // don't leave a spinner running forever — mark dead once
                        if !app.agent_dead {
                            app.agent_dead = true;
                            app.running = false;
                            app.started = None;
                            app.note("agent stopped — Ctrl-C to quit");
                        }
                    }
                }
            }
            _ = tick.tick() => {}
        }
        term.draw(|f| render(f, &mut app))?;
        if app.quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg() -> Config {
        Config {
            api_key: String::new(),
            base_url: String::new(),
            model: "m".into(),
            context_budget: 64_000,
            max_output_tokens: 4096,
            bash_timeout_ms: 60_000,
            price_in: 0.15,
            price_out: 0.6,
            price_cached: 0.003,
            ds_peak: false,
            use_map: true,
        }
    }

    fn text_of(l: &SLine) -> String {
        l.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn empty_stream_adds_no_phantom_line() {
        // after a flush, rebuild_stream("") must leave stream_wrapped empty —
        // a phantom line inflates the scroll total and eats the top row
        let mut app = App::new();
        app.stream = "## Plan\n\ntext".into();
        app.flush_stream();
        app.rebuild_stream();
        assert!(app.stream_wrapped.is_empty());
        assert!(!app.stream_dirty);
    }

    #[test]
    fn unclosed_fence_does_not_leak_into_next_message() {
        let mut app = App::new();
        app.stream = "```rust\nlet x = 1;".into();
        app.flush_stream();
        assert!(app.in_fence);
        let cfg = cfg();
        apply(&mut app, AgentEvent::Done { interrupted: false }, &cfg);
        assert!(!app.in_fence);
        app.stream = "plain answer".into();
        app.flush_stream();
        let last_txt: String = app
            .log
            .iter()
            .rev()
            .map(text_of)
            .find(|t| !t.trim().is_empty() && !t.contains("done"))
            .unwrap();
        assert_eq!(last_txt, "● plain answer", "{last_txt}");
    }
}
