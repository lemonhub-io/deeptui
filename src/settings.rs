//! Where every knob comes from: CLI flag → env var → `config.toml` → default.
//!
//! Resolution happens once, here. Components never see the whole of it: the
//! provider gets an [`Endpoint`], the engine a [`Tariff`] and a budget, the
//! tools a timeout — each only what it needs.

use anyhow::{Context, Result};
use deeptui_core::{Limits, Tariff};
use deeptui_models::{Entry, Garage};
use deeptui_provider::Endpoint;
use deeptui_repomap as repomap;
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Default)]
struct FileCfg {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    context_budget: Option<usize>,
    max_output_tokens: Option<u32>,
    bash_timeout_ms: Option<u64>,
    price_input: Option<f64>,
    price_output: Option<f64>,
    price_cached: Option<f64>,
    use_map: Option<bool>,
    map_bytes: Option<usize>,
    update_check: Option<bool>,
    max_rounds: Option<usize>,
    max_task_cost: Option<f64>,
    save_session: Option<bool>,
}

impl FileCfg {
    fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}

/// The command-line half of the resolution.
#[derive(Default)]
pub struct Flags<'a> {
    pub model: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub budget: Option<usize>,
    pub no_map: bool,
    pub map_bytes: Option<usize>,
    pub no_update_check: bool,
    pub max_rounds: Option<usize>,
    pub max_cost: Option<f64>,
}

/// A model ready to use: where it is, what it costs, and how much history
/// it may hold.
#[derive(Clone, Debug)]
pub struct Model {
    /// What `/model` calls it: `provider/model`, or the configured name.
    pub id: String,
    pub endpoint: Endpoint,
    pub tariff: Tariff,
    /// Compact history when the estimate exceeds this many tokens.
    pub context_budget: usize,
}

/// The configured budget and output cap every model is fitted into.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub budget: usize,
    pub max_output: u32,
}

impl Caps {
    /// `e`, reached with `key`, within the caps — and within its own
    /// window, less room for one reply and a margin for the estimate.
    pub fn fit(&self, e: &Entry, key: String) -> Model {
        let max_output = match e.output {
            0 => self.max_output,
            o => self.max_output.min(o),
        };
        let room = match e.context {
            0 => usize::MAX,
            c => c.saturating_sub(max_output as usize) * 9 / 10,
        };
        Model {
            id: e.id.clone(),
            endpoint: Endpoint {
                base_url: e.base_url.trim_end_matches('/').to_string(),
                api_key: key,
                model: e.model.clone(),
                max_output_tokens: max_output,
            },
            tariff: Tariff {
                input: e.price_input,
                output: e.price_output,
                cached: e.price_cached,
                // models.dev has list prices; the table knows peak windows
                deepseek_peak: Tariff::for_model(&e.model).is_some_and(|t| t.deepseek_peak),
            },
            context_budget: self.budget.min(room).max(4_000),
        }
    }
}

/// `https://api.x.com/v1` → `api.x.com`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// The key for `e`: its own or its provider's, else the configured one
/// when `e` is on the same service.
pub fn key_for(g: &Garage, e: &Entry, configured: &Endpoint) -> Option<String> {
    g.key(e).or_else(|| {
        (host(&e.base_url) == host(&configured.base_url) && !configured.api_key.is_empty())
            .then(|| configured.api_key.clone())
    })
}

/// `models.toml` beside `config.toml`; `DEEPTUI_MODELS_FILE` moves it, and
/// the tests never read the real one.
pub fn garage_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEEPTUI_MODELS_FILE") {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    dirs::config_dir().map(|d| d.join("deeptui").join("models.toml"))
}

/// The kept copy of the models.dev catalog.
pub fn catalog_cache() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    dirs::cache_dir().map(|d| d.join("deeptui").join("models.dev.json"))
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// The model to start with.
    pub model: Model,
    /// The one flags, env and `config.toml` name: always a `/model` away.
    pub configured: Model,
    pub caps: Caps,
    pub bash_timeout_ms: u64,
    /// Byte budget of the repo map injected into the system prompt;
    /// `None` leaves the map out.
    pub map: Option<usize>,
    /// Look for a newer release on startup (cached, at most once a day).
    pub update_check: bool,
    /// Rounds and dollars one task may take.
    pub limits: Limits,
    /// Keep conversations per project, for `/resume` and `--continue`.
    pub save_session: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// flag → `DEEPTUI_*` env → file, then the `OPENAI_*` env, then the default.
fn pick(
    flag: Option<&str>,
    deeptui: &str,
    file: Option<String>,
    openai: &str,
    def: &str,
) -> String {
    flag.map(str::to_string)
        .or_else(|| env(deeptui))
        .or(file)
        .or_else(|| env(openai))
        .unwrap_or_else(|| def.to_string())
}

fn map_budget(flag: Option<usize>, file: Option<usize>) -> usize {
    flag.or_else(|| env("DEEPTUI_MAP_BYTES").and_then(|v| v.parse().ok()))
        .or(file)
        .map_or(repomap::DEFAULT_BUDGET, |b| {
            b.clamp(repomap::MIN_BUDGET, repomap::MAX_BUDGET)
        })
}

/// The repo-map budget alone (for `--dump-map`, which needs no API key).
pub fn map_bytes(flag: Option<usize>) -> usize {
    map_budget(flag, FileCfg::load().map_bytes)
}

impl Settings {
    /// The start model has a key: prompts can run.
    pub fn ready(&self) -> bool {
        !self.model.endpoint.api_key.is_empty()
    }

    pub fn load(flags: &Flags) -> Result<Self> {
        let f = FileCfg::load();
        let garage = garage_path()
            .as_deref()
            .map(|p| {
                Garage::load(p).unwrap_or_else(|e| {
                    eprintln!("warning: {e} — ignoring it");
                    Garage::default()
                })
            })
            .unwrap_or_default();
        // a name on the list picks that entry; any other is a model name for
        // the configured endpoint
        let listed = flags
            .model
            .and_then(|m| garage.find(m).ok())
            .map(|i| &garage.models[i]);
        let caps = Caps {
            // floor: a degenerate budget would compact the whole history away
            budget: flags
                .budget
                .or(f.context_budget)
                .unwrap_or(60_000)
                .max(4_000),
            max_output: f.max_output_tokens.unwrap_or(8_192),
        };
        let api_key = pick(
            flags.api_key,
            "DEEPTUI_API_KEY",
            f.api_key,
            "OPENAI_API_KEY",
            "",
        );
        let base_url = pick(
            flags.base_url,
            "DEEPTUI_BASE_URL",
            f.base_url,
            "OPENAI_BASE_URL",
            "https://api.openai.com/v1",
        );
        let model = pick(
            flags.model.filter(|_| listed.is_none()),
            "DEEPTUI_MODEL",
            f.model,
            "OPENAI_MODEL",
            "gpt-4o-mini",
        );

        // the table knows real prices; the file can only price unknown models
        let tariff = Tariff::for_model(&model).unwrap_or_else(|| {
            let (input, output) = f.price_input.zip(f.price_output).unwrap_or((0.0, 0.0));
            Tariff {
                input,
                output,
                cached: f.price_cached.unwrap_or(0.0),
                deepseek_peak: false,
            }
        });

        let configured = Model {
            id: model.clone(),
            endpoint: Endpoint {
                api_key,
                base_url: base_url.trim_end_matches('/').to_string(),
                model,
                max_output_tokens: caps.max_output,
            },
            tariff,
            context_budget: caps.budget,
        };

        // asked for by name, or the one `/model` last switched to
        let remembered = (flags.model.is_none() && env("DEEPTUI_MODEL").is_none())
            .then(|| garage.current())
            .flatten();
        let mut start = match listed.or(remembered) {
            None => configured.clone(),
            Some(e) => match flags
                .api_key
                .map(str::to_string)
                .or_else(|| key_for(&garage, e, &configured.endpoint))
            {
                Some(key) => caps.fit(e, key),
                // remembered but keyless here: the configured model, if it can run
                None if listed.is_none() && !configured.endpoint.api_key.is_empty() => {
                    eprintln!(
                        "warning: no API key for {} — starting on {}",
                        e.id, configured.id
                    );
                    configured.clone()
                }
                // the TUI asks for the key; headless refuses to start
                None => caps.fit(e, String::new()),
            },
        };
        if let Some(url) = flags.base_url {
            start.endpoint.base_url = url.trim_end_matches('/').to_string();
        }

        let use_map = !flags.no_map && f.use_map.unwrap_or(true);
        Ok(Settings {
            model: start,
            configured,
            caps,
            bash_timeout_ms: f.bash_timeout_ms.unwrap_or(30_000),
            map: use_map.then(|| map_budget(flags.map_bytes, f.map_bytes)),
            // set `DEEPTUI_NO_UPDATE_CHECK` to anything non-empty to opt out
            update_check: !flags.no_update_check
                && f.update_check.unwrap_or(true)
                && env("DEEPTUI_NO_UPDATE_CHECK").is_none(),
            limits: Limits {
                max_rounds: flags.max_rounds.or(f.max_rounds).unwrap_or(60),
                max_cost: flags.max_cost.or(f.max_task_cost).unwrap_or(0.0).max(0.0),
            },
            save_session: f.save_session.unwrap_or(true),
        })
    }
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("deeptui").join("config.toml"))
}

pub fn write_example() -> Result<()> {
    let p = config_path().context("no config dir")?;
    write_example_at(&p)
}

fn write_example_at(p: &Path) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(p) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            println!("kept existing {}", p.display());
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    file.write_all(
        "# DeepTUI config — all fields optional, env vars take precedence over this file,\n\
         # CLI flags take precedence over env vars.\n\
         # api_key = \"sk-...\"\n\
         # base_url = \"https://api.deepseek.com/v1\"\n\
         # model = \"deepseek-flash\"\n\
         # context_budget = 60000\n\
         # max_output_tokens = 8192\n\
         # bash_timeout_ms = 30000\n\
         # price_input = 0.15\n\
         # price_output = 0.60\n\
         # price_cached = 0.003\n\
         # use_map = true\n\
         # map_bytes = 5000   # repo map size in bytes\n\
         # update_check = true   # look for the required release once a day\n\
         # max_rounds = 60       # model rounds per task; the last one reports status (0 = no cap)\n\
         # max_task_cost = 0.50  # $ per task before it is stopped (0 = no cap)\n\
         # save_session = true   # keep conversations per project for /resume and --continue\n"
            .as_bytes(),
    )?;
    println!("wrote {}", p.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;

    #[test]
    fn initialization_preserves_existing_credentials_and_settings() {
        let t = TmpDir::new("config-init-existing");
        let p = t.join("config.toml");
        let original = "api_key = \"test-key\"\nmodel = \"custom-model\"\n";
        std::fs::write(&p, original).unwrap();
        write_example_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
    }

    #[test]
    fn initialization_creates_a_private_example_config() {
        let t = TmpDir::new("config-init-new");
        let p = t.join("nested/config.toml");
        write_example_at(&p).unwrap();
        assert!(std::fs::read_to_string(&p)
            .unwrap()
            .contains("deepseek-flash"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    fn flags<'a>(
        model: &'a str,
        base_url: Option<&'a str>,
        budget: Option<usize>,
        no_map: bool,
    ) -> Flags<'a> {
        Flags {
            model: Some(model),
            base_url,
            api_key: Some("k"),
            budget,
            no_map,
            ..Flags::default()
        }
    }

    #[test]
    fn known_models_get_table_prices_and_peak_flags() {
        let s = Settings::load(&flags(
            "deepseek-reasoner",
            Some("http://x/v1/"),
            None,
            true,
        ))
        .expect("load with cli flags");
        let t = s.model.tariff;
        assert_eq!((t.input, t.output, t.cached), (0.28, 0.42, 0.028));
        assert!(t.deepseek_peak, "deepseek models have peak windows");
        assert_eq!(s.map, None, "--no-map must win over the config file");
        // trailing slashes are trimmed so endpoint joins stay tidy
        assert_eq!(s.model.endpoint.base_url, "http://x/v1");
        assert_eq!(s.model.endpoint.model, "deepseek-reasoner");
        assert_eq!(s.model.endpoint.api_key, "k");
    }

    #[test]
    fn load_prefers_cli_flags_and_keeps_the_budget_floor() {
        let s = Settings::load(&flags(
            "gpt-4o-mini",
            Some("https://api.x/v1/"),
            Some(10),
            true,
        ))
        .unwrap();
        assert_eq!(s.model.endpoint.model, "gpt-4o-mini");
        assert_eq!(s.model.endpoint.base_url, "https://api.x/v1");
        assert_eq!(s.model.context_budget, 4_000); // degenerate budget is floored
        let t = s.model.tariff;
        assert_eq!((t.input, t.output, t.cached), (0.15, 0.60, 0.075));
        assert!(!t.deepseek_peak);

        let s = Settings::load(&flags("gpt-4o-mini", None, Some(123_456), false)).unwrap();
        assert_eq!(s.model.context_budget, 123_456);

        // deepseek models carry the peak-billing flag and their own rates
        let s = Settings::load(&flags("deepseek-chat", None, None, false)).unwrap();
        assert!(s.model.tariff.deepseek_peak);
        assert_eq!((s.model.tariff.input, s.model.tariff.cached), (0.28, 0.028));
    }

    #[test]
    fn the_map_flag_is_clamped_and_no_update_check_wins() {
        let s = Settings::load(&Flags {
            map_bytes: Some(10),
            no_update_check: true,
            ..flags("gpt-4o-mini", None, None, false)
        })
        .unwrap();
        // the map may be off in the user's config file; when on, it is clamped
        if let Some(b) = s.map {
            assert_eq!(b, repomap::MIN_BUDGET);
        }
        assert!(!s.update_check);
        // a flag beats the environment and the file, and is clamped too
        assert_eq!(map_budget(Some(1 << 30), Some(7_000)), repomap::MAX_BUDGET);
    }

    fn entry(context: usize, output: u32) -> Entry {
        Entry {
            id: "deepseek/deepseek-chat".into(),
            model: "deepseek-chat".into(),
            base_url: "https://api.deepseek.com/".into(),
            key_env: vec!["DEEPTUI_TEST_NO_SUCH_KEY".into()],
            context,
            output,
            price_input: 0.5,
            price_output: 1.5,
            price_cached: 0.05,
            ..Entry::default()
        }
    }

    #[test]
    fn an_entry_is_fitted_into_the_caps_and_its_own_window() {
        let caps = Caps {
            budget: 60_000,
            max_output: 8_192,
        };
        let m = caps.fit(&entry(1_000_000, 4_096), "k".into());
        assert_eq!(m.id, "deepseek/deepseek-chat");
        assert_eq!(m.endpoint.base_url, "https://api.deepseek.com");
        assert_eq!(
            (m.endpoint.max_output_tokens, m.context_budget),
            (4_096, 60_000)
        );
        let t = m.tariff;
        assert_eq!((t.input, t.output, t.cached), (0.5, 1.5, 0.05));
        assert!(t.deepseek_peak, "the table still knows the peak windows");
        // a small window caps the budget below it; unknown limits don't
        let m = caps.fit(&entry(32_000, 0), "k".into());
        assert_eq!(
            (m.endpoint.max_output_tokens, m.context_budget),
            (8_192, 21_427)
        );
        assert_eq!(caps.fit(&entry(0, 0), "k".into()).context_budget, 60_000);
        assert_eq!(caps.fit(&entry(4_000, 0), "k".into()).context_budget, 4_000);
    }

    #[test]
    fn an_entry_borrows_the_configured_key_only_on_the_same_service() {
        let configured = Endpoint {
            base_url: "https://api.deepseek.com/v1".into(),
            api_key: "sk-cfg".into(),
            model: "deepseek-chat".into(),
            max_output_tokens: 1,
        };
        let none = Garage::default();
        let found = |g: &Garage, e: &Entry| key_for(g, e, &configured);
        assert_eq!(found(&none, &entry(0, 0)).as_deref(), Some("sk-cfg"));
        let elsewhere = Entry {
            base_url: "https://openrouter.ai/api/v1".into(),
            ..entry(0, 0)
        };
        assert_eq!(found(&none, &elsewhere), None);
        // a key typed in for its provider serves it anywhere
        let mut kept = Garage::default();
        kept.keys.insert("deepseek".into(), "sk-kept".into());
        assert_eq!(found(&kept, &elsewhere).as_deref(), Some("sk-kept"));
        // and its own key comes first
        let written = Entry {
            api_key: Some("sk-own".into()),
            ..elsewhere
        };
        assert_eq!(found(&kept, &written).as_deref(), Some("sk-own"));
        assert_eq!(host("http://127.0.0.1:9/v1"), "127.0.0.1:9");
    }

    #[test]
    fn task_limit_flags_win_and_a_negative_cap_is_none() {
        let s = Settings::load(&Flags {
            max_rounds: Some(7),
            max_cost: Some(-1.0),
            ..flags("gpt-4o-mini", None, None, true)
        })
        .unwrap();
        assert_eq!(
            s.limits,
            Limits {
                max_rounds: 7,
                max_cost: 0.0
            }
        );
    }
}
