use crate::repomap;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const LINE_CAP: usize = 400; // max chars of a single source line returned
const READ_MAX: usize = 2000; // hard cap on lines per read
const READ_BYTES: usize = 96 * 1024;
const READ_FILE_MAX: usize = 4 * 1024 * 1024; // read at most the head of a file
const BASH_LINES: usize = 250;
const GREP_MAX: usize = 200;
const LS_MAX: usize = 300;

/// Tool JSON schemas sent to the API. Descriptions are deliberately terse —
/// every word here is paid for on every request.
pub fn specs() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{
            "name":"read_file",
            "description":"Read file lines, numbered. Use offset/limit for ranges. Long lines truncated.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "offset":{"type":"integer","description":"1-based start line"},
                "limit":{"type":"integer","description":"max lines, default 400"}},
            "required":["path"]}}}),
        json!({"type":"function","function":{
            "name":"edit_file",
            "description":"Exact-match search/replace edits on a file. Each `old` must match exactly once. Prefer this over rewriting files.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "edits":{"type":"array","items":{"type":"object","properties":{
                    "old":{"type":"string"},"new":{"type":"string"}},
                    "required":["old","new"]}}},
            "required":["path","edits"]}}}),
        json!({"type":"function","function":{
            "name":"write_file",
            "description":"Create or fully overwrite a file (parent dirs auto-created). For edits, use edit_file.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},"content":{"type":"string"}},
            "required":["path","content"]}}}),
        json!({"type":"function","function":{
            "name":"bash",
            "description":"Run a shell command in the project root. Output truncated. Timeout default 30s, max 300s.",
            "parameters":{"type":"object","properties":{
                "cmd":{"type":"string"},
                "timeout_ms":{"type":"integer"}},
            "required":["cmd"]}}}),
        json!({"type":"function","function":{
            "name":"grep",
            "description":"Regex search file contents (respects .gitignore). Returns path:line:text.",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string"},
                "path":{"type":"string","description":"dir to search, default root"},
                "glob":{"type":"string","description":"e.g. **/*.rs"},
                "max":{"type":"integer","description":"max hits, default 100"}},
            "required":["pattern"]}}}),
        json!({"type":"function","function":{
            "name":"glob",
            "description":"List files matching a glob like **/*.rs (respects .gitignore).",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string"},
                "path":{"type":"string","description":"dir, default root"}},
            "required":["pattern"]}}}),
        json!({"type":"function","function":{
            "name":"ls",
            "description":"List one directory level: dirs first, then files with sizes.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string","description":"default root"}}}}}),
        json!({"type":"function","function":{
            "name":"repomap",
            "description":"Condensed repo skeleton: file tree + top-level symbols. Cheap overview.",
            "parameters":{"type":"object","properties":{}}}}),
    ]
}

/// Read-only tools may run in parallel with each other.
pub fn is_parallel(name: &str) -> bool {
    matches!(name, "read_file" | "grep" | "glob" | "ls" | "repomap")
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn resolve(root: &Path, p: &str) -> Result<PathBuf, String> {
    let p = p.trim();
    if p.is_empty() {
        return Err("empty path".into());
    }
    let cand = PathBuf::from(p);
    if cand.is_absolute() {
        // Explicit absolute paths are honored — the guardrail exists for
        // relative-path mistakes, and bash is unrestricted anyway.
        return Ok(normalize(&cand));
    }
    let norm = normalize(&root.join(cand));
    if !norm.starts_with(root) {
        return Err(format!("path escapes project root: {p}"));
    }
    Ok(norm)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}

fn read_file_sync(root: &Path, args: &Value) -> Result<String, String> {
    let p = resolve(root, args["path"].as_str().unwrap_or(""))?;
    // read at most READ_FILE_MAX + 1 bytes — the +1 detects truncation
    // without a stat call; a giant file can't OOM the agent
    let mut bytes = Vec::new();
    let truncated = {
        use std::io::Read;
        let f = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", rel(root, &p)))?;
        f.take(READ_FILE_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{}: {e}", rel(root, &p)))?;
        bytes.len() > READ_FILE_MAX
    };
    bytes.truncate(READ_FILE_MAX);
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(format!("{}: binary file", rel(root, &p)));
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = (args["limit"].as_u64().unwrap_or(400) as usize).clamp(1, READ_MAX);
    let mut out = String::new();
    let mut shown = 0usize;
    // with only the head loaded, the line count is a lower bound
    let total = text.lines().count();
    for (i, line) in text.lines().enumerate().skip(offset - 1) {
        if shown >= limit || out.len() > READ_BYTES {
            out.push_str(&format!(
                "… stopped at line {} of {total} (use offset/limit)\n",
                i + 1
            ));
            break;
        }
        let mut l = line;
        let mut note = "";
        if l.chars().count() > LINE_CAP {
            l = &line[..line
                .char_indices()
                .nth(LINE_CAP)
                .map(|(i, _)| i)
                .unwrap_or(line.len())];
            note = " …";
        }
        out.push_str(&format!("{:>4}│{}{}\n", i + 1, l, note));
        shown += 1;
    }
    if truncated {
        out.push_str(&format!(
            "… file truncated: only the first {} MB shown\n",
            READ_FILE_MAX / (1024 * 1024)
        ));
    }
    if shown == 0 {
        if total == 0 {
            Ok("(empty file)".into())
        } else {
            Err(format!("offset {offset} beyond {total} lines"))
        }
    } else {
        Ok(out)
    }
}

fn edit_file_sync(root: &Path, args: &Value) -> Result<String, String> {
    let p = resolve(root, args["path"].as_str().unwrap_or(""))?;
    let mut text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", rel(root, &p)))?;
    let edits = args["edits"].as_array().ok_or("edits must be an array")?;
    if edits.is_empty() {
        return Err("no edits".into());
    }
    let (mut plus, mut minus) = (0usize, 0usize);
    for (i, e) in edits.iter().enumerate() {
        let old = e["old"]
            .as_str()
            .ok_or_else(|| format!("edit {i}: missing old"))?;
        let new = e["new"]
            .as_str()
            .ok_or_else(|| format!("edit {i}: missing new"))?;
        if old.is_empty() {
            return Err(format!("edit {i}: empty old"));
        }
        let count = text.matches(old).count();
        if count == 0 {
            return Err(format!("edit {i}: `old` not found in {}", rel(root, &p)));
        }
        if count > 1 {
            return Err(format!(
                "edit {i}: `old` matches {count} times — add more context"
            ));
        }
        minus += old.lines().count();
        plus += new.lines().count();
        text = text.replacen(old, new, 1);
    }
    std::fs::write(&p, &text).map_err(|e| format!("{}: {e}", rel(root, &p)))?;
    Ok(format!(
        "applied {} edits to {} (+{plus}/-{minus} lines)",
        edits.len(),
        rel(root, &p)
    ))
}

fn write_file_sync(root: &Path, args: &Value) -> Result<String, String> {
    let p = resolve(root, args["path"].as_str().unwrap_or(""))?;
    let content = args["content"].as_str().ok_or("missing content")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, content).map_err(|e| format!("{}: {e}", rel(root, &p)))?;
    Ok(format!("wrote {} ({} B)", rel(root, &p), content.len()))
}

fn cap_lines(s: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= head + tail {
        return s.to_string();
    }
    let mut out = lines[..head].join("\n");
    out.push_str(&format!(
        "\n… [{} lines elided] …\n",
        lines.len() - head - tail
    ));
    out.push_str(&lines[lines.len() - tail..].join("\n"));
    out
}

async fn bash_run(root: &Path, args: &Value, default_timeout_ms: u64) -> Result<String, String> {
    let cmd = args["cmd"].as_str().ok_or("missing cmd")?;
    let timeout = args["timeout_ms"]
        .as_u64()
        .unwrap_or(default_timeout_ms)
        .min(300_000);
    let fut = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .current_dir(root)
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(std::time::Duration::from_millis(timeout), fut).await {
        Err(_) => Err(format!("timeout after {}ms", timeout)),
        Ok(Err(e)) => Err(format!("spawn failed: {e}")),
        Ok(Ok(o)) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if !err.trim().is_empty() {
                if !s.is_empty() {
                    s.push_str("\n--- stderr ---\n");
                }
                s.push_str(&err);
            }
            if s.len() > 40 * 1024 {
                s.truncate(40 * 1024);
                s.push_str("\n…[bytes truncated]");
            }
            let s = cap_lines(&s, 200, BASH_LINES - 200);
            Ok(format!("{s}[exit {}]", o.status.code().unwrap_or(-1)))
        }
    }
}

fn grep_sync(root: &Path, args: &Value) -> Result<String, String> {
    let re = regex::Regex::new(args["pattern"].as_str().ok_or("missing pattern")?)
        .map_err(|e| format!("bad regex: {e}"))?;
    let dir = match args["path"].as_str() {
        Some(p) if !p.is_empty() => resolve(root, p)?,
        _ => root.to_path_buf(),
    };
    let max = args["max"].as_u64().unwrap_or(100) as usize;
    let ov = match args["glob"].as_str() {
        Some(g) if !g.is_empty() => {
            let mut b = ignore::overrides::OverrideBuilder::new(&dir);
            b.add(g).map_err(|e| e.to_string())?;
            Some(b.build().map_err(|e| e.to_string())?)
        }
        _ => None,
    };
    let mut out = String::new();
    let mut hits = 0usize;
    'outer: for e in WalkBuilder::new(&dir)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            e.depth() == 0
                || !repomap::SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .build()
        .flatten()
    {
        if hits >= max || out.len() > 60 * 1024 {
            break;
        }
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if let Some(ov) = &ov {
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
        let rel = rel(root, e.path());
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

fn glob_sync(root: &Path, args: &Value) -> Result<String, String> {
    let dir = match args["path"].as_str() {
        Some(p) if !p.is_empty() => resolve(root, p)?,
        _ => root.to_path_buf(),
    };
    let pattern = args["pattern"].as_str().ok_or("missing pattern")?;
    let mut b = ignore::overrides::OverrideBuilder::new(&dir);
    b.add(pattern).map_err(|e| e.to_string())?;
    let ov = b.build().map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut n = 0usize;
    for e in WalkBuilder::new(&dir)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            e.depth() == 0
                || !repomap::SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .build()
        .flatten()
    {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if ov.matched(e.path(), false).is_whitelist() {
            out.push_str(&rel(root, e.path()));
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

fn ls_sync(root: &Path, args: &Value) -> Result<String, String> {
    let dir = match args["path"].as_str() {
        Some(p) if !p.is_empty() => resolve(root, p)?,
        _ => root.to_path_buf(),
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", rel(root, &dir)))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push(format!("{name}/"));
        } else {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(format!("{name} ({size} B)"));
        }
    }
    dirs.sort();
    files.sort();
    let mut all = dirs;
    all.extend(files);
    let total = all.len();
    all.truncate(LS_MAX);
    let mut out = all.join("\n");
    if total > LS_MAX {
        out.push_str(&format!("\n… +{} more", total - LS_MAX));
    }
    if out.is_empty() {
        out = "(empty)".into();
    }
    Ok(out)
}

/// Execute a tool. Ok/Err both become tool-result content for the model.
pub async fn exec(
    name: &str,
    args: &Value,
    root: Arc<PathBuf>,
    bash_timeout_ms: u64,
) -> Result<String, String> {
    let rt = (*root).clone();
    match name {
        "bash" => bash_run(&rt, args, bash_timeout_ms).await,
        "read_file" | "edit_file" | "write_file" | "grep" | "glob" | "ls" | "repomap" => {
            let args = args.clone();
            let name = name.to_string();
            tokio::task::spawn_blocking(move || {
                let r = rt.as_path();
                match name.as_str() {
                    "read_file" => read_file_sync(r, &args),
                    "edit_file" => edit_file_sync(r, &args),
                    "write_file" => write_file_sync(r, &args),
                    "grep" => grep_sync(r, &args),
                    "glob" => glob_sync(r, &args),
                    "ls" => ls_sync(r, &args),
                    "repomap" => {
                        let m = repomap::build(r, 6000);
                        if m.is_empty() {
                            Err("empty project".into())
                        } else {
                            Ok(m)
                        }
                    }
                    _ => unreachable!(),
                }
            })
            .await
            .map_err(|e| e.to_string())?
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;
    use serde_json::json;

    #[test]
    fn resolve_confines_relative_paths() {
        let t = TmpDir::new("resolve");
        let root = t.path();
        assert_eq!(
            resolve(root, "src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        // dot segments are normalised away
        assert_eq!(resolve(root, "./a/../b").unwrap(), root.join("b"));
        assert!(resolve(root, "").unwrap_err().contains("empty path"));
        assert!(resolve(root, "   ").unwrap_err().contains("empty path"));
        let err = resolve(root, "../outside").unwrap_err();
        assert!(err.contains("escapes project root"), "{err}");
        // absolute paths are honoured (bash is unrestricted anyway)
        assert_eq!(
            resolve(root, "/tmp/./x").unwrap(),
            std::path::PathBuf::from("/tmp/x")
        );
    }

    #[test]
    fn write_read_round_trip_numbers_lines() {
        let t = TmpDir::new("read");
        let root = t.path();
        let msg = write_file_sync(
            root,
            &json!({"path": "sub/dir/f.txt", "content": "a\nb\nc\n"}),
        )
        .unwrap();
        assert_eq!(msg, "wrote sub/dir/f.txt (6 B)");
        assert!(t.join("sub/dir/f.txt").is_file());

        let out = read_file_sync(root, &json!({"path": "sub/dir/f.txt"})).unwrap();
        assert_eq!(out, "   1│a\n   2│b\n   3│c\n");
        // offset is 1-based and a limit that stops early says so
        let out = read_file_sync(
            root,
            &json!({"path": "sub/dir/f.txt", "offset": 2, "limit": 1}),
        )
        .unwrap();
        assert_eq!(out, "   2│b\n… stopped at line 3 of 3 (use offset/limit)\n");
        let out = read_file_sync(
            root,
            &json!({"path": "sub/dir/f.txt", "offset": 2, "limit": 9}),
        )
        .unwrap();
        assert_eq!(out, "   2│b\n   3│c\n");
        // an empty file reads as such
        t.write("empty.txt", "");
        assert_eq!(
            read_file_sync(root, &json!({"path": "empty.txt"})).unwrap(),
            "(empty file)"
        );
    }

    #[test]
    fn read_file_guards_bounds_binaries_and_long_lines() {
        let t = TmpDir::new("read-guards");
        let root = t.path();
        let err = read_file_sync(root, &json!({"path": "missing.txt"})).unwrap_err();
        assert!(err.contains("missing.txt"), "{err}");

        t.write("f.txt", "a\nb\nc\n");
        assert!(read_file_sync(root, &json!({"path": "f.txt", "offset": 9}))
            .unwrap_err()
            .contains("beyond 3 lines"));

        std::fs::write(t.join("bin.dat"), b"ab\x00cd").unwrap();
        let err = read_file_sync(root, &json!({"path": "bin.dat"})).unwrap_err();
        assert!(err.contains("binary file"), "{err}");

        // a 500-char line is clipped at LINE_CAP with a marker
        t.write("long.txt", &format!("{}\n", "x".repeat(500)));
        let out = read_file_sync(root, &json!({"path": "long.txt"})).unwrap();
        assert!(out.contains('…'), "{out}");
        let body = out
            .lines()
            .next()
            .unwrap()
            .trim_start_matches(|c: char| !c.is_alphabetic());
        assert_eq!(body.trim_end_matches(" …").len(), 400);
    }

    #[test]
    fn edit_file_applies_exact_matches_only() {
        let t = TmpDir::new("edit");
        let root = t.path();
        let p = t.write("f.txt", "hello world\nsecond line\n");

        let msg = edit_file_sync(
            root,
            &json!({"path": "f.txt", "edits": [{"old": "hello world", "new": "hello there"}]}),
        )
        .unwrap();
        assert_eq!(msg, "applied 1 edits to f.txt (+1/-1 lines)");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "hello there\nsecond line\n"
        );

        // shrinking a two-line region reports +1/-2
        let msg = edit_file_sync(
            root,
            &json!({"path": "f.txt", "edits": [{"old": "hello there\nsecond", "new": "hi"}]}),
        )
        .unwrap();
        assert!(msg.ends_with("(+1/-2 lines)"), "{msg}");
    }

    #[test]
    fn edit_file_rejects_ambiguous_missing_and_empty_edits() {
        let t = TmpDir::new("edit-err");
        let root = t.path();
        t.write("f.txt", "aa aa\n");
        let err = edit_file_sync(
            root,
            &json!({"path": "f.txt", "edits": [{"old": "aa", "new": "bb"}]}),
        )
        .unwrap_err();
        assert!(err.contains("matches 2 times"), "{err}");
        let err = edit_file_sync(
            root,
            &json!({"path": "f.txt", "edits": [{"old": "zz", "new": "bb"}]}),
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
        let err = edit_file_sync(root, &json!({"path": "f.txt", "edits": []})).unwrap_err();
        assert_eq!(err, "no edits");
        // nothing was written by the failed attempts
        assert_eq!(std::fs::read_to_string(t.join("f.txt")).unwrap(), "aa aa\n");
    }

    #[test]
    fn cap_lines_keeps_head_and_tail_with_elision() {
        // 300 lines: line1..line300, well above head+tail = 250.
        let input: String = (1..=300)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = cap_lines(&input, 200, 50);

        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() < 300, "output should be truncated");
        // Head preserved.
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[199], "line200");
        // Elision marker between head and tail, with correct count.
        assert!(
            lines.iter().any(|l| l.contains("50 lines elided")),
            "expected elision marker, got: {lines:?}"
        );
        // Tail preserved.
        assert_eq!(lines[lines.len() - 1], "line300");
        assert_eq!(lines[lines.len() - 50], "line251");
    }

    #[test]
    fn cap_lines_is_a_noop_under_the_cap() {
        let s = "a\nb\nc";
        assert_eq!(cap_lines(s, 10, 10), s);
        // exactly head+tail lines still fits
        assert_eq!(cap_lines("1\n2\n3\n4", 2, 2), "1\n2\n3\n4");
        let out = cap_lines("1\n2\n3\n4\n5", 2, 2);
        assert_eq!(out, "1\n2\n… [1 lines elided] …\n4\n5");
    }

    #[test]
    fn ls_lists_dirs_first_then_files() {
        let t = TmpDir::new("ls");
        let root = t.path();
        assert_eq!(ls_sync(root, &json!({})).unwrap(), "(empty)");

        t.write("zeta.txt", "abc");
        t.write("alpha/file", "");
        t.write("beta/file", "");
        let out = ls_sync(root, &json!({"path": "."})).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, ["alpha/", "beta/", "zeta.txt (3 B)"]);
    }

    #[test]
    fn grep_reports_hits_limits_and_globs() {
        let t = TmpDir::new("grep");
        let root = t.path();
        t.write("a.rs", "needle one\nplain\nneedle two\n");
        t.write("b.txt", "needle three\n");

        let out = grep_sync(root, &json!({"pattern": "needle"})).unwrap();
        assert!(out.contains("a.rs:1: needle one"), "{out}");
        assert!(out.contains("a.rs:3: needle two"), "{out}");
        assert!(out.contains("b.txt:1: needle three"), "{out}");

        // glob narrows the walk, max stops it
        let out = grep_sync(root, &json!({"pattern": "needle", "glob": "*.rs"})).unwrap();
        assert!(out.contains("a.rs") && !out.contains("b.txt"), "{out}");
        let out = grep_sync(root, &json!({"pattern": "needle", "max": 1})).unwrap();
        assert!(out.contains("stopped at 1 hits"), "{out}");

        assert_eq!(
            grep_sync(root, &json!({"pattern": "absent"})).unwrap_err(),
            "no matches"
        );
        let err = grep_sync(root, &json!({"pattern": "(unclosed"})).unwrap_err();
        assert!(err.starts_with("bad regex"), "{err}");
        // an escaping search dir is refused
        let err = grep_sync(root, &json!({"pattern": "x", "path": "../.."})).unwrap_err();
        assert!(err.contains("escapes project root"), "{err}");
    }

    #[test]
    fn glob_matches_patterns_relative_to_the_dir() {
        let t = TmpDir::new("glob");
        let root = t.path();
        t.write("src/a.rs", "");
        t.write("src/b.txt", "");
        let out = glob_sync(root, &json!({"pattern": "**/*.rs"})).unwrap();
        assert_eq!(out, "src/a.rs\n");
        let out = glob_sync(root, &json!({"pattern": "**/*.*", "path": "src"})).unwrap();
        assert!(
            out.contains("src/a.rs") && out.contains("src/b.txt"),
            "{out}"
        );
        assert_eq!(
            glob_sync(root, &json!({"pattern": "**/*.go"})).unwrap_err(),
            "no matches"
        );
        assert_eq!(glob_sync(root, &json!({})).unwrap_err(), "missing pattern");
    }

    #[test]
    fn specs_and_parallelism_cover_every_tool() {
        let specs = specs();
        let names: Vec<&str> = specs
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "read_file",
                "edit_file",
                "write_file",
                "bash",
                "grep",
                "glob",
                "ls",
                "repomap"
            ]
        );
        for s in &specs {
            assert_eq!(s["type"], "function");
            assert_eq!(s["function"]["parameters"]["type"], "object");
            let req = s["function"]["parameters"]["required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for name in req {
                let p = name.as_str().unwrap();
                assert!(
                    s["function"]["parameters"]["properties"].get(p).is_some(),
                    "required {p} has no schema"
                );
            }
        }
        // mutating tools never run in a parallel batch
        assert!(is_parallel("read_file") && is_parallel("grep") && is_parallel("repomap"));
        for t in ["bash", "edit_file", "write_file", "nope"] {
            assert!(!is_parallel(t), "{t}");
        }
    }
}
