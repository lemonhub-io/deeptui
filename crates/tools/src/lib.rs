//! The agent's hands, as data.
//!
//! A tool is one [`Tool`] value that carries everything about it — its name,
//! its schema, the [`Effect`] it has on the world, and its implementation —
//! so adding a tool is one `const` next to its code and one line in
//! [`TOOLS`]. Nothing else keeps a list of names: the engine schedules calls
//! by their declared effect, and the API schemas are generated from the same
//! values.

mod args;
mod fs;
mod map;
mod patch;
mod search;
mod shell;
mod workspace;

pub use args::Args;
pub use workspace::Workspace;

use deeptui_core::Effect;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Ok and Err alike become the tool-result text the model reads.
pub type Output = Result<String, String>;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How a tool runs.
pub enum Exec {
    /// Filesystem work, moved to the blocking pool.
    Blocking(fn(&Workspace, &Args) -> Output),
    /// Work that awaits, such as a child process.
    Async(for<'a> fn(&'a Workspace, &'a Args) -> BoxFuture<'a, Output>),
}

pub struct Tool {
    pub name: &'static str,
    /// Terse on purpose: every word is billed on every request.
    pub about: &'static str,
    /// JSON schema of the arguments.
    pub params: fn() -> Value,
    pub effect: Effect,
    pub exec: Exec,
}

/// Every tool the model can call, in the order their schemas are sent.
pub const TOOLS: &[Tool] = &[
    fs::READ_FILE,
    fs::EDIT_FILE,
    fs::WRITE_FILE,
    shell::BASH,
    search::GREP,
    search::GLOB,
    fs::LS,
    map::REPOMAP,
];

pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The tools bound to one workspace. Cheap to clone into concurrent tasks.
#[derive(Clone)]
pub struct Toolbox {
    ws: Arc<Workspace>,
}

impl Toolbox {
    pub fn new(ws: Workspace) -> Self {
        Self { ws: Arc::new(ws) }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.ws
    }

    /// The `tools` array of a chat request.
    pub fn schemas(&self) -> Vec<Value> {
        TOOLS
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name,
                    "description": t.about,
                    "parameters": (t.params)(),
                }})
            })
            .collect()
    }

    /// What calling `name` does to the world. A tool nobody can vouch for is
    /// treated as a write: it becomes a barrier, never a concurrent peer.
    pub fn effect(&self, name: &str) -> Effect {
        find(name).map_or(Effect::Write, |t| t.effect)
    }

    /// Run one call with the model's raw JSON arguments.
    pub async fn call(&self, name: &str, raw_args: &str) -> Output {
        let Some(tool) = find(name) else {
            let have: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
            return Err(format!("unknown tool: {name} (have: {})", have.join(", ")));
        };
        let args = Args::parse(raw_args)?;
        match tool.exec {
            Exec::Async(run) => run(&self.ws, &args).await,
            Exec::Blocking(run) => {
                let ws = self.ws.clone();
                tokio::task::spawn_blocking(move || run(&ws, &args))
                    .await
                    .map_err(|e| e.to_string())?
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;

    #[test]
    fn schemas_and_effects_cover_every_tool() {
        let t = TmpDir::new("schemas");
        let tb = Toolbox::new(Workspace::new(t.path(), 5_000));
        let specs = tb.schemas();
        let names: Vec<&str> = specs
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "read_file",
                "edit_file",
                "write_file",
                "bash",
                "grep",
                "glob",
                "ls",
                "repomap"
            ]
        );
        for s in &specs {
            assert_eq!(s["type"], "function");
            assert!(!s["function"]["description"].as_str().unwrap().is_empty());
            assert_eq!(s["function"]["parameters"]["type"], "object");
            let req = s["function"]["parameters"]["required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for name in req {
                let p = name.as_str().unwrap();
                assert!(
                    s["function"]["parameters"]["properties"].get(p).is_some(),
                    "required {p} has no schema"
                );
            }
        }
        // observing tools are reads; anything that mutates, executes or is
        // unknown is a write
        for r in ["read_file", "grep", "glob", "ls", "repomap"] {
            assert_eq!(tb.effect(r), Effect::Read, "{r}");
        }
        for w in ["bash", "edit_file", "write_file", "nope"] {
            assert_eq!(tb.effect(w), Effect::Write, "{w}");
        }
    }

    #[tokio::test]
    async fn call_dispatches_every_tool_and_rejects_unknown_names() {
        let t = TmpDir::new("exec");
        let tb = Toolbox::new(Workspace::new(t.path(), 5_000));
        let run = |name: &'static str, args: Value| {
            let tb = tb.clone();
            async move { tb.call(name, &args.to_string()).await }
        };

        run("write_file", json!({"path": "a.txt", "content": "abc"}))
            .await
            .unwrap();
        let out = run("read_file", json!({"path": "a.txt"})).await.unwrap();
        assert_eq!(out.trim_end(), "   1│abc");
        let out = run("ls", json!({})).await.unwrap();
        assert!(out.contains("a.txt (3 B)"), "{out}");
        let out = run("glob", json!({"pattern": "**/*.txt"})).await.unwrap();
        assert_eq!(out, "a.txt\n");
        let out = run("grep", json!({"pattern": "abc"})).await.unwrap();
        assert!(out.contains("a.txt:1: abc"), "{out}");
        let out = run("bash", json!({"cmd": "cat a.txt"})).await.unwrap();
        assert!(out.contains("abc") && out.ends_with("[exit 0]"), "{out}");

        // the skeleton lists code files, and an empty project has none
        let out = run("repomap", json!({})).await.unwrap();
        assert_eq!(out, "a.txt\n");
        t.write("src/lib.rs", "pub fn a() {}\n");
        let out = run("repomap", json!({})).await.unwrap();
        assert!(out.contains("src/lib.rs") && out.contains("fn a"), "{out}");
        let empty = TmpDir::new("exec-empty");
        let err = Toolbox::new(Workspace::new(empty.path(), 5_000))
            .call("repomap", "{}")
            .await
            .unwrap_err();
        assert_eq!(err, "empty project");

        let err = run("nope", json!({})).await.unwrap_err();
        assert!(
            err.starts_with("unknown tool: nope (have: read_file, edit_file,"),
            "{err}"
        );
        // malformed arguments say so, instead of reading as an empty object
        // whose first required field then goes "missing"
        let err = tb.call("grep", "{not json").await.unwrap_err();
        assert!(err.starts_with("arguments are not valid JSON"), "{err}");
        // near-JSON is repaired and runs
        let out = tb.call("bash", "{\"cmd\":\"echo 'a\nb'\"}").await.unwrap();
        assert!(out.starts_with("a\nb\n"), "{out}");
    }
}
