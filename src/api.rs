use crate::config::Config;
use crate::types::Msg;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

/// Events decoded from the SSE stream.
#[derive(Debug)]
pub enum Ev {
    Text(String),
    TcDelta {
        idx: usize,
        id: Option<String>,
        name: Option<String>,
        args: String,
    },
    Usage {
        input: u64,
        output: u64,
        cached: u64,
    },
    Finish(String),
}

/// Incremental SSE parser: feeds on arbitrary byte chunks, yields `data:` payloads.
///
/// Buffers raw bytes and decodes only complete events: a multi-byte char split
/// across TCP chunks would otherwise be mangled by per-chunk lossy decoding.
/// Events end at a blank line — `\n\n` or `\r\n\r\n` (CRLF providers).
#[derive(Default)]
pub struct Sse {
    buf: Vec<u8>,
}

/// Cap on the unconsumed buffer; a stream that never emits a blank line
/// can't grow memory forever.
const SSE_BUF_MAX: usize = 1 << 20;

impl Sse {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let lf = self.buf.windows(2).position(|w| w == b"\n\n");
            let crlf = self.buf.windows(4).position(|w| w == b"\r\n\r\n");
            let (end, skip) = match (lf, crlf) {
                (Some(a), Some(b)) if b < a => (b, 4),
                (Some(a), _) => (a, 2),
                (None, Some(b)) => (b, 4),
                (None, None) => {
                    if self.buf.len() > SSE_BUF_MAX {
                        self.buf.clear();
                    }
                    break;
                }
            };
            let block = String::from_utf8_lossy(&self.buf[..end]).into_owned();
            self.buf.drain(..end + skip);
            let mut data = String::new();
            for l in block.lines() {
                if let Some(d) = l.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(d.trim_start());
                }
            }
            if !data.is_empty() {
                out.push(data);
            }
        }
        out
    }
}

/// Decode one SSE `data:` payload into events. Returns None on unparseable JSON.
pub fn parse_chunk(data: &str) -> Vec<Ev> {
    let mut out = Vec::new();
    if data.trim() == "[DONE]" {
        out.push(Ev::Finish("done".into()));
        return out;
    }
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if let Some(u) = v.get("usage") {
        let cached = u
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|c| c.as_u64())
            .or_else(|| u.get("prompt_cache_hit_tokens").and_then(|c| c.as_u64()))
            .unwrap_or(0);
        out.push(Ev::Usage {
            input: u["prompt_tokens"].as_u64().unwrap_or(0),
            output: u["completion_tokens"].as_u64().unwrap_or(0),
            cached,
        });
    }
    if let Some(err) = v.get("error") {
        out.push(Ev::Finish(format!(
            "api error: {}",
            err["message"].as_str().unwrap_or("?")
        )));
        return out;
    }
    for ch in v["choices"].as_array().into_iter().flatten() {
        let d = &ch["delta"];
        if let Some(c) = d["content"].as_str() {
            if !c.is_empty() {
                out.push(Ev::Text(c.to_string()));
            }
        }
        // some providers stream reasoning — show it dimmed? no: skip (cost transparency, screen quiet)
        for tc in d["tool_calls"].as_array().into_iter().flatten() {
            out.push(Ev::TcDelta {
                idx: tc["index"].as_u64().unwrap_or(0) as usize,
                id: tc["id"].as_str().map(|s| s.to_string()),
                name: tc["function"]["name"].as_str().map(|s| s.to_string()),
                args: tc["function"]["arguments"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            });
        }
        if let Some(f) = ch["finish_reason"].as_str() {
            out.push(Ev::Finish(f.to_string()));
        }
    }
    out
}

/// Stream one chat completion. Emits Ev's on `tx`; returns after stream end,
/// cancel, or a pre-stream HTTP error (retries on 429/5xx).
pub async fn stream_chat(
    http: &reqwest::Client,
    cfg: &Config,
    msgs: &[Msg],
    tools: &[Value],
    tx: mpsc::Sender<Ev>,
    cancel: CancellationToken,
) -> Result<()> {
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": msgs,
        "tools": tools,
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_completion_tokens": cfg.max_output_tokens,
    });
    let url = format!("{}/chat/completions", cfg.base_url);

    let mut attempt = 0u32;
    let resp = loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let send = http.post(&url).bearer_auth(&cfg.api_key).json(&body).send();
        let r = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            r = send => r,
        };
        match r {
            Ok(resp) if resp.status().is_success() => break resp,
            Ok(resp) => {
                let status = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                let retriable = status.as_u16() == 429 || status.is_server_error();
                if retriable && attempt < 3 {
                    attempt += 1;
                    let wait = std::time::Duration::from_millis(400 * (1 << attempt));
                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        _ = tokio::time::sleep(wait) => {},
                    }
                    continue;
                }
                bail!("HTTP {}: {}", status, crate::util::one_line(&txt, 300));
            }
            Err(e) => {
                if attempt < 2 && (e.is_connect() || e.is_timeout()) {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    continue;
                }
                return Err(e).context("request failed");
            }
        }
    };

    let mut sse = Sse::default();
    let mut stream = resp.bytes_stream();
    // a hung connection (socket open, no bytes) must not stall the turn forever
    let stall = std::time::Duration::from_secs(120);
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            c = tokio::time::timeout(stall, stream.next()) => match c {
                Err(_) => bail!("stream stalled for {stall:?}"),
                Ok(Some(Ok(b))) => b,
                Ok(Some(Err(e))) => bail!("stream error: {e}"),
                Ok(None) => break,
            },
        };
        for data in sse.feed(&chunk) {
            for ev in parse_chunk(&data) {
                if tx.send(ev).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta() {
        let evs = parse_chunk(r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#);
        assert!(matches!(&evs[0], Ev::Text(t) if t == "hi"));
    }

    #[test]
    fn parses_tool_delta() {
        let evs = parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"cm"}}]},"finish_reason":null}]}"#,
        );
        assert!(matches!(&evs[0], Ev::TcDelta { idx: 0, args, .. } if args == "{\"cm"));
    }

    #[test]
    fn ignores_empty_and_unparseable_payloads() {
        assert!(parse_chunk("").is_empty());
        assert!(parse_chunk("not json").is_empty());
        // an empty content delta and an empty tool_calls array yield nothing
        assert!(parse_chunk(
            r#"{"choices":[{"delta":{"content":"","tool_calls":[]},"finish_reason":null}]}"#
        )
        .is_empty());
        // a null finish_reason is not a Finish event
        assert!(parse_chunk(r#"{"choices":[{"delta":{},"finish_reason":null}]}"#).is_empty());
    }

    #[test]
    fn parses_multiple_choices_in_order() {
        let evs = parse_chunk(
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":null},{"delta":{"content":"b"},"finish_reason":"stop"}]}"#,
        );
        assert_eq!(evs.len(), 3, "{evs:?}");
        assert!(matches!(&evs[0], Ev::Text(t) if t == "a"));
        assert!(matches!(&evs[1], Ev::Text(t) if t == "b"));
        assert!(matches!(&evs[2], Ev::Finish(f) if f == "stop"));
    }

    #[test]
    fn parses_tool_delta_without_id_or_args() {
        let evs = parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"grep"}}]},"finish_reason":null}]}"#,
        );
        match &evs[0] {
            Ev::TcDelta {
                idx,
                id,
                name,
                args,
            } => {
                assert_eq!(*idx, 2);
                assert!(id.is_none());
                assert_eq!(name.as_deref(), Some("grep"));
                assert_eq!(args, "");
            }
            other => panic!("expected TcDelta, got {other:?}"),
        }
    }

    #[test]
    fn surfaces_api_errors() {
        let evs = parse_chunk(r#"{"error":{"message":"bad key"}}"#);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], Ev::Finish(f) if f == "api error: bad key"));
        // a message-less error still becomes a Finish
        assert!(matches!(&parse_chunk(r#"{"error":{}}"#)[0], Ev::Finish(f) if f == "api error: ?"));
    }

    #[test]
    fn usage_prefers_details_over_deepseek_fallback() {
        let evs = parse_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":1,"prompt_cache_hit_tokens":7}}"#,
        );
        assert!(matches!(
            evs[0],
            Ev::Usage {
                input: 9,
                output: 1,
                cached: 7
            }
        ));
        let evs = parse_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":3},"prompt_cache_hit_tokens":7}}"#,
        );
        assert!(matches!(evs[0], Ev::Usage { cached: 3, .. }));
    }

    #[test]
    fn parses_usage_and_done() {
        let evs = parse_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":64}}}"#,
        );
        assert!(matches!(
            evs[0],
            Ev::Usage {
                input: 100,
                output: 10,
                cached: 64
            }
        ));
        assert!(matches!(parse_chunk("[DONE]")[0], Ev::Finish(_)));
    }

    #[test]
    fn sse_splits_events() {
        let mut s = Sse::default();
        assert!(!s.feed(b"data: {\"a\":1}\n\nda").is_empty());
        let rest = s.feed(b"ta: {\"b\":2}\n\n");
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn sse_handles_crlf_and_split_multibyte() {
        // CRLF separators (some providers) must delimit events
        let mut s = Sse::default();
        let evs = s.feed(b"data: {\"a\":1}\r\n\r\ndata: {\"b\":2}\r\n\r\n");
        assert_eq!(evs.len(), 2);

        // a multi-byte char split across chunks must survive intact —
        // 你 is E4 BD A0; splitting mid-char used to emit replacement chars
        let mut s = Sse::default();
        let payload = "data: 你\n\n";
        let b = payload.as_bytes();
        let split = b.iter().position(|&c| c == 0xA0).unwrap();
        assert!(s.feed(&b[..split]).is_empty());
        let out = s.feed(&b[split..]);
        assert_eq!(out, vec!["你".to_string()]);
    }

    #[test]
    fn sse_caps_runaway_buffer() {
        // no event boundary ever arrives: the buffer must stay bounded
        let mut s = Sse::default();
        for _ in 0..40 {
            assert!(s.feed(&vec![b'x'; 64 * 1024]).is_empty());
        }
        // after the cap, a fresh event still parses — \n ends the garbage
        // line so "data: ok" is its own SSE field
        let out = s.feed(b"\ndata: ok\n\n");
        assert_eq!(out, vec!["ok".to_string()]);
    }

    #[test]
    fn sse_ignores_non_data_fields_and_picks_up_later_events() {
        let mut s = Sse::default();
        // comments and `event:` lines carry no payload; the block is consumed
        assert!(s.feed(b": keepalive\nevent: ping\n\n").is_empty());
        // ...but the stream keeps working afterwards
        assert_eq!(
            s.feed(b"data: one\ndata: two\n\n"),
            vec!["one\ntwo".to_string()]
        );
    }

    #[test]
    fn sse_waits_for_a_complete_boundary() {
        let mut s = Sse::default();
        // no blank line yet -> nothing emitted, not even on a lone \n
        assert!(s.feed(b"data: {\"a\":1}").is_empty());
        assert!(s.feed(b"\n").is_empty());
        assert_eq!(s.feed(b"\n"), vec![r#"{"a":1}"#.to_string()]);

        // a CRLF boundary split across chunks is still recognised
        let mut s = Sse::default();
        assert!(s.feed(b"data: x\r\n").is_empty());
        assert_eq!(s.feed(b"\r\n"), vec!["x".to_string()]);

        // whitespace after `data:` is a single optional space, extra is kept? no: trimmed
        let mut s = Sse::default();
        assert_eq!(s.feed(b"data:   padded\n\n"), vec!["padded".to_string()]);
    }
}
