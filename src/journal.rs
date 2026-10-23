//! The lifetime journal: one TSV row per finished turn, appended by a tap on
//! the session — so every frontend is journalled, and none of them knows.

use deeptui_core::{Event, Ledger, Projection, Receipt};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Journal {
    path: Option<PathBuf>,
    model: String,
}

/// `<data_dir>/deeptui/stats.tsv`. `DEEPTUI_STATS_FILE` redirects it — the
/// tests use that so they never append to, or read, the real one.
fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEEPTUI_STATS_FILE") {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    dirs::data_dir().map(|d| d.join("deeptui").join("stats.tsv"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Journal {
    pub fn open(model: &str) -> Self {
        Self::at(default_path(), model)
    }

    fn at(path: Option<PathBuf>, model: &str) -> Self {
        Self {
            path,
            model: model.to_string(),
        }
    }

    /// Every turn journalled so far; a missing file is an empty ledger.
    pub fn load(&self) -> Ledger {
        self.path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|c| Ledger::parse(&c))
            .unwrap_or_default()
    }

    pub fn record(&self, r: &Receipt) {
        if let Some(p) = &self.path {
            append(p, &Ledger::row(unix_now(), &self.model, r));
        }
    }
}

impl Projection for Journal {
    fn apply(&mut self, ev: &Event) {
        if let Event::Finished(r) = ev {
            self.record(r);
        }
    }
}

/// Append one row to `p`, creating the parent directory as needed. Stats are
/// best-effort: a read-only disk must never fail a turn.
fn append(p: &Path, row: &str) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let _ = writeln!(f, "{row}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_core::{Outcome, Usage};
    use deeptui_testkit::TmpDir;

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
    fn finished_turns_are_appended_and_read_back() {
        let t = TmpDir::new("stats");
        let file = t.join("nested/analytics.tsv");
        let mut j = Journal::at(Some(file.clone()), "stats-test-model");
        j.apply(&Event::Round(1));
        j.apply(&Event::Finished(receipt(Outcome::Done, 0.25)));
        j.apply(&Event::Finished(receipt(Outcome::Interrupted, 0.5)));

        // the parent directory is created on demand
        let text = std::fs::read_to_string(&file).expect("no analytics file");
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows.len(), 2, "{text}");
        assert!(rows[0].ends_with("\t0.250000\tok"), "{}", rows[0]);
        assert!(rows[1].ends_with("\t0.500000\tfail"), "{}", rows[1]);
        for row in &rows {
            let f: Vec<&str> = row.split('\t').collect();
            assert_eq!(f.len(), 7, "{row}");
            assert_eq!(f[1], "stats-test-model");
            assert_eq!((f[2], f[3], f[4]), ("100", "20", "5"));
            assert!(f[0].parse::<u64>().unwrap_or(0) > 1_700_000_000, "{row}");
        }
        let l = j.load();
        assert_eq!((l.tasks, l.ok), (2, 1));
        assert!((l.cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn a_missing_or_absent_file_is_an_empty_ledger() {
        let t = TmpDir::new("stats-missing");
        assert_eq!(Journal::at(None, "m").load(), Ledger::default());
        assert_eq!(
            Journal::at(Some(t.join("nope.tsv")), "m").load(),
            Ledger::default()
        );
        // and recording without a path is a quiet no-op
        Journal::at(None, "m").record(&receipt(Outcome::Done, 1.0));
    }

    #[test]
    fn the_stats_file_override_is_honoured() {
        let t = TmpDir::new("stats-override");
        let file = t.join("override.tsv");
        std::env::set_var("DEEPTUI_STATS_FILE", &file);
        assert_eq!(default_path().as_deref(), Some(file.as_path()));
        Journal::open("override-model").record(&receipt(Outcome::Done, 0.125));
        std::env::remove_var("DEEPTUI_STATS_FILE");
        let text = std::fs::read_to_string(&file).expect("override ignored");
        assert!(
            text.contains("\toverride-model\t100\t20\t5\t0.125000\tok"),
            "{text}"
        );
        assert_eq!(
            default_path(),
            None,
            "tests must not fall back to the real file"
        );
    }
}
