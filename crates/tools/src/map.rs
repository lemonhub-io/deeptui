//! `repomap` as a tool: the injected skeleton, on demand, for any subtree.

use crate::{Args, Exec, Output, Tool, Workspace};
use deeptui_core::Effect;
use deeptui_repomap as repomap;
use serde_json::json;

const DEFAULT_BUDGET: usize = 6_000;

pub const REPOMAP: Tool = Tool {
    name: "repomap",
    about: "Condensed repo skeleton: file tree + top-level symbols, ranked by importance. Cheap overview.",
    params: || {
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"subtree to map, default root"},
            "budget":{"type":"integer","description":"bytes of output, default 6000, max 60000"}}})
    },
    effect: Effect::Read,
    exec: Exec::Blocking(map),
};

/// The budget is clamped, so a runaway request can never flood the context.
fn map(ws: &Workspace, args: &Args) -> Output {
    let scoped = args.str("path").map(str::trim).filter(|p| !p.is_empty());
    let dir = ws.dir(scoped)?;
    if !dir.is_dir() {
        return Err(format!("{}: not a directory", ws.rel(&dir)));
    }
    let budget = match args.u64("budget") {
        Some(b) => (b as usize).clamp(repomap::MIN_BUDGET, repomap::MAX_BUDGET),
        None => DEFAULT_BUDGET,
    };
    let map = repomap::build(&dir, budget);
    if map.is_empty() {
        return Err(match scoped {
            Some(p) => format!("{p}: no files"),
            None => "empty project".into(),
        });
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;
    use serde_json::Value;

    fn run(t: &TmpDir, v: Value) -> Output {
        map(&Workspace::new(t.path(), 5_000), &Args::from(v))
    }

    #[test]
    fn repomap_clamps_its_budget_and_reports_bad_scopes() {
        let t = TmpDir::new("repomap-args");
        let body: String = (0..60).map(|i| format!("fn f{i}() {{}}\n")).collect();
        t.write("src/lib.rs", &body);

        // a huge budget is clamped to the tool maximum
        let out = run(&t, json!({"budget": 10_000_000})).unwrap();
        assert!(out.len() <= repomap::MAX_BUDGET, "{} bytes", out.len());
        assert!(out.contains("fn f24"), "{out}");
        assert_eq!(
            out.matches("    fn ").count(),
            repomap::MAX_SYMBOLS_PER_FILE
        );

        // a tiny one is raised to the floor, and still lists the tree
        let out = run(&t, json!({"budget": 1})).unwrap();
        assert!(out.starts_with("src/lib.rs\n"), "{out}");
        assert!(out.len() <= repomap::MIN_BUDGET, "{} bytes", out.len());

        // scoping narrows the tree; the budget is what makes it a summary
        let out = run(&t, json!({"path": "src", "budget": 900})).unwrap();
        assert!(out.starts_with("lib.rs\n"), "{out}");
        assert!(!out.contains("src/lib.rs"), "{out}");
        assert!(out.contains("    ...\n"), "{out}");
        assert!(out.len() <= 900, "{} bytes", out.len());

        // a path that escapes the root, and a file that is not a directory
        assert!(run(&t, json!({"path": "../x"}))
            .unwrap_err()
            .contains("escapes project root"));
        assert_eq!(
            run(&t, json!({"path": "src/lib.rs"})).unwrap_err(),
            "src/lib.rs: not a directory"
        );
        // a directory with no files is not an error, it is an empty map
        std::fs::create_dir_all(t.join("hollow")).unwrap();
        assert_eq!(
            run(&t, json!({"path": "hollow"})).unwrap_err(),
            "hollow: no files"
        );
        // an absent budget, an empty scope and a stray space all mean "defaults"
        assert_eq!(
            run(&t, json!({"path": "  "})).unwrap(),
            run(&t, json!({})).unwrap()
        );
    }

    #[test]
    fn a_scope_with_an_explicit_budget_maps_just_that_subtree() {
        let t = TmpDir::new("repomap-scope");
        t.write("src/lib.rs", "pub fn a() {}\n");
        t.write("other.txt", "");
        let out = run(&t, json!({"path": "src", "budget": 500})).unwrap();
        assert_eq!(out, "lib.rs\n    pub fn a() {}\n");
    }
}
