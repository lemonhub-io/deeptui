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
            "description":"Condensed repo skeleton: file tree + top-level symbols, ranked by importance. Cheap overview.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string","description":"subtree to map, default root"},
                "budget":{"type":"integer","description":"bytes of output, default 6000, max 60000"}}}}}),
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

/// Repo skeleton for the whole project or one subtree. The budget is clamped,
/// so a runaway request can never flood the context.
fn repomap_sync(root: &Path, args: &Value) -> Result<String, String> {
    let scoped = args["path"]
        .as_str()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let dir = match scoped {
        Some(p) => resolve(root, p)?,
        None => root.to_path_buf(),
    };
    if !dir.is_dir() {
        return Err(format!("{}: not a directory", rel(root, &dir)));
    }
    let budget = match args["budget"].as_u64() {
        Some(b) => (b as usize).clamp(repomap::MIN_BUDGET, repomap::MAX_BUDGET),
        None => 6_000,
    };
    let map = repomap::build(&dir, budget);
    if map.is_empty() {
        return Err(match scoped {
            Some(p) => format!("{p}: no files"),
            None => "empty project".into(),
        });
    }
    Ok(map)
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
                    "repomap" => repomap_sync(r, &args),
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

    #[tokio::test]
    async fn bash_runs_in_the_project_root_and_reports_the_exit_code() {
        let t = TmpDir::new("bash");
        let root = t.path();
        let out = bash_run(root, &json!({"cmd": "pwd; echo hi"}), 5_000)
            .await
            .unwrap();
        assert!(out.contains(&root.display().to_string()), "{out}");
        assert!(out.starts_with("hi") || out.contains("\nhi"), "{out}");
        assert!(out.ends_with("[exit 0]"), "{out}");

        // a failing command still returns its output, with the real status
        let out = bash_run(root, &json!({"cmd": "echo boom >&2; exit 7"}), 5_000)
            .await
            .unwrap();
        assert!(out.contains("boom"), "{out}");
        assert!(out.ends_with("[exit 7]"), "{out}");

        assert_eq!(
            bash_run(root, &json!({}), 5_000).await.unwrap_err(),
            "missing cmd"
        );
    }

    #[tokio::test]
    async fn bash_separates_stderr_and_truncates_runaway_output() {
        let t = TmpDir::new("bash-out");
        let root = t.path();
        let out = bash_run(root, &json!({"cmd": "echo out; echo err >&2"}), 5_000)
            .await
            .unwrap();
        // stdout keeps its newline, then the stderr block follows
        assert_eq!(out, "out\n\n--- stderr ---\nerr\n[exit 0]");

        // a command that prints 6000 lines is byte- and line-capped
        let out = bash_run(
            root,
            &json!({"cmd": "for i in $(seq 1 6000); do echo line$i; done"}),
            30_000,
        )
        .await
        .unwrap();
        assert!(out.contains("[bytes truncated]"), "no byte cap");
        assert!(out.contains("lines elided"), "no line cap");
        assert!(out.ends_with("[exit 0]"), "the status must survive the cap");
    }

    #[tokio::test]
    async fn bash_times_out_and_kills_the_command() {
        let t = TmpDir::new("bash-timeout");
        let err = bash_run(
            t.path(),
            &json!({"cmd": "echo started; sleep 30", "timeout_ms": 50}),
            30_000,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "timeout after 50ms");
    }

    #[tokio::test]
    async fn exec_dispatches_every_tool_and_rejects_unknown_names() {
        let t = TmpDir::new("exec");
        let root = Arc::new(t.path().to_path_buf());
        let run = |name: &'static str, args: Value| {
            let root = root.clone();
            async move { exec(name, &args, root, 5_000).await }
        };

        run("write_file", json!({"path": "a.txt", "content": "abc"}))
            .await
            .unwrap();
        let out = run("read_file", json!({"path": "a.txt"})).await.unwrap();
        assert_eq!(out.trim_end(), "   1│abc");
        let out = run("ls", json!({})).await.unwrap();
        assert!(out.contains("a.txt (3 B)"), "{out}");
        let out = run("glob", json!({"pattern": "**/*.txt"})).await.unwrap();
        assert_eq!(out, "a.txt\n");
        let out = run("grep", json!({"pattern": "abc"})).await.unwrap();
        assert!(out.contains("a.txt:1: abc"), "{out}");
        let out = run("bash", json!({"cmd": "cat a.txt"})).await.unwrap();
        assert!(out.contains("abc") && out.ends_with("[exit 0]"), "{out}");

        // the skeleton lists code files, and an empty project has none
        let out = run("repomap", json!({})).await.unwrap();
        assert_eq!(out, "a.txt\n");
        t.write("src/lib.rs", "pub fn a() {}\n");
        let out = run("repomap", json!({})).await.unwrap();
        assert!(out.contains("src/lib.rs") && out.contains("fn a"), "{out}");
        // scoped to a subtree, and with an explicit budget
        let out = run("repomap", json!({"path": "src", "budget": 500}))
            .await
            .unwrap();
        assert_eq!(out, "lib.rs\n    pub fn a() {}\n");
        assert_eq!(
            run("repomap", json!({"path": "a.txt"})).await.unwrap_err(),
            "a.txt: not a directory"
        );
        std::fs::create_dir(t.join("empty")).unwrap();
        assert_eq!(
            run("repomap", json!({"path": "empty"})).await.unwrap_err(),
            "empty: no files"
        );
        let empty = TmpDir::new("exec-empty");
        let err = exec(
            "repomap",
            &json!({}),
            Arc::new(empty.path().to_path_buf()),
            5_000,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "empty project");

        assert_eq!(
            run("nope", json!({})).await.unwrap_err(),
            "unknown tool: nope"
        );
    }

    #[test]
    fn repomap_clamps_its_budget_and_reports_bad_scopes() {
        let t = TmpDir::new("repomap-args");
        let root = t.path();
        let body: String = (0..60).map(|i| format!("fn f{i}() {{}}\n")).collect();
        t.write("src/lib.rs", &body);

        // a huge budget is clamped to the tool maximum
        let out = repomap_sync(root, &json!({"budget": 10_000_000})).unwrap();
        assert!(out.len() <= repomap::MAX_BUDGET, "{} bytes", out.len());
        assert!(out.contains("fn f24"), "{out}");
        assert_eq!(
            out.matches("    fn ").count(),
            repomap::MAX_SYMBOLS_PER_FILE
        );

        // a tiny one is raised to the floor, and still lists the tree
        let out = repomap_sync(root, &json!({"budget": 1})).unwrap();
        assert!(out.starts_with("src/lib.rs\n"), "{out}");
        assert!(out.len() <= repomap::MIN_BUDGET, "{} bytes", out.len());

        // scoping narrows the tree; the budget is what makes it a summary
        let out = repomap_sync(root, &json!({"path": "src", "budget": 900})).unwrap();
        assert!(out.starts_with("lib.rs\n"), "{out}");
        assert!(!out.contains("src/lib.rs"), "{out}");
        assert!(out.contains("    ...\n"), "{out}");
        assert!(out.len() <= 900, "{} bytes", out.len());

        // a path that escapes the root, and a file that is not a directory
        assert!(repomap_sync(root, &json!({"path": "../x"}))
            .unwrap_err()
            .contains("escapes project root"));
        assert_eq!(
            repomap_sync(root, &json!({"path": "src/lib.rs"})).unwrap_err(),
            "src/lib.rs: not a directory"
        );
        // a directory with no files is not an error, it is an empty map
        std::fs::create_dir_all(t.join("hollow")).unwrap();
        assert_eq!(
            repomap_sync(root, &json!({"path": "hollow"})).unwrap_err(),
            "hollow: no files"
        );
        // an absent budget, an empty scope and a stray space all mean "defaults"
        assert_eq!(
            repomap_sync(root, &json!({"path": "  "})).unwrap(),
            repomap_sync(root, &json!({})).unwrap()
        );
    }

    #[test]
    fn read_file_stops_at_the_byte_budget_and_flags_huge_files() {
        let t = TmpDir::new("read-budget");
        let root = t.path();
        // many lines: the 96 KB output budget stops the listing early
        let long: String = (1..=6000)
            .map(|i| format!("line {i:04} {}\n", "x".repeat(24)))
            .collect();
        t.write("many.txt", &long);
        let out = read_file_sync(root, &json!({"path": "many.txt"})).unwrap();
        assert!(out.contains("(use offset/limit)"), "{out}");
        assert!(!out.contains("file truncated"), "{out}");
        assert!(out.len() < 128 * 1024, "{} bytes", out.len());

        // a file past the 4 MB head cap says so
        t.write("huge.txt", &"y".repeat(4 * 1024 * 1024 + 16));
        let out = read_file_sync(root, &json!({"path": "huge.txt"})).unwrap();
        assert!(
            out.contains("file truncated: only the first 4 MB shown"),
            "{out}"
        );
    }

    #[test]
    fn grep_can_be_scoped_and_skips_binary_files() {
        let t = TmpDir::new("grep-scope");
        let root = t.path();
        t.write("src/a.rs", "needle\n");
        t.write("other/b.txt", "needle\n");
        std::fs::write(t.join("src/bin.dat"), b"needle\x00\n").unwrap();

        let out = grep_sync(root, &json!({"pattern": "needle", "path": "src"})).unwrap();
        assert!(out.contains("src/a.rs:1: needle"), "{out}");
        assert!(!out.contains("b.txt") && !out.contains("bin.dat"), "{out}");

        let out = grep_sync(root, &json!({"pattern": "needle", "glob": "**/*.txt"})).unwrap();
        assert_eq!(out, "other/b.txt:1: needle\n");
    }

    #[test]
    fn grep_stops_at_the_hit_cap() {
        let t = TmpDir::new("grep-cap");
        for i in 0..110 {
            std::fs::write(t.join(&format!("f{i:03}.txt")), "hit\n").unwrap();
        }
        // the default cap is 100 hits
        let out = grep_sync(t.path(), &json!({"pattern": "hit"})).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 101, "cap not applied");
        assert_eq!(lines[100], "… stopped at 100 hits");
        assert!(lines[..100].iter().all(|l| l.ends_with(":1: hit")), "{out}");

        let out = grep_sync(t.path(), &json!({"pattern": "hit", "max": 3})).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "{out}");
        assert_eq!(lines[3], "… stopped at 3 hits");

        // and a pattern with no hits is an error, not empty output
        assert_eq!(
            grep_sync(t.path(), &json!({"pattern": "absent"})).unwrap_err(),
            "no matches"
        );
        // a bad regex names the problem
        let err = grep_sync(t.path(), &json!({"pattern": "["})).unwrap_err();
        assert!(err.starts_with("bad regex:"), "{err}");
    }

    #[test]
    fn glob_truncates_at_the_entry_cap() {
        let t = TmpDir::new("glob-cap");
        for i in 0..GREP_MAX + 5 {
            std::fs::write(t.join(&format!("f{i:03}.txt")), "").unwrap();
        }
        let out = glob_sync(t.path(), &json!({"pattern": "**/*.txt"})).unwrap();
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

    #[test]
    fn read_file_clamps_offset_and_limit() {
        let t = TmpDir::new("read-clamp");
        t.write("a.txt", "l1\nl2\nl3\n");
        let root = t.path();
        assert_eq!(
            read_file_sync(root, &json!({"path": "a.txt", "offset": 2})).unwrap(),
            "   2│l2\n   3│l3\n"
        );
        // limit 0 becomes 1 line plus a marker
        assert_eq!(
            read_file_sync(root, &json!({"path": "a.txt", "limit": 0})).unwrap(),
            "   1│l1\n… stopped at line 2 of 3 (use offset/limit)\n"
        );
        assert_eq!(
            read_file_sync(root, &json!({"path": "a.txt", "offset": 99})).unwrap_err(),
            "offset 99 beyond 3 lines"
        );
        t.write("empty.txt", "");
        assert_eq!(
            read_file_sync(root, &json!({"path": "empty.txt"})).unwrap(),
            "(empty file)"
        );
    }

    #[test]
    fn ls_truncates_very_large_directories() {
        let t = TmpDir::new("ls-many");
        let root = t.path();
        for i in 0..320 {
            std::fs::write(t.join(&format!("f{i:03}.txt")), "").unwrap();
        }
        let out = ls_sync(root, &json!({})).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), LS_MAX + 1);
        assert_eq!(lines[0], "f000.txt (0 B)");
        assert_eq!(lines[LS_MAX], "… +20 more");
    }

    #[test]
    fn write_and_edit_handle_empty_and_multi_edit_writes() {
        let t = TmpDir::new("write-edit");
        let root = t.path();
        assert_eq!(
            write_file_sync(root, &json!({"path": "e.txt", "content": ""})).unwrap(),
            "wrote e.txt (0 B)"
        );
        assert_eq!(std::fs::read_to_string(t.join("e.txt")).unwrap(), "");
        write_file_sync(root, &json!({"path": "e.txt", "content": "shorter"})).unwrap();
        assert_eq!(std::fs::read_to_string(t.join("e.txt")).unwrap(), "shorter");
        assert_eq!(
            write_file_sync(root, &json!({"path": "e.txt"})).unwrap_err(),
            "missing content"
        );

        t.write("f.txt", "one\ntwo\nthree\n");
        let msg = edit_file_sync(
            root,
            &json!({"path": "f.txt", "edits": [
                {"old": "one", "new": "1"},
                {"old": "three", "new": "3\n4"}]}),
        )
        .unwrap();
        assert_eq!(msg, "applied 2 edits to f.txt (+3/-2 lines)");
        assert_eq!(
            std::fs::read_to_string(t.join("f.txt")).unwrap(),
            "1\ntwo\n3\n4\n"
        );

        // missing file / malformed args name the problem
        let err = edit_file_sync(
            root,
            &json!({"path": "gone.txt", "edits": [{"old": "a", "new": "b"}]}),
        )
        .unwrap_err();
        assert!(err.contains("gone.txt"), "{err}");
        let err = edit_file_sync(root, &json!({"path": "f.txt", "edits": "nope"})).unwrap_err();
        assert_eq!(err, "edits must be an array");
        let err =
            edit_file_sync(root, &json!({"path": "f.txt", "edits": [{"new": "b"}]})).unwrap_err();
        assert_eq!(err, "edit 0: missing old");
    }
}
