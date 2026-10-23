//! The project root, as a jail for relative paths.

use std::path::{Component, Path, PathBuf};

pub struct Workspace {
    root: PathBuf,
    bash_timeout_ms: u64,
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>, bash_timeout_ms: u64) -> Self {
        Self {
            root: root.into(),
            bash_timeout_ms,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Default for `bash` when the call names no timeout of its own.
    pub fn bash_timeout_ms(&self) -> u64 {
        self.bash_timeout_ms
    }

    /// A path argument made absolute. Relative paths may not climb out of
    /// the root; explicit absolute paths are honoured — the guardrail exists
    /// for relative-path mistakes, and bash is unrestricted anyway.
    pub fn resolve(&self, p: &str) -> Result<PathBuf, String> {
        let p = p.trim();
        if p.is_empty() {
            return Err("empty path".into());
        }
        let cand = PathBuf::from(p);
        if cand.is_absolute() {
            return Ok(normalize(&cand));
        }
        let norm = normalize(&self.root.join(cand));
        if !norm.starts_with(&self.root) {
            return Err(format!("path escapes project root: {p}"));
        }
        Ok(norm)
    }

    /// An optional directory argument: the root when absent or blank.
    pub fn dir(&self, p: Option<&str>) -> Result<PathBuf, String> {
        match p.map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => self.resolve(p),
            None => Ok(self.root.clone()),
        }
    }

    /// `p` for display: relative to the root when inside it.
    pub fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| p.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;

    #[test]
    fn resolve_confines_relative_paths() {
        let t = TmpDir::new("resolve");
        let ws = Workspace::new(t.path(), 0);
        let root = t.path();
        assert_eq!(ws.resolve("src/main.rs").unwrap(), root.join("src/main.rs"));
        // dot segments are normalised away
        assert_eq!(ws.resolve("./a/../b").unwrap(), root.join("b"));
        assert!(ws.resolve("").unwrap_err().contains("empty path"));
        assert!(ws.resolve("   ").unwrap_err().contains("empty path"));
        let err = ws.resolve("../outside").unwrap_err();
        assert!(err.contains("escapes project root"), "{err}");
        // absolute paths are honoured (bash is unrestricted anyway)
        assert_eq!(ws.resolve("/tmp/./x").unwrap(), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn dir_defaults_to_the_root_and_rel_strips_it() {
        let t = TmpDir::new("dir");
        let ws = Workspace::new(t.path(), 0);
        assert_eq!(ws.dir(None).unwrap(), t.path());
        assert_eq!(ws.dir(Some("  ")).unwrap(), t.path());
        assert_eq!(ws.dir(Some("src")).unwrap(), t.join("src"));
        assert!(ws.dir(Some("../..")).is_err());
        assert_eq!(ws.rel(&t.join("src/a.rs")), "src/a.rs");
        assert_eq!(ws.rel(Path::new("/elsewhere/x")), "/elsewhere/x");
    }
}
