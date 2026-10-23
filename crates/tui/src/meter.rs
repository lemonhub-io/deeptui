//! The live gauges behind the status bar: run state, token and dollar
//! counters, and the session tally — a projection of the event stream.

use deeptui_core::{Event, Ledger, Projection, Usage};
use std::time::Instant;

pub(crate) struct Meter {
    pub running: bool,
    pub round: usize,
    pub started: Option<Instant>,
    /// Everything this session has used, live.
    pub session: Usage,
    /// The current turn's usage — or the last one's, once it is over.
    pub task: Usage,
    /// Finished turns of this session.
    pub tally: Ledger,
}

impl Meter {
    pub fn new() -> Self {
        Self {
            running: false,
            round: 0,
            started: None,
            session: Usage::default(),
            task: Usage::default(),
            tally: Ledger::default(),
        }
    }

    pub fn elapsed(&self) -> f32 {
        self.started
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0)
    }

    /// Session spend per successful turn so far.
    pub fn per_ok(&self) -> f64 {
        if self.tally.ok > 0 {
            self.session.cost / self.tally.ok as f64
        } else {
            0.0
        }
    }

    /// The session behind the stream is gone: nothing is running any more.
    pub fn halt(&mut self) {
        self.running = false;
        self.started = None;
    }
}

impl Projection for Meter {
    fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Round(n) => {
                self.round = *n;
                if *n == 1 {
                    self.running = true;
                    self.started = Some(Instant::now());
                    self.task = Usage::default();
                }
            }
            Event::Usage(u) => {
                self.session += *u;
                self.task += *u;
            }
            Event::Finished(r) => {
                self.halt();
                self.tally.record(r);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_core::{Outcome, Receipt};

    fn usage(input: u64, cost: f64) -> Usage {
        Usage {
            input,
            output: 1,
            cached: 0,
            cost,
        }
    }

    #[test]
    fn the_first_round_opens_a_task_and_later_rounds_continue_it() {
        let mut m = Meter::new();
        m.task = usage(9, 9.9);
        m.apply(&Event::Round(2));
        assert_eq!((m.round, m.task.cost, m.running), (2, 9.9, false));
        m.apply(&Event::Round(1));
        assert!(m.running && m.started.is_some());
        assert_eq!((m.round, m.task), (1, Usage::default()));
    }

    #[test]
    fn usage_feeds_both_the_task_and_the_session() {
        let mut m = Meter::new();
        m.apply(&Event::Round(1));
        m.apply(&Event::Usage(usage(1_000, 0.5)));
        m.apply(&Event::Usage(usage(500, 0.25)));
        assert_eq!((m.task.input, m.task.cost), (1_500, 0.75));
        m.apply(&Event::Round(1));
        m.apply(&Event::Usage(usage(10, 0.25)));
        assert_eq!((m.task.input, m.task.cost), (10, 0.25));
        assert_eq!((m.session.input, m.session.cost), (1_510, 1.0));
    }

    #[test]
    fn finished_turns_stop_the_clock_and_are_tallied() {
        let mut m = Meter::new();
        m.apply(&Event::Round(1));
        m.apply(&Event::Usage(usage(1, 1.0)));
        for outcome in [Outcome::Done, Outcome::Interrupted] {
            m.apply(&Event::Finished(Receipt {
                outcome,
                usage: Usage::default(),
                rounds: 1,
            }));
        }
        assert!(!m.running && m.started.is_none());
        assert_eq!((m.tally.tasks, m.tally.ok), (2, 1));
        assert_eq!(m.per_ok(), 1.0);
    }
}
