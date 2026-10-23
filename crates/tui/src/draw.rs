//! Responsive terminal chrome, conversation viewport, and prompt composer.

use crate::app::App;
use crate::editor::{cursor_pos, input_lines, input_rows};
use crate::feed::Feed;
use crate::styled::SLine;
use crate::theme::*;
use crate::Intro;
use deeptui_core::text::fmt_tokens;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// The welcome stays in scrollback; session controls live in the fixed chrome.
pub(crate) fn banner(feed: &mut Feed, intro: &Intro) {
    feed.push(Vec::new());
    feed.push(vec![
        ("  ◈  ".into(), C_USER_MARK),
        ("DeepTUI".into(), C_TITLE),
        (format!("  v{}", env!("CARGO_PKG_VERSION")), C_DIM),
    ]);
    feed.note("A little space for deep work.");
    feed.push(Vec::new());
    for (label, value) in [
        ("model", intro.model.clone()),
        ("root", intro.root.display().to_string()),
        (
            "context",
            format!("{}k tokens · auto-compaction", intro.context_budget / 1000),
        ),
    ] {
        feed.push(vec![(format!("  {label:<9}"), C_DIM), (value, C_CODE)]);
    }
    if intro.lifetime.tasks > 0 {
        let life = &intro.lifetime;
        feed.note(format!(
            "{} tasks ({}✓ {}✗) · ${:.4} spent · ${:.4}/✓",
            life.tasks,
            life.ok,
            life.failed(),
            life.cost,
            life.avg_ok().unwrap_or(0.0)
        ));
    }
    if !intro.priced {
        feed.note("pricing unknown — set price_input/price_output in config.toml");
    }
    if let Some(tag) = &intro.update {
        feed.note(format!("{tag} available — deeptui --self-update"));
    }
    feed.push(Vec::new());
    feed.push(vec![
        ("  Try a task  ".into(), C_DIM),
        (
            "Find the bug. Explain this repo. Add a test.".into(),
            C_CODE,
        ),
    ]);
    feed.push(Vec::new());
}

fn line(l: &SLine) -> Line<'_> {
    Line::from(
        l.iter()
            .map(|(s, st)| Span::styled(s.as_str(), *st))
            .collect::<Vec<_>>(),
    )
}

/// Keep padding and fixed chrome out of the conversation's scroll geometry.
pub(crate) fn regions(area: Rect, text: &str) -> [Rect; 4] {
    let margin = if area.width >= 60 {
        2
    } else if area.width >= 24 {
        1
    } else {
        0
    };
    let body = Rect::new(
        area.x + margin,
        area.y,
        area.width.saturating_sub(margin * 2),
        area.height,
    );
    let header_h = if area.height >= 18 {
        3
    } else if area.height >= 12 {
        2
    } else {
        0
    };
    let remaining = area.height.saturating_sub(header_h);
    let reserve_chat = if remaining >= 7 {
        3
    } else if remaining >= 5 {
        1
    } else {
        0
    };
    let available = remaining.saturating_sub(reserve_chat + 1);
    let want = input_rows(text, body.width.saturating_sub(4) as usize).min(6) as u16 + 2;
    let input_h = want.min(available).min(8);
    Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Min(0),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .areas(body)
}

fn header(f: &mut Frame, app: &App, area: Rect, spin: char) {
    if area.is_empty() {
        return;
    }
    let state = if app.agent_dead {
        " OFFLINE "
    } else if app.meter.running {
        " WORKING "
    } else {
        " READY "
    };
    let state_style = if app.agent_dead {
        C_ERR
    } else if app.meter.running {
        C_TITLE
    } else {
        C_OK
    };
    let [brand, badge] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(state.len() as u16 + 2),
    ])
    .areas(Rect::new(area.x, area.y, area.width, 1));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("◈  DeepTUI", C_TITLE),
            Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), C_DIM),
        ])),
        brand,
    );
    f.render_widget(
        Paragraph::new(format!("{spin}{state}")).style(state_style),
        badge,
    );
    if area.height >= 3 {
        let model = format!("{}  /  {}k context", app.model, app.context_budget / 1000);
        let right_w = (model.width() as u16).min(area.width / 2);
        let [root, model_a] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)])
            .areas(Rect::new(area.x, area.y + 1, area.width, 1));
        f.render_widget(Paragraph::new(app.root.as_str()).style(C_DIM), root);
        f.render_widget(
            Paragraph::new(model)
                .style(C_DIM)
                .alignment(ratatui::layout::Alignment::Right),
            model_a,
        );
    }
    let phase = (app.meter.elapsed() * 16.0) as u16;
    let rail: Vec<Span> = (0..area.width)
        .map(|x| {
            let color =
                if app.motion && app.meter.running && x.abs_diff(phase % area.width.max(1)) < 3 {
                    ACCENT
                } else {
                    EDGE
                };
            Span::styled("─", Style::new().fg(color))
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(rail)),
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
    );
}

pub(crate) fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::new().fg(FG).bg(BG)), area);
    if area.is_empty() {
        return;
    }
    let [head, chat, input_a, status] = regions(area, &app.editor.text);
    let elapsed = app.meter.elapsed();
    let spin = if app.meter.running {
        SPIN[if app.motion {
            (elapsed * 10.0) as usize % SPIN.len()
        } else {
            0
        }]
    } else if app.agent_dead {
        '○'
    } else {
        '●'
    };
    header(f, app, head, spin);

    let feed = &mut app.feed;
    feed.ensure_wrap(chat.width as usize);
    if feed.stream_dirty {
        feed.rebuild_stream();
    }
    let extra: Vec<SLine> = if app.meter.running {
        vec![vec![
            (format!("  {spin}  "), C_TOOL),
            (format!("working · step {}", app.meter.round), C_ASSIST),
            (format!("  {:.0}s · Esc interrupt", elapsed), C_DIM),
        ]]
    } else {
        Vec::new()
    };
    let chat_h = chat.height as usize;
    let total = feed.rows() + extra.len();
    let max_scroll = total.saturating_sub(chat_h);
    if feed.follow {
        feed.scroll = max_scroll;
    }
    feed.scroll = feed.scroll.min(max_scroll);
    let visible: Vec<Line> = feed
        .wrapped
        .iter()
        .chain(&feed.stream_wrapped)
        .chain(&extra)
        .skip(feed.scroll)
        .take(chat_h)
        .map(line)
        .collect();
    f.render_widget(Paragraph::new(Text::from(visible)), chat);

    let ed = &mut app.editor;
    let m = &app.meter;
    if input_a.height >= 3 && input_a.width >= 5 {
        let border = if app.agent_dead {
            C_GUTTER
        } else if m.running && app.motion {
            Style::new().fg(breathe(elapsed))
        } else {
            C_USER_MARK
        };
        let action = if m.running {
            " Enter queues "
        } else {
            " Enter sends "
        };
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border)
            .style(Style::new().bg(PANEL))
            .padding(Padding::horizontal(1))
            .title_top(Line::from(Span::styled(" ❯  Ask DeepTUI ", C_TITLE)));
        if input_a.width >= 38 {
            block = block.title_top(Line::from(Span::styled(action, C_DIM)).right_aligned());
        }
        if input_a.width >= 62 {
            block = block.title_bottom(
                Line::from(Span::styled(
                    " Alt-Enter newline   ↑↓ history   Ctrl-C quit ",
                    C_DIM,
                ))
                .right_aligned(),
            );
        }
        let inner = block.inner(input_a);
        let iw = inner.width as usize;
        let (cur_row, cur_col) = cursor_pos(&ed.text[..ed.cur], iw);
        ed.scroll = (cur_row as u16).saturating_sub(inner.height.saturating_sub(1));
        let hint = ed.text.is_empty();
        let input_text = if hint {
            Text::from("ask anything…")
        } else {
            Text::from(
                input_lines(&ed.text, iw)
                    .into_iter()
                    .map(Line::from)
                    .collect::<Vec<_>>(),
            )
        };
        f.render_widget(
            Paragraph::new(input_text)
                .style(if hint { C_DIM } else { Style::new().fg(FG) })
                .block(block)
                .scroll((ed.scroll, 0)),
            input_a,
        );
        if !inner.is_empty() {
            f.set_cursor_position(Position::new(
                inner.x + (cur_col as u16).min(inner.width - 1),
                inner.y
                    + (cur_row as u16)
                        .saturating_sub(ed.scroll)
                        .min(inner.height - 1),
            ));
        }
    }

    // The task and outcome counters get space before optional token details.
    f.render_widget(Block::default().style(C_BAR), status);
    let right = if app.feed.follow {
        String::new()
    } else {
        format!("▲ scrolled · {} ", app.model)
    };
    let right_w = (right.width() as u16).min(status.width.saturating_sub(14));
    let [bar_l, bar_r] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)]).areas(status);
    let state = if app.agent_dead {
        "offline".into()
    } else if m.running {
        format!("step {} · {:.0}s", m.round, elapsed)
    } else {
        "idle".into()
    };
    let mut segments = vec![
        (format!(" {spin} {state}"), C_BAR_HI),
        (format!("task ${:.4}", m.task.cost), C_BAR_HI),
    ];
    if m.tally.tasks > 0 {
        segments.push((format!("{}✓ {}✗", m.tally.ok, m.tally.failed()), C_BAR));
        segments.push((
            format!("sess ${:.4} · ${:.4}/✓", m.session.cost, m.per_ok()),
            C_BAR,
        ));
    }
    segments.push((
        format!(
            "↑{} ↓{} ⛁{}",
            fmt_tokens(m.session.input),
            fmt_tokens(m.session.output),
            fmt_tokens(m.session.cached)
        ),
        C_BAR,
    ));
    let mut left = Vec::new();
    let mut used = 0;
    for (text, style) in segments {
        let gap = if left.is_empty() { 0 } else { 3 };
        let width = text.width() + gap;
        if used + width > bar_l.width as usize && !left.is_empty() {
            continue;
        }
        if gap > 0 {
            left.push((" · ".into(), C_GUTTER.bg(BAR_BG)));
        }
        left.push((text, style));
        used += width;
    }
    f.render_widget(Paragraph::new(line(&left)).style(C_BAR), bar_l);
    f.render_widget(Paragraph::new(right).style(C_BAR_WARN), bar_r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_core::Ledger;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use ratatui::Terminal;
    use std::time::Instant;

    fn intro() -> Intro {
        Intro {
            model: "test-model".into(),
            root: "/tmp/x".into(),
            context_budget: 64_000,
            lifetime: Ledger::default(),
            priced: true,
            update: None,
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

    fn draw(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        screen(term.backend().buffer(), w, h)
    }

    #[test]
    fn frame_renders_banner_chat_input_status() {
        let mut app = App::new("test-model");
        banner(&mut app.feed, &intro());
        app.feed.push_user("hello", false);
        app.feed.stream = "answer `42`\n\n- a point".into();
        app.feed.flush_stream();
        app.feed.push(vec![
            ("  ◆ ".into(), C_TOOL),
            (
                "read_file".into(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            (" src/main.rs".into(), C_DIM),
        ]);
        app.feed.push(vec![
            ("    └ ".into(), C_GUTTER),
            ("✓ 4ms".into(), C_OK),
            ("  head".into(), C_DIM),
        ]);

        let s = draw(&mut app, 70, 22);
        assert!(s.contains(env!("CARGO_PKG_VERSION")), "banner title:\n{s}");
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
    fn banner_advertises_a_newer_release_and_unknown_prices() {
        let mut feed = Feed::new();
        let mut i = intro();
        i.update = Some("v9.9.9".into());
        i.priced = false;
        i.lifetime = Ledger {
            tasks: 3,
            ok: 2,
            cost: 1.0,
        };
        banner(&mut feed, &i);
        let has = |feed: &Feed, needle: &str| {
            feed.log
                .iter()
                .any(|l| l.iter().any(|(t, _)| t.contains(needle)))
        };
        assert!(
            has(&feed, "v9.9.9 available"),
            "banner must offer the update"
        );
        assert!(has(&feed, "pricing"), "unknown prices are flagged");
        assert!(has(&feed, "3 tasks (2✓ 1✗)"), "lifetime history row");
        // …and stays quiet when the running build is current
        let mut plain = Feed::new();
        banner(&mut plain, &intro());
        assert!(!has(&plain, "available") && !has(&plain, "pricing") && !has(&plain, "tasks"));
    }

    #[test]
    fn frame_running_shows_spinner_and_state() {
        let mut app = App::new("m");
        app.meter.running = true;
        app.meter.round = 3;
        app.meter.started = Some(Instant::now());
        let s = draw(&mut app, 60, 15);
        assert!(s.contains("working · step 3"), "activity line:\n{s}");
        assert!(s.contains("step 3"), "status step:\n{s}");
        assert!(s.contains("Enter queues"), "running title:\n{s}");
    }

    #[test]
    fn scrolling_back_drops_the_tail_and_flags_the_status() {
        let mut app = App::new("test-model");
        for i in 1..=40 {
            app.feed.push_user(&format!("PROMPT{i}"), false);
        }
        let mut term = Terminal::new(TestBackend::new(50, 12)).unwrap();

        // follow mode pins the newest line to the bottom
        term.draw(|f| render(f, &mut app)).unwrap();
        let mut s = screen(term.backend().buffer(), 50, 12);
        assert!(s.contains("PROMPT40"), "tail missing:\n{s}");
        assert!(!s.contains("▲ scrolled"), "follow mode is not scrolled");

        // scrolled back: the first prompt is visible and the tail is gone
        app.feed.follow = false;
        app.feed.scroll = 0;
        term.draw(|f| render(f, &mut app)).unwrap();
        s = screen(term.backend().buffer(), 50, 12);
        assert!(
            s.contains("PROMPT1 ") || s.contains("PROMPT1\n"),
            "head:\n{s}"
        );
        assert!(!s.contains("PROMPT40"), "tail should be off-screen:\n{s}");
        assert!(s.contains("▲ scrolled · test-model"), "status hint:\n{s}");
    }

    #[test]
    fn a_tall_input_box_scrolls_to_keep_the_cursor_line_visible() {
        let mut app = App::new("m");
        app.editor.insert_str("AAA\nBBB\nCCC\nDDD\nEEE\nFFF");
        let s = draw(&mut app, 40, 10);
        assert!(s.contains("FFF"), "cursor line must stay visible:\n{s}");
        assert!(!s.contains("AAA"), "older input lines scroll away:\n{s}");
        assert!(
            app.editor.scroll > 0,
            "input scroll not tracked: {}",
            app.editor.scroll
        );
    }

    #[test]
    fn flushed_content_does_not_overscroll() {
        // the phantom stream line used to add +1 to the scroll total, pushing
        // follow-mode one line past the real end on every full chat
        let mut app = App::new("m");
        app.feed.stream = "## Plan\n\n".to_owned() + &"line\n".repeat(40) + "done";
        app.feed.flush_stream();
        draw(&mut app, 64, 10);
        // chat is 10 - 3 (input) - 1 (status) = 6 rows; follow-scroll must
        // land exactly on wrapped.len() - 6, not one further
        assert_eq!(app.feed.scroll, app.feed.wrapped.len().saturating_sub(6));
    }

    #[test]
    fn tiny_terminal_does_not_panic_and_shows_status() {
        let mut app = App::new("m");
        app.feed
            .push_user("a very long prompt that cannot possibly fit", false);
        app.feed.stream = "a very long answer that also cannot fit".into();
        for (w, h) in [(20u16, 6u16), (1, 1), (200, 2)] {
            let s = draw(&mut app, w, h);
            assert!(!s.is_empty(), "{w}x{h} rendered nothing");
            assert_eq!(s.lines().count(), h as usize);
        }
    }

    #[test]
    fn resizing_keeps_the_composer_cursor_after_its_text() {
        let mut app = App::new("test-model");
        app.editor
            .insert_str("A prompt with words, a wide 界 character, and a final Z");
        for (w, h) in [(80, 24), (36, 12), (24, 9), (110, 30)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| render(f, &mut app)).unwrap();
            let cursor = term.get_cursor_position().unwrap();
            let input = regions(Rect::new(0, 0, w, h), &app.editor.text)[2];
            assert!(input.contains(cursor), "cursor outside composer at {w}x{h}");
            let before = term
                .backend()
                .buffer()
                .cell((cursor.x - 1, cursor.y))
                .unwrap();
            assert_eq!(before.symbol(), "Z", "cursor and text disagree at {w}x{h}");
        }
    }

    #[test]
    fn an_offline_session_never_looks_ready() {
        let mut app = App::new("test-model");
        app.agent_gone();
        let s = draw(&mut app, 90, 24);
        assert!(s.contains("OFFLINE") && s.contains("offline"), "{s}");
        assert!(!s.contains("READY") && !s.contains("● idle"), "{s}");
    }

    #[test]
    fn reduced_motion_keeps_a_running_frame_still_between_events() {
        let mut app = App::new("test-model");
        app.motion = false;
        app.meter.running = true;
        app.meter.started = Some(Instant::now());
        let first = draw(&mut app, 100, 24);
        app.meter.started = Some(Instant::now() - std::time::Duration::from_millis(200));
        assert_eq!(first, draw(&mut app, 100, 24));
    }

    #[test]
    fn failed_tools_queued_prompts_and_session_counters_are_visible() {
        let mut app = App::new("m");
        app.meter.running = true;
        app.meter.tally = Ledger {
            tasks: 3,
            ok: 1,
            cost: 0.0,
        };
        app.feed.push_user("next one", true);
        app.feed.push(vec![
            ("    └ ".into(), C_GUTTER),
            ("✗ 9ms".into(), C_ERR),
            ("  boom".into(), C_DIM),
        ]);
        let s = draw(&mut app, 70, 14);
        assert!(s.contains("❯ next one (queued)"), "queued echo:\n{s}");
        assert!(
            s.contains("└ ✗ 9ms") && s.contains("boom"),
            "tool error:\n{s}"
        );
        assert!(s.contains("1✓ 2✗"), "counters:\n{s}");
    }
}
