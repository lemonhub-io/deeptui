//! The event stream: the single source of truth about what happened.
//!
//! The engine emits it; everything else — the TUI scrollback, the headless
//! console, the stats journal, a session's ledger — is a [`Projection`] of it.
//! No view keeps facts of its own that the stream does not carry.

use crate::msg::Msg;
use std::ops::AddAssign;

/// Tokens one completion consumed, priced where they were measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    /// The subset of `input` served from the provider's prompt cache.
    pub cached: u64,
    /// Dollars, set by whoever knows the tariff (0 straight off the wire).
    pub cost: f64,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, o: Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cached += o.cached;
        self.cost += o.cost;
    }
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The model answered and stopped on its own.
    Done,
    /// The user cancelled it.
    Interrupted,
    /// The transport, the provider or a limit stopped it.
    Failed(String),
}

impl Outcome {
    /// Only a finished answer counts as a success for cost-per-task.
    pub fn ok(&self) -> bool {
        matches!(self, Outcome::Done)
    }
}

/// The closing record of one turn. Every turn emits exactly one.
#[derive(Clone, Debug, PartialEq)]
pub struct Receipt {
    pub outcome: Outcome,
    /// The whole turn's usage, summed over its rounds.
    pub usage: Usage,
    pub rounds: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A new model round; `1` opens a new turn.
    Round(usize),
    /// Streamed assistant text.
    Text(String),
    /// A tool call is about to run; `args` is a one-line preview.
    ToolStart {
        name: String,
        args: String,
    },
    /// A tool call finished; ends arrive in call order.
    ToolEnd {
        name: String,
        ms: u64,
        preview: String,
        is_err: bool,
    },
    Compacted(String),
    Usage(Usage),
    /// The last event of every turn.
    Finished(Receipt),
    /// The session itself speaking — a reply to a command, not the model.
    Notice(String),
    /// The conversation was swapped (cleared, rewound, resumed): this is all
    /// it holds now, system prompt aside.
    Reset(Vec<Msg>),
    /// The session needs the user's answer — a frontend that can ask, asks.
    Ask(Ask),
    /// Later turns go to another model.
    Model {
        name: String,
        /// Its context budget: history past it is compacted.
        budget: usize,
        /// False when its price is unknown and the meter would read $0.
        priced: bool,
    },
}

/// A question the session puts to the user. Answers are command lines, so a
/// frontend runs them like anything typed.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// Pick one of `options`, starting at `selected`.
    Choose {
        title: String,
        options: Vec<Choice>,
        selected: usize,
    },
    /// Type a secret, masked and never echoed or kept, then run
    /// `command <secret>`.
    Secret { title: String, command: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    pub detail: String,
    /// What picking it runs.
    pub command: String,
}

/// A view built by folding the event stream.
pub trait Projection {
    fn apply(&mut self, ev: &Event);
}

impl<F: FnMut(&Event)> Projection for F {
    fn apply(&mut self, ev: &Event) {
        self(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_sums_field_by_field() {
        let mut u = Usage {
            input: 1,
            output: 2,
            cached: 3,
            cost: 0.5,
        };
        u += u;
        assert_eq!(
            u,
            Usage {
                input: 2,
                output: 4,
                cached: 6,
                cost: 1.0
            }
        );
    }

    #[test]
    fn only_a_finished_answer_is_ok() {
        assert!(Outcome::Done.ok());
        assert!(!Outcome::Interrupted.ok());
        assert!(!Outcome::Failed("x".into()).ok());
    }

    #[test]
    fn closures_are_projections() {
        let mut n = 0;
        let mut count = |_: &Event| n += 1;
        count.apply(&Event::Round(1));
        count.apply(&Event::Text("x".into()));
        assert_eq!(n, 2);
    }
}
