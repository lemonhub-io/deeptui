//! `/model`: the configured model, the ones added from models.dev, and the
//! switch between them. The session hands over the arguments and swaps in
//! whatever comes back; the choice is remembered for the next start.
//!
//! It is also the way in on a first run: choose a provider, paste its key —
//! checked, then kept owner-only in `models.toml` — and choose a model.
//! Pickers and the key prompt are [`Ask`]s; their answers come back here as
//! `/model …` lines.

use crate::settings::{self, key_for, Caps, Model, Settings};
use deeptui_core::text::fmt_tokens;
use deeptui_core::{Ask, Choice};
use deeptui_engine::{Reply, Switch, Switchboard};
use deeptui_models::catalog::{self, Catalog, ProviderInfo, Source};
use deeptui_models::{Entry, Garage};
use deeptui_provider::{check_key, KeyCheck, Provider};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Search results offered at once.
const SHOWN: usize = 40;
const USAGE: &str =
    "/model <n> switches · setup · search <words> · add <provider/model> · key <provider> · rm <n>";
/// Offered first when choosing a provider.
const POPULAR: &[&str] = &[
    "deepseek",
    "openai",
    "anthropic",
    "google",
    "openrouter",
    "moonshotai",
    "zhipuai",
    "alibaba",
    "xai",
    "mistral",
    "groq",
    "minimax",
];

pub struct Board {
    /// `models.toml`; none when there is nowhere to keep it.
    garage: Option<PathBuf>,
    source: Source,
    /// Parsed on first use, then kept for the session.
    catalog: Option<Catalog>,
    configured: Model,
    caps: Caps,
    /// The id of the model in use; none while nothing can run.
    current: Option<String>,
    /// The entry to switch to once its provider's key arrives.
    pending: Option<String>,
}

impl Board {
    pub fn new(s: &Settings) -> Self {
        Self::at(
            settings::garage_path(),
            Source::new(catalog::URL, settings::catalog_cache()),
            s,
        )
    }

    fn at(garage: Option<PathBuf>, source: Source, s: &Settings) -> Self {
        Self {
            garage,
            source,
            catalog: None,
            configured: s.configured.clone(),
            caps: s.caps,
            current: s.ready().then(|| s.model.id.clone()),
            pending: None,
        }
    }

    async fn run(&mut self, args: &str) -> Reply {
        let (verb, rest) = args
            .split_once(char::is_whitespace)
            .map_or((args, ""), |(v, r)| (v, r.trim()));
        match verb {
            "" => self.choose(),
            "list" => Reply::text(self.list()),
            "setup" => self.setup(false).await,
            "providers" => self.setup(true).await,
            "provider" => self.provider(rest).await,
            "key" => self.key(rest).await,
            "search" | "find" => self.search(rest).await,
            "add" => self.add(rest).await,
            "rm" | "remove" => Reply::text(self.remove(rest)),
            "refresh" => Reply::text(self.refresh().await),
            _ => self.switch(args),
        }
    }

    /// Read afresh each time: hand edits count without a restart.
    fn garage(&self) -> Result<Garage, String> {
        match &self.garage {
            Some(p) => Garage::load(p).map_err(|e| format!("{e} — fix or remove it first")),
            None => Ok(Garage::default()),
        }
    }

    async fn catalog(&mut self, refresh: bool) -> Result<(&Catalog, String), String> {
        if refresh || self.catalog.is_none() {
            let (c, note) = self.source.load(refresh).await?;
            return Ok((self.catalog.insert(c), note.unwrap_or_default()));
        }
        Ok((self.catalog.as_ref().expect("loaded above"), String::new()))
    }

    fn is_current(&self, id: &str) -> bool {
        self.current.as_deref() == Some(id)
    }

    fn about(&self, g: &Garage, e: &Entry) -> String {
        let key = match key_for(g, e, &self.configured.endpoint) {
            Some(_) => "",
            None => " · no key",
        };
        format!(
            "{} · {}{key}",
            window(e.context),
            price(e.price_input, e.price_output)
        )
    }

    fn about_configured(&self) -> String {
        let c = &self.configured;
        format!(
            "{} · {} · config.toml",
            settings::host(&c.endpoint.base_url),
            price(c.tariff.input, c.tariff.output)
        )
    }

    /// `/model`: a picker over the list, the one in use selected.
    fn choose(&self) -> Reply {
        let g = match self.garage() {
            Ok(g) => g,
            Err(e) => return Reply::text(e),
        };
        let mut options = Vec::new();
        // a configured model without a key is a default nobody chose
        if !self.configured.endpoint.api_key.is_empty() {
            options.push(Choice {
                label: self.configured.id.clone(),
                detail: self.about_configured(),
                command: "/model 1".into(),
            });
        }
        for (i, e) in g.models.iter().enumerate() {
            options.push(Choice {
                label: e.id.clone(),
                detail: self.about(&g, e),
                command: format!("/model {}", i + 2),
            });
        }
        let selected = options
            .iter()
            .position(|o| self.is_current(&o.label))
            .unwrap_or(0);
        options.push(Choice {
            label: "+ add a model".into(),
            detail: "choose a provider, then one of its models".into(),
            command: "/model setup".into(),
        });
        Reply::ask(Ask::Choose {
            title: "Switch model".into(),
            options,
            selected,
        })
    }

    fn list(&self) -> String {
        let g = match self.garage() {
            Ok(g) => g,
            Err(e) => return e,
        };
        let mut out = format!("models — {USAGE}");
        out += &self.row(1, &self.configured.id, &self.about_configured());
        for (i, e) in g.models.iter().enumerate() {
            out += &self.row(i + 2, &e.id, &self.about(&g, e));
        }
        if g.models.is_empty() {
            out += "\n  (nothing added yet: /model setup picks one from models.dev)";
        }
        out
    }

    fn row(&self, n: usize, id: &str, about: &str) -> String {
        let mark = if self.is_current(id) { '●' } else { ' ' };
        format!("\n{mark} {n:>2}  {id:<34} {about}")
    }

    /// A provider picker: the popular ones and a way to the rest — or,
    /// with `all`, every provider usable here.
    async fn setup(&mut self, all: bool) -> Reply {
        let g = match self.garage() {
            Ok(g) => g,
            Err(e) => return Reply::text(e),
        };
        let (c, note) = match self.catalog(false).await {
            Ok(c) => c,
            Err(e) => return Reply::text(format!("{e} — or set a key in config.toml")),
        };
        let providers = c.providers();
        let ask = if all {
            Ask::Choose {
                title: format!("All {} providers — type to filter", providers.len()),
                options: providers.iter().map(|p| provider_choice(&g, p)).collect(),
                selected: 0,
            }
        } else {
            let mut options: Vec<Choice> = POPULAR
                .iter()
                .filter_map(|id| providers.iter().find(|p| p.id == *id))
                .map(|p| provider_choice(&g, p))
                .collect();
            options.push(Choice {
                label: "All providers…".into(),
                detail: format!("{} on models.dev", providers.len()),
                command: "/model providers".into(),
            });
            Ask::Choose {
                title: "Choose a provider".into(),
                options,
                selected: 0,
            }
        };
        Reply {
            text: note,
            ask: Some(ask),
            ..Reply::default()
        }
    }

    /// A provider was chosen: its models, once it has a key.
    async fn provider(&mut self, id: &str) -> Reply {
        let g = match self.garage() {
            Ok(g) => g,
            Err(e) => return Reply::text(e),
        };
        let c = match self.catalog(false).await {
            Ok((c, _)) => c,
            Err(e) => return Reply::text(e),
        };
        let Some(p) = c.provider(id) else {
            return Reply::text(format!(
                "no provider {id:?} usable here — /model setup lists them"
            ));
        };
        if g.provider_key(&p.id, &p.env).is_some() {
            return Reply::ask(models_choice(c, &p));
        }
        self.pending = None;
        Reply::ask(key_ask(&p.name, &p.id, &p.env, None))
    }

    /// `key <provider>` asks for it; `key <provider> <key>` checks it, keeps
    /// it, and goes on with whatever was waiting for it.
    async fn key(&mut self, rest: &str) -> Reply {
        let (id, secret) = rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(a, b)| (a, b.trim()));
        let Some(path) = self.garage.clone() else {
            return Reply::text("nowhere to keep keys: no config directory");
        };
        let mut g = match self.garage() {
            Ok(g) => g,
            Err(e) => return Reply::text(e),
        };
        let id = match id {
            "" => match self.current.as_deref().and_then(|c| c.split_once('/')) {
                Some((p, _)) => p.to_string(),
                None => return Reply::text("usage: /model key <provider>"),
            },
            id => id.to_string(),
        };
        let info = match self.catalog(false).await {
            Ok((c, _)) => c.provider(&id),
            Err(_) => None,
        };
        // where to check it: an entry of this provider, else any of its models
        let entry = g.models.iter().find(|e| e.provider() == id).cloned();
        let base = entry.as_ref().map(|e| e.base_url.clone()).or_else(|| {
            let c = self.catalog.as_ref()?;
            Some(c.models_of(&id).first()?.entry.base_url.clone())
        });
        let (name, env) = match (&info, &entry) {
            (Some(p), _) => (p.name.clone(), p.env.clone()),
            (None, Some(e)) => (id.clone(), e.key_env.clone()),
            (None, None) => {
                return Reply::text(format!("no provider {id:?} — /model setup lists them"))
            }
        };
        if secret.is_empty() {
            return Reply::ask(key_ask(&name, &id, &env, None));
        }
        if secret.contains(char::is_whitespace) {
            let why = "that has spaces in it, so it is not one key";
            return Reply::ask(key_ask(&name, &id, &env, Some(why)));
        }
        let checked = match &base {
            Some(url) => check_key(url, secret).await,
            None => KeyCheck::Unknown("no endpoint known".into()),
        };
        let checked = match checked {
            KeyCheck::Refused(code) => {
                let why = format!("{name} refused it (HTTP {code})");
                return Reply::ask(key_ask(&name, &id, &env, Some(&why)));
            }
            KeyCheck::Accepted => "checked ✓".to_string(),
            KeyCheck::Unknown(why) => format!("not checked ({why})"),
        };
        g.keys.insert(id.clone(), secret.to_string());
        if let Err(why) = g.save(&path) {
            return Reply::text(why);
        }
        let saved = format!(
            "{name} key {checked} · kept in {} (only you can read it)",
            path.display()
        );
        // whatever was waiting for it, else one of its models to use it with
        let waiting = self
            .pending
            .take()
            .filter(|t| t.split('/').next() == Some(id.as_str()))
            .and_then(|t| g.models.iter().find(|e| e.id == t).cloned());
        let mut then = match (waiting, &info) {
            (Some(e), _) => self.switch_entry(&g, &e),
            (None, Some(p)) => match &self.catalog {
                Some(c) => Reply::ask(models_choice(c, p)),
                None => Reply::default(),
            },
            (None, None) => Reply::default(),
        };
        then.text = lines([saved, then.text]);
        then
    }

    async fn search(&mut self, query: &str) -> Reply {
        if query.is_empty() {
            return Reply::text(
                "usage: /model search <words> — e.g. deepseek, qwen coder, claude sonnet",
            );
        }
        let (c, note) = match self.catalog(false).await {
            Ok(c) => c,
            Err(e) => return Reply::text(e),
        };
        let hits = c.search(query);
        if hits.is_empty() {
            return Reply::text(lines([
                format!("nothing on models.dev usable here matches {query:?}"),
                note,
            ]));
        }
        let more = match hits.len() {
            n if n > SHOWN => format!(", first {SHOWN}: add words to narrow it"),
            _ => String::new(),
        };
        let title = format!(
            "{} for {query:?}{more} — ✓ key found · Enter adds",
            plural(hits.len(), "match", "matches")
        );
        let options = hits
            .iter()
            .take(SHOWN)
            .map(|h| Choice {
                label: format!("{} {}", if h.keyed { '✓' } else { ' ' }, h.entry.id),
                detail: model_detail(&h.entry, h.reasoning),
                command: format!("/model add {}", h.entry.id),
            })
            .collect();
        Reply {
            text: note,
            ask: Some(Ask::Choose {
                title,
                options,
                selected: 0,
            }),
            ..Reply::default()
        }
    }

    async fn add(&mut self, spec: &str) -> Reply {
        if spec.is_empty() {
            return Reply::text("usage: /model add <provider/model> — /model setup picks one");
        }
        let Some(path) = self.garage.clone() else {
            return Reply::text("nowhere to keep models: no config directory");
        };
        let mut g = match self.garage() {
            Ok(g) => g,
            Err(e) => return Reply::text(e),
        };
        let (found, note) = match self.catalog(false).await {
            Ok((c, note)) => (c.find(spec), note),
            Err(e) => return Reply::text(e),
        };
        let e = match found {
            Ok(e) => e,
            Err(why) => return Reply::text(lines([why, note])),
        };
        let n = g.add(e.clone()) + 2;
        if let Err(why) = g.save(&path) {
            return Reply::text(why);
        }
        let added = format!(
            "added #{n} {} — {} context · {} in · {} out · {} cached, per 1M",
            e.id,
            window(e.context),
            usd(e.price_input),
            usd(e.price_output),
            usd(e.price_cached)
        );
        let mut reply = self.switch_entry(&g, &e);
        reply.text = lines([added, note, reply.text]);
        reply
    }

    fn remove(&mut self, name: &str) -> String {
        let Some(path) = self.garage.clone() else {
            return "nothing to remove".into();
        };
        let mut g = match self.garage() {
            Ok(g) => g,
            Err(e) => return e,
        };
        let i = match self.pick(&g, name) {
            Ok(Some(i)) => i,
            Ok(None) => return "the configured model lives in config.toml".into(),
            Err(e) => return e,
        };
        let e = g.models.remove(i);
        if g.current.as_deref() == Some(e.id.as_str()) {
            g.current = None;
        }
        if let Err(why) = g.save(&path) {
            return why;
        }
        let still = if self.is_current(&e.id) {
            " — still in use until you switch"
        } else {
            ""
        };
        format!("removed {}{still}", e.id)
    }

    async fn refresh(&mut self) -> String {
        match self.catalog(true).await {
            Ok((c, note)) => {
                let (providers, models, usable) = c.counts();
                lines([
                    format!(
                        "models.dev: {models} models from {providers} providers, {usable} usable here"
                    ),
                    note,
                ])
            }
            Err(e) => e,
        }
    }

    /// The entry `name` means — `None` for the configured model: a number
    /// as `/model list` shows them, `config`, or a name the list knows.
    fn pick(&self, g: &Garage, name: &str) -> Result<Option<usize>, String> {
        match name.parse::<usize>() {
            Ok(1) => Ok(None),
            Ok(n) if (2..g.models.len() + 2).contains(&n) => Ok(Some(n - 2)),
            Ok(n) => Err(format!(
                "no model #{n}: /model list shows {}",
                g.models.len() + 1
            )),
            Err(_) if name == "config" || name == self.configured.id => Ok(None),
            Err(_) => g.find(name).map(Some).map_err(|why| {
                format!("{why} — /model list shows them; /model add {name} looks on models.dev")
            }),
        }
    }

    fn switch(&mut self, name: &str) -> Reply {
        let g = match self.garage() {
            Ok(g) => g,
            Err(e) => return Reply::text(e),
        };
        match self.pick(&g, name) {
            Ok(Some(i)) => self.switch_entry(&g, &g.models[i]),
            Ok(None) => self.switch_model(self.configured.clone(), None),
            Err(e) => Reply::text(e),
        }
    }

    /// Switch to `e` — or, without a key for it, ask for one and switch
    /// once it arrives.
    fn switch_entry(&mut self, g: &Garage, e: &Entry) -> Reply {
        match key_for(g, e, &self.configured.endpoint) {
            Some(key) => self.switch_model(self.caps.fit(e, key), Some(e.id.clone())),
            None => {
                self.pending = Some(e.id.clone());
                let name = self
                    .catalog
                    .as_ref()
                    .and_then(|c| c.provider(e.provider()))
                    .map_or(e.provider().to_string(), |p| p.name);
                Reply::ask(key_ask(&name, e.provider(), &e.key_env, None))
            }
        }
    }

    /// Switch to `m`, and remember `id` (none: the configured model) as the
    /// one to start with next time.
    fn switch_model(&mut self, m: Model, id: Option<String>) -> Reply {
        if self.is_current(&m.id) {
            return Reply::text(format!("already on {}", m.id));
        }
        if m.endpoint.api_key.is_empty() {
            return Reply::text(format!("{} has no API key in config.toml", m.id));
        }
        let provider = match Provider::new(m.endpoint.clone()) {
            Ok(p) => p,
            Err(e) => return Reply::text(format!("{e:#}")),
        };
        if let Some(p) = &self.garage {
            if let Ok(mut g) = Garage::load(p) {
                if g.current != id {
                    g.current = id;
                    let _ = g.save(p);
                }
            }
        }
        self.current = Some(m.id.clone());
        self.pending = None;
        let text = format!(
            "→ {} · {} per 1M · compacts past {} tokens",
            m.id,
            price(m.tariff.input, m.tariff.output),
            fmt_tokens(m.context_budget as u64)
        );
        Reply {
            text,
            switch: Some(Switch {
                name: m.endpoint.model,
                provider,
                tariff: m.tariff,
                context_budget: m.context_budget,
            }),
            ask: None,
        }
    }
}

impl Switchboard for Board {
    fn handle<'a>(&'a mut self, args: &'a str) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(self.run(args))
    }
}

fn provider_choice(g: &Garage, p: &ProviderInfo) -> Choice {
    let key = if g.provider_key(&p.id, &p.env).is_some() {
        "key ✓"
    } else {
        "needs a key"
    };
    Choice {
        label: p.name.clone(),
        detail: format!("{} · {key}", plural(p.models, "model", "models")),
        command: format!("/model provider {}", p.id),
    }
}

fn models_choice(c: &Catalog, p: &ProviderInfo) -> Ask {
    Ask::Choose {
        title: format!("{} — choose a model", p.name),
        options: c
            .models_of(&p.id)
            .iter()
            .map(|h| Choice {
                label: h.entry.id.clone(),
                detail: model_detail(&h.entry, h.reasoning),
                command: format!("/model add {}", h.entry.id),
            })
            .collect(),
        selected: 0,
    }
}

fn model_detail(e: &Entry, reasoning: bool) -> String {
    format!(
        "{} · {}{}",
        window(e.context),
        price(e.price_input, e.price_output),
        if reasoning { " · reasoning" } else { "" }
    )
}

/// The masked prompt for a provider's key; `why` says what was wrong with
/// the last one.
fn key_ask(name: &str, id: &str, env: &[String], why: Option<&str>) -> Ask {
    let lead = match why {
        Some(why) => format!("{why} — paste the {name} API key again"),
        None => format!("Paste your {name} API key"),
    };
    let env = match env.first() {
        Some(v) => format!(" (or set {v})"),
        None => String::new(),
    };
    Ask::Secret {
        title: format!("{lead}{env}"),
        command: format!("/model key {id}"),
    }
}

/// The non-empty ones, a line each.
fn lines<const N: usize>(parts: [String; N]) -> String {
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `0.660` → `$0.66`, `2.0` → `$2`.
fn usd(x: f64) -> String {
    let s = format!("{x:.3}");
    format!("${}", s.trim_end_matches('0').trim_end_matches('.'))
}

fn price(input: f64, output: f64) -> String {
    if input == 0.0 && output == 0.0 {
        "price unknown".into()
    } else {
        format!("{}/{}", usd(input), usd(output))
    }
}

/// A context window: `1M`, `262k`, `?`.
fn window(tokens: usize) -> String {
    match tokens {
        0 => "?".into(),
        t if t >= 1_000_000 => {
            let s = format!("{:.1}", t as f64 / 1e6);
            format!("{}M", s.trim_end_matches(".0"))
        }
        t => format!("{}k", t / 1000),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Flags;
    use deeptui_testkit::{Stub, TmpDir};

    /// DeepSeek and the catalog served by `ds`, OpenRouter by `or`.
    fn catalog(ds: &Stub, or: &Stub) -> String {
        r#"{
          "deepseek": {"name": "DeepSeek", "env": ["DEEPTUI_TEST_NO_DEEPSEEK_KEY"],
            "npm": "@ai-sdk/openai-compatible", "api": "DS",
            "models": {"deepseek-v4-pro": {"name": "DeepSeek V4 Pro", "tool_call": true,
              "reasoning": true, "limit": {"context": 1048576, "output": 393216},
              "cost": {"input": 0.66, "output": 1.98, "cache_read": 0.022}}}},
          "openrouter": {"name": "OpenRouter", "env": ["DEEPTUI_TEST_NO_OR_KEY"],
            "npm": "@openrouter/ai-sdk-provider", "api": "OR",
            "models": {"qwen/qwen3-coder": {"name": "Qwen3 Coder", "tool_call": true,
              "limit": {"context": 262144, "output": 65536},
              "cost": {"input": 0.22, "output": 0.95}}}}
        }"#
        .replace("\"DS\"", &format!("{:?}", ds.base_url()))
        .replace("\"OR\"", &format!("{:?}", or.base_url()))
    }

    /// A board whose configured model sits on `ds`, keyed with `key`.
    fn board(t: &TmpDir, ds: &Stub, key: &str) -> Board {
        let base = ds.base_url();
        let s = Settings::load(&Flags {
            model: Some("deepseek-chat"),
            base_url: Some(&base),
            api_key: Some(key),
            no_map: true,
            ..Flags::default()
        })
        .unwrap();
        let source = Source::new(
            format!("{base}/api.json"),
            Some(t.join("cache/models.dev.json")),
        );
        Board::at(Some(t.join("cfg/models.toml")), source, &s)
    }

    /// A reply, flattened: its text, the model it switches to, its question.
    struct Said {
        text: String,
        to: Option<String>,
        ask: String,
    }

    async fn say(b: &mut Board, args: &str) -> Said {
        let r = b.handle(args).await;
        let ask = match r.ask {
            None => String::new(),
            Some(Ask::Choose { title, options, .. }) => {
                let labels: Vec<String> = options
                    .iter()
                    .map(|o| format!("{} ({}) {}", o.label, o.detail, o.command))
                    .collect();
                format!("{title}: {}", labels.join(" | "))
            }
            Some(Ask::Secret { title, command }) => format!("secret {title} → {command}"),
        };
        Said {
            text: r.text,
            to: r.switch.map(|s| s.name),
            ask,
        }
    }

    #[tokio::test]
    async fn search_add_switch_and_remove_from_one_list() {
        let t = TmpDir::new("models-board");
        let (ds, or) = (Stub::new(), Stub::new());
        let mut b = board(&t, &ds, "sk-cfg");

        let list = say(&mut b, "list").await.text;
        assert!(list.contains("●  1  deepseek-chat"), "{list}");
        assert!(list.contains("nothing added yet"), "{list}");
        let pick = say(&mut b, "").await.ask;
        assert!(pick.starts_with("Switch model: deepseek-chat ("), "{pick}");
        assert!(pick
            .ends_with("+ add a model (choose a provider, then one of its models) /model setup"));

        ds.enqueue(200, catalog(&ds, &or));
        let found = say(&mut b, "search coder").await.ask;
        assert!(
            found.starts_with("1 match for \"coder\" — ✓ key found"),
            "{found}"
        );
        assert!(
            found.contains("  openrouter/qwen/qwen3-coder (262k · $0.22/$0.95) /model add openrouter/qwen/qwen3-coder"),
            "{found}"
        );

        // the configured key serves the same service: added and switched
        let added = say(&mut b, "add deepseek/deepseek-v4-pro").await;
        assert!(added
            .text
            .starts_with("added #2 deepseek/deepseek-v4-pro — 1M context · $0.66 in"));
        assert!(
            added
                .text
                .contains("\n→ deepseek/deepseek-v4-pro · $0.66/$1.98 per 1M"),
            "{}",
            added.text
        );
        assert_eq!(added.to.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(ds.requests().len(), 1, "the catalog is fetched once");
        let kept = Garage::load(&t.join("cfg/models.toml")).unwrap();
        assert_eq!(kept.current.as_deref(), Some("deepseek/deepseek-v4-pro"));

        // another service: added, then its key is asked for
        let added = say(&mut b, "add qwen/qwen3-coder").await;
        assert!(added.text.starts_with("added #3"), "{}", added.text);
        assert_eq!(added.to, None);
        assert_eq!(
            added.ask,
            "secret Paste your OpenRouter API key (or set DEEPTUI_TEST_NO_OR_KEY) → /model key openrouter"
        );
        let list = say(&mut b, "list").await.text;
        assert!(list.contains("●  2  deepseek/deepseek-v4-pro"), "{list}");
        assert!(list.contains("262k · $0.22/$0.95 · no key"), "{list}");

        // a refused key is asked for again; a good one is kept and the
        // waiting switch goes ahead
        or.enqueue(401, "");
        let refused = say(&mut b, "key openrouter sk-bad").await;
        assert!(
            refused.ask.starts_with(
                "secret OpenRouter refused it (HTTP 401) — paste the OpenRouter API key again"
            ),
            "{}",
            refused.ask
        );
        assert!(Garage::load(&t.join("cfg/models.toml"))
            .unwrap()
            .keys
            .is_empty());
        or.enqueue(200, r#"{"data":[]}"#);
        let kept = say(&mut b, "key openrouter sk-or").await;
        assert!(
            kept.text.starts_with("OpenRouter key checked ✓ · kept in "),
            "{}",
            kept.text
        );
        assert!(
            kept.text.contains("\n→ openrouter/qwen/qwen3-coder"),
            "{}",
            kept.text
        );
        assert!(!kept.text.contains("sk-or"));
        assert_eq!(kept.to.as_deref(), Some("qwen/qwen3-coder"));
        let reqs = or.requests();
        assert!(reqs[1]
            .to_lowercase()
            .contains("authorization: bearer sk-or"));
        let g = Garage::load(&t.join("cfg/models.toml")).unwrap();
        assert_eq!(g.keys["openrouter"], "sk-or");

        assert!(say(&mut b, "v4-pro")
            .await
            .text
            .starts_with("→ deepseek/deepseek-v4-pro"));
        assert_eq!(
            say(&mut b, "v4-pro").await.text,
            "already on deepseek/deepseek-v4-pro"
        );
        let back = say(&mut b, "1").await;
        assert!(back.text.starts_with("→ deepseek-chat"), "{}", back.text);
        assert_eq!(back.to.as_deref(), Some("deepseek-chat"));
        let kept = Garage::load(&t.join("cfg/models.toml")).unwrap();
        assert_eq!(kept.current, None, "back on the configured model");

        assert!(say(&mut b, "9")
            .await
            .text
            .starts_with("no model #9: /model list shows 3"));
        assert_eq!(
            say(&mut b, "rm 3").await.text,
            "removed openrouter/qwen/qwen3-coder"
        );
        assert_eq!(
            say(&mut b, "rm 1").await.text,
            "the configured model lives in config.toml"
        );
        assert!(say(&mut b, "add").await.text.starts_with("usage:"));

        ds.enqueue(304, "");
        assert_eq!(
            say(&mut b, "refresh").await.text,
            "models.dev: 2 models from 2 providers, 2 usable here"
        );
    }

    #[tokio::test]
    async fn a_first_run_picks_a_provider_then_its_key_then_a_model() {
        let t = TmpDir::new("models-first-run");
        let (ds, or) = (Stub::new(), Stub::new());
        let mut b = board(&t, &ds, "");

        // nothing configured: only the way to add one
        assert_eq!(
            say(&mut b, "").await.ask,
            "Switch model: + add a model (choose a provider, then one of its models) /model setup"
        );
        ds.enqueue(200, catalog(&ds, &or));
        assert_eq!(
            say(&mut b, "setup").await.ask,
            "Choose a provider: DeepSeek (1 model · needs a key) /model provider deepseek \
             | OpenRouter (1 model · needs a key) /model provider openrouter \
             | All providers… (2 on models.dev) /model providers"
        );
        assert!(say(&mut b, "providers")
            .await
            .ask
            .starts_with("All 2 providers — type to filter: DeepSeek"));

        assert_eq!(
            say(&mut b, "provider deepseek").await.ask,
            "secret Paste your DeepSeek API key (or set DEEPTUI_TEST_NO_DEEPSEEK_KEY) → /model key deepseek"
        );
        assert!(say(&mut b, "key deepseek two words")
            .await
            .ask
            .starts_with("secret that has spaces in it"));

        // checked against the provider, kept, then its models are offered
        ds.enqueue(200, r#"{"data":[]}"#);
        let kept = say(&mut b, "key deepseek sk-ds").await;
        assert!(
            kept.text.starts_with("DeepSeek key checked ✓"),
            "{}",
            kept.text
        );
        assert_eq!(
            kept.ask,
            "DeepSeek — choose a model: deepseek/deepseek-v4-pro (1M · $0.66/$1.98 · reasoning) \
             /model add deepseek/deepseek-v4-pro"
        );
        let added = say(&mut b, "add deepseek/deepseek-v4-pro").await;
        assert_eq!(added.to.as_deref(), Some("deepseek-v4-pro"));
        assert!(
            added.text.contains("\n→ deepseek/deepseek-v4-pro"),
            "{}",
            added.text
        );
        // with a key on file, the provider goes straight to its models
        assert!(say(&mut b, "provider deepseek")
            .await
            .ask
            .starts_with("DeepSeek — choose a model"));
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(
            (usd(0.66), usd(2.0), usd(0.003)),
            ("$0.66".into(), "$2".into(), "$0.003".into())
        );
        assert_eq!(price(0.0, 0.0), "price unknown");
        assert_eq!(window(1_048_576), "1M");
        assert_eq!(window(1_500_000), "1.5M");
        assert_eq!(window(262_144), "262k");
        assert_eq!(window(0), "?");
        assert_eq!(lines(["a".into(), String::new(), "b".into()]), "a\nb");
    }
}
