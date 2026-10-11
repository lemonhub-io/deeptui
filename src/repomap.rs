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

/// Most files listed by one map; the walk inspects more, so the list is a
/// deterministic prefix of the sorted tree rather than of filesystem order.
const MAX_FILES: usize = 500;
const MAX_WALK: usize = 4_000;
/// Most symbols listed per file; the rest collapse into one `...` line.
pub const MAX_SYMBOLS_PER_FILE: usize = 25;
/// Symbols every code file keeps before the budget is spent by rank — a long
/// file list must never reduce a late file to a bare path.
const MIN_SYMBOLS_PER_FILE: usize = 3;
/// Symbol lines are clipped to this many characters, plus their indentation.
const SYMBOL_MAX_CHARS: usize = 110;
/// Files larger than this are not scanned for symbols (generated blobs).
const MAX_SCAN_BYTES: u64 = 1024 * 1024;

const INDENT: usize = 4; // symbols sit under their path
const DEPTH_STEP: usize = 2; // one nesting level shows as two extra spaces
const DEPTH_COLS: usize = 4; // ...one source nesting level is four columns
const MAX_DEPTH: usize = 3;
const ELLIPSIS: &str = "    ...\n";

/// Default/maximum map sizes in bytes (≈4 bytes per token).
pub const DEFAULT_BUDGET: usize = 5_000;
pub const MIN_BUDGET: usize = 500;
pub const MAX_BUDGET: usize = 60_000;

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

/// Render one symbol line, indented under its path by nesting depth.
fn render(text: &str, depth: usize) -> String {
    let pad = INDENT + depth * DEPTH_STEP;
    let mut s = String::with_capacity(pad + text.len() + 1);
    s.push_str(&" ".repeat(pad));
    s.push_str(text);
    s.push('\n');
    s
}

/// Top-level and nested symbol lines of a source file, in source order.
/// Nesting is inferred from indentation so `impl`/`class` bodies read as such.
fn symbols_of(content: &str, re: &Regex) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        if !re.is_match(line) {
            continue;
        }
        let col: usize = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .map(|c| if c == '\t' { DEPTH_COLS } else { 1 })
            .sum();
        let mut text = line.trim();
        let depth = (col / DEPTH_COLS).min(MAX_DEPTH);
        // a binding inside a body is a local variable, not structure — only
        // column-0 `let`/`const` (js, ts, rust) describes the file
        if col > 0
            && ["let ", "const ", "var ", "static "]
                .iter()
                .any(|kw| text.starts_with(kw))
        {
            continue;
        }
        text = &text[..text
            .char_indices()
            .nth(SYMBOL_MAX_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(text.len())];
        out.push(render(text, depth));
    }
    out
}

/// How much a file deserves the symbol budget. Files are listed by path in the
/// output; this order decides who gets symbols, and who gets the most of them.
fn rank(rel: &Path, symbols: usize) -> i32 {
    let p = rel
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let stem = rel
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // source files beat tests, fixtures, docs and lock files at the same depth
    let mut score = 40 - 6 * rel.components().count() as i32;
    if matches!(
        stem.as_str(),
        "main" | "lib" | "mod" | "index" | "app" | "cli" | "agent" | "core" | "server" | "run"
    ) {
        score += 25;
    }
    if matches!(
        stem.as_str(),
        "cargo" | "package" | "pyproject" | "makefile" | "readme" | "go"
    ) {
        score += 12;
    }
    if p.starts_with("src/") || p.starts_with("lib/") || p.starts_with("app/") {
        score += 8;
    }
    if p.contains("/test")
        || p.starts_with("test")
        || p.contains("/spec")
        || p.contains("_test.")
        || p.ends_with(".lock")
        || p.contains("/fixtures/")
        || p.starts_with("bench")
        || p.contains("/generated/")
    {
        score -= 20;
    }
    score + symbols.min(MAX_SYMBOLS_PER_FILE) as i32
}

/// One walked file: its listing line, its symbols, and its rank.
struct Entry {
    rel: PathBuf,
    path_line: String,
    /// Rendered symbol lines, already capped at `MAX_SYMBOLS_PER_FILE`.
    symbols: Vec<String>,
    /// Symbols the per-file cap hid.
    hidden: usize,
    rank: i32,
}

impl Entry {
    fn new(root: &Path, rel: PathBuf, re: &Regex) -> Entry {
        let code = rel
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| CODE_EXTS.contains(&e))
            .unwrap_or(false);
        let mut symbols = Vec::new();
        if code {
            let full = root.join(&rel);
            let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
            if size <= MAX_SCAN_BYTES {
                if let Ok(content) = std::fs::read_to_string(&full) {
                    symbols = symbols_of(&content, re);
                }
            }
        }
        let hidden = symbols.len().saturating_sub(MAX_SYMBOLS_PER_FILE);
        symbols.truncate(MAX_SYMBOLS_PER_FILE);
        let rank = rank(&rel, symbols.len() + hidden);
        Entry {
            path_line: format!("{}\n", rel.display()),
            rel,
            symbols,
            hidden,
            rank,
        }
    }

    fn clipped(&self, take: usize) -> bool {
        take > 0 && (take < self.symbols.len() || self.hidden > 0)
    }
}

/// Take one more symbol for file `i`, paying for its `...` marker the first
/// time and refunding it if the file turns out to be complete. Returns false
/// when the symbol does not fit or there is nothing left to take.
fn take_symbol(
    e: &Entry,
    i: usize,
    take: &mut [usize],
    mark: &mut [bool],
    left: &mut usize,
) -> bool {
    let k = take[i];
    if k >= e.symbols.len() {
        return false;
    }
    let cost = e.symbols[k].len() + if mark[i] { 0 } else { ELLIPSIS.len() };
    if cost > *left {
        return false;
    }
    *left -= cost;
    mark[i] = true;
    take[i] += 1;
    if take[i] == e.symbols.len() && e.hidden == 0 && mark[i] {
        mark[i] = false;
        *left += ELLIPSIS.len();
    }
    true
}

/// Condensed skeleton of the repo: file paths + top-level symbol lines.
/// Injected into the system prompt so the model gets repo awareness for a
/// few hundred tokens instead of reading dozens of files.
///
/// The byte budget is spent by importance (see `rank`) and every code file
/// keeps a floor of symbols before a more important file grows past it, so a
/// tight budget degrades into "less detail", never into "no detail".
pub fn build(root: &Path, max_bytes: usize) -> String {
    let max_bytes = max_bytes.clamp(MIN_BUDGET, MAX_BUDGET);
    let mut files = walk_files(root, MAX_WALK);
    let walk_capped = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);
    if files.is_empty() {
        return String::new();
    }

    let re = symbol_re();
    let entries: Vec<Entry> = files
        .into_iter()
        .map(|rel| Entry::new(root, rel, re))
        .collect();

    let mut by_rank: Vec<usize> = (0..entries.len()).collect();
    by_rank.sort_by(|a, b| {
        entries[*b]
            .rank
            .cmp(&entries[*a].rank)
            .then_with(|| entries[*a].rel.cmp(&entries[*b].rel))
    });

    // Every walked path is listed — unless the bare paths alone overrun the
    // budget. Then the tree keeps at most 60% of it (symbols are the point of
    // the map) and the lowest-ranked files are dropped wholesale rather than
    // listed without their symbols.
    let mut kept = by_rank.clone();
    let all_paths: usize = entries.iter().map(|e| e.path_line.len()).sum();
    if all_paths + ELLIPSIS.len() > max_bytes {
        let path_budget = max_bytes * 3 / 5;
        kept.clear();
        let mut used = 0usize;
        for &i in &by_rank {
            let n = entries[i].path_line.len();
            if !kept.is_empty() && used + n + ELLIPSIS.len() > path_budget {
                break;
            }
            used += n;
            kept.push(i);
        }
    }
    let cut = kept.len() < entries.len() || walk_capped;

    let path_bytes: usize = kept.iter().map(|i| entries[*i].path_line.len()).sum();
    let mut left = max_bytes.saturating_sub(path_bytes + usize::from(cut) * ELLIPSIS.len());

    let mut take = vec![0usize; entries.len()];
    let mut mark = vec![false; entries.len()];
    // pass 1: the floor for every code file, in rank order, so that a low rank
    // costs detail before it costs all symbols
    for &i in &kept {
        while take[i] < entries[i].symbols.len().min(MIN_SYMBOLS_PER_FILE) {
            if !take_symbol(&entries[i], i, &mut take, &mut mark, &mut left) {
                break;
            }
        }
    }
    // pass 2: the remainder goes to the top of the ranking, then downwards
    for &i in &kept {
        while take_symbol(&entries[i], i, &mut take, &mut mark, &mut left) {}
    }

    // The output stays grouped by path: the tree is what makes it readable.
    let mut shown = kept.clone();
    shown.sort_by(|a, b| entries[*a].rel.cmp(&entries[*b].rel));
    let mut out = String::with_capacity(max_bytes);
    for &i in &shown {
        let e = &entries[i];
        out.push_str(&e.path_line);
        for s in e.symbols.iter().take(take[i]) {
            out.push_str(s);
        }
        if e.clipped(take[i]) {
            out.push_str(ELLIPSIS);
        }
    }
    if cut {
        out.push_str(ELLIPSIS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;

    /// Symbol lines listed under `path`, in map order.
    fn symbols_under(map: &str, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut inside = false;
        for l in map.lines() {
            if l.starts_with("    ") {
                if inside && l != "    ..." {
                    out.push(l.to_string());
                }
            } else {
                inside = l == path;
            }
        }
        out
    }

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
            "    pub fn one() {}",
            "    struct Two;",
            "      impl Two {}",
            "    fn hidden() {}",
        ] {
            assert!(map.contains(&format!("{sym}\n")), "missing {sym} in {map}");
        }
        // non-code extensions contribute no symbols
        assert!(!map.contains("not_code"), "{map}");
    }

    #[test]
    fn nested_symbols_are_indented_by_their_depth() {
        let t = TmpDir::new("map-depth");
        t.write(
            "a.py",
            "class A:\n    def m(self):\n        if x:\n            def deep():\n                pass\n",
        );
        let map = build(t.path(), 6000);
        // depth is capped at 3 levels: 4 + 2*3 spaces
        for (line, want) in [
            ("class A:", "    "),
            ("def m(self):", "      "),
            ("def deep():", "          "),
        ] {
            assert!(
                map.lines().any(|l| l == format!("{want}{line}")),
                "missing {want}{line} in:\n{map}"
            );
        }
        // the statement inside the body is not a symbol
        assert!(!map.contains("if x"), "{map}");
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
            "    class A:",
            "      def method(self):",
            "      async def other(self):",
            "    export function two() {}",
            "    const arrow = () => {};",
            "    class C {}",
            "    let loose = 1;",
            "    func Go() {}",
            "    type T struct{}",
            "    pub(crate) fn hidden() {}",
            "    impl A {",
            "      fn m(&self) {}",
        ] {
            assert!(
                map.contains(&format!("{sym}\n")),
                "missing {sym} in:\n{map}"
            );
        }
        // imports, statements and assignments are not symbols
        for noise in ["X = 1", "import os", "package main", "        pass"] {
            assert!(!map.contains(noise), "{noise} leaked into:\n{map}");
        }
    }

    #[test]
    fn locals_are_not_symbols_but_module_level_bindings_are() {
        let t = TmpDir::new("map-locals");
        t.write(
            "a.rs",
            "const LIMIT: usize = 3;\nfn f() {\n    let x = 1;\n    static S: i32 = 2;\n}\n",
        );
        t.write(
            "b.js",
            "let loose = 1;\nfunction g() {\n  const inner = 2;\n}\n",
        );
        let map = build(t.path(), 6000);
        assert!(map.contains("    const LIMIT: usize = 3;\n"), "{map}");
        assert!(map.contains("    let loose = 1;\n"), "{map}");
        assert!(map.contains("    fn f() {\n"), "{map}");
        for local in ["let x = 1", "static S: i32", "const inner = 2"] {
            assert!(!map.contains(local), "{local} leaked into:\n{map}");
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
        let map = build(t.path(), 500);
        assert!(!map.is_empty());
        // paths alone would need ~9 KiB, so the listing has to stop
        assert!(map.len() <= 500, "{} bytes", map.len());
        let full: usize = (0..60)
            .map(|i| format!("some/deeply/nested/path/{stem}_{i}.rs\n").len())
            .sum();
        assert!(full > 9000, "{full}");
        assert!(!map.contains("_59.rs"), "tail should be dropped");
        assert!(map.ends_with("    ...\n"), "{map}");
        // a degenerate budget still yields a usable map
        assert_eq!(build(t.path(), 0).len(), build(t.path(), 0).len());
        assert!(!build(t.path(), 0).is_empty());
    }

    #[test]
    fn symbols_survive_even_when_the_bare_tree_is_too_large() {
        let t = TmpDir::new("map-tree-budget");
        // 60 files with long paths: the listing alone is ~5 KiB
        let stem = "y".repeat(60);
        for i in 0..60 {
            t.write(
                &format!("src/deep/nest/{stem}_{i:02}.rs"),
                "fn entry() {}\n",
            );
        }
        let map = build(t.path(), MIN_BUDGET);
        assert!(map.len() <= MIN_BUDGET, "{} bytes", map.len());
        // the tree gives way to the symbols instead of eating the whole budget
        assert!(map.matches(".rs\n").count() < 60, "the tree was not cut");
        assert!(
            map.contains("    fn entry() {}\n"),
            "no symbols left:\n{map}"
        );
        assert!(map.ends_with("    ...\n"), "the list must say it was cut");
    }

    #[test]
    fn every_code_file_keeps_a_floor_of_symbols_on_a_tight_budget() {
        let t = TmpDir::new("map-floor");
        for i in 0..40 {
            let body: String = (0..8).map(|k| format!("def f{k}():\n    pass\n")).collect();
            t.write(&format!("pkg{i}/mod.py"), &body);
        }
        let map = build(t.path(), 3_000);
        let files: Vec<&str> = map.lines().filter(|l| !l.starts_with(' ')).collect();
        assert_eq!(files.len(), 40, "the tree must stay complete");
        // pass 1 gives every file its floor before any file grows past it
        let bare = files
            .iter()
            .filter(|f| symbols_under(&map, f).is_empty())
            .count();
        assert_eq!(bare, 0, "some file was reduced to a bare path:\n{map}");
        assert!(map.len() <= 3_000, "{} bytes", map.len());
    }

    #[test]
    fn a_tight_budget_gives_detail_to_the_most_important_files_first() {
        let t = TmpDir::new("map-rank");
        let many: String = (0..25)
            .map(|i| format!("pub fn entry{i}_x() {{}}\n"))
            .collect();
        t.write("src/main.rs", &many);
        t.write(
            "bench/fixtures/gen.py",
            &many.replace("pub fn entry", "def gen"),
        );
        let map = build(t.path(), 600);
        let main = symbols_under(&map, "src/main.rs").len();
        let gen = symbols_under(&map, "bench/fixtures/gen.py").len();
        assert!(main > gen, "main={main} gen={gen}\n{map}");
        // the ranked-down file is cut back to its floor and says so
        assert_eq!(gen, MIN_SYMBOLS_PER_FILE, "{map}");
        assert!((MIN_SYMBOLS_PER_FILE..MAX_SYMBOLS_PER_FILE).contains(&main));
        assert_eq!(map.matches("    ...").count(), 2, "{map}");
        assert!(map.len() <= 600, "{} bytes", map.len());
    }

    #[test]
    fn the_map_never_exceeds_its_budget_and_grows_monotonically() {
        let t = TmpDir::new("map-grow");
        for i in 0..12 {
            let body: String = (0..20).map(|k| format!("fn f{k}() {{}}\n")).collect();
            t.write(&format!("src/m{i}.rs"), &body);
        }
        let mut prev = 0usize;
        for budget in [500, 1_000, 2_000, 6_000, MAX_BUDGET] {
            let map = build(t.path(), budget);
            assert!(map.len() <= budget, "{budget}: {} bytes", map.len());
            assert!(map.len() >= prev, "budget {budget} shrank the map");
            prev = map.len();
        }
        // the largest budget maps every file completely: no truncation left
        let big = build(t.path(), MAX_BUDGET);
        assert_eq!(big.matches("    ...").count(), 0, "{big}");
        assert_eq!(big.matches("    fn f").count(), 12 * 20);
        // oversized requests are clamped, not honoured
        assert_eq!(
            build(t.path(), 10 * MAX_BUDGET),
            build(t.path(), MAX_BUDGET)
        );
    }

    #[test]
    fn the_walk_cap_is_reported_in_the_map() {
        let t = TmpDir::new("map-files");
        for i in 0..(MAX_FILES + 20) {
            t.write(&format!("f{i:04}.rs"), "");
        }
        let map = build(t.path(), MAX_BUDGET);
        assert!(map.ends_with("    ...\n"), "{}", &map[map.len() - 40..]);
        assert_eq!(map.matches(".rs\n").count(), MAX_FILES);
        // the cap keeps a sorted prefix, so the result is stable
        assert!(map.contains("f0499.rs\n"), "the cap slipped");
        assert!(!map.contains("f0500.rs"), "past the cap");
    }
}
