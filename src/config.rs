use anyhow::{bail, Context, Result};
use serde::Deserialize;

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
}

/// $ per 1M tokens: (prefix, input miss, output, input cache-hit, deepseek peak hours ×2).
/// Matched by longest model-name prefix; unknown models use config-file prices.
/// Cache-hit pricing matters: DeepSeek hits are ~50x cheaper than misses.
struct Price {
    prefix: &'static str,
    input: f64,
    output: f64,
    cached: f64,
    ds_peak: bool,
}

const fn p(prefix: &'static str, i: f64, o: f64, c: f64, ds_peak: bool) -> Price {
    Price {
        prefix,
        input: i,
        output: o,
        cached: c,
        ds_peak,
    }
}

const PRICES: &[Price] = &[
    p("gpt-4o-mini", 0.15, 0.60, 0.075, false),
    p("gpt-4o", 2.50, 10.00, 1.25, false),
    p("gpt-4.1-nano", 0.10, 0.40, 0.025, false),
    p("gpt-4.1-mini", 0.40, 1.60, 0.10, false),
    p("gpt-4.1", 2.00, 8.00, 0.50, false),
    p("gpt-5-nano", 0.05, 0.40, 0.005, false),
    p("gpt-5-mini", 0.25, 2.00, 0.025, false),
    p("gpt-5", 1.25, 10.00, 0.125, false),
    p("o4-mini", 1.10, 4.40, 0.275, false),
    p("o3", 2.00, 8.00, 0.50, false),
    // DeepSeek: off-peak rates; peak windows (Mon–Fri 01–04, 06–10 UTC) double them.
    p("deepseek-flash", 0.15, 0.60, 0.003, true),
    p("deepseek-v4-pro", 0.66, 1.98, 0.022, true),
    p("deepseek-reasoner", 0.28, 0.42, 0.028, true),
    p("deepseek-chat", 0.28, 0.42, 0.028, true),
    p("claude-haiku", 0.80, 4.00, 0.08, false),
    p("claude-sonnet", 3.00, 15.00, 0.30, false),
    p("claude-opus", 15.00, 75.00, 1.50, false),
    p("qwen", 0.20, 0.80, 0.05, false),
];

/// DeepSeek peak billing window: weekdays 01:00–04:00 and 06:00–10:00 UTC.
fn deepseek_peak_now() -> bool {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let dow = (secs / 86400 + 3) % 7; // 1970-01-01 was a Thursday; 0=Mon … 6=Sun
    let hour = (secs % 86400) / 3600;
    dow < 5 && ((1..4).contains(&hour) || (6..10).contains(&hour))
}

#[derive(Clone, Debug)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Compact history when the estimate exceeds this many tokens.
    pub context_budget: usize,
    pub max_output_tokens: u32,
    pub bash_timeout_ms: u64,
    pub price_in: f64,
    pub price_out: f64,
    pub price_cached: f64,
    pub ds_peak: bool,
    pub use_map: bool,
}

impl Config {
    pub fn load(
        model: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
        budget: Option<usize>,
        no_map: bool,
    ) -> Result<Self> {
        let f: FileCfg = Self::config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();

        let get = |cli: Option<&str>, env: &str, file: Option<String>, def: &str| -> String {
            cli.map(|s| s.to_string())
                .or_else(|| std::env::var(env).ok().filter(|v| !v.is_empty()))
                .or(file)
                .unwrap_or_else(|| def.to_string())
        };

        let api_key = get(api_key, "RAGENT_API_KEY", f.api_key, "");
        let api_key = if api_key.is_empty() {
            std::env::var("OPENAI_API_KEY").unwrap_or_default()
        } else {
            api_key
        };
        if api_key.is_empty() {
            bail!("no API key: set RAGENT_API_KEY / OPENAI_API_KEY, --api-key, or api_key in config.toml");
        }
        let base_url = get(base_url, "RAGENT_BASE_URL", f.base_url, "");
        let base_url = if base_url.is_empty() {
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into())
        } else {
            base_url
        };
        let model = get(model, "RAGENT_MODEL", f.model, "");
        let model = if model.is_empty() {
            std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into())
        } else {
            model
        };

        let mut ds_peak = false;
        let (pi, po, pc) = match Self::lookup_price(&model) {
            Some(pr) => {
                ds_peak = pr.ds_peak;
                (pr.input, pr.output, pr.cached)
            }
            None => {
                let (i, o) = f.price_input.zip(f.price_output).unwrap_or((0.0, 0.0));
                (i, o, f.price_cached.unwrap_or(0.0))
            }
        };

        Ok(Config {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            // floor: a degenerate budget would compact the whole history away
            context_budget: budget.or(f.context_budget).unwrap_or(60_000).max(4_000),
            max_output_tokens: f.max_output_tokens.unwrap_or(8_192),
            bash_timeout_ms: f.bash_timeout_ms.unwrap_or(30_000),
            price_in: pi,
            price_out: po,
            price_cached: pc,
            ds_peak,
            use_map: if no_map {
                false
            } else {
                f.use_map.unwrap_or(true)
            },
        })
    }

    fn lookup_price(model: &str) -> Option<&'static Price> {
        PRICES
            .iter()
            .filter(|p| model.starts_with(p.prefix))
            .max_by_key(|p| p.prefix.len())
    }

    /// Cost of one call. `cached` is the subset of `input` served from cache.
    pub fn cost(&self, input: u64, output: u64, cached: u64) -> f64 {
        let mult = if self.ds_peak && deepseek_peak_now() {
            2.0
        } else {
            1.0
        };
        let cached = cached.min(input) as f64;
        let miss = input as f64 - cached;
        (miss * self.price_in + cached * self.price_cached + output as f64 * self.price_out) * mult
            / 1e6
    }

    pub fn config_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("ragent").join("config.toml"))
    }

    pub fn write_example() -> Result<()> {
        let p = Self::config_path().context("no config dir")?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(
            &p,
            "# ragent config — all fields optional, env vars take precedence over this file,\n\
             # CLI flags take precedence over env vars.\n\
             # api_key = \"sk-...\"\n\
             # base_url = \"https://api.deepseek.com/v1\"\n\
             # model = \"deepseek-chat\"\n\
             # context_budget = 60000\n\
             # max_output_tokens = 8192\n\
             # bash_timeout_ms = 30000\n\
             # price_input = 0.15\n\
             # price_output = 0.60\n\
             # price_cached = 0.003\n",
        )?;
        println!("wrote {}", p.display());
        Ok(())
    }
}
