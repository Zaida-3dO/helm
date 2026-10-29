//! What local tokens would have cost on Claude.
//!
//! A chat on a local model costs nothing, and its tokens stay out of the plan's figures. Beside
//! that, the cost report and `app_usage_summary` show what the same tokens would have cost at
//! Claude's API prices, so the saving is visible. These prices are an estimate for comparison, not
//! a bill: they are kept in this one small table, with the date they were read, and they are
//! worth checking again now and then.

/// US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    /// The setting's key: `opus`, `sonnet` or `haiku`.
    pub key: &'static str,
    pub name: &'static str,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Writing to the prompt cache (the five-minute kind), a quarter more than plain input.
    pub cache_write: f64,
}

/// When the table below was read from Anthropic's published API prices.
pub const PRICES_READ: &str = "2026-10-29";

pub const PRICES: [Price; 3] = [
    Price {
        key: "opus",
        name: "Opus",
        input: 4.00,
        output: 20.00,
        cache_read: 0.20,
        cache_write: 5.00,
    },
    Price {
        key: "sonnet",
        name: "Sonnet",
        input: 2.00,
        output: 10.00,
        cache_read: 0.20,
        cache_write: 2.50,
    },
    Price {
        key: "haiku",
        name: "Haiku",
        input: 1.00,
        output: 5.00,
        cache_read: 0.10,
        cache_write: 1.25,
    },
];

/// The price for a setting's key; Sonnet, the default, for anything else.
pub fn price(key: &str) -> Price {
    PRICES
        .iter()
        .find(|p| p.key == key)
        .copied()
        .unwrap_or(PRICES[1])
}

/// What these token counts cost at `p`, in US dollars.
pub fn would_cost(p: &Price, input: i64, output: i64, cache_read: i64, cache_write: i64) -> f64 {
    let m = |tokens: i64, per_million: f64| tokens.max(0) as f64 * per_million / 1_000_000.0;
    m(input, p.input)
        + m(output, p.output)
        + m(cache_read, p.cache_read)
        + m(cache_write, p.cache_write)
}

/// The comparison model's key, chosen in Settings. Registered by the crate that owns that
/// setting; with none registered the comparison is Sonnet.
#[derive(Clone)]
pub struct CompareChoice(pub std::sync::Arc<dyn Fn() -> String + Send + Sync>);

impl CompareChoice {
    pub fn key(&self) -> String {
        (self.0)()
    }
}

/// The comparison model's key from the app's services.
pub fn compare_key(services: &helm_ipc::Services) -> String {
    services
        .get::<CompareChoice>()
        .map(|c| c.key())
        .unwrap_or_else(|| "sonnet".to_owned())
}
