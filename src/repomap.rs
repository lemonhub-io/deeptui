use ignore::WalkBuilder;
use regex::Regex;
use std::path::{Path, PathBuf};

pub const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", ".next", "vendor",
    "__pycache__", ".venv", "venv", "coverage", ".cache", "out", "bin", "obj",
];

const CODE_EXTS: &[&str] = &[
    "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "cpp", "hpp",
    "cc", "cs", "rb", "php", "swift", "kt", "kts", "scala", "sh", "bash", "lua",
    "zig", "v", "dart", "ex", "exs", "erl", "hs", "ml", "clj", "r", "m", "mm",
];

fn symbol_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^\s*(?:pub(?:\([^)]*\))?\s+|export\s+(?:default\s+)?|async\s+|static\s+|def\s+|public\s+|private\s+|protected\s+)*\
             (?:fn|struct|enum|impl|trait|class|interface|type|func|module|mod|namespace|const|let|def|function)\b[^\n]{0,100}",
        )
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
