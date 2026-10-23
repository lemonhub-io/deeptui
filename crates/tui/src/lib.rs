//! Terminal UI. It owns no conversation and records no stats: it folds the
//! session's event stream into a picture, and turns keys into [`Remote`]
//! calls. Everything it shows is either in the stream or in its [`Intro`].
//!
//! - [`app`] composes the parts and applies events and keys;
//! - [`feed`] is the scrollback, [`editor`] the input line, [`meter`] the gauges;
//! - [`keymap`] maps keys to intents, [`draw`] renders a frame;
//! - [`md`] and [`styled`] turn assistant markdown into wrapped styled lines.

mod app;
mod draw;
mod editor;
mod feed;
mod keymap;
mod md;
mod meter;
mod styled;
mod theme;

use anyhow::Result;
use app::App;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as Term, EventStream, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use deeptui_core::{Event, Ledger, Projection};
use deeptui_engine::Remote;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

/// What the startup card shows — known before the first event arrives.
pub struct Intro {
    pub model: String,
    pub root: PathBuf,
    pub context_budget: usize,
    /// Every journalled turn, for the history row.
    pub lifetime: Ledger,
    /// False when the tariff is unknown and the meter would read $0.
    pub priced: bool,
    /// A newer release already known from the cached check.
    pub update: Option<String>,
}

/// Run the UI until the user quits. `late_update` resolves to a newer
/// release tag, whenever the background check lands.
pub async fn run(
    remote: Remote,
    events: mpsc::Receiver<Event>,
    intro: Intro,
    late_update: impl Future<Output = Option<String>>,
) -> Result<()> {
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
    let res = run_ui(&mut term, remote, events, intro, late_update).await;
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

async fn run_ui(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    remote: Remote,
    mut events: mpsc::Receiver<Event>,
    intro: Intro,
    late_update: impl Future<Output = Option<String>>,
) -> Result<()> {
    let mut app = App::new(intro.model.clone());
    app.root = intro.root.display().to_string();
    app.context_budget = intro.context_budget;
    app.motion = std::env::var_os("DEEPTUI_REDUCED_MOTION")
        .is_none_or(|value| value.is_empty() || value == "0");
    app.update = intro.update.clone();
    draw::banner(&mut app.feed, &intro);

    let mut late_update = std::pin::pin!(late_update);
    let mut checked = false;
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    term.draw(|f| draw::render(f, &mut app))?;

    loop {
        tokio::select! {
            e = keys.next() => match e {
                Some(Ok(Term::Key(k))) => app.key(k, &remote),
                Some(Ok(Term::Paste(s))) => app.editor.insert_str(&styled::sanitize(&s)),
                Some(Ok(Term::Mouse(m))) => match m.kind {
                    MouseEventKind::ScrollUp => app.feed.scroll_by(-3),
                    MouseEventKind::ScrollDown => {
                        app.feed.scroll_by(3);
                        let area = term.size().map(|s| ratatui::layout::Rect::new(0, 0, s.width, s.height)).unwrap_or_default();
                        let chat = draw::regions(area, &app.editor.text)[1];
                        app.feed.refollow(chat.height as usize);
                    }
                    _ => {}
                },
                _ => {}
            },
            // a closed stream resolves instantly: stop polling it once dead,
            // or the loop would spin
            ev = events.recv(), if !app.agent_dead => match ev {
                Some(ev) => {
                    app.apply(&ev);
                    while let Ok(ev) = events.try_recv() {
                        app.apply(&ev);
                    }
                }
                // the session died or closed the stream
                None => app.agent_gone(),
            },
            _ = tick.tick(), if app.meter.running && app.motion => {}
            tag = &mut late_update, if !checked => {
                checked = true;
                if let Some(tag) = tag {
                    app.update_found(tag);
                }
            }
        }
        term.draw(|f| draw::render(f, &mut app))?;
        if app.quit {
            return Ok(());
        }
    }
}
