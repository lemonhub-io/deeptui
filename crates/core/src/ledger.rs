//! Cost per successful task — the metric that counts — as a fold over
//! receipts. The same fold serves a live session and a lifetime journal; the
//! journal is just that fold, persisted as TSV so users can grep/awk it.

use crate::event::{Event, Projection, Receipt};

/// Spend and success counts over many turns.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ledger {
    pub tasks: u64,
    pub ok: u64,
    pub cost: f64,
}

impl Ledger {
    /// Total spend divided by successful tasks.
    pub fn avg_ok(&self) -> Option<f64> {
        if self.ok == 0 {
            None
        } else {
            Some(self.cost / self.ok as f64)
        }
    }

    pub fn failed(&self) -> u64 {
        self.tasks - self.ok
    }

    pub fn record(&mut self, r: &Receipt) {
        self.tasks += 1;
        self.ok += u64::from(r.outcome.ok());
        self.cost += r.usage.cost;
    }

    /// One journal row: `ts model in out cached cost ok|fail`, tab separated.
    pub fn row(ts: u64, model: &str, r: &Receipt) -> String {
        let u = &r.usage;
        format!(
            "{ts}\t{model}\t{}\t{}\t{}\t{:.6}\t{}",
            u.input,
            u.output,
            u.cached,
            u.cost,
            if r.outcome.ok() { "ok" } else { "fail" }
        )
    }

    /// Fold journal rows into totals. Rows with fewer than 7 fields are
    /// skipped; a cost that fails to parse still counts as a task but
    /// contributes no spend.
    pub fn parse(tsv: &str) -> Ledger {
        let mut s = Ledger::default();
        for l in tsv.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() >= 7 {
                s.tasks += 1;
                if f[6] == "ok" {
                    s.ok += 1;
                }
                s.cost += f[5].parse::<f64>().unwrap_or(0.0);
            }
        }
        s
    }
}

impl Projection for Ledger {
    fn apply(&mut self, ev: &Event) {
        if let Event::Finished(r) = ev {
            self.record(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Outcome, Usage};

    fn receipt(outcome: Outcome, cost: f64) -> Receipt {
        Receipt {
            outcome,
            usage: Usage {
                input: 100,
                output: 20,
                cached: 5,
                cost,
            },
            rounds: 1,
        }
    }

    #[test]
    fn avg_ok_is_none_until_a_task_succeeds() {
        assert_eq!(Ledger::default().avg_ok(), None);
        let s = Ledger {
            tasks: 3,
            ok: 0,
            cost: 1.5,
        };
        assert_eq!(s.avg_ok(), None);
    }

    #[test]
    fn avg_ok_spends_over_successes_only() {
        let s = Ledger {
            tasks: 4,
            ok: 2,
            cost: 1.0,
        };
        assert_eq!(s.avg_ok(), Some(0.5));
        assert_eq!(s.failed(), 2);
        let s = Ledger {
            tasks: 1,
            ok: 1,
            cost: 0.0,
        };
        assert_eq!(s.avg_ok(), Some(0.0));
    }

    #[test]
    fn parse_counts_only_well_formed_rows() {
        let content = "\
1700000000\tgpt-4o-mini\t100\t10\t0\t0.500000\tok\n\
1700000001\tgpt-4o-mini\t100\t10\t0\t0.250000\tfail\n\
1700000002\tgpt-4o-mini\t100\t10\t0\t\tbad-cost\n\
1800000003\tdeepseek-chat\t1\t1\t1\t0.125000\tok\n\
too\tfew\tfields\n";
        let s = Ledger::parse(content);
        assert_eq!(s.tasks, 4);
        assert_eq!(s.ok, 2);
        assert!((s.cost - 0.875).abs() < 1e-12, "{}", s.cost);
    }

    #[test]
    fn parse_of_empty_or_junk_is_default() {
        let s = Ledger::parse("");
        assert_eq!((s.tasks, s.ok), (0, 0));
        assert_eq!(s.cost, 0.0);
        assert_eq!(Ledger::parse("\n\n").tasks, 0);
        // a row that only has 'ok' in the last field but too few columns
        assert_eq!(Ledger::parse("a\tb\tc\tok").tasks, 0);
    }

    #[test]
    fn rows_round_trip_through_parse() {
        let line = Ledger::row(
            1_700_000_000,
            "test-model",
            &receipt(Outcome::Done, 0.123456),
        );
        assert_eq!(line, "1700000000\ttest-model\t100\t20\t5\t0.123456\tok");
        let s = Ledger::parse(&line);
        assert_eq!((s.tasks, s.ok), (1, 1));
        assert!((s.cost - 0.123456).abs() < 1e-12);
        // anything but a finished answer is journalled as a failure
        let fail = Ledger::row(1, "m", &receipt(Outcome::Interrupted, 0.5));
        assert!(fail.ends_with("\t0.500000\tfail"), "{fail}");
    }

    #[test]
    fn the_fold_counts_finished_turns_only() {
        let mut l = Ledger::default();
        l.apply(&Event::Round(1));
        l.apply(&Event::Finished(receipt(Outcome::Done, 0.25)));
        l.apply(&Event::Finished(receipt(Outcome::Failed("x".into()), 0.5)));
        assert_eq!((l.tasks, l.ok, l.failed()), (2, 1, 1));
        assert!((l.cost - 0.75).abs() < 1e-12);
        assert_eq!(l.avg_ok(), Some(0.75));
    }
}
