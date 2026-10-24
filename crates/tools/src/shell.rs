//! `bash`: the escape hatch. Runs in the project root with a timeout, and its
//! output keeps head and tail — where the command line and the verdict are.
//!
//! The command gets its own process group, and the group dies with the call
//! however the call ends — exit, timeout or a cancelled turn — so a test
//! server left in the background can neither leak past the call nor hold the
//! output pipes open and stall it. What the model reads is cleaned of
//! terminal noise it would otherwise pay for: escapes, progress redraws,
//! runs of identical lines.

use crate::{Args, BoxFuture, Exec, Output, Tool, Workspace};
use deeptui_core::Effect;
use serde_json::json;
use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

const BASH_LINES: usize = 250;
/// Bytes kept from the start and from the end of each stream.
const HEAD_BYTES: usize = 16 * 1024;
const TAIL_BYTES: usize = 12 * 1024;
const MAX_TIMEOUT_MS: u64 = 300_000;
/// Once the shell is gone, how long its pipes may take to drain.
const DRAIN: Duration = Duration::from_millis(200);

pub const BASH: Tool = Tool {
    name: "bash",
    about: "Run a shell command in the project root. Output truncated (head+tail). Timeout default 30s, max 300s. Processes it starts die when it returns (setsid to keep one).",
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

/// One stream, kept as head + tail; the middle of a runaway log is counted,
/// not stored.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl Capture {
    fn push(&mut self, b: &[u8]) {
        self.total += b.len();
        let n = HEAD_BYTES.saturating_sub(self.head.len()).min(b.len());
        self.head.extend_from_slice(&b[..n]);
        self.tail.extend(&b[n..]);
        let over = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..over);
    }

    fn text(&self) -> String {
        let skipped = self.total - self.head.len() - self.tail.len();
        if skipped == 0 && self.tail.is_empty() {
            return String::from_utf8_lossy(&self.head).into_owned();
        }
        // cut on character boundaries: the ragged ends were mid-sequence
        let head = &self.head[..self.head.len() - ragged_end(&self.head)];
        let (a, b) = self.tail.as_slices();
        let tail = [a, b].concat();
        let lead = tail
            .iter()
            .take(3)
            .take_while(|&&c| c & 0xC0 == 0x80)
            .count();
        let mut s = String::from_utf8_lossy(head).into_owned();
        if skipped > 0 {
            s.push_str(&format!("\n…[{skipped} bytes elided]…\n"));
        }
        s.push_str(&String::from_utf8_lossy(&tail[lead..]));
        s
    }
}

/// Bytes of an incomplete UTF-8 sequence at the end of `b`.
fn ragged_end(b: &[u8]) -> usize {
    for back in 1..=b.len().min(3) {
        let c = b[b.len() - back];
        if c & 0xC0 != 0x80 {
            let need = match c {
                0xF0.. => 4,
                0xE0.. => 3,
                0xC0.. => 2,
                _ => 1,
            };
            return if need > back { back } else { 0 };
        }
    }
    0
}

/// Kills the command's process group when dropped — on every way out of
/// [`run`], including a cancelled turn dropping its future.
struct Reaper(Option<u32>);

impl Reaper {
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pg) = self.0 {
            // SAFETY: plain syscall; the group is ours (process_group(0))
            unsafe {
                libc::killpg(pg as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        self.kill();
    }
}

enum Ev {
    Out(std::io::Result<usize>),
    Err(std::io::Result<usize>),
    Exit(std::io::Result<std::process::ExitStatus>),
    Deadline,
}

async fn run(ws: &Workspace, args: &Args) -> Output {
    let cmd = args.need("cmd")?;
    let timeout = args
        .u64("timeout_ms")
        .unwrap_or(ws.bash_timeout_ms())
        .min(MAX_TIMEOUT_MS);
    let mut c = tokio::process::Command::new("bash");
    c.arg("-c")
        .arg(cmd)
        .current_dir(ws.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // no pagers, colours or prompts: nobody is at this terminal
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .kill_on_drop(true);
    #[cfg(unix)]
    c.process_group(0);
    let mut child = c.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let reaper = Reaper(child.id());
    let (Some(mut so), Some(mut se)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("spawn failed: no pipes".into());
    };
    let (mut out, mut err) = (Capture::default(), Capture::default());
    let (mut so_open, mut se_open) = (true, true);
    let (mut b1, mut b2) = (vec![0u8; 8192], vec![0u8; 8192]);
    let mut status = None;
    let mut timed_out = false;
    let deadline = tokio::time::sleep(Duration::from_millis(timeout));
    tokio::pin!(deadline);
    while so_open || se_open || status.is_none() {
        let ev = tokio::select! {
            r = so.read(&mut b1), if so_open => Ev::Out(r),
            r = se.read(&mut b2), if se_open => Ev::Err(r),
            s = child.wait(), if status.is_none() => Ev::Exit(s),
            _ = &mut deadline => Ev::Deadline,
        };
        match ev {
            Ev::Out(Ok(n)) if n > 0 => out.push(&b1[..n]),
            Ev::Out(_) => so_open = false,
            Ev::Err(Ok(n)) if n > 0 => err.push(&b2[..n]),
            Ev::Err(_) => se_open = false,
            Ev::Exit(s) => {
                status = Some(s);
                // stragglers die with the shell; what they wrote may drain
                reaper.kill();
                deadline.as_mut().reset(Instant::now() + DRAIN);
            }
            // the shell is gone and its pipes are still held: stop reading
            Ev::Deadline if status.is_some() => break,
            Ev::Deadline if timed_out => return Err(format!("timeout after {timeout}ms")),
            Ev::Deadline => {
                timed_out = true;
                reaper.kill();
                let _ = child.start_kill();
                deadline.as_mut().reset(Instant::now() + DRAIN);
            }
        }
    }
    let mut s = tidy(&out.text());
    let e = tidy(&err.text());
    if !e.trim().is_empty() {
        if !s.is_empty() {
            s.push_str("\n--- stderr ---\n");
        }
        s.push_str(&e);
    }
    let mut s = cap_lines(&s, 200, BASH_LINES - 200);
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    if timed_out {
        return Err(format!(
            "timeout after {timeout}ms (killed); output so far:\n{s}"
        ));
    }
    let verdict = match status {
        Some(Ok(st)) => match st.code() {
            Some(c) => format!("[exit {c}]"),
            None => signal(st),
        },
        Some(Err(e)) => format!("[wait failed: {e}]"),
        None => "[exit ?]".into(),
    };
    Ok(format!("{s}{verdict}"))
}

#[cfg(unix)]
fn signal(st: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    format!("[killed by signal {}]", st.signal().unwrap_or(0))
}

#[cfg(not(unix))]
fn signal(_: std::process::ExitStatus) -> String {
    "[exit -1]".into()
}

/// Terminal noise the model would pay for: ANSI escapes, carriage-return
/// redraws (only the last frame of a progress bar survives), and runs of
/// three or more identical lines.
fn tidy(s: &str) -> String {
    let clean = strip_ansi(s);
    let mut out = String::with_capacity(clean.len());
    let mut prev: Option<&str> = None;
    let mut reps = 0usize;
    let flush = |out: &mut String, reps: usize| {
        if reps > 1 {
            out.push_str(&format!("[previous line repeated {reps} more times]\n"));
        } else if reps == 1 {
            let last = out.lines().last().unwrap_or("").to_string();
            out.push_str(&last);
            out.push('\n');
        }
    };
    for line in clean.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.rsplit('\r').find(|f| !f.is_empty()).unwrap_or("");
        if prev == Some(line) {
            reps += 1;
            continue;
        }
        flush(&mut out, reps);
        reps = 0;
        out.push_str(line);
        out.push('\n');
        prev = Some(line);
    }
    flush(&mut out, reps);
    if !clean.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// `s` without CSI (`ESC [ … final`), OSC (`ESC ] … BEL|ST`) and two-byte
/// escape sequences.
fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                while let Some(&n) = it.peek() {
                    it.next();
                    if ('\x40'..='\x7e').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(n) = it.next() {
                    if n == '\x07' || (n == '\x1b' && it.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
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

        // a command that prints 6000 lines is byte- and line-capped, and its
        // last line — usually the verdict — survives
        let out = sh(
            &t,
            json!({"cmd": "for i in $(seq 1 6000); do echo line$i; done"}),
            30_000,
        )
        .await
        .unwrap();
        assert!(out.contains("lines elided"), "no cap");
        assert!(out.len() < 32 * 1024, "{} bytes", out.len());
        assert!(out.ends_with("line6000\n[exit 0]"), "the tail was lost");

        // the same, on stderr only
        let out = sh(
            &t,
            json!({"cmd": "for i in $(seq 1 6000); do echo line$i; done >&2; exit 3"}),
            30_000,
        )
        .await
        .unwrap();
        assert!(out.contains("lines elided"), "no cap");
        assert!(
            out.ends_with("line6000\n[exit 3]"),
            "{}",
            &out[out.len() - 40..]
        );

        // the byte cap never splits a multi-byte character
        let out = sh(
            &t,
            json!({"cmd": "for i in $(seq 1 20000); do printf 字; done"}),
            30_000,
        )
        .await
        .unwrap();
        assert!(out.contains("bytes elided"), "no byte cap");
        assert!(!out.contains('\u{fffd}'), "a character was split");
    }

    #[tokio::test]
    async fn bash_times_out_and_kills_the_command() {
        let t = TmpDir::new("bash-timeout");
        let err = sh(
            &t,
            json!({"cmd": "echo started; sleep 30", "timeout_ms": 300}),
            30_000,
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("timeout after 300ms (killed)"), "{err}");
        // what it printed before it hung is not lost
        assert!(err.ends_with("output so far:\nstarted\n"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_children_neither_stall_the_call_nor_outlive_it() {
        let t = TmpDir::new("bash-bg");
        let t0 = std::time::Instant::now();
        // the sleeper inherits the output pipe; the call must not wait on it
        let out = sh(
            &t,
            json!({"cmd": "(sleep 0.4; touch late.txt) & echo bg"}),
            10_000,
        )
        .await
        .unwrap();
        assert_eq!(out, "bg\n[exit 0]");
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        // ...and it died with the call
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(!t.join("late.txt").exists(), "background job leaked");

        // a timeout takes the whole group down too
        let t0 = std::time::Instant::now();
        let err = sh(
            &t,
            json!({"cmd": "sleep 30 & sleep 30", "timeout_ms": 100}),
            10_000,
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("timeout after 100ms"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn tidy_drops_terminal_noise() {
        assert_eq!(tidy("\x1b[1;31mred\x1b[0m ok\n"), "red ok\n");
        assert_eq!(tidy("\x1b]0;title\x07x\x1b]8;;u\x1b\\y"), "xy");
        // a progress bar keeps its last frame; CRLF endings are just endings
        assert_eq!(tidy(" 10%\r 50%\r100%\ndone\r\n"), "100%\ndone\n");
        // runs of 3+ identical lines collapse; pairs stay as they are
        assert_eq!(
            tidy("a\nx\nx\nx\nx\nb\nb\n"),
            "a\nx\n[previous line repeated 3 more times]\nb\nb\n"
        );
        assert_eq!(tidy("no newline"), "no newline");
        assert_eq!(tidy(""), "");
    }

    #[test]
    fn capture_keeps_head_and_tail() {
        let mut c = Capture::default();
        c.push(b"start\n");
        c.push(&vec![b'x'; HEAD_BYTES + TAIL_BYTES + 100]);
        c.push(b"\nverdict");
        let s = c.text();
        assert!(s.starts_with("start\nxxx"), "{}", &s[..20]);
        assert!(s.ends_with("x\nverdict"));
        assert!(s.contains("…[114 bytes elided]…"), "{}", s.len());
    }
}
