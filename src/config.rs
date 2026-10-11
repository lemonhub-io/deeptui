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
fn deepseek_peak_at(secs: u64) -> bool {
    let dow = (secs / 86400 + 3) % 7; // 1970-01-01 was a Thursday; 0=Mon … 6=Sun
    let hour = (secs % 86400) / 3600;
    dow < 5 && ((1..4).contains(&hour) || (6..10).contains(&hour))
}

fn deepseek_peak_now() -> bool {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    deepseek_peak_at(secs)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn price(model: &str) -> &'static Price {
        Config::lookup_price(model).unwrap_or_else(|| panic!("no price for {model}"))
    }

    fn cfg() -> Config {
        Config {
            api_key: "k".into(),
            base_url: "http://x/v1".into(),
            model: "gpt-4o-mini".into(),
            context_budget: 60_000,
            max_output_tokens: 8_192,
            bash_timeout_ms: 30_000,
            price_in: 0.15,
            price_out: 0.60,
            price_cached: 0.075,
            ds_peak: false,
            use_map: true,
        }
    }

    #[test]
    fn price_lookup_matches_the_longest_prefix() {
        // dated snapshots and suffixes resolve to the base model
        assert_eq!(price("gpt-4o-mini-2024-07-18").prefix, "gpt-4o-mini");
        assert_eq!(price("gpt-4o-mini").input, 0.15);
        // ...and never to a shorter prefix that also matches
        assert_eq!(price("gpt-4o-2024-08-06").prefix, "gpt-4o");
        assert_eq!(price("gpt-5-mini").prefix, "gpt-5-mini");
        assert_eq!(price("qwen2.5-coder-32b").prefix, "qwen");
        assert!(price("deepseek-reasoner").ds_peak);
        assert!(!price("claude-sonnet-4").ds_peak);
        // unknown models fall through to config-file prices instead
        assert!(Config::lookup_price("mystery-model").is_none());
        assert!(Config::lookup_price("").is_none());
    }

    #[test]
    fn cost_bills_cache_hits_separately_and_clamps_them() {
        let c = cfg();
        // 1M fresh input + 1M output at 0.15/0.60 per 1M = 0.75
        assert!((c.cost(1_000_000, 1_000_000, 0) - 0.75).abs() < 1e-12);
        // cached input is billed at the cached rate
        assert!((c.cost(1_000_000, 0, 1_000_000) - 0.075).abs() < 1e-12);
        // 600k misses + 400k cache hits
        assert!((c.cost(1_000_000, 0, 400_000) - 0.6 * 0.15 - 0.4 * 0.075).abs() < 1e-12);
        // a cached count larger than the input cannot make the bill negative
        assert!((c.cost(1_000_000, 0, 9_000_000) - 0.075).abs() < 1e-12);
        assert_eq!(c.cost(0, 0, 0), 0.0);
    }

    #[test]
    fn peak_multiplier_only_ever_doubles() {
        let mut c = cfg();
        c.ds_peak = true;
        let base = c.cost(1_000_000, 1_000_000, 0);
        c.ds_peak = false;
        let off = c.cost(1_000_000, 1_000_000, 0);
        // if the machine happens to run inside a peak window the bill doubles
        let mult = base / off;
        assert!(
            (mult - 1.0).abs() < 1e-12 || (mult - 2.0).abs() < 1e-12,
            "unexpected multiplier {mult}"
        );
    }

    #[test]
    fn peak_windows_are_weekday_hours_in_utc() {
        // 1970-01-01 was a Thursday (dow 3)
        assert!(!deepseek_peak_at(0));
        assert!(deepseek_peak_at(3600)); // 01:00
        assert!(!deepseek_peak_at(4 * 3600)); // 04:00 is off-peak again
        assert!(deepseek_peak_at(6 * 3600));
        assert!(deepseek_peak_at(9 * 3600));
        assert!(!deepseek_peak_at(10 * 3600));
        assert!(!deepseek_peak_at(23 * 3600));
        // Saturday (day 2) and Sunday (day 3) never peak
        for day in [2u64, 3] {
            for hour in [1u64, 7] {
                assert!(!deepseek_peak_at(day * 86400 + hour * 3600));
            }
        }
        // Monday (day 4) 06:00 is peak again
        assert!(deepseek_peak_at(4 * 86400 + 6 * 3600));
    }

    #[test]
    fn load_prefers_cli_flags_and_keeps_the_budget_floor() {
        let c = Config::load(
            Some("gpt-4o-mini"),
            Some("https://api.x/v1/"),
            Some("sk-cli"),
            Some(10),
            true,
        )
        .unwrap();
        assert_eq!(c.api_key, "sk-cli");
        assert_eq!(c.model, "gpt-4o-mini");
        assert_eq!(c.base_url, "https://api.x/v1"); // trailing slash trimmed
        assert_eq!(c.context_budget, 4_000); // degenerate budget is floored
        assert!(!c.use_map); // --no-map always wins
        assert_eq!(
            (c.price_in, c.price_out, c.price_cached),
            (0.15, 0.60, 0.075)
        );
        assert!(!c.ds_peak);

        let c = Config::load(
            Some("gpt-4o-mini"),
            Some("https://api.x/v1"),
            Some("k"),
            Some(123_456),
            false,
        )
        .unwrap();
        assert_eq!(c.context_budget, 123_456);

        // deepseek models carry the peak-billing flag and their own rates
        let c = Config::load(Some("deepseek-chat"), None, Some("k"), None, false).unwrap();
        assert!(c.ds_peak);
        assert_eq!(c.price_in, 0.28);
        assert_eq!(c.price_cached, 0.028);
    }
}
