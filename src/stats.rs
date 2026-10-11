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
    dirs::data_dir().map(|d| d.join("ragent").join("stats.tsv"))
}

pub fn load() -> Stats {
    let mut s = Stats::default();
    if let Some(p) = path() {
        if let Ok(c) = std::fs::read_to_string(p) {
            for l in c.lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if f.len() >= 7 {
                    s.tasks += 1;
                    if f[6] == "ok" {
                        s.ok += 1;
                    }
                    s.cost += f[5].parse::<f64>().unwrap_or(0.0);
                }
            }
        }
    }
    s
}

pub fn record(model: &str, input: u64, output: u64, cached: u64, cost: f64, ok: bool) {
    let Some(p) = path() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
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
