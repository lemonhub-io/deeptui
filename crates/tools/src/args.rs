//! Tool arguments as the model sent them, read leniently: malformed JSON is an
//! empty object, and a missing field names itself in the error the model sees.

use serde_json::Value;

pub struct Args(Value);

impl Args {
    pub fn parse(raw: &str) -> Self {
        Args(serde_json::from_str(raw).unwrap_or_else(|_| Value::Object(Default::default())))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fields_are_read_leniently() {
        let a = Args::parse(r#"{"path":"a.rs","limit":3}"#);
        assert_eq!(a.str("path"), Some("a.rs"));
        assert_eq!(a.u64("limit"), Some(3));
        assert_eq!(a.need("cmd").unwrap_err(), "missing cmd");
        // wrong types are absent, not panics
        assert_eq!(a.str("limit"), None);
        assert!(a.get("nope").is_null());
        // garbage and non-objects degrade to "nothing given"
        assert_eq!(Args::parse("{oops").str("path"), None);
        assert_eq!(Args::from(json!([1, 2])).str("path"), None);
    }
}
