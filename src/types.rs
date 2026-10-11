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
        Self { role: "system".into(), content: Some(s.into()), ..Default::default() }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(s.into()), ..Default::default() }
    }
    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
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
