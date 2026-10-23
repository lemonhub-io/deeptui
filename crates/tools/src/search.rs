//! Finding things: regex over contents, globs over paths. Both walk the
//! project exactly as the repo map does, and both stop at a hit cap.

use crate::{Args, Exec, Output, Tool, Workspace};
use deeptui_core::Effect;
use ignore::overrides::{Override, OverrideBuilder};
use serde_json::json;
use std::path::Path;

const GREP_MAX: usize = 200;

pub const GREP: Tool = Tool {
    name: "grep",
    about: "Regex search file contents (respects .gitignore). Returns path:line:text.",
    params: || {
        json!({"type":"object","properties":{
            "pattern":{"type":"string"},
            "path":{"type":"string","description":"dir to search, default root"},
            "glob":{"type":"string","description":"e.g. **/*.rs"},
            "max":{"type":"integer","description":"max hits, default 100"}},
        "required":["pattern"]})
    },
    effect: Effect::Read,
    exec: Exec::Blocking(grep),
};

pub const GLOB: Tool = Tool {
    name: "glob",
    about: "List files matching a glob like **/*.rs (respects .gitignore).",
    params: || {
        json!({"type":"object","properties":{
            "pattern":{"type":"string"},
            "path":{"type":"string","description":"dir, default root"}},
        "required":["pattern"]})
    },
    effect: Effect::Read,
    exec: Exec::Blocking(glob),
};

fn matcher(dir: &Path, glob: &str) -> Result<Override, String> {
    let mut b = OverrideBuilder::new(dir);
    b.add(glob).map_err(|e| e.to_string())?;
    b.build().map_err(|e| e.to_string())
}

/// Files under `dir` in project-walk order.
fn files(dir: &Path) -> impl Iterator<Item = ignore::DirEntry> {
    deeptui_repomap::walk(dir)
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
}

fn grep(ws: &Workspace, args: &Args) -> Output {
    let re = regex::Regex::new(args.need("pattern")?).map_err(|e| format!("bad regex: {e}"))?;
    let dir = ws.dir(args.str("path"))?;
    let max = args.u64("max").unwrap_or(100) as usize;
    let only = match args.str("glob") {
        Some(g) if !g.is_empty() => Some(matcher(&dir, g)?),
        _ => None,
    };
    let mut out = String::new();
    let mut hits = 0usize;
    'outer: for e in files(&dir) {
        if hits >= max || out.len() > 60 * 1024 {
            break;
        }
        if let Some(ov) = &only {
            if !ov.matched(e.path(), false).is_whitelist() {
                continue;
            }
        }
        if let Ok(meta) = e.metadata() {
            if meta.len() > 2 * 1024 * 1024 {
                continue;
            }
        }
        let bytes = match std::fs::read(e.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let rel = ws.rel(e.path());
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                let t: String = line.trim().chars().take(200).collect();
                out.push_str(&format!("{rel}:{}: {t}\n", i + 1));
                hits += 1;
                if hits >= max {
                    out.push_str(&format!("… stopped at {max} hits\n"));
                    break 'outer;
                }
            }
        }
    }
    if hits == 0 {
        Err("no matches".into())
    } else {
        Ok(out)
    }
}

fn glob(ws: &Workspace, args: &Args) -> Output {
    let dir = ws.dir(args.str("path"))?;
    let ov = matcher(&dir, args.need("pattern")?)?;
    let mut out = String::new();
    let mut n = 0usize;
    for e in files(&dir) {
        if ov.matched(e.path(), false).is_whitelist() {
            out.push_str(&ws.rel(e.path()));
            out.push('\n');
            n += 1;
            if n >= GREP_MAX {
                out.push_str("…\n");
                break;
            }
        }
    }
    if n == 0 {
        Err("no matches".into())
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;
    use serde_json::Value;

    fn ws(t: &TmpDir) -> Workspace {
        Workspace::new(t.path(), 5_000)
    }

    fn a(v: Value) -> Args {
        Args::from(v)
    }

    #[test]
    fn grep_reports_hits_limits_and_globs() {
        let t = TmpDir::new("grep");
        let ws = ws(&t);
        t.write("a.rs", "needle one\nplain\nneedle two\n");
        t.write("b.txt", "needle three\n");

        let out = grep(&ws, &a(json!({"pattern": "needle"}))).unwrap();
        assert!(out.contains("a.rs:1: needle one"), "{out}");
        assert!(out.contains("a.rs:3: needle two"), "{out}");
        assert!(out.contains("b.txt:1: needle three"), "{out}");

        // glob narrows the walk, max stops it
        let out = grep(&ws, &a(json!({"pattern": "needle", "glob": "*.rs"}))).unwrap();
        assert!(out.contains("a.rs") && !out.contains("b.txt"), "{out}");
        let out = grep(&ws, &a(json!({"pattern": "needle", "max": 1}))).unwrap();
        assert!(out.contains("stopped at 1 hits"), "{out}");

        assert_eq!(
            grep(&ws, &a(json!({"pattern": "absent"}))).unwrap_err(),
            "no matches"
        );
        let err = grep(&ws, &a(json!({"pattern": "(unclosed"}))).unwrap_err();
        assert!(err.starts_with("bad regex"), "{err}");
        // an escaping search dir is refused
        let err = grep(&ws, &a(json!({"pattern": "x", "path": "../.."}))).unwrap_err();
        assert!(err.contains("escapes project root"), "{err}");
    }

    #[test]
    fn grep_can_be_scoped_and_skips_binary_files() {
        let t = TmpDir::new("grep-scope");
        let ws = ws(&t);
        t.write("src/a.rs", "needle\n");
        t.write("other/b.txt", "needle\n");
        std::fs::write(t.join("src/bin.dat"), b"needle\x00\n").unwrap();

        let out = grep(&ws, &a(json!({"pattern": "needle", "path": "src"}))).unwrap();
        assert!(out.contains("src/a.rs:1: needle"), "{out}");
        assert!(!out.contains("b.txt") && !out.contains("bin.dat"), "{out}");

        let out = grep(&ws, &a(json!({"pattern": "needle", "glob": "**/*.txt"}))).unwrap();
        assert_eq!(out, "other/b.txt:1: needle\n");
    }

    #[test]
    fn grep_stops_at_the_hit_cap() {
        let t = TmpDir::new("grep-cap");
        let ws = ws(&t);
        for i in 0..110 {
            std::fs::write(t.join(&format!("f{i:03}.txt")), "hit\n").unwrap();
        }
        // the default cap is 100 hits
        let out = grep(&ws, &a(json!({"pattern": "hit"}))).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 101, "cap not applied");
        assert_eq!(lines[100], "… stopped at 100 hits");
        assert!(lines[..100].iter().all(|l| l.ends_with(":1: hit")), "{out}");

        let out = grep(&ws, &a(json!({"pattern": "hit", "max": 3}))).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "{out}");
        assert_eq!(lines[3], "… stopped at 3 hits");

        // and a pattern with no hits is an error, not empty output
        assert_eq!(
            grep(&ws, &a(json!({"pattern": "absent"}))).unwrap_err(),
            "no matches"
        );
        // a bad regex names the problem
        let err = grep(&ws, &a(json!({"pattern": "["}))).unwrap_err();
        assert!(err.starts_with("bad regex:"), "{err}");
    }

    #[test]
    fn glob_matches_patterns_relative_to_the_dir() {
        let t = TmpDir::new("glob");
        let ws = ws(&t);
        t.write("src/a.rs", "");
        t.write("src/b.txt", "");
        let out = glob(&ws, &a(json!({"pattern": "**/*.rs"}))).unwrap();
        assert_eq!(out, "src/a.rs\n");
        let out = glob(&ws, &a(json!({"pattern": "**/*.*", "path": "src"}))).unwrap();
        assert!(
            out.contains("src/a.rs") && out.contains("src/b.txt"),
            "{out}"
        );
        assert_eq!(
            glob(&ws, &a(json!({"pattern": "**/*.go"}))).unwrap_err(),
            "no matches"
        );
        assert_eq!(glob(&ws, &a(json!({}))).unwrap_err(), "missing pattern");
    }

    #[test]
    fn glob_truncates_at_the_entry_cap() {
        let t = TmpDir::new("glob-cap");
        for i in 0..GREP_MAX + 5 {
            std::fs::write(t.join(&format!("f{i:03}.txt")), "").unwrap();
        }
        let out = glob(&ws(&t), &a(json!({"pattern": "**/*.txt"}))).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), GREP_MAX + 1);
        assert_eq!(lines[GREP_MAX], "…");
        // the walk order is unspecified, but every entry is a real file
        assert!(
            lines[..GREP_MAX]
                .iter()
                .all(|l| l.starts_with('f') && l.ends_with(".txt")),
            "{out}"
        );
    }
}
