//! Frame drawing: chat scrollback + stream, input box, status bar, banner.

use super::input::{cursor_pos, input_rows};
use super::{App, C_BAR, C_BAR_HI, C_DIM, C_GUTTER, C_OK, C_TOOL, C_USER, SPIN};
use crate::config::Config;
use crate::util::{fmt_tokens, sline, SLine};
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// Startup card: name/model/root/budget + lifetime stats + key hints.
pub(super) fn banner(app: &mut App, cfg: &Config, root: &std::path::Path) {
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
    // Set from the last cached check — the banner never waits on the network.
    if let Some(tag) = &app.update {
        rows.push((
            "update".into(),
            format!("{tag} available — ragent --self-update"),
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

pub(super) fn render(f: &mut Frame, app: &mut App) {
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

    let visible: Vec<Line> = app
        .wrapped
        .iter()
        .chain(&app.stream_wrapped)
        .chain(&extra)
        .skip(app.scroll)
        .take(chat_h)
        .map(|l| {
            Line::from(
                l.iter()
                    .map(|(s, st)| Span::styled(s.as_str(), *st))
                    .collect::<Vec<Span>>(),
            )
        })
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::C_ERR;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Instant;

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
            map_bytes: 5_000,
            update_check: false,
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
    fn banner_advertises_a_newer_release() {
        let mut app = App::new();
        app.model = "test-model".into();
        app.update = Some("v9.9.9".into());
        banner(&mut app, &cfg(), std::path::Path::new("/tmp/x"));
        assert!(
            app.log
                .iter()
                .any(|l| l.iter().any(|(t, _)| t.contains("v9.9.9 available"))),
            "banner must offer the update"
        );
        // …and stays quiet when the running build is current
        let mut plain = App::new();
        banner(&mut plain, &cfg(), std::path::Path::new("/tmp/x"));
        assert!(plain
            .log
            .iter()
            .all(|l| l.iter().all(|(t, _)| !t.contains("available"))));
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

    #[test]
    fn scrolling_back_drops_the_tail_and_flags_the_status() {
        let mut app = App::new();
        app.model = "test-model".into();
        for i in 1..=40 {
            app.push_user(&format!("PROMPT{i}"), false);
        }
        let backend = TestBackend::new(50, 12);
        let mut term = Terminal::new(backend).unwrap();

        // follow mode pins the newest line to the bottom
        term.draw(|f| render(f, &mut app)).unwrap();
        let mut s = screen(term.backend().buffer(), 50, 12);
        assert!(s.contains("PROMPT40"), "tail missing:\n{s}");
        assert!(!s.contains("▲ scrolled"), "follow mode is not scrolled");

        // scrolled back: the first prompt is visible and the tail is gone
        app.follow = false;
        app.scroll = 0;
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
        let mut app = App::new();
        app.model = "m".into();
        app.input = "AAA\nBBB\nCCC\nDDD\nEEE\nFFF".into();
        app.cur = app.input.len();
        let backend = TestBackend::new(40, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let s = screen(term.backend().buffer(), 40, 10);

        assert!(s.contains("FFF"), "cursor line must stay visible:\n{s}");
        assert!(!s.contains("AAA"), "older input lines scroll away:\n{s}");
        assert!(
            app.input_scroll > 0,
            "input scroll not tracked: {}",
            app.input_scroll
        );
    }
    #[test]
    fn flushed_content_does_not_overscroll() {
        // the phantom stream line used to add +1 to the scroll total, pushing
        // follow-mode one line past the real end on every full chat
        let mut app = App::new();
        app.stream = "## Plan\n\n".to_owned() + &"line\n".repeat(40) + "done";
        app.flush_stream();
        let backend = TestBackend::new(64, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        // chat is 10 - 3 (input) - 1 (status) = 6 rows; follow-scroll must
        // land exactly on wrapped.len() - 6, not one further
        assert_eq!(app.scroll, app.wrapped.len().saturating_sub(6));
    }

    #[test]
    fn tiny_terminal_does_not_panic_and_shows_status() {
        let mut app = App::new();
        app.model = "m".into();
        app.push_user("a very long prompt that cannot possibly fit", false);
        app.stream = "a very long answer that also cannot fit".into();
        for (w, h) in [(20u16, 6u16), (1, 1), (200, 2)] {
            let backend = TestBackend::new(w, h);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| render(f, &mut app)).unwrap();
            let s = screen(term.backend().buffer(), w, h);
            assert!(!s.is_empty(), "{w}x{h} rendered nothing");
            assert_eq!(s.lines().count(), h as usize);
        }
    }

    #[test]
    fn failed_tools_and_queued_prompts_are_visible() {
        let mut app = App::new();
        app.running = true;
        app.fail_cnt = 2;
        app.ok_cnt = 1;
        app.push_user("next one", true);
        app.push(vec![
            ("    └ ".into(), C_GUTTER),
            ("✗ 9ms".into(), C_ERR),
            ("  boom".into(), C_DIM),
        ]);
        let backend = TestBackend::new(70, 14);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let s = screen(term.backend().buffer(), 70, 14);
        assert!(s.contains("❯ next one (queued)"), "queued echo:\n{s}");
        assert!(
            s.contains("└ ✗ 9ms") && s.contains("boom"),
            "tool error:\n{s}"
        );
        assert!(s.contains("1✓ 2✗"), "counters:\n{s}");
    }
}
