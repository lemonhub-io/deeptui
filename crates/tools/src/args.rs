//! Tool arguments as the model sent them. Near-JSON is repaired — weak models
//! put raw newlines inside strings, wrap the object in a markdown fence,
//! encode it twice or trail prose after it — and what cannot be repaired is an
//! error naming the parse failure, so the model fixes its call instead of
//! chasing a misleading `missing <field>`.

use serde_json::Value;

pub struct Args(Value);

impl Args {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        if s.is_empty() {
            return Ok(Args(Value::Object(Default::default())));
        }
        let strict = match serde_json::from_str::<Value>(s) {
            Ok(v) => return Self::object(v),
            Err(e) => e,
        };
        match serde_json::from_str::<Value>(&repair(unfence(s))) {
            Ok(v) => Self::object(v),
            Err(_) => Err(format!(
                "arguments are not valid JSON ({strict}); resend the call"
            )),
        }
    }

    fn object(v: Value) -> Result<Self, String> {
        match v {
            Value::Object(_) => Ok(Args(v)),
            Value::Null => Ok(Args(Value::Object(Default::default()))),
            // an object encoded twice arrives as a JSON string
            Value::String(s) => match serde_json::from_str(&s) {
                Ok(v @ Value::Object(_)) => Ok(Args(v)),
                _ => Err("arguments must be a JSON object".into()),
            },
            _ => Err("arguments must be a JSON object".into()),
        }
    }

    pub fn get(&self, key: &str) -> &Value {
        &self.0[key]
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.0[key].as_str()
    }

    /// A required string: `missing <key>` when absent.
    pub fn need(&self, key: &str) -> Result<&str, String> {
        self.str(key).ok_or_else(|| format!("missing {key}"))
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.0[key].as_u64()
    }
}

impl From<Value> for Args {
    fn from(v: Value) -> Self {
        Args(v)
    }
}

/// The body of a ```` ```json ```` fence, or `s` itself.
fn unfence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let body = rest.split_once('\n').map_or("", |(_, b)| b);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// Escape what JSON forbids inside strings (raw control characters, unknown
/// backslash escapes) and cut anything after the top-level value closes.
fn repair(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let (mut in_str, mut depth) = (false, 0i32);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '"' => in_str = false,
                '\\' => match chars.peek() {
                    Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                        out.push(c);
                        out.extend(chars.next());
                        continue;
                    }
                    // `\d` in a regex, `\'` in code: the backslash is literal
                    _ => {
                        out.push_str("\\\\");
                        continue;
                    }
                },
                '\n' => {
                    out.push_str("\\n");
                    continue;
                }
                '\r' => {
                    out.push_str("\\r");
                    continue;
                }
                '\t' => {
                    out.push_str("\\t");
                    continue;
                }
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                    continue;
                }
                _ => {}
            }
            out.push(c);
            continue;
        }
        out.push(c);
        match c {
            '"' => in_str = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok(raw: &str) -> Value {
        Args::parse(raw)
            .unwrap_or_else(|e| panic!("{raw:?}: {e}"))
            .0
    }

    #[test]
    fn fields_are_read_leniently() {
        let a = Args::parse(r#"{"path":"a.rs","limit":3}"#).unwrap();
        assert_eq!(a.str("path"), Some("a.rs"));
        assert_eq!(a.u64("limit"), Some(3));
        assert_eq!(a.need("cmd").unwrap_err(), "missing cmd");
        // wrong types are absent, not panics
        assert_eq!(a.str("limit"), None);
        assert!(a.get("nope").is_null());
        assert_eq!(Args::from(json!([1, 2])).str("path"), None);
        // no arguments at all is an empty object
        assert_eq!(ok(""), json!({}));
        assert_eq!(ok(" null "), json!({}));
    }

    #[test]
    fn near_json_is_repaired() {
        // raw control characters inside a string
        assert_eq!(
            ok("{\"cmd\":\"echo a\n\tb\"}"),
            json!({"cmd": "echo a\n\tb"})
        );
        // a markdown fence, a double encoding, trailing prose
        assert_eq!(ok("```json\n{\"path\":\"a\"}\n```"), json!({"path": "a"}));
        assert_eq!(ok(r#""{\"path\":\"a\"}""#), json!({"path": "a"}));
        assert_eq!(
            ok(r#"{"path":"a"} I will now read it."#),
            json!({"path": "a"})
        );
        // unknown escapes keep their backslash; valid ones are untouched
        assert_eq!(
            ok(r#"{"pattern":"\d+\.rs","s":"q\"\\n"}"#),
            json!({"pattern": "\\d+\\.rs", "s": "q\"\\n"})
        );
        // braces inside strings do not end the object early
        assert_eq!(
            ok("{\"new\":\"fn a() {\n}\"} junk"),
            json!({"new": "fn a() {\n}"})
        );
    }

    #[test]
    fn unrepairable_arguments_name_the_problem() {
        let err = Args::parse("{oops").err().unwrap();
        assert!(err.starts_with("arguments are not valid JSON ("), "{err}");
        assert!(err.ends_with("resend the call"), "{err}");
        // a truncated string is not guessed at
        assert!(Args::parse(r#"{"content":"half"#).is_err());
        assert_eq!(
            Args::parse("[1,2]").err().unwrap(),
            "arguments must be a JSON object"
        );
        assert_eq!(
            Args::parse(r#""just text""#).err().unwrap(),
            "arguments must be a JSON object"
        );
    }
}
