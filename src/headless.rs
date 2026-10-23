//! Headless mode: one prompt through the same session the TUI drives.
//! Assistant text goes to stdout, the play-by-play to stderr, and the
//! receipt closes the run.

use deeptui_core::text::fmt_tokens;
use deeptui_core::{Event, Ledger, Outcome, Projection, Receipt};
use deeptui_engine::Remote;
use std::io::Write;
use std::time::Instant;
use tokio::sync::mpsc;

/// Events rendered as terminal lines.
struct Console<O: Write, E: Write> {
    out: O,
    err: E,
}

impl<O: Write, E: Write> Projection for Console<O, E> {
    fn apply(&mut self, ev: &Event) {
        let e = &mut self.err;
        let _ = match ev {
            Event::Text(t) => self
                .out
                .write_all(t.as_bytes())
                .and_then(|_| self.out.flush()),
            Event::ToolStart { name, args } => writeln!(e, "\x1b[2m▸ {name} {args}\x1b[0m"),
            Event::ToolEnd {
                name,
                ms,
                preview,
                is_err,
            } => {
                let m = if *is_err { "✗" } else { "✓" };
                writeln!(e, "\x1b[2m  └ {m} {ms}ms {name} · {preview}\x1b[0m")
            }
            Event::Compacted(n) => writeln!(e, "\x1b[2m⛁ {n}\x1b[0m"),
            Event::Finished(r) => match &r.outcome {
                Outcome::Done => Ok(()),
                Outcome::Interrupted => writeln!(e, "\x1b[2m— interrupted\x1b[0m"),
                Outcome::Failed(why) => writeln!(e, "\x1b[31m✗ {why}\x1b[0m"),
            },
            Event::Round(_) | Event::Usage(_) => Ok(()),
        };
    }
}

/// The closing line: this task's bill, then the lifetime cost per success.
fn summary(r: &Receipt, secs: f32, life: &Ledger) -> String {
    let u = &r.usage;
    format!(
        "\x1b[2m[task {}] ${:.4} · ↑{} ↓{} ⛁{} · {:.1}s · life ${:.4}/✓ ({}✓ {}✗, ${:.2} tot)\x1b[0m",
        if r.outcome.ok() { "ok" } else { "FAIL" },
        u.cost,
        fmt_tokens(u.input),
        fmt_tokens(u.output),
        fmt_tokens(u.cached),
        secs,
        life.avg_ok().unwrap_or(0.0),
        life.ok,
        life.failed(),
        life.cost,
    )
}

/// Run `prompt` to its receipt and return the process exit code. Ctrl-C
/// interrupts the turn instead of killing the process mid-write.
pub async fn run(
    remote: Remote,
    mut events: mpsc::Receiver<Event>,
    prompt: String,
    mut life: Ledger,
) -> i32 {
    let t0 = Instant::now();
    remote.ask(prompt);
    let r = remote.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        r.cancel();
    });

    let mut console = Console {
        out: std::io::stdout(),
        err: std::io::stderr(),
    };
    let receipt = loop {
        match events.recv().await {
            Some(Event::Finished(r)) => {
                console.apply(&Event::Finished(r.clone()));
                break r;
            }
            Some(ev) => console.apply(&ev),
            None => {
                eprintln!("\x1b[31m✗ the session stopped before the task finished\x1b[0m");
                return 1;
            }
        }
    };
    println!();
    life.record(&receipt);
    eprintln!("{}", summary(&receipt, t0.elapsed().as_secs_f32(), &life));
    i32::from(matches!(receipt.outcome, Outcome::Failed(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_core::Usage;

    #[test]
    fn text_goes_to_stdout_and_everything_else_to_stderr() {
        let mut c = Console {
            out: Vec::new(),
            err: Vec::new(),
        };
        for ev in [
            Event::Round(1),
            Event::Text("hel".into()),
            Event::ToolStart {
                name: "ls".into(),
                args: "{}".into(),
            },
            Event::ToolEnd {
                name: "ls".into(),
                ms: 3,
                preview: "a.txt".into(),
                is_err: true,
            },
            Event::Text("lo".into()),
            Event::Finished(Receipt {
                outcome: Outcome::Failed("boom".into()),
                usage: Usage::default(),
                rounds: 1,
            }),
        ] {
            c.apply(&ev);
        }
        assert_eq!(String::from_utf8(c.out).unwrap(), "hello");
        let err = String::from_utf8(c.err).unwrap();
        assert!(err.contains("▸ ls {}"), "{err}");
        assert!(err.contains("└ ✗ 3ms ls · a.txt"), "{err}");
        assert!(err.contains("✗ boom"), "{err}");
    }

    #[test]
    fn the_summary_reports_the_task_and_the_lifetime() {
        let r = Receipt {
            outcome: Outcome::Done,
            usage: Usage {
                input: 12_345,
                output: 678,
                cached: 9_000,
                cost: 0.0123,
            },
            rounds: 2,
        };
        let life = Ledger {
            tasks: 4,
            ok: 3,
            cost: 0.3,
        };
        let s = summary(&r, 1.5, &life);
        assert!(
            s.contains("[task ok] $0.0123 · ↑12k ↓678 ⛁9.0k · 1.5s"),
            "{s}"
        );
        assert!(s.contains("life $0.1000/✓ (3✓ 1✗, $0.30 tot)"), "{s}");
    }
}
