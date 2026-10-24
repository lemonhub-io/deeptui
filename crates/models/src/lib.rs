//! Which models deeptui can reach. Two halves:
//!
//! - [`catalog`]: models.dev — every provider's models with their prices,
//!   limits and endpoints — fetched at most once a day, revalidated by ETag,
//!   and searched offline;
//! - [`Garage`]: the user's own short list, `models.toml`. Each entry is a
//!   snapshot of what the catalog said when it was added, so starting up
//!   never waits on the network, and every field can be edited by hand.
//!
//! Nothing here talks to a model; it only says where one lives.

pub mod catalog;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// One added model: everything needed to reach it and bill it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// `provider/model`, as models.dev names it.
    pub id: String,
    /// Sent as `model` in each request.
    pub model: String,
    /// OpenAI-compatible, without a trailing slash.
    pub base_url: String,
    /// Env vars that may hold the key, tried in order.
    #[serde(default)]
    pub key_env: Vec<String>,
    /// A key written in by hand; the env vars win over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Tokens of context and of output; 0 when unknown.
    #[serde(default)]
    pub context: usize,
    #[serde(default)]
    pub output: u32,
    /// $ per 1M tokens.
    #[serde(default)]
    pub price_input: f64,
    #[serde(default)]
    pub price_output: f64,
    #[serde(default)]
    pub price_cached: f64,
}

impl Entry {
    /// The models.dev provider it comes from: the id's first part.
    pub fn provider(&self) -> &str {
        self.id.split('/').next().unwrap_or_default()
    }

    /// The first of its env vars that is set, else the key written in.
    pub fn key(&self) -> Option<String> {
        self.key_env
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|k| !k.is_empty()))
            .or_else(|| self.api_key.clone().filter(|k| !k.is_empty()))
    }
}

/// The user's models, kept in `models.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Garage {
    /// The id last switched to; none means the configured model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// Keys typed into the TUI, by provider: one serves all its models.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, String>,
    #[serde(default, rename = "model")]
    pub models: Vec<Entry>,
}

const HEADER: &str = "# Models added with /model, from models.dev. Edit freely. A key is looked\n\
                      # for in an entry's key_env variables, then its api_key, then [keys].\n\n";

impl Garage {
    /// A missing file is an empty garage; a broken one is an error, so a
    /// later save cannot wipe what the user wrote.
    pub fn load(path: &Path) -> Result<Garage, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Garage::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Atomic and owner-only: entries may hold keys.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = path.with_extension("toml.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&tmp)
            .and_then(|mut f| f.write_all(format!("{HEADER}{text}").as_bytes()))
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The entry `name` means: its id, its model, or a piece of an id no
    /// other entry has.
    pub fn find(&self, name: &str) -> Result<usize, String> {
        let exact = |f: fn(&Entry) -> &str| self.models.iter().position(|e| f(e) == name);
        if let Some(i) = exact(|e| &e.id).or_else(|| exact(|e| &e.model)) {
            return Ok(i);
        }
        let hits: Vec<usize> = (0..self.models.len())
            .filter(|&i| self.models[i].id.contains(name))
            .collect();
        match hits.as_slice() {
            [i] => Ok(*i),
            [] => Err(format!("no model {name:?} on the list")),
            many => Err(format!(
                "{name:?} could be {}",
                many.iter()
                    .map(|&i| self.models[i].id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Add `e`, or refresh the entry with its id — keeping a key written
    /// into the old one. Its index comes back.
    pub fn add(&mut self, mut e: Entry) -> usize {
        match self.models.iter().position(|o| o.id == e.id) {
            Some(i) => {
                e.api_key = e.api_key.or(self.models[i].api_key.take());
                self.models[i] = e;
                i
            }
            None => {
                self.models.push(e);
                self.models.len() - 1
            }
        }
    }

    /// The key for `e`: its own, else its provider's.
    pub fn key(&self, e: &Entry) -> Option<String> {
        e.key().or_else(|| self.provider_key(e.provider(), &[]))
    }

    /// A provider's key: from one of `env`, else the one kept here.
    pub fn provider_key(&self, provider: &str, env: &[String]) -> Option<String> {
        env.iter()
            .find_map(|v| std::env::var(v).ok().filter(|k| !k.is_empty()))
            .or_else(|| self.keys.get(provider).filter(|k| !k.is_empty()).cloned())
    }

    /// The entry the user last switched to, if it is still listed.
    pub fn current(&self) -> Option<&Entry> {
        let id = self.current.as_deref()?;
        self.models.iter().find(|e| e.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;

    fn entry(id: &str) -> Entry {
        let (_, model) = id.split_once('/').unwrap();
        Entry {
            id: id.into(),
            model: model.into(),
            base_url: "https://x/v1".into(),
            key_env: vec!["DEEPTUI_TEST_NO_SUCH_KEY".into()],
            ..Entry::default()
        }
    }

    #[test]
    fn the_garage_round_trips_through_its_file_privately() {
        let t = TmpDir::new("garage");
        let p = t.join("cfg/models.toml");
        assert_eq!(Garage::load(&p).unwrap(), Garage::default());
        let mut g = Garage::default();
        g.add(entry("deepseek/deepseek-v4-pro"));
        g.add(Entry {
            api_key: Some("sk-1".into()),
            ..entry("openrouter/qwen/qwen3-coder")
        });
        g.current = Some("openrouter/qwen/qwen3-coder".into());
        g.keys.insert("deepseek".into(), "sk-ds".into());
        g.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# Models added"), "{text}");
        assert!(text.contains("[[model]]"), "{text}");
        assert_eq!(Garage::load(&p).unwrap(), g);
        assert_eq!(g.current().unwrap().key().as_deref(), Some("sk-1"));
        // an entry's own key, else its provider's
        assert_eq!(g.key(&g.models[0]).as_deref(), Some("sk-ds"));
        assert_eq!(g.key(&g.models[1]).as_deref(), Some("sk-1"));
        assert_eq!(g.models[1].provider(), "openrouter");
        assert_eq!(g.provider_key("openrouter", &[]), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // a broken file is reported, never treated as empty
        std::fs::write(&p, "[[model]\n").unwrap();
        assert!(Garage::load(&p).unwrap_err().contains("models.toml"));
    }

    #[test]
    fn entries_are_found_by_id_model_or_a_unique_piece() {
        let mut g = Garage::default();
        for id in [
            "deepseek/deepseek-v4-pro",
            "openrouter/deepseek/deepseek-v4-pro",
            "openrouter/qwen/qwen3-coder",
        ] {
            g.add(entry(id));
        }
        assert_eq!(g.find("deepseek/deepseek-v4-pro"), Ok(0));
        assert_eq!(g.find("deepseek-v4-pro"), Ok(0), "a model name wins");
        assert_eq!(g.find("qwen"), Ok(2));
        assert!(g.find("openrouter").unwrap_err().contains("could be"));
        assert!(g.find("gpt").unwrap_err().contains("no model"));
    }

    #[test]
    fn re_adding_refreshes_an_entry_but_keeps_its_written_key() {
        let mut g = Garage::default();
        g.add(Entry {
            api_key: Some("sk-1".into()),
            price_input: 1.0,
            ..entry("a/m")
        });
        let i = g.add(Entry {
            price_input: 0.5,
            ..entry("a/m")
        });
        assert_eq!((i, g.models.len()), (0, 1));
        assert_eq!(g.models[0].price_input, 0.5);
        assert_eq!(g.models[0].api_key.as_deref(), Some("sk-1"));
        // no current, or a current that was removed by hand: none
        assert!(g.current().is_none());
        g.current = Some("gone/x".into());
        assert!(g.current().is_none());
    }
}
