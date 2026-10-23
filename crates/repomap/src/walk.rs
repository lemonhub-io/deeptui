//! The project walk every listing shares — the repo map, `grep`, `glob` — so
//! all of them agree on what "the project" is: `.gitignore` is honoured,
//! dotfiles are not hidden, and vendor/build directories are never entered.

use ignore::{Walk, WalkBuilder};
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

/// Walk `dir` under the project policy. Yields `dir` itself first.
pub fn walk(dir: &Path) -> Walk {
    WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(|e| {
            e.depth() == 0 || !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .build()
}

/// Relative paths of the files under `root`, at most `cap`, sorted.
pub fn walk_files(root: &Path, cap: usize) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for e in walk(root).flatten() {
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
