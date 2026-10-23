//! The system prompt: a fixed rulebook, the project root, an optional repo
//! map, then whatever instruction files the project carries.

use std::path::Path;

const RULES: &str = "\
You are DeepTUI, an autonomous coding agent in a terminal. Solve the task with tools, then stop.

Rules:
- Prefer edit_file (search/replace) over write_file for existing files.
- Batch independent read/grep/glob calls in one reply.
- Never restate file contents to the user; cite paths and line numbers.
- Keep final answers under 8 lines unless asked for more.
- After changing code, run the relevant build/test command once.
- Don't guess paths: ls/glob/grep first. Don't stop until the task is done.";

/// Instruction files read from the root, in this order, each capped.
const INSTRUCTION_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", ".deeptui/rules.md"];
const INSTRUCTION_CHARS: usize = 4000;

/// `map_bytes` sizes the injected repo map; `None` leaves it out.
pub fn system_prompt(root: &Path, map_bytes: Option<usize>) -> String {
    let mut s = RULES.to_string();
    s.push_str(&format!("\nProject root: {}", root.display()));
    if let Some(bytes) = map_bytes {
        let map = deeptui_repomap::build(root, bytes);
        if !map.is_empty() {
            s.push_str("\n\n## Project map\n");
            s.push_str(&map);
        }
    }
    for f in INSTRUCTION_FILES {
        if let Ok(c) = std::fs::read_to_string(root.join(f)) {
            let c: String = c.chars().take(INSTRUCTION_CHARS).collect();
            s.push_str(&format!("\n\n## {f}\n{c}"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_repomap as repomap;
    use deeptui_testkit::TmpDir;

    #[test]
    fn prompt_names_the_root_and_the_rules() {
        let t = TmpDir::new("prompt");
        let p = system_prompt(t.path(), None);
        assert!(p.starts_with("You are DeepTUI"));
        assert!(p.contains(&format!("Project root: {}", t.path().display())));
        assert!(p.contains("Prefer edit_file"));
        assert!(!p.contains("## Project map"));
    }

    #[test]
    fn project_map_is_optional_and_contains_paths() {
        let t = TmpDir::new("prompt-map");
        t.write("src/lib.rs", "pub fn exported() {}\n");
        let with = system_prompt(t.path(), Some(5_000));
        assert!(with.contains("## Project map"), "{with}");
        assert!(with.contains("src/lib.rs"));
        assert!(with.contains("    pub fn exported() {}"), "{with}");
        // dropping the map loses the repo skeleton but keeps the rules
        let without = system_prompt(t.path(), None);
        assert!(!without.contains("## Project map"));
        assert!(without.contains("Prefer edit_file"));
        assert!(!without.contains("src/lib.rs"));
    }

    #[test]
    fn the_injected_map_respects_the_configured_byte_budget() {
        let t = TmpDir::new("prompt-budget");
        let body: String = (0..14)
            .map(|i| format!("pub fn function_{i}_name() {{}}\n"))
            .collect();
        for i in 0..8 {
            t.write(&format!("src/m{i}.rs"), &body);
        }
        let big = system_prompt(t.path(), Some(repomap::DEFAULT_BUDGET));
        let small = system_prompt(t.path(), Some(repomap::MIN_BUDGET));
        // the default budget maps every file completely...
        assert_eq!(big.matches("    pub fn ").count(), 8 * 14);
        // ...while a tight one degrades into fewer symbols, not fewer files
        assert!(small.matches("    pub fn ").count() < 8 * 14);
        assert!(small.len() < big.len(), "the budget was ignored");
        assert!(small.contains("src/m7.rs"), "the tree must survive");
    }

    #[test]
    fn instruction_files_are_appended_in_order() {
        let t = TmpDir::new("prompt-files");
        t.write("AGENTS.md", "agents rules");
        t.write("CLAUDE.md", "claude rules");
        t.write(".deeptui/rules.md", "local rules");
        let p = system_prompt(t.path(), None);
        let a = p
            .find("## AGENTS.md\nagents rules")
            .expect("AGENTS.md missing");
        let c = p
            .find("## CLAUDE.md\nclaude rules")
            .expect("CLAUDE.md missing");
        let r = p
            .find("## .deeptui/rules.md\nlocal rules")
            .expect(".deeptui/rules.md missing");
        assert!(a < c && c < r, "wrong order");
    }

    #[test]
    fn instruction_files_are_capped_and_missing_ones_skipped() {
        let t = TmpDir::new("prompt-cap");
        let p = system_prompt(t.path(), Some(5_000));
        assert!(!p.contains("## AGENTS.md"));
        assert!(!p.contains("## CLAUDE.md"));
        assert!(!p.contains("## .deeptui/rules.md"));

        // a directory named AGENTS.md must not blow up the prompt builder
        std::fs::create_dir_all(t.join("AGENTS.md")).unwrap();
        assert!(!system_prompt(t.path(), None).contains("## AGENTS.md"));
        std::fs::remove_dir_all(t.join("AGENTS.md")).unwrap();

        t.write("AGENTS.md", &"x".repeat(5000));
        let p = system_prompt(t.path(), None);
        let body = p.split("## AGENTS.md\n").nth(1).unwrap();
        assert_eq!(body.chars().count(), 4000);
        assert_eq!(body, "x".repeat(4000));
    }
}
