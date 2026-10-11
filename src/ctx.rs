use crate::types::Msg;
use crate::util::est_tokens;
use std::collections::HashSet;

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
        if m.role == "tool" {
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
    let ids: HashSet<String> = keep
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .flat_map(|v| v.iter())
        .map(|t| t.id.clone())
        .collect();
    let before = keep.len();
    keep.retain(|m| {
        m.role != "tool"
            || m.tool_call_id.as_deref().map(|id| ids.contains(id)).unwrap_or(false)
    });
    let orphans = before - keep.len();
    *msgs = keep;
    Some(format!(
        "compacted: elided {elided} outputs, dropped {dropped} msgs (+{orphans} orphans)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FnCall, ToolCall};

    fn tc(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FnCall { name: "bash".into(), arguments: "{}".into() },
        }
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
        let ids: HashSet<String> = msgs
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .flat_map(|v| v.iter())
            .map(|t| t.id.clone())
            .collect();
        for m in &msgs {
            if m.role == "tool" {
                assert!(ids.contains(m.tool_call_id.as_deref().unwrap()));
            }
        }
    }
}
