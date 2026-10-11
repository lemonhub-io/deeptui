use crate::agent::{self, AgentEvent};
use crate::config::Config;
use crate::types::Msg;
use crate::util::{fmt_tokens, sanitize, sline, wrap_sline, SLine};
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use unicode_width::UnicodeWidthChar;

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
        self.stream_dirty = false;
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
                    None => { open = false; cancel.cancel(); }
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

/// Wrapped row count of the input at inner width `w`.
fn input_rows(input: &str, w: usize) -> usize {
    let w = w.max(1);
    input
        .split('\n')
        .map(|l| {
            let lw: usize = l
                .chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            (lw / w) + 1
        })
        .sum()
}

/// (row, col) in wrapped coordinates of a prefix of `input` (used for cursor).
fn cursor_pos(prefix: &str, w: usize) -> (usize, usize) {
    let w = w.max(1);
    let (mut row, mut col) = (0usize, 0usize);
    for c in prefix.chars() {
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += UnicodeWidthChar::width(c).unwrap_or(0);
            if col >= w {
                row += 1;
                col = 0;
            }
        }
    }
    (row, col)
}

/// Style for one run of inline markdown with the currently open spans.
fn md_span_style(code: bool, bold: bool, italic: bool, strike: bool) -> Style {
    let mut st = if code { C_ICODE } else { Style::default() };
    if bold {
        st = st.add_modifier(Modifier::BOLD);
    }
    if italic {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if strike {
        st = st.add_modifier(Modifier::CROSSED_OUT);
    }
    st
}

fn md_flush(out: &mut SLine, buf: &mut String, code: bool, bold: bool, italic: bool, strike: bool) {
    if !buf.is_empty() {
        out.push((
            std::mem::take(buf),
            md_span_style(code, bold, italic, strike),
        ));
    }
}

/// Parse `[label](url)` at the start of `s` → (label, url, bytes consumed).
fn md_link(s: &str) -> Option<(String, String, usize)> {
    let rest = s.strip_prefix('[')?;
    let close = rest.find(']')?;
    let label = &rest[..close];
    if label.is_empty() || label.contains(['[', ']']) {
        return None;
    }
    let url_part = rest[close + 1..].strip_prefix('(')?;
    let end = url_part.find(')')?;
    let url = url_part[..end].trim();
    if url.is_empty() || url.contains(char::is_whitespace) {
        return None;
    }
    Some((label.to_string(), url.to_string(), close + end + 4))
}

/// Style inline spans inside one markdown line: `code`, **bold**, *italic*,
/// ~~strike~~, [text](url) and backslash escapes.
fn md_inline(t: &str) -> SLine {
    let mut out: SLine = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic, mut code, mut strike) = (false, false, false, false);
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if !code => {
                if let Some(n) = chars.next() {
                    buf.push(n);
                }
            }
            '`' => {
                md_flush(&mut out, &mut buf, code, bold, italic, strike);
                code = !code;
            }
            '*' | '_' if !code => {
                if chars.peek() == Some(&c) {
                    chars.next();
                    md_flush(&mut out, &mut buf, code, bold, italic, strike);
                    bold = !bold;
                } else if c == '*' {
                    md_flush(&mut out, &mut buf, code, bold, italic, strike);
                    italic = !italic;
                } else {
                    buf.push(c); // a lone `_` (snake_case) stays literal
                }
            }
            '~' if !code && chars.peek() == Some(&'~') => {
                chars.next();
                md_flush(&mut out, &mut buf, code, bold, italic, strike);
                strike = !strike;
            }
            '[' if !code => {
                let rest: String = std::iter::once('[').chain(chars.clone()).collect();
                match md_link(&rest) {
                    Some((label, url, n)) => {
                        for _ in 1..n {
                            chars.next();
                        }
                        md_flush(&mut out, &mut buf, code, bold, italic, strike);
                        out.push((label.clone(), C_LINK));
                        if url != label {
                            out.push((format!(" ({url})"), C_DIM));
                        }
                    }
                    None => buf.push(c),
                }
            }
            _ => buf.push(c),
        }
    }
    md_flush(&mut out, &mut buf, code, bold, italic, strike);
    out
}

/// Place the ● turn marker on a line's leading indent.
fn mark_line(l: &mut SLine) {
    if l.first().is_some_and(|(s, _)| s == "  ") {
        l[0] = ("● ".into(), C_ASSIST);
    } else {
        l.insert(0, ("● ".into(), C_ASSIST));
    }
}

/// Startup card: name/model/root/budget + lifetime stats + key hints.
fn banner(app: &mut App, cfg: &Config, root: &std::path::Path) {
    let life = crate::stats::load();
    let mut rows: Vec<(String, String)> = vec![
        ("model".into(), cfg.model.clone()),
        ("root".into(), root.display().to_string()),
        (
            "context".into(),
            format!("{}k tok", cfg.context_budget / 1000),
        ),
    ];
    if life.tasks > 0 {
        rows.push((
            "history".into(),
            format!(
                "{} tasks ({}✓ {}✗) · ${:.4} spent · ${:.4}/✓",
                life.tasks,
                life.ok,
                life.tasks - life.ok,
                life.cost,
                life.avg_ok().unwrap_or(0.0)
            ),
        ));
    }
    if cfg.price_in == 0.0 {
        rows.push((
            "pricing".into(),
            "unknown — set price_input/price_output in config.toml".into(),
        ));
    }
    let label_w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let inner_w = rows
        .iter()
        .map(|(_, v)| label_w + 2 + v.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(30, 74);
    let title = format!(" ragent {} ", env!("CARGO_PKG_VERSION"));
    let top_fill = inner_w.saturating_sub(title.chars().count());
    app.log.push(vec![
        (" ╭─".into(), C_GUTTER),
        (
            title,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        (format!("{}╮", "─".repeat(top_fill)), C_GUTTER),
    ]);
    for (k, v) in rows {
        let pad = inner_w.saturating_sub(label_w + 2 + v.chars().count());
        app.log.push(vec![
            (" │ ".into(), C_GUTTER),
            (format!("{k:<label_w$}"), C_DIM),
            ("  ".into(), Style::default()),
            (v, Style::default()),
            (" ".repeat(pad), Style::default()),
            ("│".into(), C_GUTTER),
        ]);
    }
    app.log
        .push(sline(format!(" ╰{}╯", "─".repeat(inner_w + 1)), C_GUTTER));
    app.note("Enter send · Alt-Enter newline · Esc interrupt · ↑ hist · ^C quit");
    app.log.push(Vec::new());
}

/// ATX heading level (1..=6) and its text, if `t` is one.
fn md_heading(t: &str) -> Option<(usize, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    match rest.chars().next() {
        None => Some((n, "")),
        Some(' ') => Some((n, rest.trim_start())),
        _ => None,
    }
}

/// `(prefix, rest)` for a bullet, task or ordered list item.
fn md_item(t: &str, indent: usize) -> Option<(String, &str)> {
    let depth = (indent / 2).min(2);
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            let bullet = ['•', '◦', '▪'][depth];
            let pfx = format!("  {}{bullet} ", "  ".repeat(depth));
            let (mark, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
                ("☐ ", r)
            } else if let Some(r) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                ("☑ ", r)
            } else {
                ("", rest)
            };
            return Some((format!("{pfx}{mark}"), rest));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let (num, after) = t.split_at(digits);
        let rest = after
            .strip_prefix('.')
            .or_else(|| after.strip_prefix(')'))
            .and_then(|r| r.strip_prefix(' '));
        if let Some(rest) = rest {
            return Some((format!("  {}{num}. ", "  ".repeat(depth)), rest));
        }
    }
    None
}

/// One raw assistant-text line → styled spans (with left indent).
/// Returns the line and whether it holds visible content (for the ● marker).
fn md_line(raw: &str, in_fence: &mut bool) -> (SLine, bool) {
    let t = raw.trim_start();
    if t.starts_with("```") || t.starts_with("~~~") {
        *in_fence = !*in_fence;
        return (
            vec![("  ▎ ".into(), C_GUTTER), (t.to_string(), C_DIM)],
            true,
        );
    }
    if *in_fence {
        return (
            vec![("  ▎ ".into(), C_GUTTER), (raw.to_string(), C_CODE)],
            !t.is_empty(),
        );
    }
    if t.is_empty() {
        return (Vec::new(), false);
    }
    if let Some((lvl, txt)) = md_heading(t) {
        let st = match lvl {
            1 => C_HEAD,
            2 => C_H2,
            _ => C_H3,
        };
        let mut l: SLine = vec![("  ".into(), Style::default())];
        for (s, x) in md_inline(txt) {
            l.push((s, if x == Style::default() { st } else { x }));
        }
        return (l, true);
    }
    if t.starts_with('>') {
        let (mut rest, mut depth) = (t, 0);
        while let Some(r) = rest.strip_prefix('>') {
            depth += 1;
            rest = r.trim_start();
        }
        let mut l: SLine = vec![(format!("  {}", "▎ ".repeat(depth.min(4))), C_GUTTER)];
        for (s, x) in md_inline(rest) {
            l.push((s, if x == Style::default() { C_DIM } else { x }));
        }
        return (l, true);
    }
    if let Some((pfx, rest)) = md_item(t, raw.len() - t.len()) {
        let mut l: SLine = vec![(pfx, C_DIM)];
        l.extend(md_inline(rest));
        return (l, true);
    }
    let mut l: SLine = vec![("  ".into(), Style::default())];
    l.extend(md_inline(t));
    (l, true)
}

fn md_is_table_row(t: &str) -> bool {
    t.starts_with('|') && t.matches('|').count() >= 2
}

fn md_is_table_sep(t: &str) -> bool {
    md_is_table_row(t)
        && md_cells(t).iter().all(|c| {
            !c.is_empty()
                && c.chars().any(|ch| ch == '-')
                && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

/// A `---`, `***` or `___` thematic break.
fn md_is_rule(t: &str) -> bool {
    let c = match t.chars().next() {
        Some(c @ ('-' | '*' | '_')) => c,
        _ => return false,
    };
    t.chars().filter(|ch| !ch.is_whitespace()).count() >= 3
        && t.chars().all(|ch| ch.is_whitespace() || ch == c)
}

fn md_cells(row: &str) -> Vec<String> {
    let r = row.replace("\\|", "\u{1}");
    let r = r.trim().trim_start_matches('|').trim_end_matches('|');
    r.split('|')
        .map(|c| c.replace('\u{1}', "|").trim().to_string())
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum MdAlign {
    Left,
    Center,
    Right,
}

fn md_spans_w(l: &SLine) -> usize {
    l.iter().map(|(t, _)| md_text_w(t)).sum()
}

fn md_text_w(t: &str) -> usize {
    t.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

fn md_fit(l: &SLine, w: usize) -> SLine {
    if md_spans_w(l) <= w {
        return l.clone();
    }
    let mut out: SLine = Vec::new();
    let mut used = 0usize;
    for (t, st) in l {
        let mut buf = String::new();
        for ch in t.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > w.saturating_sub(1) {
                if !buf.is_empty() {
                    out.push((buf, *st));
                }
                out.push(("…".into(), C_DIM));
                return out;
            }
            buf.push(ch);
            used += cw;
        }
        if !buf.is_empty() {
            out.push((buf, *st));
        }
    }
    out
}

/// Render a pipe table as a boxed, column-aligned table.
fn md_table(rows: &[String], sep: &str, width: usize) -> Vec<(SLine, bool)> {
    let ncol = md_cells(sep)
        .len()
        .max(rows.first().map_or(0, |r| md_cells(r).len()));
    let raw_rows: Vec<Vec<SLine>> = rows
        .iter()
        .map(|r| {
            let cells = md_cells(r);
            (0..ncol)
                .map(|i| md_inline(cells.get(i).map_or("", |c| c.as_str())))
                .collect()
        })
        .collect();
    let plain = |r: &str| {
        let mut l: SLine = vec![("  ".into(), Style::default())];
        l.extend(md_inline(r.trim()));
        (l, true)
    };
    let overhead = 3 * ncol + 3;
    if width <= overhead + ncol {
        return rows.iter().map(|r| plain(r)).collect();
    }
    let aligns: Vec<MdAlign> = md_cells(sep)
        .iter()
        .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => MdAlign::Center,
            (false, true) => MdAlign::Right,
            _ => MdAlign::Left,
        })
        .collect();
    let mut colw = vec![1usize; ncol];
    for row in &raw_rows {
        for (i, cell) in row.iter().enumerate() {
            colw[i] = colw[i].max(md_spans_w(cell));
        }
    }
    let avail = width - overhead;
    while colw.iter().sum::<usize>() > avail {
        let idx = (0..ncol).max_by_key(|i| colw[*i]).unwrap_or(0);
        if colw[idx] <= 1 {
            break;
        }
        colw[idx] -= 1;
    }
    let rule = |left: &str, mid: &str, right: &str| -> SLine {
        let mut l: SLine = vec![("  ".into(), Style::default()), (left.into(), C_GUTTER)];
        for (i, cw) in colw.iter().enumerate() {
            l.push(("─".repeat(cw + 2), C_GUTTER));
            l.push(((if i + 1 == ncol { right } else { mid }).into(), C_GUTTER));
        }
        l
    };
    let cells = |row: &Vec<SLine>, header: bool| -> SLine {
        let mut l: SLine = vec![("  ".into(), Style::default()), ("│".into(), C_GUTTER)];
        for (i, cell) in row.iter().enumerate() {
            let mut spans = md_fit(cell, colw[i]);
            if header {
                for s in spans.iter_mut() {
                    if s.1 == Style::default() {
                        s.1 = C_H3;
                    }
                }
            }
            let pad = colw[i].saturating_sub(md_spans_w(&spans));
            let (lft, rgt) = match aligns.get(i).copied().unwrap_or(MdAlign::Left) {
                MdAlign::Left => (0, pad),
                MdAlign::Right => (pad, 0),
                MdAlign::Center => (pad / 2, pad - pad / 2),
            };
            if lft > 0 {
                l.push((" ".repeat(lft + 1), Style::default()));
            } else {
                l.push((" ".into(), Style::default()));
            }
            l.extend(spans);
            l.push((" ".repeat(rgt + 1), Style::default()));
            l.push(("│".into(), C_GUTTER));
        }
        l
    };
    let mut out: Vec<(SLine, bool)> = vec![(rule("┌", "┬", "┐"), false)];
    for (i, row) in raw_rows.iter().enumerate() {
        out.push((cells(row, i == 0), true));
        if i == 0 {
            out.push((rule("├", "┼", "┤"), false));
        }
    }
    out.push((rule("└", "┴", "┘"), false));
    out
}

/// Block-level markdown: a whole assistant message (or the live, still growing
/// stream) into styled scrollback lines. `width` sizes rules and tables.
fn md_block(text: &str, in_fence: &mut bool, width: usize) -> Vec<(SLine, bool)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<(SLine, bool)> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let raw = lines[i];
        let t = raw.trim_start();
        if !*in_fence {
            if md_is_table_row(t)
                && i + 1 < lines.len()
                && md_is_table_sep(lines[i + 1].trim_start())
            {
                let sep = lines[i + 1].trim_start().to_string();
                let mut rows = vec![t.to_string()];
                i += 2;
                while i < lines.len() && md_is_table_row(lines[i].trim_start()) {
                    rows.push(lines[i].trim_start().to_string());
                    i += 1;
                }
                out.extend(md_table(&rows, &sep, width));
                continue;
            }
            if md_is_rule(t) {
                out.push((
                    vec![
                        ("  ".into(), Style::default()),
                        ("─".repeat(width.saturating_sub(4).max(3)), C_GUTTER),
                    ],
                    true,
                ));
                i += 1;
                continue;
            }
            // a 4-space/tab indented block after a blank line is code, unless it
            // starts with a list marker (that is a nested list instead)
            let ind = raw.len() - t.len();
            let prev_blank = i == 0 || lines[i - 1].trim().is_empty();
            if prev_blank
                && (raw.starts_with('\t') || ind >= 4)
                && !t.is_empty()
                && md_item(t, ind).is_none()
            {
                while i < lines.len() {
                    let r = lines[i];
                    let indent = r.len() - r.trim_start().len();
                    if r.trim().is_empty() {
                        // a blank line only stays inside the block when more
                        // indented code follows it
                        let more = lines.get(i + 1).is_some_and(|n| {
                            !n.trim().is_empty()
                                && (n.starts_with('\t') || n.len() - n.trim_start().len() >= 4)
                        });
                        if !more {
                            break;
                        }
                        out.push((vec![("  ▎ ".into(), C_GUTTER)], false));
                        i += 1;
                        continue;
                    }
                    if indent == 0 {
                        break;
                    }
                    let body = r
                        .strip_prefix('\t')
                        .or_else(|| r.strip_prefix("    "))
                        .unwrap_or(r);
                    out.push((
                        vec![("  ▎ ".into(), C_GUTTER), (body.to_string(), C_CODE)],
                        true,
                    ));
                    i += 1;
                }
                continue;
            }
        }
        out.push(md_line(raw, in_fence));
        i += 1;
    }
    out
}

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.ensure_wrap(area.width as usize);
    if app.stream_dirty {
        app.rebuild_stream();
    }

    let iw = area.width.saturating_sub(2) as usize;
    let want_rows = input_rows(&app.input, iw);
    let input_h = ((want_rows as u16) + 2)
        .clamp(3, 8)
        .min(area.height.saturating_sub(4).max(3));
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(area);
    let (chat, input_a, status) = (chunks[0], chunks[1], chunks[2]);
    let chat_h = chat.height as usize;
    let elapsed = app
        .started
        .map(|t| t.elapsed().as_secs_f32())
        .unwrap_or(0.0);

    // virtual activity line at the tail while a task runs
    let spin = SPIN[(elapsed * 8.0) as usize % SPIN.len()];
    let extra: Vec<SLine> = if app.running {
        vec![vec![
            (format!("{spin} "), C_TOOL),
            (
                format!("working · step {} · {:.0}s", app.iter, elapsed),
                C_DIM,
            ),
        ]]
    } else {
        Vec::new()
    };

    // scroll bookkeeping
    let real_total = app.wrapped.len() + app.stream_wrapped.len();
    let total = real_total + extra.len();
    let max_scroll = total.saturating_sub(chat_h);
    if app.follow {
        app.scroll = max_scroll;
    }
    app.scroll = app.scroll.min(max_scroll);

    let mut visible: Vec<Line> = Vec::with_capacity(chat_h);
    for i in app.scroll..(app.scroll + chat_h).min(total) {
        let src: &SLine = if i < app.wrapped.len() {
            &app.wrapped[i]
        } else if i < real_total {
            &app.stream_wrapped[i - app.wrapped.len()]
        } else {
            &extra[0]
        };
        visible.push(Line::from(
            src.iter()
                .map(|(s, st)| Span::styled(s.as_str(), *st))
                .collect::<Vec<Span>>(),
        ));
    }
    f.render_widget(Paragraph::new(Text::from(visible)), chat);

    // input box
    let inner_h = input_a.height.saturating_sub(2);
    let (cur_row, cur_col) = cursor_pos(&app.input[..app.cur], iw);
    app.input_scroll = if cur_row as u16 >= inner_h {
        cur_row as u16 - inner_h + 1
    } else {
        0
    };
    let title = if app.running {
        " ❯ input — Enter queues · Esc interrupts "
    } else {
        " ❯ input — Enter sends · Alt-Enter newline · Ctrl-C quits "
    };
    let border = if app.running { C_GUTTER } else { C_USER };
    let input_txt: &str = if app.input.is_empty() {
        " ask anything…"
    } else {
        app.input.as_str()
    };
    let hint = app.input.is_empty();
    f.render_widget(
        Paragraph::new(input_txt)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(title)
                    .border_style(border),
            )
            .style(if hint { C_DIM } else { Style::default() })
            .wrap(Wrap { trim: false })
            .scroll((app.input_scroll, 0)),
        input_a,
    );
    f.set_cursor_position(Position::new(
        input_a.x + 1 + cur_col as u16,
        (input_a.y + 1 + cur_row as u16).saturating_sub(app.input_scroll),
    ));

    // status bar: left segments · right model/scroll hint
    let (mark, mark_st) = if app.running {
        (spin.to_string(), C_TOOL.bg(Color::Indexed(237)))
    } else {
        ("●".into(), C_OK.bg(Color::Indexed(237)))
    };
    let sep = || (" │ ".to_string(), C_GUTTER.bg(Color::Indexed(237)));
    let mut left: Vec<(String, Style)> = vec![
        (" ".into(), C_BAR),
        (mark, mark_st),
        (" ".into(), C_BAR),
        (
            if app.running {
                format!("step {} · {:.0}s", app.iter, elapsed)
            } else {
                "idle".into()
            },
            C_BAR_HI,
        ),
        sep(),
        (
            format!(
                "↑{} ↓{} ⛁{}",
                fmt_tokens(app.in_tok),
                fmt_tokens(app.out_tok),
                fmt_tokens(app.cached)
            ),
            C_BAR,
        ),
        sep(),
        (format!("task ${:.4}", app.task_cost), C_BAR_HI),
    ];
    if app.ok_cnt + app.fail_cnt > 0 {
        left.push(sep());
        left.push((
            format!(
                "sess ${:.4} · {}✓ {}✗ · ${:.4}/✓",
                app.cost,
                app.ok_cnt,
                app.fail_cnt,
                if app.ok_cnt > 0 {
                    app.cost / app.ok_cnt as f64
                } else {
                    0.0
                }
            ),
            C_BAR,
        ));
    }
    let mut right_txt = format!("{} ", app.model);
    if !app.follow {
        right_txt = format!("▲ scrolled · {} ", app.model);
    }
    let right_w = right_txt.chars().count() as u16;
    let bars = Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)]).split(status);
    f.render_widget(
        Paragraph::new(Line::from(
            left.iter()
                .map(|(s, x)| Span::styled(s.as_str(), *x))
                .collect::<Vec<Span>>(),
        ))
        .style(C_BAR),
        bars[0],
    );
    let right_st = if app.follow {
        C_BAR
    } else {
        Style::new().bg(Color::Indexed(237)).fg(Color::Yellow)
    };
    f.render_widget(Paragraph::new(right_txt).style(right_st), bars[1]);
}

fn prev_char(s: &str, i: usize) -> (usize, char) {
    let c = s[..i].chars().next_back().unwrap();
    (i - c.len_utf8(), c)
}

/// Byte index of the start of the word before position `i`-ish cursor `s.len()` context.
fn word_back(s: &str) -> usize {
    let mut i = s.len();
    while i > 0 {
        let (start, c) = prev_char(s, i);
        if !c.is_whitespace() {
            break;
        }
        i = start;
    }
    while i > 0 {
        let (start, c) = prev_char(s, i);
        if c.is_whitespace() {
            break;
        }
        i = start;
    }
    i
}

fn on_key(app: &mut App, k: KeyEvent, cmd: &mpsc::Sender<UserCmd>) -> bool {
    if k.kind == KeyEventKind::Release {
        return false;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('c') if ctrl => return true,
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return true,
        KeyCode::Char('l') if ctrl => {
            app.log.clear();
            app.wrapped.clear();
            app.wrap_len = 0;
            app.scroll = 0;
            app.in_fence = false;
            app.stream_dirty = true;
        }
        KeyCode::Esc => {
            if app.running {
                let _ = cmd.try_send(UserCmd::Cancel);
            } else if !app.input.is_empty() {
                app.input.clear();
                app.cur = 0;
            }
        }
        KeyCode::Enter if alt || shift => {
            app.input.insert(app.cur, '\n');
            app.cur += 1;
        }
        KeyCode::Enter => {
            let text = app.input.trim_end().to_string();
            if !text.is_empty() {
                let queued = app.running;
                app.push_user(&text, queued);
                app.hist.push(text.clone());
                app.hi = app.hist.len();
                app.draft.clear();
                let _ = cmd.try_send(UserCmd::Prompt(text));
                app.input.clear();
                app.cur = 0;
                app.follow = true;
            }
        }
        KeyCode::Backspace => {
            if app.cur > 0 {
                let prev = app.input[..app.cur].chars().next_back().unwrap().len_utf8();
                app.input.replace_range(app.cur - prev..app.cur, "");
                app.cur -= prev;
            }
        }
        KeyCode::Delete => {
            if app.cur < app.input.len() {
                let n = app.input[app.cur..].chars().next().unwrap().len_utf8();
                app.input.replace_range(app.cur..app.cur + n, "");
            }
        }
        KeyCode::Left => {
            if ctrl {
                app.cur = word_back(&app.input[..app.cur]);
            } else if app.cur > 0 {
                app.cur -= app.input[..app.cur].chars().next_back().unwrap().len_utf8();
            }
        }
        KeyCode::Right => {
            if ctrl {
                while app.cur < app.input.len()
                    && !app.input[app.cur..].chars().next().unwrap().is_whitespace()
                {
                    app.cur += app.input[app.cur..].chars().next().unwrap().len_utf8();
                }
                while app.cur < app.input.len()
                    && app.input[app.cur..].chars().next().unwrap().is_whitespace()
                {
                    app.cur += app.input[app.cur..].chars().next().unwrap().len_utf8();
                }
            } else if app.cur < app.input.len() {
                app.cur += app.input[app.cur..].chars().next().unwrap().len_utf8();
            }
        }
        KeyCode::Home => app.cur = 0,
        KeyCode::End => app.cur = app.input.len(),
        KeyCode::Char('a') if ctrl => app.cur = 0,
        KeyCode::Char('e') if ctrl => app.cur = app.input.len(),
        KeyCode::Char('u') if ctrl => {
            app.input.replace_range(..app.cur, "");
            app.cur = 0;
        }
        KeyCode::Char('k') if ctrl => {
            app.input.truncate(app.cur);
        }
        KeyCode::Char('w') if ctrl => {
            let n = word_back(&app.input[..app.cur]);
            app.input.replace_range(n..app.cur, "");
            app.cur = n;
        }
        KeyCode::Up => {
            if app.hi > 0 {
                if app.hi == app.hist.len() {
                    app.draft = app.input.clone();
                }
                app.hi -= 1;
                app.input = app.hist[app.hi].clone();
                app.cur = app.input.len();
            }
        }
        KeyCode::Down => {
            if app.hi < app.hist.len() {
                app.hi += 1;
                app.input = if app.hi == app.hist.len() {
                    app.draft.clone()
                } else {
                    app.hist[app.hi].clone()
                };
                app.cur = app.input.len();
            }
        }
        KeyCode::PageUp => app.scroll_by(-10),
        KeyCode::PageDown => app.scroll_by(10),
        KeyCode::Char(c) if !ctrl => {
            app.input.insert(app.cur, c);
            app.cur += c.len_utf8();
        }
        _ => {}
    }
    false
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
                    None => { /* driver gone: stay until user quits */ }
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
    use ratatui::backend::TestBackend;

    fn cfg() -> Config {
        Config {
            api_key: String::new(),
            base_url: String::new(),
            model: "test-model".into(),
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

    fn screen(buf: &ratatui::buffer::Buffer, w: u16, h: u16) -> String {
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn inline_code_and_bold() {
        let l = md_inline("a `x` b **y** c");
        let texts: Vec<&str> = l.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(texts, ["a ", "x", " b ", "y", " c"]);
        assert_eq!(l[1].1, C_ICODE);
        assert!(l[3].1.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn line_fence_heading_bullet() {
        let mut fence = false;
        let (l, c) = md_line("```rust", &mut fence);
        assert!(fence && c && l[0].0 == "  ▎ ");
        let (l, _) = md_line("fn main() {}", &mut fence);
        assert_eq!(l[0].0, "  ▎ ");
        assert_eq!(l[1].1, C_CODE);
        md_line("```", &mut fence);
        assert!(!fence);
        let (l, c) = md_line("# Title", &mut fence);
        assert!(c && l[1].1 == C_HEAD && l[1].0 == "Title");
        let (l, _) = md_line("- item", &mut fence);
        assert_eq!(l[0].0, "  • ");
    }

    #[test]
    fn marker_swaps_leading_indent() {
        let mut fence = false;
        let (mut l, _) = md_line("hello", &mut fence);
        mark_line(&mut l);
        assert_eq!(l[0].0, "● ");
        let (mut l, _) = md_line("```python", &mut fence);
        mark_line(&mut l);
        assert_eq!(l[0].0, "● ");
        assert_eq!(l[1].0, "  ▎ ");
    }

    #[test]
    fn frame_renders_banner_chat_input_status() {
        let mut app = App::new();
        app.model = "test-model".into();
        banner(&mut app, &cfg(), std::path::Path::new("/tmp/x"));
        app.push_user("hello", false);
        app.stream = "answer `42`\n\n- a point".into();
        app.flush_stream();
        app.push(vec![
            ("  ◆ ".into(), C_TOOL),
            (
                "read_file".into(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            (" src/main.rs".into(), C_DIM),
        ]);
        app.push(vec![
            ("    └ ".into(), C_GUTTER),
            ("✓ 4ms".into(), C_OK),
            ("  head".into(), C_DIM),
        ]);

        let backend = TestBackend::new(70, 22);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let s = screen(term.backend().buffer(), 70, 22);

        assert!(s.contains("ragent 0.1.0"), "banner title:\n{s}");
        assert!(s.contains("test-model"), "model row:\n{s}");
        assert!(s.contains("❯ hello"), "user marker:\n{s}");
        assert!(s.contains("● answer"), "assist marker:\n{s}");
        assert!(s.contains("• a point"), "bullet:\n{s}");
        assert!(s.contains("◆ read_file"), "tool line:\n{s}");
        assert!(s.contains("└ ✓ 4ms"), "tool end:\n{s}");
        assert!(s.contains("idle"), "status:\n{s}");
        assert!(s.contains("ask anything"), "placeholder:\n{s}");
    }

    #[test]
    fn block_renders_tables_rules_lists_and_headings() {
        let md = "# One\n## Two\n### Three\n\n1. first\n2. second\n\n- [x] done\n- [ ] todo\n\n---\n\n| name | qty |\n|:-----|----:|\n| apples | 12 |\n| pears | 3 |\n";
        let mut fence = false;
        let out = md_block(md, &mut fence, 60);
        let text: Vec<String> = out
            .iter()
            .map(|(l, _)| l.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect();
        assert_eq!(text[0], "  One");
        assert_eq!(text[1], "  Two");
        assert_eq!(text[2], "  Three");
        assert_eq!(out[0].0[1].1, C_HEAD);
        assert_eq!(out[1].0[1].1, C_H2);
        assert_eq!(out[2].0[1].1, C_H3);
        assert!(text.contains(&"  1. first".to_string()), "{text:?}");
        assert!(text.contains(&"  • ☑ done".to_string()), "{text:?}");
        assert!(text.contains(&"  • ☐ todo".to_string()), "{text:?}");
        assert!(text.iter().any(|t| t.starts_with("  ────")), "{text:?}");
        // the boxed table: borders, header and aligned right column
        assert!(text.iter().any(|t| t.starts_with("  ┌─")), "{text:?}");
        assert!(text.iter().any(|t| t == "  │ name   │ qty │"), "{text:?}");
        assert!(text.iter().any(|t| t == "  │ apples │  12 │"), "{text:?}");
        assert!(text.iter().any(|t| t == "  │ pears  │   3 │"), "{text:?}");
    }

    #[test]
    fn indented_code_block_gets_the_gutter_bar() {
        let mut fence = false;
        let out = md_block("text\n\n    let x = 1;\n    x + 1\n\nafter", &mut fence, 40);
        let text: Vec<String> = out
            .iter()
            .map(|(l, _)| l.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect();
        assert_eq!(
            text,
            vec!["  text", "", "  ▎ let x = 1;", "  ▎ x + 1", "", "  after",]
        );
        assert_eq!(out[2].0[1].1, C_CODE);
    }

    #[test]
    fn inline_styles_italics_strike_and_links() {
        let l = md_inline("an *italic* and ~~gone~~ and [docs](http://x/y) end");
        let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "an italic and gone and docs (http://x/y) end");
        assert!(l[1].1.add_modifier.contains(Modifier::ITALIC), "{l:?}");
        assert!(l[3].1.add_modifier.contains(Modifier::CROSSED_OUT), "{l:?}");
        assert_eq!(l[5].1, C_LINK);
        // underscores inside words stay literal, escaped markers are kept
        let l = md_inline("snake_case_name and \\*literal\\*");
        let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "snake_case_name and *literal*");
    }

    #[test]
    fn wrapped_lines_keep_gutter_and_word_boundaries() {
        use crate::util::wrap_sline;
        let l = vec![
            ("  ▎ ".to_string(), C_GUTTER),
            ("    if a { b }".to_string(), C_CODE),
        ];
        let rows = wrap_sline(&l, 14);
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r[0].0, "  ▎ ");
            assert_eq!(r[1].0, "    ");
        }
        // paragraphs wrap on spaces, not mid-word
        let p = vec![
            ("  ".to_string(), Style::default()),
            ("alpha beta gamma".to_string(), Style::default()),
        ];
        let rows = wrap_sline(&p, 12);
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, vec!["  alpha beta", "  gamma"]);
        // list numbers/bullets blank out on the wrapped row
        let li = vec![
            ("  2. ".to_string(), C_DIM),
            ("alpha beta gamma".to_string(), Style::default()),
        ];
        let rows = wrap_sline(&li, 12);
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, vec!["  2. alpha", "     beta", "     gamma"]);
    }

    #[test]
    fn frame_running_shows_spinner_and_state() {
        let mut app = App::new();
        app.running = true;
        app.iter = 3;
        app.started = Some(Instant::now());
        let backend = TestBackend::new(60, 15);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let s = screen(term.backend().buffer(), 60, 15);
        assert!(s.contains("working · step 3"), "activity line:\n{s}");
        assert!(s.contains("step 3"), "status step:\n{s}");
        assert!(s.contains("Enter queues"), "running title:\n{s}");
    }
}
