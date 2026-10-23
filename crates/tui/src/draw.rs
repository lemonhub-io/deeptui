//! Frame drawing: chat feed + activity line, input box, status bar — and the
//! startup card.

use crate::app::App;
use crate::editor::{cursor_pos, input_rows};
use crate::feed::Feed;
use crate::styled::{sline, SLine};
use crate::theme::*;
use crate::Intro;
use deeptui_core::text::fmt_tokens;
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// Startup card: name/model/root/budget + lifetime stats + key hints.
pub(crate) fn banner(feed: &mut Feed, intro: &Intro) {
    let life = &intro.lifetime;
    let mut rows: Vec<(String, String)> = vec![
        ("model".into(), intro.model.clone()),
        ("root".into(), intro.root.display().to_string()),
        (
            "context".into(),
            format!("{}k tok", intro.context_budget / 1000),
        ),
    ];
    if life.tasks > 0 {
        rows.push((
            "history".into(),
            format!(
                "{} tasks ({}✓ {}✗) · ${:.4} spent · ${:.4}/✓",
                life.tasks,
                life.ok,
                life.failed(),
                life.cost,
                life.avg_ok().unwrap_or(0.0)
            ),
        ));
    }
    if !intro.priced {
        rows.push((
            "pricing".into(),
            "unknown — set price_input/price_output in config.toml".into(),
        ));
    }
    // From the last cached check — the banner never waits on the network.
    if let Some(tag) = &intro.update {
        rows.push((
            "update".into(),
            format!("{tag} available — deeptui --self-update"),
        ));
    }
    let label_w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let inner_w = rows
        .iter()
        .map(|(_, v)| label_w + 2 + v.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(30, 74);
    let title = format!(" DeepTUI {} ", env!("CARGO_PKG_VERSION"));
    let top_fill = inner_w.saturating_sub(title.chars().count());
    feed.push(vec![
        (" ╭─".into(), C_GUTTER),
        (title, C_TITLE),
        (format!("{}╮", "─".repeat(top_fill)), C_GUTTER),
    ]);
    for (k, v) in rows {
        let pad = inner_w.saturating_sub(label_w + 2 + v.chars().count());
        feed.push(vec![
            (" │ ".into(), C_GUTTER),
            (format!("{k:<label_w$}"), C_DIM),
            ("  ".into(), Style::default()),
            (v, Style::default()),
            (" ".repeat(pad), Style::default()),
            ("│".into(), C_GUTTER),
        ]);
    }
    feed.push(sline(format!(" ╰{}╯", "─".repeat(inner_w + 1)), C_GUTTER));
    feed.note("Enter send · Alt-Enter newline · Esc interrupt · ↑ hist · ^C quit");
    feed.push(Vec::new());
}

fn line(l: &SLine) -> Line<'_> {
    Line::from(
        l.iter()
            .map(|(s, st)| Span::styled(s.as_str(), *st))
            .collect::<Vec<Span>>(),
    )
}

pub(crate) fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let feed = &mut app.feed;
    let ed = &mut app.editor;
    let m = &app.meter;
    feed.ensure_wrap(area.width as usize);
    if feed.stream_dirty {
        feed.rebuild_stream();
    }

    let iw = area.width.saturating_sub(2) as usize;
    let want_rows = input_rows(&ed.text, iw);
    let input_h = ((want_rows as u16) + 2)
        .clamp(3, 8)
        .min(area.height.saturating_sub(4).max(3));
    let [chat, input_a, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .areas(area);
    let chat_h = chat.height as usize;
    let elapsed = m.elapsed();

    // virtual activity line at the tail while a task runs
    let spin = SPIN[(elapsed * 8.0) as usize % SPIN.len()];
    let extra: Vec<SLine> = if m.running {
        vec![vec![
            (format!("{spin} "), C_TOOL),
            (
                format!("working · step {} · {:.0}s", m.round, elapsed),
                C_DIM,
            ),
        ]]
    } else {
        Vec::new()
    };

    // scroll bookkeeping
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

    // input box
    let inner_h = input_a.height.saturating_sub(2);
    let (cur_row, cur_col) = cursor_pos(&ed.text[..ed.cur], iw);
    ed.scroll = if cur_row as u16 >= inner_h {
        cur_row as u16 - inner_h + 1
    } else {
        0
    };
    let title = if m.running {
        " ❯ input — Enter queues · Esc interrupts "
    } else {
        " ❯ input — Enter sends · Alt-Enter newline · Ctrl-C quits "
    };
    let border = if m.running { C_GUTTER } else { C_USER };
    let hint = ed.text.is_empty();
    let input_txt: &str = if hint { " ask anything…" } else { &ed.text };
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
            .scroll((ed.scroll, 0)),
        input_a,
    );
    f.set_cursor_position(Position::new(
        input_a.x + 1 + cur_col as u16,
        (input_a.y + 1 + cur_row as u16).saturating_sub(ed.scroll),
    ));

    // status bar: left segments · right model/scroll hint
    let (mark, mark_st) = if m.running {
        (spin.to_string(), C_TOOL.bg(BAR_BG))
    } else {
        ("●".into(), C_OK.bg(BAR_BG))
    };
    let sep = || (" │ ".to_string(), C_GUTTER.bg(BAR_BG));
    let mut left: SLine = vec![
        (" ".into(), C_BAR),
        (mark, mark_st),
        (" ".into(), C_BAR),
        (
            if m.running {
                format!("step {} · {:.0}s", m.round, elapsed)
            } else {
                "idle".into()
            },
            C_BAR_HI,
        ),
        sep(),
        (
            format!(
                "↑{} ↓{} ⛁{}",
                fmt_tokens(m.session.input),
                fmt_tokens(m.session.output),
                fmt_tokens(m.session.cached)
            ),
            C_BAR,
        ),
        sep(),
        (format!("task ${:.4}", m.task.cost), C_BAR_HI),
    ];
    if m.tally.tasks > 0 {
        left.push(sep());
        left.push((
            format!(
                "sess ${:.4} · {}✓ {}✗ · ${:.4}/✓",
                m.session.cost,
                m.tally.ok,
                m.tally.failed(),
                m.per_ok()
            ),
            C_BAR,
        ));
    }
    let right_txt = if feed.follow {
        format!("{} ", app.model)
    } else {
        format!("▲ scrolled · {} ", app.model)
    };
    let right_w = right_txt.chars().count() as u16;
    let [bar_l, bar_r] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)]).areas(status);
    f.render_widget(Paragraph::new(line(&left)).style(C_BAR), bar_l);
    let right_st = if feed.follow { C_BAR } else { C_BAR_WARN };
    f.render_widget(Paragraph::new(right_txt).style(right_st), bar_r);
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
