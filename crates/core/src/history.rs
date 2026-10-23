//! Keeping a history sendable and affordable: token estimates, invariant
//! repair, and two-stage compaction.

use crate::msg::{Msg, Role};
use std::collections::HashSet;

/// Cheap token estimate: ~4 chars/tok for ASCII, ~1 char/tok for non-ASCII.
/// Deliberately avoids a tokenizer dependency: fast startup, zero cost.
pub fn est_tokens(s: &str) -> usize {
    let mut a = 0usize;
    let mut n = 0usize;
    for c in s.chars() {
        if c.is_ascii() {
            a += 1;
        } else {
            n += 1;
        }
    }
    a / 4 + n + 1
}

fn msg_tokens(m: &Msg) -> usize {
    let mut t = 4;
    if let Some(c) = &m.content {
        t += est_tokens(c);
    }
    if let Some(tcs) = &m.tool_calls {
        for tc in tcs {
            t += est_tokens(&tc.function.arguments) + 10;
        }
    }
    t
}

pub fn total_tokens(msgs: &[Msg]) -> usize {
    msgs.iter().map(msg_tokens).sum()
}

/// Ids of every tool call some assistant message still makes.
fn call_ids(msgs: &[Msg]) -> HashSet<String> {
    msgs.iter()
        .flat_map(|m| m.tool_calls.iter())
        .flat_map(|v| v.iter())
        .map(|t| t.id.clone())
        .collect()
}

/// Drop tool results whose call is gone. Returns how many were dropped.
fn drop_orphans(msgs: &mut Vec<Msg>) -> usize {
    let ids = call_ids(msgs);
    let before = msgs.len();
    msgs.retain(|m| {
        m.role != Role::Tool
            || m.tool_call_id
                .as_deref()
                .map(|id| ids.contains(id))
                .unwrap_or(false)
    });
    before - msgs.len()
}

/// Scrub message states the chat API rejects — left behind by interrupted
/// streams or future regressions. Runs each round before `compact`.
/// - tool_calls never answered by a tool message are stripped
/// - assistant messages left with neither content nor tool_calls are dropped
/// - tool results whose call is missing are dropped
///
/// Returns the number of fixes applied.
pub fn repair(msgs: &mut Vec<Msg>) -> usize {
    let answered: HashSet<String> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    let mut fixed = 0usize;
    for m in msgs.iter_mut().filter(|m| m.role == Role::Assistant) {
        if let Some(tcs) = &mut m.tool_calls {
            let n = tcs.len();
            tcs.retain(|t| answered.contains(&t.id));
            fixed += n - tcs.len();
            if tcs.is_empty() {
                m.tool_calls = None;
            }
        }
    }
    let n0 = msgs.len();
    msgs.retain(|m| m.role != Role::Assistant || m.content.is_some() || m.tool_calls.is_some());
    let empties = n0 - msgs.len();
    fixed + empties + drop_orphans(msgs)
}

/// Two-stage compaction, run before each API call when over `budget`.
/// Stage 1 (cheap, surgical): replace old tool outputs >240 chars with "[elided]"
/// — old file reads/searches are almost never re-read by the model.
/// Stage 2 (fallback): drop the middle of the history, then repair tool-call
/// orphans so the message list stays API-valid.
/// Returns a human note when it did something.
pub fn compact(msgs: &mut Vec<Msg>, budget: usize) -> Option<String> {
    if total_tokens(msgs) <= budget || msgs.len() < 8 {
        return None;
    }

    // Stage 1: elide stale tool outputs (keep the last 8 messages untouched).
    let mut elided = 0usize;
    let cut = msgs.len().saturating_sub(8);
    for m in msgs.iter_mut().take(cut).skip(1) {
        if m.role == Role::Tool {
            if let Some(c) = &m.content {
                if c.len() > 240 && m.orig_len == 0 {
                    m.orig_len = c.len();
                    m.content = Some(format!("[{} B output elided]", c.len()));
                    elided += 1;
                }
            }
        }
    }
    if total_tokens(msgs) <= budget * 9 / 10 {
        return Some(format!("compacted: elided {elided} tool outputs"));
    }

    // Stage 2: truncate the middle. Keep system + first user msg + last 12.
    let n = msgs.len();
    let head = 2usize.min(n);
    let tail = 12usize.min(n.saturating_sub(head));
    if n <= head + tail + 4 {
        return Some(format!("compacted: elided {elided} tool outputs"));
    }
    let dropped = n - head - tail;
    let mut keep: Vec<Msg> = msgs[..head].to_vec();
    keep.push(Msg::user(format!(
        "[context compacted: {dropped} earlier messages removed to save tokens]"
    )));
    keep.extend(msgs[n - tail..].to_vec());

    // Repair: tool results whose assistant call was dropped are invalid → remove.
    let orphans = drop_orphans(&mut keep);
    *msgs = keep;
    Some(format!(
        "compacted: elided {elided} outputs, dropped {dropped} msgs (+{orphans} orphans)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::ToolCall;

    fn tc(id: &str) -> ToolCall {
        ToolCall::new(id, "bash", "{}")
    }

    fn roles(msgs: &[Msg]) -> Vec<Role> {
        msgs.iter().map(|m| m.role).collect()
    }

    #[test]
    fn token_est() {
        assert!(est_tokens("hello world") < 6);
        assert!(est_tokens("你好世界") >= 4);
    }

    #[test]
    fn est_tokens_counts_scripts_differently() {
        assert_eq!(est_tokens(""), 1);
        // 400 ASCII chars ~= 100 tokens (+1 floor)
        assert_eq!(est_tokens(&"x".repeat(400)), 101);
        // CJK is ~1 token/char, not 1 per 4
        assert_eq!(est_tokens(&"字".repeat(100)), 101);
    }

    #[test]
    fn repairs_interrupt_artifacts() {
        // cancelled before any token: empty assistant msg must go
        let mut msgs = vec![
            Msg::system("s"),
            Msg::user("q"),
            Msg::assistant(None, vec![]),
        ];
        assert_eq!(repair(&mut msgs), 1);
        assert_eq!(msgs.len(), 2);

        // cancelled mid tool-call + orphan tool result + dangling call
        let mut msgs = vec![
            Msg::system("s"),
            Msg::user("q"),
            Msg::assistant(Some("working".into()), vec![tc("c1")]),
            Msg::tool("c1", "done"),
            Msg::assistant(None, vec![]),
            Msg::assistant(None, vec![tc("c2")]),
            Msg::tool("ghost", "x"),
        ];
        repair(&mut msgs);
        assert_eq!(
            roles(&msgs),
            [Role::System, Role::User, Role::Assistant, Role::Tool]
        );
        // every surviving tool_call id has a response and vice versa
        let resp_ids: HashSet<String> =
            msgs.iter().filter_map(|m| m.tool_call_id.clone()).collect();
        assert_eq!(call_ids(&msgs), resp_ids);
    }

    #[test]
    fn total_tokens_sums_content_and_tool_args() {
        assert_eq!(total_tokens(&[]), 0);
        assert_eq!(total_tokens(&[Msg::user("x")]), 5); // 4 + 1 estimated
                                                        // a tool call adds its argument estimate plus a flat 10
        let m = Msg::assistant(None, vec![tc("c1")]);
        assert_eq!(total_tokens(&[m]), 15);
    }

    #[test]
    fn repair_leaves_clean_history_untouched() {
        let mut msgs = vec![
            Msg::system("s"),
            Msg::user("q"),
            Msg::assistant(Some("a".into()), vec![tc("c1")]),
            Msg::tool("c1", "r"),
        ];
        assert_eq!(repair(&mut msgs), 0);
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn repair_drops_tool_results_without_a_request() {
        // id present but unknown
        let mut msgs = vec![Msg::system("s"), Msg::user("q"), Msg::tool("ghost", "r")];
        assert_eq!(repair(&mut msgs), 1);
        assert_eq!(msgs.len(), 2);

        // no tool_call_id at all
        let mut orphan = Msg::tool("c", "r");
        orphan.tool_call_id = None;
        let mut msgs = vec![Msg::system("s"), orphan];
        assert_eq!(repair(&mut msgs), 1);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn repair_keeps_content_when_stripping_unanswered_calls() {
        let mut msgs = vec![
            Msg::system("s"),
            Msg::user("q"),
            Msg::assistant(Some("partial".into()), vec![tc("c1"), tc("c2")]),
            Msg::tool("c1", "r"),
        ];
        assert_eq!(repair(&mut msgs), 1);
        let a = msgs.iter().find(|m| m.role == Role::Assistant).unwrap();
        assert_eq!(a.content.as_deref(), Some("partial"));
        let calls = a.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "c1");
    }

    #[test]
    fn compact_is_a_noop_when_small() {
        let mut msgs = vec![Msg::system("s"), Msg::user("q")];
        assert!(compact(&mut msgs, 1_000_000).is_none());
        // over budget but fewer than 8 messages: nothing may be dropped
        let mut msgs = vec![
            Msg::system("x".repeat(4000)),
            Msg::user("y".repeat(4000)),
            Msg::assistant(Some("z".repeat(4000)), vec![]),
        ];
        assert!(compact(&mut msgs, 100).is_none());
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn compact_stage_one_needs_no_message_drop() {
        let mut msgs = vec![Msg::system("s")];
        for i in 0..20 {
            msgs.push(Msg::assistant(None, vec![tc(&format!("c{i}"))]));
            msgs.push(Msg::tool(format!("c{i}"), "x".repeat(300)));
        }
        let before = total_tokens(&msgs);
        let n = msgs.len();
        // just over budget: eliding old tool outputs is enough
        let note = compact(&mut msgs, before - 1).unwrap();
        assert!(note.contains("elided"), "{note}");
        assert!(!note.contains("dropped"), "stage 2 not needed: {note}");
        assert_eq!(msgs.len(), n);
        assert!(total_tokens(&msgs) < before);
        // the head (system + earliest turns) is untouched
        assert_eq!(msgs[0].role, Role::System);
        // elision is recorded once and is idempotent
        assert!(msgs.iter().any(|m| m.orig_len > 0));
        assert!(compact(&mut msgs, before - 1).is_none());
    }

    #[test]
    fn compact_stage_two_drops_the_middle_and_repairs_orphans() {
        let mut msgs = vec![Msg::system("s"), Msg::user("first")];
        for i in 0..2 {
            msgs.push(Msg::assistant(None, vec![tc(&format!("c{i}"))]));
            msgs.push(Msg::tool(format!("c{i}"), "y".repeat(300)));
        }
        // this call lands outside the kept tail while its result stays inside
        msgs.push(Msg::assistant(None, vec![tc("cut")]));
        msgs.push(Msg::tool("cut", "y".repeat(300)));
        for i in 2..7 {
            msgs.push(Msg::assistant(None, vec![tc(&format!("c{i}"))]));
            msgs.push(Msg::tool(format!("c{i}"), "y".repeat(300)));
        }
        msgs.push(Msg::assistant(Some("tail".into()), vec![]));
        assert_eq!(msgs.len(), 19);

        let note = compact(&mut msgs, 100).unwrap();
        assert_eq!(
            note,
            "compacted: elided 4 outputs, dropped 5 msgs (+1 orphans)"
        );
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[1].role, Role::User);
        assert!(msgs[2]
            .content
            .as_deref()
            .unwrap()
            .contains("context compacted: 5"));
        // the orphaned tool result is gone, and no new orphans were created
        let calls = call_ids(&msgs);
        for m in msgs.iter().filter(|m| m.role == Role::Tool) {
            assert!(calls.contains(m.tool_call_id.as_deref().unwrap()), "{m:?}");
        }
        assert!(msgs.len() < 19);
    }

    #[test]
    fn compact_leaves_the_recent_tail_untouched() {
        let mut msgs = vec![Msg::system("s")];
        for i in 0..20 {
            msgs.push(Msg::assistant(None, vec![tc(&format!("c{i}"))]));
            msgs.push(Msg::tool(format!("c{i}"), "x".repeat(300)));
        }
        let before = total_tokens(&msgs);
        let tail: Vec<Option<String>> = msgs[msgs.len() - 8..]
            .iter()
            .map(|m| m.content.clone())
            .collect();
        let note = compact(&mut msgs, before - 1).unwrap();
        assert!(note.contains("elided"), "{note}");
        // the newest 8 messages (and their outputs) are never elided
        for (m, want) in msgs[msgs.len() - 8..].iter().zip(tail.iter()) {
            assert_eq!(&m.content, want, "recent message was rewritten");
            assert_eq!(m.orig_len, 0);
        }
        // ...but the older ones were
        assert!(msgs[..msgs.len() - 8].iter().any(|m| m.orig_len > 240));
    }

    #[test]
    fn compact_reports_stage_one_when_dropping_is_impossible() {
        // over the 90% threshold, yet too short to truncate (head 2 + tail 12)
        let mut msgs = vec![Msg::system("s".repeat(4000))];
        for i in 0..4 {
            msgs.push(Msg::assistant(None, vec![tc(&format!("c{i}"))]));
            msgs.push(Msg::tool(format!("c{i}"), "y".repeat(400)));
        }
        assert_eq!(msgs.len(), 9);
        let before = total_tokens(&msgs);
        let note = compact(&mut msgs, 100).unwrap();
        assert_eq!(note, "compacted: elided 0 tool outputs");
        // nothing was dropped and nothing was elided (all outputs are recent)
        assert_eq!(msgs.len(), 9);
        assert_eq!(total_tokens(&msgs), before);
    }

    #[test]
    fn elides_old_tool_outputs() {
        let mut msgs = vec![Msg::system("s")];
        for i in 0..20 {
            msgs.push(Msg::user(format!("q{i}")));
            msgs.push(Msg::assistant(None, vec![tc(&format!("c{i}"))]));
            msgs.push(Msg::tool(format!("c{i}"), "x".repeat(2000)));
        }
        let note = compact(&mut msgs, 5000).unwrap();
        assert!(note.contains("elided"));
        assert!(total_tokens(&msgs) < 5000 * 2);
        // tool result pairing still valid
        let ids = call_ids(&msgs);
        for m in msgs.iter().filter(|m| m.role == Role::Tool) {
            assert!(ids.contains(m.tool_call_id.as_deref().unwrap()));
        }
    }
}
