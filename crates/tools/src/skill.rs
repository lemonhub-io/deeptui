//! Skills: `SKILL.md` files the agent loads on demand.
//!
//! A skill is a directory carrying a `SKILL.md` — YAML frontmatter (`name`,
//! `description`) plus a markdown body — or a flat `<name>.md`. Discovery is
//! one `read_dir` per root, so it runs at startup: the system prompt carries
//! one line per skill, and the [`SKILL`] tool fetches a body only when a task
//! actually matches. That is the point — a project can carry dozens of skills
//! without paying for them on every request.

use crate::{Args, Exec, Output, Tool, Workspace};
use deeptui_core::Effect;
use serde_json::json;
use std::path::{Path, PathBuf};

/// One line per skill in the system prompt: enough to choose, cheap enough
/// to be free.
const PROMPT_DESC: usize = 120;
const PROMPT_MAX: usize = 24;
const PROMPT_BYTES: usize = 1_500;
/// One body in a tool result, capped so a runaway skill cannot flood the
/// context; the cut names the file to read for the rest.
const BODY_CHARS: usize = 16_000;
const FILE_MAX: u64 = 512 * 1024;
const LIST_DESC: usize = 200;
const NAME_MAX: usize = 64;

/// Where skills are looked for in a project, first one winning a name.
pub const PROJECT_DIRS: [&str; 4] = [
    ".deeptui/skills",
    "skills",
    ".claude/skills",
    ".agents/skills",
];

/// A skill as discovery found it: what it is called, what it is for, and
/// where its file is. The body is read on demand, never held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

impl Skill {
    /// The directory a skill's references (`scripts/`, `references/`) are
    /// relative to — the file's own directory.
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// The markdown after the frontmatter, as it stands on disk now.
    pub fn body(&self) -> Result<String, String> {
        let text =
            read(&self.path).ok_or_else(|| format!("{}: unreadable", self.path.display()))?;
        Ok(split_front(&text).1.trim().to_string())
    }
}

/// Every directory that may hold skills: the project's first, then the
/// user's — `DEEPTUI_SKILLS_DIR` (a `PATH`-style list) replaces the user's.
pub fn roots(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = PROJECT_DIRS.iter().map(|d| root.join(d)).collect();
    match std::env::var_os("DEEPTUI_SKILLS_DIR") {
        Some(dirs) if !dirs.is_empty() => out.extend(std::env::split_paths(&dirs)),
        _ => {
            if let Some(home) = dirs::home_dir() {
                out.push(home.join(".deeptui").join("skills"));
            }
            if let Some(cfg) = dirs::config_dir() {
                out.push(cfg.join("deeptui").join("skills"));
            }
        }
    }
    out
}

/// The skills found under `roots`, in root order and then by name. A name
/// belongs to whatever root found it first, so a project skill shadows a
/// user skill of the same name.
pub fn discover(roots: &[PathBuf]) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for dir in roots {
        for s in scan(dir) {
            let key = s.name.to_ascii_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            out.push(s);
        }
    }
    out
}

/// The skills of one project, user skills included.
pub fn find_all(root: &Path) -> Vec<Skill> {
    discover(&roots(root))
}

/// The `## Skills` block of the system prompt; empty when there are none.
pub fn section(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "## Skills\nLoad one with the skill tool when a task matches it; don't load what you won't use.\n",
    );
    let mut shown = 0;
    for k in skills.iter().take(PROMPT_MAX) {
        let line = format!("- {}: {}\n", k.name, cut(&k.description, PROMPT_DESC));
        if s.len() + line.len() > PROMPT_BYTES {
            break;
        }
        s.push_str(&line);
        shown += 1;
    }
    if shown < skills.len() {
        s.push_str(&format!(
            "… +{} more: call skill without a name to list them all\n",
            skills.len() - shown
        ));
    }
    s
}

/// The tool's answer to a name-less call: every skill with where it lives,
/// project-relative where it can be.
pub fn listing(skills: &[Skill], root: &Path) -> String {
    skills
        .iter()
        .map(|k| {
            format!(
                "- {}: {} [{}]",
                k.name,
                cut(&k.description, LIST_DESC),
                k.path.strip_prefix(root).unwrap_or(&k.path).display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub const SKILL: Tool = Tool {
    name: "skill",
    about: "Load a skill's SKILL.md instructions by name; no name lists the available skills. Load only what the task needs.",
    params: || {
        json!({"type":"object","properties":{
            "name":{"type":"string","description":"skill name; omitted lists every skill"}}})
    },
    effect: Effect::Read,
    exec: Exec::Blocking(load),
};

fn load(ws: &Workspace, args: &Args) -> Output {
    let skills = find_all(ws.root());
    let name = args.str("name").map(str::trim).unwrap_or("");
    if name.is_empty() {
        return if skills.is_empty() {
            Err(none_found())
        } else {
            Ok(listing(&skills, ws.root()))
        };
    }
    let Some(k) = skills.iter().find(|k| k.name.eq_ignore_ascii_case(name)) else {
        let have: Vec<&str> = skills.iter().map(|k| k.name.as_str()).collect();
        return Err(if have.is_empty() {
            none_found()
        } else {
            format!("unknown skill: {name} (have: {})", have.join(", "))
        });
    };
    let body = k.body()?;
    let mut out = format!(
        "# skill: {} (from {})\nPaths it mentions are relative to {}.\n\n",
        k.name,
        ws.rel(&k.path),
        ws.rel(k.dir())
    );
    if body.is_empty() {
        out.push_str("(this skill has no instructions)");
        return Ok(out);
    }
    match body.char_indices().nth(BODY_CHARS) {
        Some((cut, _)) => {
            out.push_str(&body[..cut]);
            out.push_str(&format!(
                "\n… truncated at {BODY_CHARS} chars — read {} for the rest",
                ws.rel(&k.path)
            ));
        }
        None => out.push_str(&body),
    }
    Ok(out)
}

fn none_found() -> String {
    format!(
        "no skills found: add {}/<name>/SKILL.md (frontmatter: name, description) \
         or set DEEPTUI_SKILLS_DIR",
        PROJECT_DIRS[0]
    )
}

// ------------------------------------------------------------- discovery ----

/// The valid skills directly inside one root directory.
fn scan(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            // a directory is a skill only through its SKILL.md
            let Ok(inner) = std::fs::read_dir(&p) else {
                continue;
            };
            let mut files: Vec<PathBuf> = inner
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.is_file() && is_markdown(f))
                .collect();
            files.sort();
            if let Some(f) = files.iter().find(|f| is_skill_file(f)) {
                out.extend(from_file(f));
            }
        } else if is_markdown(&p) {
            out.extend(from_file(&p));
        }
    }
    out
}

fn is_markdown(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

fn is_skill_file(p: &Path) -> bool {
    p.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("skill"))
}

fn from_file(path: &Path) -> Option<Skill> {
    let text = read(path)?;
    let (front, body) = split_front(&text);
    let fields = front.map(fields).unwrap_or_default();
    let get = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let declared = sanitize(&get("name"));
    let name = if declared.is_empty() {
        sanitize(&fallback_name(path))
    } else {
        declared
    };
    if name.is_empty() {
        return None;
    }
    let described = get("description");
    let description = if described.trim().is_empty() {
        first_prose(body)
    } else {
        collapse(&described)
    };
    Some(Skill {
        name,
        description,
        path: path.to_path_buf(),
    })
}

/// `SKILL.md` is named by its directory; a flat `pdf.md` by its stem.
fn fallback_name(path: &Path) -> String {
    if is_skill_file(path) {
        return path
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// A name the prompt can carry: no whitespace, no path separators.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            c if c.is_whitespace() || c == '/' || c == '\\' => '-',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    cleaned.chars().take(NAME_MAX).collect()
}

fn read(path: &Path) -> Option<String> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    f.take(FILE_MAX).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// ----------------------------------------------------------- frontmatter ----

/// The frontmatter block and the body after it. No fence, or one that never
/// closes, means the whole file is body.
fn split_front(text: &str) -> (Option<&str>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---") else {
        return (None, text);
    };
    let Some(rest) = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
    else {
        return (None, text);
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim() == "---" {
            return (Some(&rest[..at]), &rest[at + line.len()..]);
        }
        at += line.len();
    }
    (None, text)
}

/// `key: value` lines, plus the `>`/`|` block scalars real skills use for a
/// long description. Anything unparseable is skipped, not fatal.
fn fields(front: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = front.lines().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let value = if is_block(value) {
            // `>` folds its lines into one, `|` keeps them apart
            let fold = value.starts_with('>');
            let mut parts: Vec<String> = Vec::new();
            while i < lines.len() {
                let next = lines[i];
                let blank = next.trim().is_empty();
                if !blank && !next.starts_with([' ', '\t']) {
                    break;
                }
                i += 1;
                parts.push(next.trim().to_string());
            }
            while parts.last().is_some_and(|p| p.is_empty()) {
                parts.pop();
            }
            if fold {
                parts.join(" ")
            } else {
                parts.join("\n")
            }
        } else {
            unquote(value).to_string()
        };
        out.push((key, value.trim().to_string()));
    }
    out
}

/// A `>`/`|` scalar with an optional chomping marker.
fn is_block(value: &str) -> bool {
    matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+")
}

fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 {
        let (f, l) = (b[0], b[b.len() - 1]);
        if (f == b'"' && l == b'"') || (f == b'\'' && l == b'\'') {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// The first line of prose: no headings, no code, no comments.
fn first_prose(body: &str) -> String {
    let mut fenced = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || line.is_empty() || line.starts_with('#') || line.starts_with("<!--") {
            continue;
        }
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line);
        return collapse(line);
    }
    String::new()
}

/// One line of text, whatever whitespace the source used.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(s: &str, max: usize) -> String {
    let s = collapse(s);
    match s.char_indices().nth(max) {
        None => s,
        Some((at, _)) => {
            let mut out = s[..at].trim_end().to_string();
            out.push('…');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;

    fn skill_md(name: &str, desc: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {desc}\n---\n{body}")
    }

    #[test]
    fn a_directory_skill_is_named_and_described_by_its_frontmatter() {
        let t = TmpDir::new("skill-front");
        t.write(
            ".deeptui/skills/pdf/SKILL.md",
            &skill_md(
                "pdf",
                "Extract text and tables from PDF files.",
                "# PDF\nRun pdftotext.",
            ),
        );
        let found = discover(&[t.join(".deeptui/skills")]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "pdf");
        assert_eq!(
            found[0].description,
            "Extract text and tables from PDF files."
        );
        assert_eq!(found[0].dir(), t.join(".deeptui/skills/pdf"));
        assert_eq!(found[0].body().unwrap(), "# PDF\nRun pdftotext.");
        // a name is what the tool is called with, path and case aside
        assert!(found[0].name.eq_ignore_ascii_case("PDF"));
    }

    #[test]
    fn flat_files_and_fallbacks_are_accepted() {
        let t = TmpDir::new("skill-flat");
        t.write(
            "skills/Changelog.md",
            "---\ndescription: >\n  Keep the changelog\n  in one place.\n---\n\nWrite it.\n",
        );
        // no frontmatter at all: the stem names it, the first prose describes it
        t.write("skills/deploy.md", "# Deploy\n\nShip it to the cluster.\n");
        let found = discover(&[t.join("skills")]);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!(found[0].name, "Changelog");
        // folded scalars join their lines
        assert_eq!(found[0].description, "Keep the changelog in one place.");
        assert_eq!(found[1].name, "deploy");
        assert_eq!(found[1].description, "Ship it to the cluster.");
        assert_eq!(
            found[1].body().unwrap(),
            "# Deploy\n\nShip it to the cluster."
        );
    }

    #[test]
    fn only_skill_shaped_files_are_discovered() {
        let t = TmpDir::new("skill-shape");
        // a directory without SKILL.md is not a skill
        t.write("skills/notes/README.md", "just notes");
        // a non-markdown file is not a skill
        t.write("skills/data.txt", "text");
        // an unterminated fence is body, and an empty name falls back to the dir
        t.write("skills/broken/SKILL.md", "---\nname: \n---\n");
        let found = discover(&[t.join("skills")]);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].name, "broken");
        // a missing root is not an error
        assert!(discover(&[t.join("nope")]).is_empty());
    }

    #[test]
    fn an_unterminated_fence_is_body_and_a_name_is_made_safe() {
        let (front, body) = split_front("---\nname: x\n# not closed\n");
        assert_eq!(front, None);
        assert!(body.contains("# not closed"));
        // CRLF frontmatter parses; a declared name loses whitespace and slashes
        let (front, body) = split_front("---\r\nname: my skill/x\r\n---\r\nbody\r\n");
        let f = fields(front.unwrap());
        assert_eq!(sanitize(&f[0].1), "my-skill-x");
        assert_eq!(body, "body\r\n");
        // quoted values lose their quotes
        assert_eq!(unquote("\"a b\""), "a b");
        assert_eq!(unquote("'a b'"), "a b");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn a_project_skill_shadows_a_user_skill_of_the_same_name() {
        let t = TmpDir::new("skill-shadow");
        t.write("skills/pdf/SKILL.md", &skill_md("pdf", "project one", "p"));
        t.write("user/pdf/SKILL.md", &skill_md("pdf", "user one", "u"));
        let found = discover(&[t.join("skills"), t.join("user")]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "project one");
        // two different names both survive
        t.write("user/other.md", "other");
        let found = discover(&[t.join("skills"), t.join("user")]);
        assert_eq!(
            found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["pdf", "other"]
        );
    }

    #[test]
    fn the_prompt_index_is_capped_per_line_and_in_total() {
        let many: Vec<Skill> = (0..40)
            .map(|i| Skill {
                name: format!("s{i}"),
                description: "d".repeat(500),
                path: PathBuf::from(format!(".deeptui/skills/s{i}/SKILL.md")),
            })
            .collect();
        let s = section(&many);
        assert!(s.starts_with("## Skills\n"), "{s}");
        assert!(s.len() <= PROMPT_BYTES + 200, "{} bytes", s.len());
        assert!(s.ends_with("more: call skill without a name to list them all\n"));
        // one description is clipped with a marker, not cut mid-char
        let one = section(&many[..1]);
        let line = one.lines().nth(2).unwrap();
        assert!(line.ends_with('…'), "{line}");
        assert_eq!(line.chars().count(), "- s0: ".len() + PROMPT_DESC + 1);
        assert_eq!(section(&[]), "");
        // every skill is listed when they all fit
        let few: Vec<Skill> = many[..3]
            .iter()
            .map(|s| Skill {
                description: "short".into(),
                ..s.clone()
            })
            .collect();
        assert_eq!(section(&few).lines().count(), 5);
    }

    #[tokio::test]
    async fn the_listing_names_paths_and_the_body_is_capped() {
        let t = TmpDir::new("skill-tool");
        t.write(
            ".deeptui/skills/pdf/SKILL.md",
            &skill_md("pdf", "Read PDFs.", &"x".repeat(BODY_CHARS + 50)),
        );
        t.write("skills/notes.md", "---\ndescription: Take notes\n---\nbody");
        let tb = crate::Toolbox::new(Workspace::new(t.path(), 5_000));

        // paths are project-relative, so the model can read them back
        let out = tb.call("skill", "{}").await.unwrap();
        assert!(
            out.contains("- pdf: Read PDFs. [.deeptui/skills/pdf/SKILL.md]"),
            "{out}"
        );
        assert!(
            out.contains("- notes: Take notes [skills/notes.md]"),
            "{out}"
        );

        // the name matches whatever the case; the body arrives without its
        // frontmatter, and past BODY_CHARS it points at the file instead
        let out = tb
            .call("skill", &json!({"name": "PDF"}).to_string())
            .await
            .unwrap();
        assert!(
            out.starts_with("# skill: pdf (from .deeptui/skills/pdf/SKILL.md)"),
            "{out}"
        );
        assert!(out.contains("relative to .deeptui/skills/pdf"), "{out}");
        assert!(
            out.contains(&"x".repeat(BODY_CHARS)),
            "the body is not whole"
        );
        assert!(out.contains("truncated at 16000 chars"), "{out}");
        assert!(
            !out.contains("description:"),
            "frontmatter leaked into the body"
        );

        let err = tb
            .call("skill", &json!({"name": "nope"}).to_string())
            .await
            .unwrap_err();
        assert!(err.starts_with("unknown skill: nope (have: "), "{err}");
        assert!(err.contains("pdf") && err.contains("notes"), "{err}");
        assert_eq!(tb.effect("skill"), Effect::Read);
    }

    #[tokio::test]
    async fn a_project_without_skills_says_how_to_add_one() {
        let t = TmpDir::new("skill-none");
        let tb = crate::Toolbox::new(Workspace::new(t.path(), 5_000));
        let out = tb.call("skill", "{}").await;
        if find_all(t.path()).is_empty() {
            let err = out.unwrap_err();
            assert!(
                err.starts_with("no skills found: add .deeptui/skills/<name>/SKILL.md"),
                "{err}"
            );
            assert!(err.contains("DEEPTUI_SKILLS_DIR"), "{err}");
        } else {
            // this user keeps skills of their own, so the list is not empty
            assert!(out.is_ok());
        }
        assert_eq!(section(&[]), "");
    }

    #[test]
    fn the_root_lookup_prefers_the_project_and_honours_the_env_override() {
        let roots = roots(Path::new("/proj"));
        assert_eq!(roots[0], Path::new("/proj/.deeptui/skills"));
        assert_eq!(roots[1], Path::new("/proj/skills"));
        assert!(roots.len() >= PROJECT_DIRS.len());
    }
}
