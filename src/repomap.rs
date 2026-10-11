use ignore::WalkBuilder;
use regex::Regex;
use std::path::{Path, PathBuf};

pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    "coverage",
    ".cache",
    "out",
    "bin",
    "obj",
];

const CODE_EXTS: &[&str] = &[
    "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "cpp", "hpp", "cc", "cs", "rb",
    "php", "swift", "kt", "kts", "scala", "sh", "bash", "lua", "zig", "v", "dart", "ex", "exs",
    "erl", "hs", "ml", "clj", "r", "m", "mm",
];

fn symbol_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // NB: a trailing `\` inside a raw string is a literal backslash, not
        // a line continuation — `concat!` is what joins these two halves.
        Regex::new(concat!(
            r"^\s*(?:pub(?:\([^)]*\))?\s+|export\s+(?:default\s+)?|async\s+|static\s+|def\s+|public\s+|private\s+|protected\s+)*",
            r"(?:fn|struct|enum|impl|trait|class|interface|type|func|module|mod|namespace|const|let|def|function)\b[^\n]{0,100}",
        ))
        .unwrap()
    })
}

/// Walk the project respecting .gitignore. Returns relative paths.
pub fn walk_files(root: &Path, cap: usize) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for e in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(|e| {
            e.depth() == 0 || !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .build()
        .flatten()
    {
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if let Ok(rel) = e.path().strip_prefix(root) {
                files.push(rel.to_path_buf());
            }
            if files.len() >= cap {
                break;
            }
        }
    }
    files.sort();
    files
}

/// Condensed skeleton of the repo: file paths + top-level symbol lines.
/// Injected into the system prompt so the model gets repo awareness for a
/// few hundred tokens instead of reading dozens of files.
pub fn build(root: &Path, max_bytes: usize) -> String {
    let files = walk_files(root, 500);
    if files.is_empty() {
        return String::new();
    }
    let re = symbol_re();
    let mut out = String::new();
    for rel in &files {
        out.push_str(&rel.display().to_string());
        out.push('\n');
        let is_code = rel
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| CODE_EXTS.contains(&e))
            .unwrap_or(false);
        if is_code && out.len() < max_bytes * 2 / 3 {
            if let Ok(content) = std::fs::read_to_string(root.join(rel)) {
                let mut n = 0;
                for line in content.lines() {
                    if re.is_match(line) {
                        let t = line.trim();
                        let t: String = t.chars().take(110).collect();
                        out.push_str("    ");
                        out.push_str(&t);
                        out.push('\n');
                        n += 1;
                        if n >= 25 {
                            out.push_str("    ...\n");
                            break;
                        }
                    }
                }
            }
        }
        if out.len() >= max_bytes {
            // out of budget: append the remaining file list only
            for r in files.iter().skip_while(|f| *f != rel).skip(1) {
                out.push_str(&r.display().to_string());
                out.push('\n');
                if out.len() >= max_bytes + 4000 {
                    out.push_str("...\n");
                    break;
                }
            }
            break;
        }
    }
    out.truncate(max_bytes + 4000);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;

    #[test]
    fn walk_files_is_sorted_and_skips_vendor_dirs() {
        let t = TmpDir::new("walk");
        t.write("b.py", "");
        t.write("a.rs", "");
        t.write("node_modules/dep/index.js", "");
        t.write("target/debug/build", "");
        t.write("src/deep/c.rs", "");
        let files = walk_files(t.path(), 100);
        let rel: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(rel, ["a.rs", "b.py", "src/deep/c.rs"]);
    }

    #[test]
    fn walk_files_honours_the_cap() {
        let t = TmpDir::new("walk-cap");
        for i in 0..10 {
            t.write(&format!("f{i}.rs"), "");
        }
        assert_eq!(walk_files(t.path(), 4).len(), 4);
        // the cap is checked after each push, so 0 still yields the first file
        assert_eq!(walk_files(t.path(), 0).len(), 1);
    }

    #[test]
    fn build_lists_paths_and_symbols_for_code_only() {
        let t = TmpDir::new("map");
        t.write(
            "src/main.rs",
            "use x;\npub fn one() {}\n\nstruct Two;\n    impl Two {}\nfn hidden() {}\n",
        );
        t.write("README.md", "function not_code() {}\n");
        let map = build(t.path(), 6000);
        assert!(map.starts_with("README.md\nsrc/main.rs\n"), "{map}");
        // every symbol line from the .rs file is indented under its path
        for sym in [
            "pub fn one() {}",
            "struct Two;",
            "impl Two {}",
            "fn hidden() {}",
        ] {
            assert!(
                map.contains(&format!("    {sym}\n")),
                "missing {sym} in {map}"
            );
        }
        // non-code extensions contribute no symbols
        assert!(!map.contains("not_code"), "{map}");
    }

    #[test]
    fn build_stops_at_25_symbols_per_file_and_marks_the_rest() {
        let t = TmpDir::new("map-cap");
        let body: String = (0..30).map(|i| format!("fn f{i}() {{}}\n")).collect();
        t.write("many.rs", &body);
        let map = build(t.path(), 6000);
        assert_eq!(map.matches("    fn f").count(), 25);
        assert!(map.contains("    ...\n"), "{map}");
        assert!(!map.contains("fn f29"), "{map}");
    }

    #[test]
    fn symbol_scan_covers_the_common_languages() {
        let t = TmpDir::new("map-langs");
        t.write(
            "app.py",
            "import os\nclass A:\n    def method(self):\n        pass\n    async def other(self):\n        pass\nX = 1\n",
        );
        t.write(
            "app.js",
            "export function two() {}\nconst arrow = () => {};\nclass C {}\nlet loose = 1;\n",
        );
        t.write("main.go", "package main\nfunc Go() {}\ntype T struct{}\n");
        t.write(
            "lib.rs",
            "pub(crate) fn hidden() {}\nimpl A {\n    fn m(&self) {}\n}\n",
        );
        let map = build(t.path(), 6000);
        for sym in [
            "class A:",
            "def method(self):",
            "async def other(self):",
            "export function two() {}",
            "const arrow = () => {};",
            "class C {}",
            "let loose = 1;",
            "func Go() {}",
            "type T struct{}",
            "pub(crate) fn hidden() {}",
            "impl A {",
            "fn m(&self) {}",
        ] {
            assert!(
                map.contains(&format!("    {sym}\n")),
                "missing {sym} in:\n{map}"
            );
        }
        // imports, statements and assignments are not symbols
        for noise in ["X = 1", "import os", "package main", "        pass"] {
            assert!(!map.contains(noise), "{noise} leaked into:\n{map}");
        }
    }

    #[test]
    fn over_long_symbol_lines_are_clipped() {
        let t = TmpDir::new("map-clip");
        let name = format!("{}{}", "n".repeat(120), "m".repeat(120));
        t.write("a.rs", &format!("fn {name}() {{}}\n"));
        let map = build(t.path(), 6000);
        let line = map
            .lines()
            .find(|l| l.starts_with("    fn "))
            .expect("no symbol");
        assert_eq!(line.chars().count(), 4 + 110);
        assert!(!line.contains('m'), "tail kept: {line}");
    }

    #[test]
    fn walk_files_respects_gitignore_inside_a_repo() {
        let t = TmpDir::new("map-git");
        std::fs::create_dir_all(t.join(".git")).unwrap();
        t.write(".gitignore", "secret.txt\nignored/\n");
        t.write("kept.rs", "fn a() {}\n");
        t.write("secret.txt", "fn hidden() {}\n");
        t.write("ignored/deep.rs", "fn deeper() {}\n");
        let rel: Vec<String> = walk_files(t.path(), 100)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert!(rel.contains(&"kept.rs".to_string()), "{rel:?}");
        assert!(
            !rel.iter()
                .any(|p| p.contains("secret") || p.contains("ignored")),
            "gitignore not honoured: {rel:?}"
        );
    }

    #[test]
    fn build_is_empty_for_an_empty_dir() {
        let t = TmpDir::new("map-empty");
        assert_eq!(build(t.path(), 6000), "");
    }

    #[test]
    fn build_stops_listing_once_the_byte_budget_is_spent() {
        let t = TmpDir::new("map-bytes");
        let stem = "x".repeat(120);
        for i in 0..60 {
            t.write(&format!("some/deeply/nested/path/{stem}_{i}.rs"), "");
        }
        let map = build(t.path(), 300);
        assert!(!map.is_empty());
        // paths flush past the budget in one go, but never by more than 4 KiB
        assert!(map.len() <= 300 + 4000, "{} bytes", map.len());
        // the full listing would be ~9.7 KiB, so it must be cut short
        let full: usize = (0..60)
            .map(|i| format!("some/deeply/nested/path/{stem}_{i}.rs\n").len())
            .sum();
        assert!(full > 9000, "{full}");
        assert!(map.len() < full);
        assert!(!map.contains("_59.rs"), "tail should be dropped");
    }
}
