//! File tools: read, edit, write, list. Every output is capped, and every cap
//! tells the model how to page past it.

use crate::patch::{imbalance, line_of, locate, ungutter};
use crate::{Args, Exec, Output, Tool, Workspace};
use deeptui_core::Effect;
use serde_json::json;
use std::path::Path;

const LINE_CAP: usize = 400; // max chars of a single source line returned
const READ_MAX: usize = 2000; // hard cap on lines per read
const READ_BYTES: usize = 96 * 1024;
const READ_FILE_MAX: usize = 4 * 1024 * 1024; // read at most the head of a file
const LS_MAX: usize = 300;

pub const READ_FILE: Tool = Tool {
    name: "read_file",
    about: "Read file lines, numbered. Use offset/limit for ranges. Long lines truncated.",
    params: || {
        json!({"type":"object","properties":{
            "path":{"type":"string"},
            "offset":{"type":"integer","description":"1-based start line"},
            "limit":{"type":"integer","description":"max lines, default 400"}},
        "required":["path"]})
    },
    effect: Effect::Read,
    exec: Exec::Blocking(read_file),
};

pub const EDIT_FILE: Tool = Tool {
    name: "edit_file",
    about: "Exact-match search/replace edits on a file. Each `old` must match exactly once. Prefer this over rewriting files.",
    params: || {
        json!({"type":"object","properties":{
            "path":{"type":"string"},
            "edits":{"type":"array","items":{"type":"object","properties":{
                "old":{"type":"string"},"new":{"type":"string"}},
                "required":["old","new"]}}},
        "required":["path","edits"]})
    },
    effect: Effect::Write,
    exec: Exec::Blocking(edit_file),
};

pub const WRITE_FILE: Tool = Tool {
    name: "write_file",
    about: "Create or fully overwrite a file (parent dirs auto-created). For edits, use edit_file.",
    params: || {
        json!({"type":"object","properties":{
            "path":{"type":"string"},"content":{"type":"string"}},
        "required":["path","content"]})
    },
    effect: Effect::Write,
    exec: Exec::Blocking(write_file),
};

pub const LS: Tool = Tool {
    name: "ls",
    about: "List one directory level: dirs first, then files with sizes.",
    params: || {
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"default root"}}})
    },
    effect: Effect::Read,
    exec: Exec::Blocking(ls),
};

fn read_file(ws: &Workspace, args: &Args) -> Output {
    let p = ws.resolve(args.str("path").unwrap_or(""))?;
    if p.is_dir() {
        return Err(format!("{}: is a directory (use ls)", ws.rel(&p)));
    }
    // read at most READ_FILE_MAX + 1 bytes — the +1 detects truncation
    // without a stat call; a giant file can't OOM the agent
    let mut bytes = Vec::new();
    let truncated = {
        use std::io::Read;
        let f = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", ws.rel(&p)))?;
        f.take(READ_FILE_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{}: {e}", ws.rel(&p)))?;
        bytes.len() > READ_FILE_MAX
    };
    bytes.truncate(READ_FILE_MAX);
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(format!("{}: binary file", ws.rel(&p)));
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = args.u64("offset").unwrap_or(1).max(1) as usize;
    let limit = (args.u64("limit").unwrap_or(400) as usize).clamp(1, READ_MAX);
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

fn edit_file(ws: &Workspace, args: &Args) -> Output {
    let p = ws.resolve(args.str("path").unwrap_or(""))?;
    let raw = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", ws.rel(&p)))?;
    // a CRLF file is edited as LF — what the model sees and sends — and
    // written back as CRLF
    let crlf = raw.contains("\r\n") && raw.matches('\n').count() == raw.matches("\r\n").count();
    let mut text = if crlf { raw.replace("\r\n", "\n") } else { raw };
    let before_json = is_json(&p) && serde_json::from_str::<serde_json::Value>(&text).is_ok();
    let edits = args
        .get("edits")
        .as_array()
        .ok_or("edits must be an array")?;
    if edits.is_empty() {
        return Err("no edits".into());
    }
    let (mut plus, mut minus) = (0usize, 0usize);
    // where each edit landed, kept current as later edits shift lines
    let mut at: Vec<usize> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
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
        let hit = locate(&text, old, new)
            .or_else(|miss| match ungutter(old) {
                // copied from read_file output, line numbers and all
                Some(o) => {
                    let n = ungutter(new).unwrap_or_else(|| new.to_string());
                    locate(&text, &o, &n)
                }
                None => Err(miss),
            })
            .map_err(|e| format!("edit {i}: {e}"))?;
        let gone = &text[hit.range.clone()];
        minus += gone.lines().count();
        plus += hit.with.lines().count();
        if !is_prose(&p) {
            for (c, d) in imbalance(gone, &hit.with) {
                notes.push(format!(
                    "warning: edit {i} leaves `{c}` unbalanced by {d:+}"
                ));
            }
        }
        if hit.loose {
            notes.push(format!("note: edit {i} matched ignoring whitespace"));
        }
        let line = line_of(&text, hit.range.start);
        let shift = hit.with.matches('\n').count() as i64 - gone.matches('\n').count() as i64;
        for l in at.iter_mut().filter(|l| **l > line) {
            *l = (*l as i64 + shift) as usize;
        }
        at.push(line);
        text.replace_range(hit.range, &hit.with);
    }
    if before_json {
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
            notes.push(format!("warning: no longer valid JSON: {e}"));
        }
    }
    if crlf {
        text = text.replace('\n', "\r\n");
    }
    std::fs::write(&p, &text).map_err(|e| format!("{}: {e}", ws.rel(&p)))?;
    let at: Vec<String> = at.iter().map(|l| format!("L{l}")).collect();
    let mut out = format!(
        "applied {} edits to {} (+{plus}/-{minus} lines) at {}",
        edits.len(),
        ws.rel(&p),
        at.join(", ")
    );
    for n in notes {
        out.push('\n');
        out.push_str(&n);
    }
    Ok(out)
}

fn write_file(ws: &Workspace, args: &Args) -> Output {
    let p = ws.resolve(args.str("path").unwrap_or(""))?;
    let content = args.need("content")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // a JSON file that parsed before must still parse (JSONC never did)
    let was_json = is_json(&p)
        && std::fs::read_to_string(&p)
            .is_ok_and(|t| serde_json::from_str::<serde_json::Value>(&t).is_ok());
    std::fs::write(&p, content).map_err(|e| format!("{}: {e}", ws.rel(&p)))?;
    let mut out = format!("wrote {} ({} B)", ws.rel(&p), content.len());
    if was_json {
        if let Err(e) = serde_json::from_str::<serde_json::Value>(content) {
            out.push_str(&format!("\nwarning: no longer valid JSON: {e}"));
        }
    }
    Ok(out)
}

fn ext(p: &Path) -> &str {
    p.extension().and_then(|e| e.to_str()).unwrap_or("")
}

fn is_json(p: &Path) -> bool {
    ext(p).eq_ignore_ascii_case("json")
}

/// Text where a lone bracket is ordinary punctuation, not a syntax slip.
fn is_prose(p: &Path) -> bool {
    matches!(
        ext(p).to_ascii_lowercase().as_str(),
        "md" | "markdown" | "txt" | "rst" | "adoc" | "org" | "csv" | "tsv" | "log" | ""
    )
}

fn ls(ws: &Workspace, args: &Args) -> Output {
    let dir = ws.dir(args.str("path"))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", ws.rel(&dir)))?;
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
    fn write_read_round_trip_numbers_lines() {
        let t = TmpDir::new("read");
        let ws = ws(&t);
        let msg = write_file(
            &ws,
            &a(json!({"path": "sub/dir/f.txt", "content": "a\nb\nc\n"})),
        )
        .unwrap();
        assert_eq!(msg, "wrote sub/dir/f.txt (6 B)");
        assert!(t.join("sub/dir/f.txt").is_file());

        let out = read_file(&ws, &a(json!({"path": "sub/dir/f.txt"}))).unwrap();
        assert_eq!(out, "   1│a\n   2│b\n   3│c\n");
        // offset is 1-based and a limit that stops early says so
        let out = read_file(
            &ws,
            &a(json!({"path": "sub/dir/f.txt", "offset": 2, "limit": 1})),
        )
        .unwrap();
        assert_eq!(out, "   2│b\n… stopped at line 3 of 3 (use offset/limit)\n");
        let out = read_file(
            &ws,
            &a(json!({"path": "sub/dir/f.txt", "offset": 2, "limit": 9})),
        )
        .unwrap();
        assert_eq!(out, "   2│b\n   3│c\n");
        // an empty file reads as such
        t.write("empty.txt", "");
        assert_eq!(
            read_file(&ws, &a(json!({"path": "empty.txt"}))).unwrap(),
            "(empty file)"
        );
    }

    #[test]
    fn read_file_guards_bounds_binaries_and_long_lines() {
        let t = TmpDir::new("read-guards");
        let ws = ws(&t);
        let err = read_file(&ws, &a(json!({"path": "missing.txt"}))).unwrap_err();
        assert!(err.contains("missing.txt"), "{err}");
        assert_eq!(read_file(&ws, &a(json!({}))).unwrap_err(), "empty path");

        t.write("f.txt", "a\nb\nc\n");
        assert!(read_file(&ws, &a(json!({"path": "f.txt", "offset": 9})))
            .unwrap_err()
            .contains("beyond 3 lines"));

        std::fs::write(t.join("bin.dat"), b"ab\x00cd").unwrap();
        let err = read_file(&ws, &a(json!({"path": "bin.dat"}))).unwrap_err();
        assert!(err.contains("binary file"), "{err}");

        // a 500-char line is clipped at LINE_CAP with a marker
        t.write("long.txt", &format!("{}\n", "x".repeat(500)));
        let out = read_file(&ws, &a(json!({"path": "long.txt"}))).unwrap();
        assert!(out.contains('…'), "{out}");
        let body = out
            .lines()
            .next()
            .unwrap()
            .trim_start_matches(|c: char| !c.is_alphabetic());
        assert_eq!(body.trim_end_matches(" …").len(), 400);
    }

    #[test]
    fn read_file_stops_at_the_byte_budget_and_flags_huge_files() {
        let t = TmpDir::new("read-budget");
        let ws = ws(&t);
        // many lines: the 96 KB output budget stops the listing early
        let long: String = (1..=6000)
            .map(|i| format!("line {i:04} {}\n", "x".repeat(24)))
            .collect();
        t.write("many.txt", &long);
        let out = read_file(&ws, &a(json!({"path": "many.txt"}))).unwrap();
        assert!(out.contains("(use offset/limit)"), "{out}");
        assert!(!out.contains("file truncated"), "{out}");
        assert!(out.len() < 128 * 1024, "{} bytes", out.len());

        // a file past the 4 MB head cap says so
        t.write("huge.txt", &"y".repeat(4 * 1024 * 1024 + 16));
        let out = read_file(&ws, &a(json!({"path": "huge.txt"}))).unwrap();
        assert!(
            out.contains("file truncated: only the first 4 MB shown"),
            "{out}"
        );
    }

    #[test]
    fn read_file_clamps_offset_and_limit() {
        let t = TmpDir::new("read-clamp");
        t.write("a.txt", "l1\nl2\nl3\n");
        let ws = ws(&t);
        assert_eq!(
            read_file(&ws, &a(json!({"path": "a.txt", "offset": 2}))).unwrap(),
            "   2│l2\n   3│l3\n"
        );
        // limit 0 becomes 1 line plus a marker
        assert_eq!(
            read_file(&ws, &a(json!({"path": "a.txt", "limit": 0}))).unwrap(),
            "   1│l1\n… stopped at line 2 of 3 (use offset/limit)\n"
        );
        assert_eq!(
            read_file(&ws, &a(json!({"path": "a.txt", "offset": 99}))).unwrap_err(),
            "offset 99 beyond 3 lines"
        );
    }

    #[test]
    fn edit_file_applies_exact_matches_only() {
        let t = TmpDir::new("edit");
        let ws = ws(&t);
        let p = t.write("f.txt", "hello world\nsecond line\n");

        let msg = edit_file(
            &ws,
            &a(json!({"path": "f.txt", "edits": [{"old": "hello world", "new": "hello there"}]})),
        )
        .unwrap();
        assert_eq!(msg, "applied 1 edits to f.txt (+1/-1 lines) at L1");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "hello there\nsecond line\n"
        );

        // shrinking a two-line region reports +1/-2
        let msg = edit_file(
            &ws,
            &a(json!({"path": "f.txt", "edits": [{"old": "hello there\nsecond", "new": "hi"}]})),
        )
        .unwrap();
        assert!(msg.ends_with("(+1/-2 lines) at L1"), "{msg}");
    }

    #[test]
    fn edit_file_rejects_ambiguous_missing_and_empty_edits() {
        let t = TmpDir::new("edit-err");
        let ws = ws(&t);
        t.write("f.txt", "aa aa\n");
        let err = edit_file(
            &ws,
            &a(json!({"path": "f.txt", "edits": [{"old": "aa", "new": "bb"}]})),
        )
        .unwrap_err();
        assert!(err.contains("matches 2 times"), "{err}");
        let err = edit_file(
            &ws,
            &a(json!({"path": "f.txt", "edits": [{"old": "zz", "new": "bb"}]})),
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
        let err = edit_file(&ws, &a(json!({"path": "f.txt", "edits": []}))).unwrap_err();
        assert_eq!(err, "no edits");
        let err = edit_file(
            &ws,
            &a(json!({"path": "f.txt", "edits": [{"old": "", "new": "x"}]})),
        )
        .unwrap_err();
        assert_eq!(err, "edit 0: empty old");
        // nothing was written by the failed attempts
        assert_eq!(std::fs::read_to_string(t.join("f.txt")).unwrap(), "aa aa\n");
    }

    #[test]
    fn write_and_edit_handle_empty_and_multi_edit_writes() {
        let t = TmpDir::new("write-edit");
        let ws = ws(&t);
        assert_eq!(
            write_file(&ws, &a(json!({"path": "e.txt", "content": ""}))).unwrap(),
            "wrote e.txt (0 B)"
        );
        assert_eq!(std::fs::read_to_string(t.join("e.txt")).unwrap(), "");
        write_file(&ws, &a(json!({"path": "e.txt", "content": "shorter"}))).unwrap();
        assert_eq!(std::fs::read_to_string(t.join("e.txt")).unwrap(), "shorter");
        assert_eq!(
            write_file(&ws, &a(json!({"path": "e.txt"}))).unwrap_err(),
            "missing content"
        );

        t.write("f.txt", "one\ntwo\nthree\n");
        let msg = edit_file(
            &ws,
            &a(json!({"path": "f.txt", "edits": [
                {"old": "one", "new": "1"},
                {"old": "three", "new": "3\n4"}]})),
        )
        .unwrap();
        assert_eq!(msg, "applied 2 edits to f.txt (+3/-2 lines) at L1, L3");
        assert_eq!(
            std::fs::read_to_string(t.join("f.txt")).unwrap(),
            "1\ntwo\n3\n4\n"
        );

        // missing file / malformed args name the problem
        let err = edit_file(
            &ws,
            &a(json!({"path": "gone.txt", "edits": [{"old": "a", "new": "b"}]})),
        )
        .unwrap_err();
        assert!(err.contains("gone.txt"), "{err}");
        let err = edit_file(&ws, &a(json!({"path": "f.txt", "edits": "nope"}))).unwrap_err();
        assert_eq!(err, "edits must be an array");
        let err =
            edit_file(&ws, &a(json!({"path": "f.txt", "edits": [{"new": "b"}]}))).unwrap_err();
        assert_eq!(err, "edit 0: missing old");
    }

    #[test]
    fn edit_file_forgives_what_models_get_wrong() {
        let t = TmpDir::new("edit-forgive");
        let ws = ws(&t);
        // CRLF stays CRLF, though the model searched with LF
        let p = t.write("w.rs", "fn a() {\r\n    x();\r\n}\r\n");
        let msg = edit_file(
            &ws,
            &a(json!({"path": "w.rs", "edits": [{"old": "    x();\n}", "new": "    y();\n}"}]})),
        )
        .unwrap();
        assert_eq!(msg, "applied 1 edits to w.rs (+2/-2 lines) at L2");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "fn a() {\r\n    y();\r\n}\r\n"
        );

        // read_file's gutter copied into old and new
        let p = t.write("g.py", "def f():\n    return 1\n");
        let msg = edit_file(
            &ws,
            &a(json!({"path": "g.py", "edits": [
                {"old": "   2│    return 1", "new": "   2│    return 2"}]})),
        )
        .unwrap();
        assert!(msg.starts_with("applied 1 edits to g.py"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "def f():\n    return 2\n"
        );

        // an indentation slip lands, re-indented, and says so
        let msg = edit_file(
            &ws,
            &a(json!({"path": "g.py", "edits": [
                {"old": " def f():\n     return 2", "new": " def f():\n     return 3"}]})),
        )
        .unwrap();
        assert!(
            msg.ends_with("\nnote: edit 0 matched ignoring whitespace"),
            "{msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "def f():\n    return 3\n"
        );

        // a miss shows the region it most likely meant
        let err = edit_file(
            &ws,
            &a(json!({"path": "g.py", "edits": [{"old": "    retrun 3", "new": "x"}]})),
        )
        .unwrap_err();
        assert!(
            err.starts_with("edit 0: `old` not found; closest is lines 2-2"),
            "{err}"
        );
        assert!(err.ends_with("   2│    return 3"), "{err}");
    }

    #[test]
    fn edits_and_writes_flag_likely_breakage() {
        let t = TmpDir::new("edit-warn");
        let ws = ws(&t);
        t.write("m.rs", "fn a() {\n}\n");
        let msg = edit_file(
            &ws,
            &a(json!({"path": "m.rs", "edits": [{"old": "fn a() {\n}", "new": "fn a() {"}]})),
        )
        .unwrap();
        assert!(
            msg.ends_with("\nwarning: edit 0 leaves `{` unbalanced by +1"),
            "{msg}"
        );
        // prose has no bracket discipline to keep
        t.write("n.md", "a\n");
        let msg = edit_file(
            &ws,
            &a(json!({"path": "n.md", "edits": [{"old": "a", "new": "1) a"}]})),
        )
        .unwrap();
        assert!(!msg.contains("warning"), "{msg}");

        t.write("c.json", "{\"a\": 1}");
        let msg = edit_file(
            &ws,
            &a(json!({"path": "c.json", "edits": [{"old": "1}", "new": "1,}"}]})),
        )
        .unwrap();
        assert!(msg.contains("warning: no longer valid JSON"), "{msg}");
        let msg = write_file(&ws, &a(json!({"path": "c.json", "content": "{}"}))).unwrap();
        assert_eq!(msg, "wrote c.json (2 B)", "it was already broken");
        let msg = write_file(&ws, &a(json!({"path": "c.json", "content": "{"}))).unwrap();
        assert!(msg.contains("warning: no longer valid JSON"), "{msg}");
        // a directory is not a file to read
        std::fs::create_dir_all(t.join("d")).unwrap();
        assert_eq!(
            read_file(&ws, &a(json!({"path": "d"}))).unwrap_err(),
            "d: is a directory (use ls)"
        );
    }

    #[test]
    fn ls_lists_dirs_first_then_files() {
        let t = TmpDir::new("ls");
        let ws = ws(&t);
        assert_eq!(ls(&ws, &a(json!({}))).unwrap(), "(empty)");

        t.write("zeta.txt", "abc");
        t.write("alpha/file", "");
        t.write("beta/file", "");
        let out = ls(&ws, &a(json!({"path": "."}))).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, ["alpha/", "beta/", "zeta.txt (3 B)"]);
    }

    #[test]
    fn ls_truncates_very_large_directories() {
        let t = TmpDir::new("ls-many");
        for i in 0..320 {
            std::fs::write(t.join(&format!("f{i:03}.txt")), "").unwrap();
        }
        let out = ls(&ws(&t), &a(json!({}))).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), LS_MAX + 1);
        assert_eq!(lines[0], "f000.txt (0 B)");
        assert_eq!(lines[LS_MAX], "… +20 more");
    }
}
