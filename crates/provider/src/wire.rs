//! The OpenAI-compatible chunk format, decoded into provider-neutral deltas.

use deeptui_core::{Delta, Usage};
use serde_json::Value;

/// The payload that ends a completion stream; a transport sentinel, not data.
pub const DONE: &str = "[DONE]";

/// Decode one SSE `data:` payload. Unparseable JSON yields nothing.
pub fn decode(data: &str) -> Vec<Delta> {
    let mut out = Vec::new();
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
        out.push(Delta::Usage(Usage {
            input: u["prompt_tokens"].as_u64().unwrap_or(0),
            output: u["completion_tokens"].as_u64().unwrap_or(0),
            cached,
            cost: 0.0,
        }));
    }
    if let Some(err) = v.get("error") {
        out.push(Delta::Error(
            err["message"].as_str().unwrap_or("?").to_string(),
        ));
        return out;
    }
    for ch in v["choices"].as_array().into_iter().flatten() {
        let d = &ch["delta"];
        if let Some(c) = d["content"].as_str() {
            if !c.is_empty() {
                out.push(Delta::Text(c.to_string()));
            }
        }
        // reasoning deltas are skipped on purpose: cost transparency, quiet screen
        for tc in d["tool_calls"].as_array().into_iter().flatten() {
            out.push(Delta::Call {
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
            out.push(Delta::Finish(f.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, cached: u64) -> Delta {
        Delta::Usage(Usage {
            input,
            output,
            cached,
            cost: 0.0,
        })
    }

    #[test]
    fn parses_text_delta() {
        let evs = decode(r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#);
        assert_eq!(evs, [Delta::Text("hi".into())]);
    }

    #[test]
    fn parses_tool_delta() {
        let evs = decode(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"cm"}}]},"finish_reason":null}]}"#,
        );
        assert!(matches!(&evs[0], Delta::Call { idx: 0, args, .. } if args == "{\"cm"));
    }

    #[test]
    fn ignores_empty_and_unparseable_payloads() {
        assert!(decode("").is_empty());
        assert!(decode("not json").is_empty());
        // the stream sentinel is the transport's business, not a delta
        assert!(decode(DONE).is_empty());
        // an empty content delta and an empty tool_calls array yield nothing
        assert!(decode(
            r#"{"choices":[{"delta":{"content":"","tool_calls":[]},"finish_reason":null}]}"#
        )
        .is_empty());
        // a null finish_reason is not a Finish delta
        assert!(decode(r#"{"choices":[{"delta":{},"finish_reason":null}]}"#).is_empty());
    }

    #[test]
    fn parses_multiple_choices_in_order() {
        let evs = decode(
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":null},{"delta":{"content":"b"},"finish_reason":"stop"}]}"#,
        );
        assert_eq!(
            evs,
            [
                Delta::Text("a".into()),
                Delta::Text("b".into()),
                Delta::Finish("stop".into())
            ]
        );
    }

    #[test]
    fn parses_tool_delta_without_id_or_args() {
        let evs = decode(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"grep"}}]},"finish_reason":null}]}"#,
        );
        assert_eq!(
            evs,
            [Delta::Call {
                idx: 2,
                id: None,
                name: Some("grep".into()),
                args: String::new(),
            }]
        );
    }

    #[test]
    fn surfaces_api_errors() {
        let evs = decode(r#"{"error":{"message":"bad key"}}"#);
        assert_eq!(evs, [Delta::Error("bad key".into())]);
        // a message-less error is still an error
        assert_eq!(decode(r#"{"error":{}}"#), [Delta::Error("?".into())]);
    }

    #[test]
    fn usage_prefers_details_over_deepseek_fallback() {
        let evs = decode(
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":1,"prompt_cache_hit_tokens":7}}"#,
        );
        assert_eq!(evs, [usage(9, 1, 7)]);
        let evs = decode(
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":3},"prompt_cache_hit_tokens":7}}"#,
        );
        assert_eq!(evs, [usage(9, 1, 3)]);
    }

    #[test]
    fn parses_usage() {
        let evs = decode(
            r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":64}}}"#,
        );
        assert_eq!(evs, [usage(100, 10, 64)]);
    }
}
