//! Where every knob comes from: CLI flag → env var → `config.toml` → default.
//!
//! Resolution happens once, here. Components never see the whole of it: the
//! provider gets an [`Endpoint`], the engine a [`Tariff`] and a budget, the
//! tools a timeout — each only what it needs.

use anyhow::{bail, Context, Result};
use deeptui_core::Tariff;
use deeptui_provider::Endpoint;
use deeptui_repomap as repomap;
use serde::Deserialize;
use std::path::PathBuf;

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
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub endpoint: Endpoint,
    pub tariff: Tariff,
    /// Compact history when the estimate exceeds this many tokens.
    pub context_budget: usize,
    pub bash_timeout_ms: u64,
    /// Byte budget of the repo map injected into the system prompt;
    /// `None` leaves the map out.
    pub map: Option<usize>,
    /// Look for a newer release on startup (cached, at most once a day).
    pub update_check: bool,
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
    pub fn load(flags: &Flags) -> Result<Self> {
        let f = FileCfg::load();
        let api_key = pick(
            flags.api_key,
            "DEEPTUI_API_KEY",
            f.api_key,
            "OPENAI_API_KEY",
            "",
        );
        if api_key.is_empty() {
            bail!("no API key: set DEEPTUI_API_KEY / OPENAI_API_KEY, --api-key, or api_key in config.toml");
        }
        let base_url = pick(
            flags.base_url,
            "DEEPTUI_BASE_URL",
            f.base_url,
            "OPENAI_BASE_URL",
            "https://api.openai.com/v1",
        );
        let model = pick(
            flags.model,
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

        let use_map = !flags.no_map && f.use_map.unwrap_or(true);
        Ok(Settings {
            endpoint: Endpoint {
                api_key,
                base_url: base_url.trim_end_matches('/').to_string(),
                model,
                max_output_tokens: f.max_output_tokens.unwrap_or(8_192),
            },
            tariff,
            // floor: a degenerate budget would compact the whole history away
            context_budget: flags
                .budget
                .or(f.context_budget)
                .unwrap_or(60_000)
                .max(4_000),
            bash_timeout_ms: f.bash_timeout_ms.unwrap_or(30_000),
            map: use_map.then(|| map_budget(flags.map_bytes, f.map_bytes)),
            // set `DEEPTUI_NO_UPDATE_CHECK` to anything non-empty to opt out
            update_check: !flags.no_update_check
                && f.update_check.unwrap_or(true)
                && env("DEEPTUI_NO_UPDATE_CHECK").is_none(),
        })
    }
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("deeptui").join("config.toml"))
}

pub fn write_example() -> Result<()> {
    let p = config_path().context("no config dir")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(
        &p,
        "# DeepTUI config — all fields optional, env vars take precedence over this file,\n\
         # CLI flags take precedence over env vars.\n\
         # api_key = \"sk-...\"\n\
         # base_url = \"https://api.deepseek.com/v1\"\n\
         # model = \"deepseek-chat\"\n\
         # context_budget = 60000\n\
         # max_output_tokens = 8192\n\
         # bash_timeout_ms = 30000\n\
         # price_input = 0.15\n\
         # price_output = 0.60\n\
         # price_cached = 0.003\n\
         # use_map = true\n\
         # map_bytes = 5000   # repo map size in bytes\n\
         # update_check = true   # look for a newer release once a day\n",
    )?;
    println!("wrote {}", p.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let t = s.tariff;
        assert_eq!((t.input, t.output, t.cached), (0.28, 0.42, 0.028));
        assert!(t.deepseek_peak, "deepseek models have peak windows");
        assert_eq!(s.map, None, "--no-map must win over the config file");
        // trailing slashes are trimmed so endpoint joins stay tidy
        assert_eq!(s.endpoint.base_url, "http://x/v1");
        assert_eq!(s.endpoint.model, "deepseek-reasoner");
        assert_eq!(s.endpoint.api_key, "k");
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
        assert_eq!(s.endpoint.model, "gpt-4o-mini");
        assert_eq!(s.endpoint.base_url, "https://api.x/v1");
        assert_eq!(s.context_budget, 4_000); // degenerate budget is floored
        let t = s.tariff;
        assert_eq!((t.input, t.output, t.cached), (0.15, 0.60, 0.075));
        assert!(!t.deepseek_peak);

        let s = Settings::load(&flags("gpt-4o-mini", None, Some(123_456), false)).unwrap();
        assert_eq!(s.context_budget, 123_456);

        // deepseek models carry the peak-billing flag and their own rates
        let s = Settings::load(&flags("deepseek-chat", None, None, false)).unwrap();
        assert!(s.tariff.deepseek_peak);
        assert_eq!((s.tariff.input, s.tariff.cached), (0.28, 0.028));
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
}
