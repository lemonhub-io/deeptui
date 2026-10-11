use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FnCall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FnCall {
    pub name: String,
    /// JSON-encoded arguments (providers stream this as a string).
    pub arguments: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Length of the original content before elision (bookkeeping only).
    #[serde(skip)]
    pub orig_len: usize,
}

impl Msg {
    pub fn system(s: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(s.into()),
            ..Default::default()
        }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(s.into()),
            ..Default::default()
        }
    }
    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            ..Default::default()
        }
    }
    pub fn tool(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            tool_call_id: Some(id.into()),
            content: Some(content.into()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc() -> ToolCall {
        ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FnCall {
                name: "bash".into(),
                arguments: "{\"cmd\":\"ls\"}".into(),
            },
        }
    }

    #[test]
    fn constructors_set_only_the_relevant_fields() {
        let s = Msg::system("sys");
        assert_eq!(s.role, "system");
        assert_eq!(s.content.as_deref(), Some("sys"));
        assert!(s.tool_calls.is_none() && s.tool_call_id.is_none());

        let u = Msg::user("hi");
        assert_eq!(
            (u.role.as_str(), u.content.as_deref()),
            ("user", Some("hi"))
        );

        // an empty call list collapses to None: the API rejects `[]`
        let a = Msg::assistant(Some("text".into()), vec![]);
        assert_eq!(a.role, "assistant");
        assert!(a.tool_calls.is_none());
        let a = Msg::assistant(None, vec![tc()]);
        assert!(a.content.is_none());
        assert_eq!(a.tool_calls.as_ref().unwrap()[0].function.name, "bash");

        let t = Msg::tool("c1", "out");
        assert_eq!(t.role, "tool");
        assert_eq!(t.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(t.content.as_deref(), Some("out"));
    }

    #[test]
    fn wire_format_omits_absent_fields_and_the_bookkeeping_len() {
        let a = Msg::assistant(Some("done".into()), vec![]);
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"role": "assistant", "content": "done"})
        );

        let mut t = Msg::tool("c1", "out");
        t.orig_len = 999; // bookkeeping must never reach the wire
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"role": "tool", "content": "out", "tool_call_id": "c1"})
        );
    }

    #[test]
    fn tool_calls_round_trip_through_json() {
        let a = Msg::assistant(None, vec![tc()]);
        let wire = serde_json::to_string(&a).unwrap();
        assert!(wire.contains("\"type\":\"function\""), "{wire}");
        let back: Msg = serde_json::from_str(&wire).unwrap();
        let calls = back.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.arguments, "{\"cmd\":\"ls\"}");
        assert_eq!(back.orig_len, 0); // skipped on deserialize as well
    }

    #[test]
    fn deserializes_a_provider_style_payload() {
        let m: Msg = serde_json::from_str(
            r#"{"role":"assistant","content":null,"tool_calls":[{"id":"x","type":"function","function":{"name":"grep","arguments":"{}"}}]}"#,
        )
        .unwrap();
        assert!(m.content.is_none());
        assert_eq!(m.tool_calls.unwrap()[0].function.name, "grep");
        // a plain content message leaves the call fields empty (not null)
        let m: Msg = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert!(m.tool_calls.is_none() && m.tool_call_id.is_none());
    }
}
