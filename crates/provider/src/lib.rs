//! An OpenAI-compatible chat-completions client, reduced to two verbs:
//! [`Provider::request`] freezes a history into a body, and
//! [`Provider::open`] turns that body into a [`Completion`] — a pull-based
//! stream of provider-neutral [`Delta`]s.
//!
//! Cancellation is deliberately not this crate's concern: dropping the `open`
//! future or the `Completion` aborts the request, which is all a caller needs.

mod sse;
pub mod wire;

pub use sse::Sse;

use anyhow::{bail, Context, Result};
use deeptui_core::text::one_line;
use deeptui_core::{Delta, Msg};
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Duration;

/// Where completions come from.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Without a trailing slash, e.g. `https://api.deepseek.com/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_output_tokens: u32,
}

/// A hung connection (socket open, no bytes) must not stall the turn forever.
const STALL: Duration = Duration::from_secs(120);

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: &'a [Msg],
    tools: &'a [Value],
    stream: bool,
    stream_options: StreamOptions,
    max_completion_tokens: u32,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// A serialized request: built once, re-sent verbatim on a retry.
pub struct Request(Vec<u8>);

pub struct Provider {
    http: reqwest::Client,
    ep: Endpoint,
}

impl Provider {
    pub fn new(ep: Endpoint) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { http, ep })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.ep
    }

    pub fn request(&self, messages: &[Msg], tools: &[Value]) -> Request {
        let body = Body {
            model: &self.ep.model,
            messages,
            tools,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            max_completion_tokens: self.ep.max_output_tokens,
        };
        Request(serde_json::to_vec(&body).expect("messages and schemas always serialize"))
    }

    /// Send `req` and return the response stream once it is flowing. Rate
    /// limits and 5xx are retried with backoff, connect failures briefly;
    /// any other HTTP error fails fast with the body flattened to one line.
    pub async fn open(&self, req: &Request) -> Result<Completion> {
        let url = format!("{}/chat/completions", self.ep.base_url);
        let mut attempt = 0u32;
        loop {
            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.ep.api_key)
                .header(CONTENT_TYPE, "application/json")
                .body(req.0.clone())
                .send()
                .await;
            match sent {
                Ok(resp) if resp.status().is_success() => return Ok(Completion::new(resp)),
                Ok(resp) => {
                    let status = resp.status();
                    let txt = resp.text().await.unwrap_or_default();
                    let retriable = status.as_u16() == 429 || status.is_server_error();
                    if retriable && attempt < 3 {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(400 * (1 << attempt))).await;
                        continue;
                    }
                    bail!("HTTP {}: {}", status, one_line(&txt, 300));
                }
                Err(e) => {
                    if attempt < 2 && (e.is_connect() || e.is_timeout()) {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        continue;
                    }
                    return Err(e).context("request failed");
                }
            }
        }
    }
}

/// One streaming completion, pulled delta by delta.
pub struct Completion {
    resp: reqwest::Response,
    sse: Sse,
    queue: VecDeque<Delta>,
    done: bool,
}

impl Completion {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            resp,
            sse: Sse::default(),
            queue: VecDeque::new(),
            done: false,
        }
    }

    /// The next delta; `None` once the provider sends `[DONE]` or hangs up.
    pub async fn next(&mut self) -> Result<Option<Delta>> {
        loop {
            if let Some(d) = self.queue.pop_front() {
                return Ok(Some(d));
            }
            if self.done {
                return Ok(None);
            }
            let chunk = match tokio::time::timeout(STALL, self.resp.chunk()).await {
                Err(_) => bail!("stream stalled for {STALL:?}"),
                Ok(Err(e)) => bail!("stream error: {e}"),
                Ok(Ok(None)) => {
                    self.done = true;
                    continue;
                }
                Ok(Ok(Some(b))) => b,
            };
            for data in self.sse.feed(&chunk) {
                if data.trim() == wire::DONE {
                    self.done = true;
                    break;
                }
                self.queue.extend(wire::decode(&data));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::{sse, Stub};

    fn provider(stub: &Stub) -> Provider {
        Provider::new(Endpoint {
            api_key: "test-key".into(),
            base_url: stub.base_url(),
            model: "test-model".into(),
            max_output_tokens: 64,
        })
        .unwrap()
    }

    /// Compact rendering of one delta, for order-sensitive assertions.
    fn fmt(d: &Delta) -> String {
        match d {
            Delta::Text(t) => format!("text:{t}"),
            Delta::Call {
                idx,
                id,
                name,
                args,
            } => format!(
                "tc:{idx}:{}:{}:{args}",
                id.clone().unwrap_or_default(),
                name.clone().unwrap_or_default()
            ),
            Delta::Usage(u) => format!("usage:{}:{}:{}", u.input, u.output, u.cached),
            Delta::Finish(f) => format!("finish:{f}"),
            Delta::Error(e) => format!("error:{e}"),
        }
    }

    async fn drain(p: &Provider, msgs: &[Msg], tools: &[Value]) -> Result<Vec<String>> {
        let mut c = p.open(&p.request(msgs, tools)).await?;
        let mut out = Vec::new();
        while let Some(d) = c.next().await? {
            out.push(fmt(&d));
        }
        Ok(out)
    }

    #[tokio::test]
    async fn open_sends_a_valid_request_and_streams_deltas() {
        let stub = Stub::new();
        stub.enqueue(
            200,
            sse(&[
                r#"{"choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
                r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
                r#"{"usage":{"prompt_tokens":11,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":4}}}"#,
                "[DONE]",
                r#"{"choices":[{"delta":{"content":"after the end"}}]}"#,
            ]),
        );
        let p = provider(&stub);
        let msgs = vec![Msg::system("sys"), Msg::user("hi")];
        let tools = vec![serde_json::json!({"type":"function"})];
        let got = drain(&p, &msgs, &tools).await.unwrap();
        // [DONE] ends the completion: nothing after it is a delta
        assert_eq!(got, ["text:hel", "text:lo", "finish:stop", "usage:11:3:4"]);

        // the request carries the model, tools, streaming flags and the key
        let req = stub.requests().into_iter().next().expect("no request");
        let lower = req.to_lowercase();
        assert!(req.starts_with("POST /v1/chat/completions"), "{req}");
        assert!(lower.contains("bearer test-key"), "{req}");
        assert!(lower.contains("content-type: application/json"), "{req}");
        for needle in [
            "\"model\":\"test-model\"",
            "\"stream\":true",
            "\"include_usage\":true",
            "\"max_completion_tokens\":64",
            "\"role\":\"user\"",
            "\"content\":\"hi\"",
            "\"tools\":[{\"type\":\"function\"}]",
        ] {
            assert!(req.contains(needle), "missing {needle} in {req}");
        }
    }

    #[tokio::test]
    async fn a_stream_without_done_ends_when_the_body_does() {
        let stub = Stub::new();
        stub.enqueue(200, sse(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#]));
        let got = drain(&provider(&stub), &[], &[]).await.unwrap();
        assert_eq!(got, ["text:ok"]);
    }

    #[tokio::test]
    async fn open_retries_rate_limits_then_succeeds() {
        let stub = Stub::new();
        stub.enqueue(429, "slow down");
        stub.enqueue(
            200,
            sse(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#, "[DONE]"]),
        );
        let got = drain(&provider(&stub), &[], &[]).await.unwrap();
        assert_eq!(got, ["text:ok"]);

        let reqs = stub.requests();
        assert_eq!(reqs.len(), 2, "retry must resend, got {reqs:#?}");
        let body = |r: &String| r.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        assert_eq!(body(&reqs[0]), body(&reqs[1]));
    }

    #[tokio::test]
    async fn open_fails_fast_on_client_errors() {
        let stub = Stub::new();
        stub.enqueue(400, "bad\napi\nkey");
        let err = drain(&provider(&stub), &[], &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 400"), "{err}");
        // the body is flattened to one line so the UI stays readable
        assert!(err.contains("bad⏎api⏎key"), "{err}");
        assert_eq!(stub.requests().len(), 1, "4xx must not be retried");
    }
}
