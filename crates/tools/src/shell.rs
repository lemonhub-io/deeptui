//! `bash`: the escape hatch. Runs in the project root with a timeout, and its
//! output keeps head and tail — where the command line and the verdict are.

use crate::{Args, BoxFuture, Exec, Output, Tool, Workspace};
use deeptui_core::Effect;
use serde_json::json;
use std::time::Duration;

const BASH_LINES: usize = 250;
const BASH_BYTES: usize = 40 * 1024;
const MAX_TIMEOUT_MS: u64 = 300_000;

pub const BASH: Tool = Tool {
    name: "bash",
    about:
        "Run a shell command in the project root. Output truncated. Timeout default 30s, max 300s.",
    params: || {
        json!({"type":"object","properties":{
            "cmd":{"type":"string"},
            "timeout_ms":{"type":"integer"}},
        "required":["cmd"]})
    },
    effect: Effect::Write,
    exec: Exec::Async(bash),
};

fn bash<'a>(ws: &'a Workspace, args: &'a Args) -> BoxFuture<'a, Output> {
    Box::pin(run(ws, args))
}

async fn run(ws: &Workspace, args: &Args) -> Output {
    let cmd = args.need("cmd")?;
    let timeout = args
        .u64("timeout_ms")
        .unwrap_or(ws.bash_timeout_ms())
        .min(MAX_TIMEOUT_MS);
    let child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .current_dir(ws.root())
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_millis(timeout), child).await {
        Err(_) => Err(format!("timeout after {timeout}ms")),
        Ok(Err(e)) => Err(format!("spawn failed: {e}")),
        Ok(Ok(o)) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                if !s.is_empty() {
                    s.push_str("\n--- stderr ---\n");
                }
                s.push_str(&err);
            }
            if s.len() > BASH_BYTES {
                s.truncate(s.floor_char_boundary(BASH_BYTES));
                s.push_str("\n…[bytes truncated]");
            }
            let s = cap_lines(&s, 200, BASH_LINES - 200);
            Ok(format!("{s}[exit {}]", o.status.code().unwrap_or(-1)))
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;
    use serde_json::Value;

    async fn sh(t: &TmpDir, args: Value, default_ms: u64) -> Output {
        run(&Workspace::new(t.path(), default_ms), &Args::from(args)).await
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

    #[tokio::test]
    async fn bash_runs_in_the_project_root_and_reports_the_exit_code() {
        let t = TmpDir::new("bash");
        let out = sh(&t, json!({"cmd": "pwd; echo hi"}), 5_000).await.unwrap();
        assert!(out.contains(&t.path().display().to_string()), "{out}");
        assert!(out.starts_with("hi") || out.contains("\nhi"), "{out}");
        assert!(out.ends_with("[exit 0]"), "{out}");

        // a failing command still returns its output, with the real status
        let out = sh(&t, json!({"cmd": "echo boom >&2; exit 7"}), 5_000)
            .await
            .unwrap();
        assert!(out.contains("boom"), "{out}");
        assert!(out.ends_with("[exit 7]"), "{out}");

        assert_eq!(sh(&t, json!({}), 5_000).await.unwrap_err(), "missing cmd");
    }

    #[tokio::test]
    async fn bash_separates_stderr_and_truncates_runaway_output() {
        let t = TmpDir::new("bash-out");
        let out = sh(&t, json!({"cmd": "echo out; echo err >&2"}), 5_000)
            .await
            .unwrap();
        // stdout keeps its newline, then the stderr block follows
        assert_eq!(out, "out\n\n--- stderr ---\nerr\n[exit 0]");

        // a command that prints 6000 lines is byte- and line-capped
        let out = sh(
            &t,
            json!({"cmd": "for i in $(seq 1 6000); do echo line$i; done"}),
            30_000,
        )
        .await
        .unwrap();
        assert!(out.contains("[bytes truncated]"), "no byte cap");
        assert!(out.contains("lines elided"), "no line cap");
        assert!(out.ends_with("[exit 0]"), "the status must survive the cap");

        // the byte cap never splits a multi-byte character
        let out = sh(
            &t,
            json!({"cmd": "for i in $(seq 1 20000); do printf 字; done"}),
            30_000,
        )
        .await
        .unwrap();
        assert!(out.contains("[bytes truncated]"), "no byte cap");
    }

    #[tokio::test]
    async fn bash_times_out_and_kills_the_command() {
        let t = TmpDir::new("bash-timeout");
        let err = sh(
            &t,
            json!({"cmd": "echo started; sleep 30", "timeout_ms": 50}),
            30_000,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "timeout after 50ms");
    }
}
