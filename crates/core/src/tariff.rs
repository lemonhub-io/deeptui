//! What tokens cost. A tariff is a price *schedule* — DeepSeek doubles its
//! rates in weekday peak windows — so pricing takes the moment as an argument
//! instead of reading a clock: the core stays pure, the tests stay exact.

use crate::event::Usage;

/// $ per 1M tokens for one model.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tariff {
    /// Input that missed the prompt cache.
    pub input: f64,
    pub output: f64,
    /// Input served from the cache — DeepSeek hits are ~50x cheaper than misses.
    pub cached: f64,
    /// Rates double in DeepSeek's peak windows (see [`deepseek_peak_at`]).
    pub deepseek_peak: bool,
}

struct Entry {
    prefix: &'static str,
    tariff: Tariff,
}

const fn p(prefix: &'static str, input: f64, output: f64, cached: f64, peak: bool) -> Entry {
    Entry {
        prefix,
        tariff: Tariff {
            input,
            output,
            cached,
            deepseek_peak: peak,
        },
    }
}

/// Matched by longest model-name prefix; unknown models use config-file prices.
const TABLE: &[Entry] = &[
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

fn lookup(model: &str) -> Option<&'static Entry> {
    TABLE
        .iter()
        .filter(|e| model.starts_with(e.prefix))
        .max_by_key(|e| e.prefix.len())
}

/// DeepSeek peak billing window: weekdays 01:00–04:00 and 06:00–10:00 UTC.
pub fn deepseek_peak_at(secs: u64) -> bool {
    let dow = (secs / 86400 + 3) % 7; // 1970-01-01 was a Thursday; 0=Mon … 6=Sun
    let hour = (secs % 86400) / 3600;
    dow < 5 && ((1..4).contains(&hour) || (6..10).contains(&hour))
}

impl Tariff {
    /// The built-in tariff for `model`, if the table knows it.
    pub fn for_model(model: &str) -> Option<Tariff> {
        lookup(model).map(|e| e.tariff)
    }

    /// False when nothing is known about the price: the meter would read $0.
    pub fn is_known(&self) -> bool {
        self.input != 0.0
    }

    /// Dollars for `u`, billed at unix time `at`.
    pub fn cost(&self, u: &Usage, at: u64) -> f64 {
        let mult = if self.deepseek_peak && deepseek_peak_at(at) {
            2.0
        } else {
            1.0
        };
        let cached = u.cached.min(u.input) as f64;
        let miss = u.input as f64 - cached;
        (miss * self.input + cached * self.cached + u.output as f64 * self.output) * mult / 1e6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(model: &str) -> &'static Entry {
        lookup(model).unwrap_or_else(|| panic!("no price for {model}"))
    }

    fn usage(input: u64, output: u64, cached: u64) -> Usage {
        Usage {
            input,
            output,
            cached,
            cost: 0.0,
        }
    }

    const MINI: Tariff = Tariff {
        input: 0.15,
        output: 0.60,
        cached: 0.075,
        deepseek_peak: false,
    };

    #[test]
    fn price_lookup_matches_the_longest_prefix() {
        // dated snapshots and suffixes resolve to the base model
        assert_eq!(entry("gpt-4o-mini-2024-07-18").prefix, "gpt-4o-mini");
        assert_eq!(entry("gpt-4o-mini").tariff.input, 0.15);
        // ...and never to a shorter prefix that also matches
        assert_eq!(entry("gpt-4o-2024-08-06").prefix, "gpt-4o");
        assert_eq!(entry("gpt-5-mini").prefix, "gpt-5-mini");
        assert_eq!(entry("qwen2.5-coder-32b").prefix, "qwen");
        assert!(entry("deepseek-reasoner").tariff.deepseek_peak);
        assert!(!entry("claude-sonnet-4").tariff.deepseek_peak);
        // unknown models fall through to config-file prices instead
        assert!(Tariff::for_model("mystery-model").is_none());
        assert!(Tariff::for_model("").is_none());
    }

    #[test]
    fn cost_bills_cache_hits_separately_and_clamps_them() {
        let c = MINI;
        // 1M fresh input + 1M output at 0.15/0.60 per 1M = 0.75
        assert!((c.cost(&usage(1_000_000, 1_000_000, 0), 0) - 0.75).abs() < 1e-12);
        // cached input is billed at the cached rate
        assert!((c.cost(&usage(1_000_000, 0, 1_000_000), 0) - 0.075).abs() < 1e-12);
        // 600k misses + 400k cache hits
        let mixed = c.cost(&usage(1_000_000, 0, 400_000), 0);
        assert!((mixed - 0.6 * 0.15 - 0.4 * 0.075).abs() < 1e-12);
        // a cached count larger than the input cannot make the bill negative
        assert!((c.cost(&usage(1_000_000, 0, 9_000_000), 0) - 0.075).abs() < 1e-12);
        assert_eq!(c.cost(&usage(0, 0, 0), 0), 0.0);
    }

    #[test]
    fn peak_windows_double_deepseek_bills_only() {
        let monday_7am = 4 * 86400 + 7 * 3600;
        let monday_noon = 4 * 86400 + 12 * 3600;
        let u = usage(1_000_000, 1_000_000, 0);
        let ds = Tariff::for_model("deepseek-chat").unwrap();
        assert!((ds.cost(&u, monday_7am) - 2.0 * ds.cost(&u, monday_noon)).abs() < 1e-12);
        // a tariff without the flag ignores the clock entirely
        assert_eq!(MINI.cost(&u, monday_7am), MINI.cost(&u, monday_noon));
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
    fn unknown_prices_are_flagged() {
        assert!(MINI.is_known());
        assert!(!Tariff::default().is_known());
    }
}
