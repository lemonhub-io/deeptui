use std::io::Write;
use std::path::PathBuf;

/// One completed agent task (one prompt → one run loop).
/// Persisted as TSV lines so users can grep/awk their own analytics.
#[derive(Default)]
pub struct Stats {
    pub tasks: u64,
    pub ok: u64,
    pub cost: f64,
}

impl Stats {
    /// Total spend divided by successful tasks — the metric that counts.
    pub fn avg_ok(&self) -> Option<f64> {
        if self.ok == 0 {
            None
        } else {
            Some(self.cost / self.ok as f64)
        }
    }
}

fn path() -> Option<PathBuf> {
    // `RAGENT_STATS_FILE` redirects the analytics file (used by the tests so
    // they never append to, or read, the real one).
    if let Some(p) = std::env::var_os("RAGENT_STATS_FILE") {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    dirs::data_dir().map(|d| d.join("ragent").join("stats.tsv"))
}

/// Fold TSV lines into totals. Rows with fewer than 7 fields are skipped; a
/// cost that fails to parse still counts as a task but contributes no spend.
fn parse(content: &str) -> Stats {
    let mut s = Stats::default();
    for l in content.lines() {
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

pub fn load() -> Stats {
    read(path())
}

/// Fold an optional analytics file into totals; a missing file is empty stats.
fn read(p: Option<PathBuf>) -> Stats {
    p.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse(&c))
        .unwrap_or_default()
}

pub fn record(model: &str, input: u64, output: u64, cached: u64, cost: f64, ok: bool) {
    if let Some(p) = path() {
        append(&p, model, input, output, cached, cost, ok);
    }
}

/// Append one TSV row to `p`, creating the parent directory as needed.
fn append(
    p: &std::path::Path,
    model: &str,
    input: u64,
    output: u64,
    cached: u64,
    cost: f64,
    ok: bool,
) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(
            f,
            "{ts}\t{model}\t{input}\t{output}\t{cached}\t{cost:.6}\t{}",
            if ok { "ok" } else { "fail" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avg_ok_is_none_until_a_task_succeeds() {
        assert_eq!(Stats::default().avg_ok(), None);
        let s = Stats {
            tasks: 3,
            ok: 0,
            cost: 1.5,
        };
        assert_eq!(s.avg_ok(), None);
    }

    #[test]
    fn avg_ok_spends_over_successes_only() {
        let s = Stats {
            tasks: 4,
            ok: 2,
            cost: 1.0,
        };
        assert_eq!(s.avg_ok(), Some(0.5));
        let s = Stats {
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
        let s = parse(content);
        assert_eq!(s.tasks, 4);
        assert_eq!(s.ok, 2);
        assert!((s.cost - 0.875).abs() < 1e-12, "{}", s.cost);
    }

    #[test]
    fn parse_of_empty_or_junk_is_default() {
        let s = parse("");
        assert_eq!((s.tasks, s.ok), (0, 0));
        assert_eq!(s.cost, 0.0);
        assert_eq!(parse("\n\n").tasks, 0);
        // a row that only has 'ok' in the last field but too few columns
        assert_eq!(parse("a\tb\tc\tok").tasks, 0);
    }

    #[test]
    fn recorded_lines_round_trip_through_parse() {
        // the exact shape `record` writes (tab separated, 6-decimal cost)
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{:.6}\t{}",
            1700000000u64, "test-model", 100u64, 20u64, 5u64, 0.123456, "ok"
        );
        assert_eq!(line.split('\t').count(), 7);
        let s = parse(&line);
        assert_eq!((s.tasks, s.ok), (1, 1));
        assert!((s.cost - 0.123456).abs() < 1e-12);
        // record's cost formatting keeps 6 decimals
        assert!(line.contains("0.123456\tok"));
    }

    #[test]
    fn record_writes_a_row_that_load_reads_back() {
        let t = crate::testutil::TmpDir::new("stats");
        let file = t.join("nested/analytics.tsv");
        append(&file, "stats-test-model", 100, 20, 5, 0.25, true);
        append(&file, "stats-test-model", 100, 20, 5, 0.5, false);

        // the parent directory is created on demand
        let text = std::fs::read_to_string(&file).expect("no analytics file");
        let mine: Vec<&str> = text.lines().collect();
        assert_eq!(mine.len(), 2, "{text}");
        assert!(mine[0].ends_with("\t0.250000\tok"), "{}", mine[0]);
        assert!(mine[1].ends_with("\t0.500000\tfail"), "{}", mine[1]);
        for row in &mine {
            let f: Vec<&str> = row.split('\t').collect();
            assert_eq!(f.len(), 7, "{row}");
            assert_eq!(f[1], "stats-test-model");
            assert_eq!((f[2], f[3], f[4]), ("100", "20", "5"));
            assert!(f[0].parse::<u64>().unwrap_or(0) > 1_700_000_000, "{row}");
        }
        let parsed = read(Some(file));
        assert_eq!((parsed.tasks, parsed.ok), (2, 1));
        assert!((parsed.cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn read_handles_a_missing_or_absent_file() {
        let t = crate::testutil::TmpDir::new("stats-missing");
        assert_eq!(read(None).tasks, 0);
        let s = read(Some(t.join("nope.tsv")));
        assert_eq!((s.tasks, s.ok), (0, 0));
        assert_eq!(s.cost, 0.0);
    }

    #[test]
    fn record_honours_the_stats_file_override() {
        let t = crate::testutil::TmpDir::new("stats-override");
        let file = t.join("override.tsv");
        std::env::set_var("RAGENT_STATS_FILE", &file);
        assert_eq!(path().as_deref(), Some(file.as_path()));
        record("override-model", 7, 3, 0, 0.125, true);
        std::env::remove_var("RAGENT_STATS_FILE");
        let text = std::fs::read_to_string(&file).expect("override ignored");
        assert!(
            text.contains("\toverride-model\t7\t3\t0\t0.125000\tok"),
            "{text}"
        );
        assert_eq!(path(), None, "tests must not fall back to the real file");
    }

    #[test]
    fn load_is_empty_without_an_override() {
        // the pure reader is what a missing file falls back to
        let s = read(None);
        assert_eq!((s.tasks, s.ok), (0, 0));
        assert_eq!(s.cost, 0.0);
        assert_eq!(s.avg_ok(), None);

        // ...and with no override there is no file at all under `cargo test`
        let p = path();
        if std::env::var_os("RAGENT_STATS_FILE").is_none() {
            assert_eq!(p, None, "tests must not touch the real analytics file");
        }
    }
}
