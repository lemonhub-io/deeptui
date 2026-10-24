//! models.dev: one JSON document holding every provider and model it knows.
//! Only some of them can be reached from here — the model must call tools,
//! and its provider must take OpenAI-shaped chat completions — and only
//! those are offered.

use crate::Entry;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const URL: &str = "https://models.dev/api.json";
/// How long a fetched copy is used without asking again.
const FRESH: Duration = Duration::from_secs(24 * 3600);

#[derive(Deserialize)]
#[serde(transparent)]
pub struct Catalog(BTreeMap<String, Provider>);

#[derive(Deserialize)]
pub struct Provider {
    #[serde(default)]
    name: String,
    #[serde(default)]
    env: Vec<String>,
    npm: Option<String>,
    api: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, Model>,
}

#[derive(Deserialize)]
struct Model {
    #[serde(default)]
    name: String,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
    cost: Option<Cost>,
    #[serde(default)]
    limit: Limit,
    status: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    /// Per-model overrides of the provider's SDK, URL and API shape.
    provider: Option<Over>,
    #[serde(default)]
    release_date: String,
}

#[derive(Deserialize)]
struct Cost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    cache_read: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Limit {
    #[serde(default)]
    context: usize,
    #[serde(default)]
    output: u32,
}

#[derive(Deserialize)]
struct Over {
    npm: Option<String>,
    api: Option<String>,
    shape: Option<String>,
}

/// Providers models.dev gives no URL for, at their well-known
/// OpenAI-compatible endpoints.
const KNOWN: &[(&str, &str)] = &[
    ("openai", "https://api.openai.com/v1"),
    ("anthropic", "https://api.anthropic.com/v1"),
    (
        "google",
        "https://generativelanguage.googleapis.com/v1beta/openai",
    ),
    ("xai", "https://api.x.ai/v1"),
    ("groq", "https://api.groq.com/openai/v1"),
    ("mistral", "https://api.mistral.ai/v1"),
    ("cerebras", "https://api.cerebras.ai/v1"),
    ("togetherai", "https://api.together.xyz/v1"),
    ("deepinfra", "https://api.deepinfra.com/v1/openai"),
];

/// SDKs whose `api` URL takes OpenAI-shaped chat completions.
const COMPATIBLE: &[&str] = &[
    "@ai-sdk/openai-compatible",
    "@ai-sdk/openai",
    "@openrouter/ai-sdk-provider",
];

/// Providers that make the models they serve: ranked above resellers of
/// the same model.
const MAKERS: &[&str] = &[
    "deepseek",
    "openai",
    "anthropic",
    "google",
    "xai",
    "mistral",
    "moonshotai",
    "zhipuai",
    "alibaba",
    "minimax",
];

/// A provider, as far as it is usable here.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    /// Env vars that may hold its key.
    pub env: Vec<String>,
    /// Its models usable here.
    pub models: usize,
}

/// One search result.
pub struct Hit {
    pub entry: Entry,
    pub name: String,
    pub reasoning: bool,
    /// A key for it is already at hand.
    pub keyed: bool,
}

impl Catalog {
    pub fn parse(json: &str) -> Result<Catalog, String> {
        serde_json::from_str(json).map_err(|e| format!("models.dev sent something unreadable: {e}"))
    }

    /// Where `m` is reached, or why it cannot be.
    fn reach(pid: &str, p: &Provider, m: &Model) -> Result<String, &'static str> {
        if !m.tool_call {
            return Err("it cannot call tools");
        }
        if m.kind.as_deref().is_some_and(|k| k != "chat") {
            return Err("it is not a chat model");
        }
        let over = m.provider.as_ref();
        if over.and_then(|o| o.shape.as_deref()) == Some("responses") {
            return Err("it only speaks the Responses API");
        }
        let npm = over
            .and_then(|o| o.npm.as_deref())
            .or(p.npm.as_deref())
            .unwrap_or("");
        let url = match over.and_then(|o| o.api.as_deref()).or(p.api.as_deref()) {
            Some(api) if api.contains("${") => return Err("its URL needs account-specific values"),
            Some(api) if COMPATIBLE.contains(&npm) => api,
            Some(_) => return Err("its API is not OpenAI-compatible"),
            None => KNOWN
                .iter()
                .find(|(id, _)| *id == pid)
                .map(|(_, url)| *url)
                .ok_or("its API is not OpenAI-compatible")?,
        };
        Ok(url.trim_end_matches('/').to_string())
    }

    fn entry(pid: &str, p: &Provider, mid: &str, m: &Model, base_url: String) -> Entry {
        let input = m.cost.as_ref().map_or(0.0, |c| c.input);
        Entry {
            id: format!("{pid}/{mid}"),
            model: mid.to_string(),
            base_url,
            key_env: p.env.clone(),
            api_key: None,
            context: m.limit.context,
            output: m.limit.output,
            price_input: input,
            price_output: m.cost.as_ref().map_or(0.0, |c| c.output),
            // no cache price: a hit costs what a miss does
            price_cached: m.cost.as_ref().and_then(|c| c.cache_read).unwrap_or(input),
        }
    }

    /// Usable models matching every word of `query`. First come those a key
    /// is at hand for, then those of a provider named in the query, then
    /// those from their maker, then the newest.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut hits = Vec::new();
        for (pid, p) in &self.0 {
            for (mid, m) in &p.models {
                if m.status.as_deref() == Some("deprecated") {
                    continue;
                }
                let hay = format!("{pid}/{mid} {} {}", m.name, p.name).to_lowercase();
                if !words.iter().all(|w| hay.contains(w.as_str())) {
                    continue;
                }
                let Ok(url) = Self::reach(pid, p, m) else {
                    continue;
                };
                let entry = Self::entry(pid, p, mid, m, url);
                let hit = Hit {
                    keyed: entry.key().is_some(),
                    entry,
                    name: m.name.clone(),
                    reasoning: m.reasoning,
                };
                let named = words.iter().any(|w| w == pid);
                let rank = (hit.keyed, named, MAKERS.contains(&pid.as_str()));
                hits.push((rank, m.release_date.as_str(), hit));
            }
        }
        hits.sort_by(|(ra, da, _), (rb, db, _)| rb.cmp(ra).then(db.cmp(da)));
        hits.into_iter().map(|(_, _, h)| h).collect()
    }

    /// The model `spec` names: `provider/model`, or a model id one provider
    /// offers — or the one provider of several a key is at hand for.
    pub fn find(&self, spec: &str) -> Result<Entry, String> {
        if let Some((pid, mid)) = spec.split_once('/') {
            if let Some(p) = self.0.get(pid) {
                let m = p.models.get(mid).ok_or_else(|| {
                    format!("{} has no model {mid:?} — try /model search {mid}", p.name)
                })?;
                let url = Self::reach(pid, p, m)
                    .map_err(|why| format!("{spec} can't be used here: {why}"))?;
                return Ok(Self::entry(pid, p, mid, m, url));
            }
        }
        // a bare id — or one with a slash, as aggregators name theirs
        let mut offers: Vec<Entry> = self
            .0
            .iter()
            .filter_map(|(pid, p)| {
                let m = p.models.get(spec)?;
                let url = Self::reach(pid, p, m).ok()?;
                Some(Self::entry(pid, p, spec, m, url))
            })
            .collect();
        if offers.len() > 1 {
            let keyed: Vec<usize> = (0..offers.len())
                .filter(|&i| offers[i].key().is_some())
                .collect();
            if let [i] = keyed.as_slice() {
                return Ok(offers.swap_remove(*i));
            }
        }
        match offers.len() {
            0 => Err(format!(
                "models.dev has no usable model {spec:?} — try /model search {spec}"
            )),
            1 => Ok(offers.remove(0)),
            n => {
                let ids: Vec<&str> = offers.iter().take(6).map(|e| e.id.as_str()).collect();
                let more = if n > 6 { ", …" } else { "" };
                Err(format!(
                    "{spec} is offered by {n} providers — name one: {}{more}",
                    ids.join(", ")
                ))
            }
        }
    }

    /// Providers with a model usable here, by name.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        let mut all: Vec<ProviderInfo> = self.0.keys().filter_map(|id| self.provider(id)).collect();
        all.sort_by_key(|p| p.name.to_lowercase());
        all
    }

    pub fn provider(&self, id: &str) -> Option<ProviderInfo> {
        let p = self.0.get(id)?;
        let models = p
            .models
            .values()
            .filter(|m| m.status.as_deref() != Some("deprecated"))
            .filter(|m| Self::reach(id, p, m).is_ok())
            .count();
        (models > 0).then(|| ProviderInfo {
            id: id.to_string(),
            name: if p.name.is_empty() { id } else { &p.name }.to_string(),
            env: p.env.clone(),
            models,
        })
    }

    /// A provider's usable models, newest first.
    pub fn models_of(&self, id: &str) -> Vec<Hit> {
        let Some(p) = self.0.get(id) else {
            return Vec::new();
        };
        let mut hits: Vec<(&str, Hit)> = p
            .models
            .iter()
            .filter(|(_, m)| m.status.as_deref() != Some("deprecated"))
            .filter_map(|(mid, m)| {
                let url = Self::reach(id, p, m).ok()?;
                let entry = Self::entry(id, p, mid, m, url);
                let hit = Hit {
                    keyed: entry.key().is_some(),
                    entry,
                    name: m.name.clone(),
                    reasoning: m.reasoning,
                };
                Some((m.release_date.as_str(), hit))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(a.0));
        hits.into_iter().map(|(_, h)| h).collect()
    }

    /// Providers, models, and the models usable here.
    pub fn counts(&self) -> (usize, usize, usize) {
        let models = self.0.values().map(|p| p.models.len()).sum();
        let usable = self
            .0
            .iter()
            .flat_map(|(pid, p)| p.models.values().map(move |m| (pid, p, m)))
            .filter(|(pid, p, m)| Self::reach(pid, p, m).is_ok())
            .count();
        (self.0.len(), models, usable)
    }
}

/// Where the catalog comes from, and where a copy of it is kept.
pub struct Source {
    url: String,
    cache: Option<PathBuf>,
    http: reqwest::Client,
}

enum Fetched {
    /// Unchanged since the kept copy.
    Same,
    New(String, Option<String>),
}

impl Source {
    pub fn new(url: impl Into<String>, cache: Option<PathBuf>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("deeptui/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            url: url.into(),
            cache,
            http,
        }
    }

    /// The catalog: the kept copy while it is fresh, else asked for again —
    /// with the copy's ETag, so an unchanged catalog costs one empty reply —
    /// and when that fails, the stale copy plus a note saying so. `refresh`
    /// asks even when the copy is fresh.
    pub async fn load(&self, refresh: bool) -> Result<(Catalog, Option<String>), String> {
        let age = self
            .cache
            .as_deref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .map(|t| SystemTime::now().duration_since(t).unwrap_or_default());
        if !refresh && age.is_some_and(|a| a < FRESH) {
            if let Some(c) = self.kept() {
                return Ok((c, None));
            }
        }
        match self.fetch(age.is_some()).await {
            Ok(Fetched::Same) => {
                if let Some(p) = &self.cache {
                    let _ = std::fs::File::options()
                        .write(true)
                        .open(p)
                        .and_then(|f| f.set_modified(SystemTime::now()));
                }
                let c = self
                    .kept()
                    .ok_or("the kept copy of models.dev is unreadable")?;
                Ok((c, None))
            }
            Ok(Fetched::New(json, etag)) => {
                let c = Catalog::parse(&json)?;
                if let Some(p) = &self.cache {
                    keep(p, &json, etag.as_deref());
                }
                Ok((c, None))
            }
            Err(e) => match (age, self.kept()) {
                (Some(a), Some(c)) => Ok((
                    c,
                    Some(format!(
                        "models.dev unreachable ({e}); using the copy from {} ago",
                        span(a)
                    )),
                )),
                _ => Err(format!("models.dev unreachable: {e}")),
            },
        }
    }

    fn kept(&self) -> Option<Catalog> {
        let text = std::fs::read_to_string(self.cache.as_ref()?).ok()?;
        Catalog::parse(&text).ok()
    }

    async fn fetch(&self, revalidate: bool) -> Result<Fetched, String> {
        let mut req = self.http.get(&self.url).header("accept-encoding", "gzip");
        let tag = self
            .cache
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p.with_extension("etag")).ok());
        if let Some(tag) = tag.filter(|_| revalidate) {
            req = req.header("if-none-match", tag.trim());
        }
        let r = req.send().await.map_err(|e| e.without_url().to_string())?;
        if r.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(Fetched::Same);
        }
        if !r.status().is_success() {
            return Err(format!("HTTP {}", r.status()));
        }
        let header = |name: &str| {
            r.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let gzip = header("content-encoding").is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
        let etag = header("etag");
        let body = r.bytes().await.map_err(|e| e.without_url().to_string())?;
        Ok(Fetched::New(decode(&body, gzip)?, etag))
    }
}

fn decode(body: &[u8], gzip: bool) -> Result<String, String> {
    let bytes = if gzip {
        let mut out = Vec::with_capacity(body.len() * 10);
        flate2::read::GzDecoder::new(body)
            .read_to_end(&mut out)
            .map_err(|e| format!("broken gzip: {e}"))?;
        out
    } else {
        body.to_vec()
    };
    String::from_utf8(bytes).map_err(|_| "not UTF-8".into())
}

/// Best effort and atomic: a failed write keeps the previous copy.
fn keep(path: &Path, json: &str, etag: Option<&str>) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        let tag = path.with_extension("etag");
        match etag {
            Some(t) => {
                let _ = std::fs::write(tag, t);
            }
            None => {
                let _ = std::fs::remove_file(tag);
            }
        }
    }
}

/// `90000s` → `1d`: the coarsest unit that is not zero.
fn span(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::{Stub, TmpDir};

    /// One of each case `reach` tells apart.
    const SAMPLE: &str = r#"{
      "deepseek": {"name": "DeepSeek", "env": ["DEEPTUI_TEST_KEY_DEEPSEEK"],
        "npm": "@ai-sdk/openai-compatible", "api": "https://api.deepseek.com/",
        "models": {
          "deepseek-v4-pro": {"name": "DeepSeek V4 Pro", "tool_call": true, "reasoning": true,
            "release_date": "2026-08-13", "limit": {"context": 1048576, "output": 393216},
            "cost": {"input": 0.66, "output": 1.98, "cache_read": 0.022}},
          "deepseek-old": {"name": "Old", "tool_call": true, "status": "deprecated",
            "limit": {"context": 64000, "output": 8192}, "cost": {"input": 1, "output": 2}},
          "deepseek-embed": {"name": "Embed", "tool_call": true, "type": "embedding",
            "limit": {"context": 8000, "output": 0}}
        }},
      "openai": {"name": "OpenAI", "env": ["DEEPTUI_TEST_NO_KEY_OPENAI"], "npm": "@ai-sdk/openai",
        "models": {
          "gpt-5-mini": {"name": "GPT-5 Mini", "tool_call": true, "release_date": "2025-08-07",
            "limit": {"context": 400000, "output": 128000},
            "cost": {"input": 0.25, "output": 2}},
          "gpt-5-codex": {"name": "GPT-5 Codex", "tool_call": true,
            "provider": {"shape": "responses"}, "limit": {"context": 400000, "output": 128000}},
          "tts-1": {"name": "TTS", "tool_call": false, "limit": {"context": 0, "output": 0}}
        }},
      "openrouter": {"name": "OpenRouter", "env": ["DEEPTUI_TEST_NO_KEY_OR"],
        "npm": "@openrouter/ai-sdk-provider", "api": "https://openrouter.ai/api/v1",
        "models": {
          "deepseek-v4-pro": {"name": "DeepSeek V4 Pro", "tool_call": true, "release_date": "2026-08-14",
            "limit": {"context": 1048576, "output": 65536}, "cost": {"input": 0.7, "output": 2.1}},
          "qwen/qwen3-coder": {"name": "Qwen3 Coder", "tool_call": true, "release_date": "2025-07-23",
            "limit": {"context": 262144, "output": 65536}, "cost": {"input": 0.22, "output": 0.95}}
        }},
      "anthropicish": {"name": "Shim", "npm": "@ai-sdk/anthropic", "api": "https://shim/v1",
        "models": {"claude-x": {"name": "Claude X", "tool_call": true, "limit": {"context": 1, "output": 1}}}},
      "azure": {"name": "Azure", "npm": "@ai-sdk/openai-compatible",
        "api": "https://${AZURE_RESOURCE_NAME}.services.ai.azure.com/models",
        "models": {"gpt-z": {"name": "Z", "tool_call": true, "limit": {"context": 1, "output": 1}}}},
      "bedrock": {"name": "Bedrock", "npm": "@ai-sdk/amazon-bedrock",
        "models": {"nova": {"name": "Nova", "tool_call": true, "limit": {"context": 1, "output": 1}}}}
    }"#;

    fn sample() -> Catalog {
        Catalog::parse(SAMPLE).unwrap()
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.entry.id.as_str()).collect()
    }

    #[test]
    fn only_tool_callers_behind_openai_shaped_apis_are_offered() {
        let c = sample();
        // order aside: another test holds a key that reorders them
        let hits = c.search("");
        let mut all = ids(&hits);
        all.sort();
        assert_eq!(
            all,
            [
                "deepseek/deepseek-v4-pro",
                "openai/gpt-5-mini",
                "openrouter/deepseek-v4-pro",
                "openrouter/qwen/qwen3-coder",
            ]
        );
        assert_eq!(c.counts(), (6, 11, 5), "the deprecated one is still usable");
        let why = |spec: &str| c.find(spec).unwrap_err();
        assert!(why("openai/tts-1").ends_with("it cannot call tools"));
        assert!(why("openai/gpt-5-codex").ends_with("Responses API"));
        assert!(why("deepseek/deepseek-embed").ends_with("not a chat model"));
        assert!(why("anthropicish/claude-x").ends_with("not OpenAI-compatible"));
        assert!(why("bedrock/nova").ends_with("not OpenAI-compatible"));
        assert!(why("azure/gpt-z").ends_with("account-specific values"));
        assert!(why("openai/gpt-9").contains("OpenAI has no model \"gpt-9\""));
    }

    #[test]
    fn providers_and_their_models_are_listed_as_far_as_usable() {
        let c = sample();
        let names: Vec<String> = c.providers().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["DeepSeek", "OpenAI", "OpenRouter"]);
        let ds = c.provider("deepseek").unwrap();
        assert_eq!(
            (ds.models, ds.env[0].as_str()),
            (1, "DEEPTUI_TEST_KEY_DEEPSEEK")
        );
        assert!(c.provider("bedrock").is_none() && c.provider("nope").is_none());
        // newest first
        assert_eq!(
            ids(&c.models_of("openrouter")),
            ["openrouter/deepseek-v4-pro", "openrouter/qwen/qwen3-coder"]
        );
        assert!(c.models_of("nope").is_empty());
    }

    #[test]
    fn an_entry_carries_the_endpoint_limits_and_prices() {
        let e = sample().find("deepseek/deepseek-v4-pro").unwrap();
        assert_eq!(
            e,
            Entry {
                id: "deepseek/deepseek-v4-pro".into(),
                model: "deepseek-v4-pro".into(),
                base_url: "https://api.deepseek.com".into(),
                key_env: vec!["DEEPTUI_TEST_KEY_DEEPSEEK".into()],
                api_key: None,
                context: 1_048_576,
                output: 393_216,
                price_input: 0.66,
                price_output: 1.98,
                price_cached: 0.022,
            }
        );
        // a known provider without a URL; no cache price means no discount
        let e = sample().find("openai/gpt-5-mini").unwrap();
        assert_eq!(e.base_url, "https://api.openai.com/v1");
        assert_eq!(e.price_cached, 0.25);
        // an aggregator's slashed id, whole or bare
        let e = sample().find("openrouter/qwen/qwen3-coder").unwrap();
        assert_eq!((e.model.as_str(), e.context), ("qwen/qwen3-coder", 262_144));
        assert_eq!(sample().find("qwen/qwen3-coder").unwrap().id, e.id);
        assert_eq!(sample().find("gpt-5-mini").unwrap().id, "openai/gpt-5-mini");
    }

    #[test]
    fn words_narrow_a_search_and_a_key_at_hand_decides_ties() {
        let c = sample();
        assert_eq!(
            ids(&c.search("QWEN coder")),
            ["openrouter/qwen/qwen3-coder"]
        );
        assert!(c.search("deepseek old").is_empty(), "deprecated");
        // the maker before a reseller with a newer listing
        assert_eq!(
            ids(&c.search("deepseek pro"))[0],
            "deepseek/deepseek-v4-pro"
        );
        let two = c.find("deepseek-v4-pro").unwrap_err();
        assert!(two.contains("offered by 2 providers"), "{two}");

        // only this test reads the variable
        std::env::set_var("DEEPTUI_TEST_KEY_DEEPSEEK", "sk-test");
        let hits = c.search("v4 pro");
        assert_eq!(
            ids(&hits),
            ["deepseek/deepseek-v4-pro", "openrouter/deepseek-v4-pro"]
        );
        assert!(hits[0].keyed && hits[0].reasoning && !hits[1].keyed);
        assert_eq!(
            c.find("deepseek-v4-pro").unwrap().id,
            "deepseek/deepseek-v4-pro"
        );
        std::env::remove_var("DEEPTUI_TEST_KEY_DEEPSEEK");
    }

    #[tokio::test]
    async fn the_copy_is_reused_while_fresh_and_revalidated_by_etag() {
        let t = TmpDir::new("catalog");
        let stub = Stub::new();
        let cache = t.join("cache/models.dev.json");
        let src = Source::new(format!("{}/api.json", stub.base_url()), Some(cache.clone()));

        stub.enqueue_with(200, &[("ETag", "W/\"v1\"")], SAMPLE);
        let (c, note) = src.load(false).await.unwrap();
        assert_eq!((c.counts().0, note), (6, None));
        let req = stub.requests()[0].to_lowercase();
        assert!(req.contains("accept-encoding: gzip"), "{req}");
        assert!(!req.contains("if-none-match"), "{req}");
        assert_eq!(
            std::fs::read_to_string(cache.with_extension("etag")).unwrap(),
            "W/\"v1\""
        );

        // fresh: no request at all
        src.load(false).await.unwrap();
        assert_eq!(stub.requests().len(), 1);

        // a refresh asks with the tag; unchanged is served from the copy
        stub.enqueue(304, "");
        let (c, _) = src.load(true).await.unwrap();
        assert_eq!(c.counts().0, 6);
        let req = stub.requests()[1].to_lowercase();
        assert!(req.contains("if-none-match: w/\"v1\""), "{req}");
    }

    #[tokio::test]
    async fn offline_a_stale_copy_is_served_with_a_note() {
        let t = TmpDir::new("catalog-offline");
        let cache = t.join("models.dev.json");
        // a port nobody listens on
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/api.json", l.local_addr().unwrap())
        };
        let src = Source::new(&dead, Some(cache.clone()));
        let err = src.load(false).await.err().unwrap();
        assert!(err.starts_with("models.dev unreachable"), "{err}");

        std::fs::write(&cache, SAMPLE).unwrap();
        let two_days = SystemTime::now() - Duration::from_secs(2 * 86_400 + 60);
        std::fs::File::options()
            .write(true)
            .open(&cache)
            .unwrap()
            .set_modified(two_days)
            .unwrap();
        let (c, note) = src.load(false).await.unwrap();
        assert_eq!(c.counts().0, 6);
        let note = note.unwrap();
        assert!(note.ends_with("using the copy from 2d ago"), "{note}");
    }

    #[test]
    fn gzip_bodies_are_inflated() {
        use std::io::Write;
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(SAMPLE.as_bytes()).unwrap();
        let packed = z.finish().unwrap();
        assert_eq!(decode(&packed, true).unwrap(), SAMPLE);
        assert_eq!(decode(b"{}", false).unwrap(), "{}");
        assert!(decode(b"not gzip", true).is_err());
        assert_eq!(span(Duration::from_secs(90_000)), "1d");
        assert_eq!(span(Duration::from_secs(7_300)), "2h");
    }
}
