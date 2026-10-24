//! Every conversation per project, kept on disk so `/resume` and `--continue`
//! can pick one up — in a later headless run as much as in the TUI. It is the
//! session's [`Shelf`]; nothing else knows it exists.

use deeptui_core::history::is_aside;
use deeptui_core::{Msg, Role};
use deeptui_engine::{Shelf, Shelved};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Conversations kept per project; starting one more deletes the oldest.
const KEEP: usize = 50;
/// Conversations `/resume` offers.
const SHOWN: usize = 10;

pub struct Transcript {
    /// This project's shelf: one file per conversation.
    dir: Option<PathBuf>,
    /// Where the current conversation saves, once it has said anything.
    current: Option<PathBuf>,
}

/// `<data_dir>/deeptui/sessions`. `DEEPTUI_SESSION_DIR` redirects it; the
/// tests never touch the real one.
fn default_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEEPTUI_SESSION_DIR") {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    dirs::data_dir().map(|d| d.join("deeptui").join("sessions"))
}

/// FNV-1a: stable across builds and platforms, unlike std's hasher, so a
/// project keeps its directory from one release to the next.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

impl Transcript {
    pub fn for_root(root: &Path) -> Self {
        let name = format!("{:016x}", fnv(root.as_os_str().as_encoded_bytes()));
        Self::at(default_dir().map(|d| d.join(name)))
    }

    fn at(dir: Option<PathBuf>) -> Self {
        Self { dir, current: None }
    }

    /// The other conversations' files, newest first.
    fn others(&self) -> Vec<(PathBuf, SystemTime)> {
        let Some(entries) = self.dir.as_ref().and_then(|d| std::fs::read_dir(d).ok()) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter(|p| self.current.as_ref() != Some(p))
            .filter_map(|p| {
                let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
                Some((p, t))
            })
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.1));
        files
    }

    /// What `/resume` offers: the newest readable other conversations.
    fn shelved(&self) -> impl Iterator<Item = (PathBuf, SystemTime, Vec<Msg>)> {
        self.others()
            .into_iter()
            .filter_map(|(p, t)| {
                let msgs = load(&p)?;
                Some((p, t, msgs))
            })
            .take(SHOWN)
    }
}

/// A saved conversation without its system prompt — the caller brings a
/// fresh one, since the repo map may have moved on.
fn load(path: &Path) -> Option<Vec<Msg>> {
    let text = std::fs::read_to_string(path).ok()?;
    let msgs: Vec<Msg> = serde_json::from_str(&text).ok()?;
    let rest: Vec<Msg> = msgs
        .into_iter()
        .filter(|m| m.role != Role::System)
        .collect();
    (!rest.is_empty()).then_some(rest)
}

/// Best effort and atomic: a failed or interrupted write leaves the previous
/// file in place. Owner-only, since tool output can carry whatever the
/// project's files held.
fn write(path: &Path, msgs: &[Msg]) {
    let Ok(json) = serde_json::to_vec(msgs) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut f| f.write_all(&json));
    if written.is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

impl Shelf for Transcript {
    /// A conversation gets its file once it holds more than the system
    /// prompt — an opened and abandoned session leaves nothing behind.
    fn save(&mut self, history: &[Msg]) {
        if history.iter().all(|m| m.role == Role::System) {
            return;
        }
        let Some(dir) = &self.dir else {
            return;
        };
        let path = match &self.current {
            Some(p) => p.clone(),
            None => {
                let _ = std::fs::create_dir_all(dir);
                let ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis());
                // named by when it began; never over another one
                let p = (ms..)
                    .map(|ms| dir.join(format!("{ms}.json")))
                    .find(|p| !p.exists())
                    .unwrap();
                for (old, _) in self.others().into_iter().skip(KEEP - 1) {
                    let _ = std::fs::remove_file(old);
                }
                self.current = Some(p.clone());
                p
            }
        };
        write(&path, history);
    }

    fn fresh(&mut self) {
        self.current = None;
    }

    fn list(&self) -> Vec<Shelved> {
        let now = SystemTime::now();
        self.shelved()
            .map(|(_, t, msgs)| Shelved {
                age: now.duration_since(t).map_or(0, |d| d.as_secs()),
                messages: msgs.len(),
                title: msgs
                    .iter()
                    .find(|m| m.role == Role::User && !is_aside(m))
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default(),
            })
            .collect()
    }

    fn open(&mut self, i: usize) -> Option<Vec<Msg>> {
        let (path, _, msgs) = self.shelved().nth(i)?;
        self.current = Some(path);
        Some(msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_core::ToolCall;
    use deeptui_testkit::TmpDir;

    fn talk(prompt: &str) -> Vec<Msg> {
        vec![
            Msg::system("old prompt"),
            Msg::user(prompt),
            Msg::assistant(None, vec![ToolCall::new("c1", "ls", "{}")]),
            Msg::tool("c1", "a.txt"),
            Msg::assistant(Some("fixed".into()), vec![]),
        ]
    }

    #[test]
    fn conversations_are_shelved_apart_and_open_without_their_system_prompt() {
        let t = TmpDir::new("transcript");
        let mut tr = Transcript::at(Some(t.join("nested")));
        // nothing said yet: no file
        tr.save(&talk("x")[..1]);
        assert!(!t.join("nested").exists());

        tr.save(&talk("fix it"));
        let first = tr.current.clone().unwrap();
        assert!(tr.list().is_empty(), "the current one is not offered");
        tr.fresh();
        tr.save(&talk("add a flag"));
        let list = tr.list();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].messages, list[0].title.as_str()), (4, "fix it"));
        assert!(list[0].age < 60);

        let back = tr.open(0).unwrap();
        assert_eq!(back.len(), 4);
        assert_eq!(back[0].content.as_deref(), Some("fix it"));
        assert_eq!(back[1].tool_calls.as_ref().unwrap()[0].id, "c1");
        assert_eq!(back[2].tool_call_id.as_deref(), Some("c1"));
        // opened, it is current: later saves go to its file
        assert_eq!(tr.current.as_ref(), Some(&first));
        assert_eq!(tr.list()[0].title, "add a flag");
        assert!(tr.open(1).is_none());
        assert!(!first.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // junk is skipped, not offered
        std::fs::write(t.join("nested/junk.json"), "{not json").unwrap();
        assert_eq!(tr.list().len(), 1);
        // no directory: a quiet no-op
        let mut none = Transcript::at(None);
        none.save(&talk("x"));
        assert!(none.list().is_empty() && none.open(0).is_none());
    }

    #[test]
    fn starting_one_more_conversation_than_kept_drops_the_oldest() {
        let t = TmpDir::new("transcript-keep");
        std::fs::create_dir_all(t.join("p")).unwrap();
        for i in 0..KEEP {
            std::fs::write(t.join(&format!("p/{i}.json")), "[]").unwrap();
        }
        let mut tr = Transcript::at(Some(t.join("p")));
        tr.save(&talk("new"));
        let n = std::fs::read_dir(t.join("p")).unwrap().count();
        assert_eq!(n, KEEP);
        assert!(tr.current.as_ref().unwrap().exists());
    }

    #[test]
    fn each_project_gets_its_own_stable_directory() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv(b"/src/a"), fnv(b"/src/b"));
    }
}
