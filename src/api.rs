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
#[derive(Default)]
pub struct Sse {
    buf: String,
}

impl Sse {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..end + 2).collect();
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
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            c = stream.next() => match c {
                Some(Ok(b)) => b,
                Some(Err(e)) => bail!("stream error: {e}"),
                None => break,
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
}
