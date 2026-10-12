//! Models the gateway can run and the prices usage is charged at.
//!
//! Prices are micro-USD per million tokens, so one token of a model listed at
//! `$4 / MTok` costs exactly 4 micro-USD. Costs round up: the ledger never
//! undercharges a budget.

use crate::contracts::ai::AiModel;

/// Standard prices of one prompt-size tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prices {
    pub input: i64,
    /// Five-minute cache writes; the gateway only writes ephemeral caches.
    pub cache_write: i64,
    pub cache_read: i64,
    pub output: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// Owners can pick this model; other entries only price fallback attempts.
    pub selectable: bool,
    /// Prompts above this many tokens are charged at `long_prompt` prices.
    long_prompt: Option<(u64, Prices)>,
    prices: Prices,
    /// Accepts `fallbacks: "default"`; refused requests then rerun on one of
    /// these models.
    pub fallback_models: &'static [&'static str],
    /// Returns progress updates between tool calls as thinking blocks with
    /// `thinking.display: "updates"`.
    pub progress_updates: bool,
}

const fn usd(dollars: i64, cents_hundredths: i64) -> i64 {
    dollars * 1_000_000 + cents_hundredths * 10_000
}

const OPUS_5_PRICES: Prices = Prices {
    input: usd(5, 0),
    cache_write: usd(6, 25),
    cache_read: usd(0, 50),
    output: usd(25, 0),
};

/// Charged for any model the catalog does not know (the most expensive tier).
const UNKNOWN_MODEL_PRICES: Prices = Prices {
    input: usd(10, 0),
    cache_write: usd(12, 50),
    cache_read: usd(1, 0),
    output: usd(50, 0),
};

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-opus-5-5",
        name: "Claude Opus 5.5",
        selectable: true,
        long_prompt: None,
        prices: Prices {
            input: usd(4, 0),
            cache_write: usd(5, 0),
            cache_read: usd(0, 20),
            output: usd(20, 0),
        },
        fallback_models: &["claude-opus-5", "claude-opus-4-8"],
        progress_updates: true,
    },
    ModelSpec {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5",
        selectable: true,
        long_prompt: None,
        prices: Prices {
            input: usd(2, 0),
            cache_write: usd(2, 50),
            cache_read: usd(0, 10),
            output: usd(10, 0),
        },
        fallback_models: &["claude-sonnet-5"],
        progress_updates: true,
    },
    ModelSpec {
        id: "claude-haiku-5-5",
        name: "Claude Haiku 5.5",
        selectable: true,
        long_prompt: Some((
            100_000,
            Prices {
                input: usd(0, 50),
                cache_write: 625_000,
                cache_read: usd(0, 5),
                output: usd(2, 50),
            },
        )),
        prices: Prices {
            input: usd(0, 10),
            cache_write: 125_000,
            cache_read: usd(0, 1),
            output: usd(0, 50),
        },
        fallback_models: &[],
        progress_updates: false,
    },
    ModelSpec {
        id: "claude-opus-5",
        name: "Claude Opus 5",
        selectable: false,
        long_prompt: None,
        prices: OPUS_5_PRICES,
        fallback_models: &[],
        progress_updates: false,
    },
    ModelSpec {
        id: "claude-opus-4-8",
        name: "Claude Opus 4.8",
        selectable: false,
        long_prompt: None,
        prices: OPUS_5_PRICES,
        fallback_models: &[],
        progress_updates: false,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        name: "Claude Sonnet 5",
        selectable: false,
        long_prompt: None,
        prices: Prices {
            input: usd(2, 0),
            cache_write: usd(2, 50),
            cache_read: usd(0, 20),
            output: usd(10, 0),
        },
        fallback_models: &[],
        progress_updates: false,
    },
];

/// Token counts of one model attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

impl Tokens {
    fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_creation)
            .saturating_add(self.cache_read)
    }
}

/// Look a model up by its id or a dated snapshot of it
/// (`claude-haiku-4-5-20251001`).
pub fn lookup(model: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|spec| {
        model == spec.id
            || model
                .strip_prefix(spec.id)
                .and_then(|suffix| suffix.strip_prefix('-'))
                .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
    })
}

pub fn selectable(model: &str) -> Option<&'static ModelSpec> {
    MODELS
        .iter()
        .find(|spec| spec.selectable && spec.id == model)
}

pub fn selectable_models() -> Vec<AiModel> {
    MODELS
        .iter()
        .filter(|spec| spec.selectable)
        .map(|spec| AiModel {
            id: spec.id.to_owned(),
            name: spec.name.to_owned(),
            input_micros_per_mtok: spec.prices.input,
            output_micros_per_mtok: spec.prices.output,
        })
        .collect()
}

fn prices_for(model: &str, prompt_tokens: u64) -> Prices {
    let Some(spec) = lookup(model) else {
        return UNKNOWN_MODEL_PRICES;
    };
    match spec.long_prompt {
        Some((threshold, prices)) if prompt_tokens > threshold => prices,
        _ => spec.prices,
    }
}

fn charge(tokens: u64, micros_per_mtok: i64) -> i128 {
    i128::from(tokens) * i128::from(micros_per_mtok)
}

fn micros(scaled: i128) -> i64 {
    // Round up to whole micro-USD and saturate instead of overflowing.
    i64::try_from((scaled + 999_999) / 1_000_000).unwrap_or(i64::MAX)
}

/// Exact cost of one recorded attempt in micro-USD.
pub fn cost_micros(model: &str, tokens: Tokens) -> i64 {
    let prices = prices_for(model, tokens.prompt());
    micros(
        charge(tokens.input, prices.input)
            + charge(tokens.cache_creation, prices.cache_write)
            + charge(tokens.cache_read, prices.cache_read)
            + charge(tokens.output, prices.output),
    )
}

/// Highest prices any attempt of a request to `model` can be charged at,
/// including server-side fallback attempts.
fn worst_prices(spec: &ModelSpec, prompt_tokens: u64, fallbacks: bool) -> (i64, i64) {
    let mut input = prices_for(spec.id, prompt_tokens).cache_write;
    let mut output = prices_for(spec.id, prompt_tokens).output;
    if fallbacks {
        // A refused attempt can be charged for its prompt before the fallback
        // model reads the prompt again and writes the answer.
        let fallback = spec
            .fallback_models
            .iter()
            .map(|model| prices_for(model, prompt_tokens))
            .fold((0, 0), |(input, output), prices| {
                (input.max(prices.cache_write), output.max(prices.output))
            });
        input += fallback.0;
        output = output.max(fallback.1);
    }
    (input, output)
}

/// Worst-case cost of one request: every prompt token written to the cache
/// and the full output allowance spent.
pub fn worst_case_micros(
    spec: &ModelSpec,
    prompt_tokens: u64,
    max_tokens: u32,
    fallbacks: bool,
) -> i64 {
    let (input, output) = worst_prices(spec, prompt_tokens, fallbacks);
    micros(charge(prompt_tokens, input) + charge(u64::from(max_tokens), output))
}

/// Largest output allowance whose worst case still fits in `available`
/// micro-USD, capped at `ceiling`.
pub fn affordable_max_tokens(
    spec: &ModelSpec,
    prompt_tokens: u64,
    available: i64,
    ceiling: u32,
    fallbacks: bool,
) -> u32 {
    let (input, output) = worst_prices(spec, prompt_tokens, fallbacks);
    let remaining = i128::from(available) * 1_000_000 - charge(prompt_tokens, input);
    if remaining <= 0 || output <= 0 {
        return 0;
    }
    u32::try_from(remaining / i128::from(output))
        .unwrap_or(u32::MAX)
        .min(ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_match_the_published_rates() {
        let opus = lookup("claude-opus-5-5").unwrap();
        assert_eq!(opus.prices.input, 4_000_000);
        assert_eq!(opus.prices.cache_read, 200_000);
        let sonnet = lookup("claude-sonnet-5-5").unwrap();
        assert_eq!(sonnet.prices.cache_write, 2_500_000);
        assert_eq!(sonnet.prices.cache_read, 100_000);
        let haiku = lookup("claude-haiku-5-5").unwrap();
        assert_eq!(haiku.prices.cache_write, 125_000);
        assert_eq!(haiku.long_prompt.unwrap().1.cache_write, 625_000);
    }

    #[test]
    fn costs_are_exact_and_round_up() {
        let tokens = Tokens {
            input: 1_000,
            output: 500,
            cache_creation: 2_000,
            cache_read: 10_000,
        };
        // 1000*4 + 2000*5 + 10000*0.2 + 500*20 micro-USD.
        assert_eq!(cost_micros("claude-opus-5-5", tokens), 26_000);
        let one_token = Tokens {
            cache_read: 1,
            ..Tokens::default()
        };
        assert_eq!(cost_micros("claude-haiku-5-5", one_token), 1);
        assert_eq!(cost_micros("claude-opus-5-5", Tokens::default()), 0);
    }

    #[test]
    fn haiku_prompts_over_the_threshold_use_long_prompt_prices() {
        let short = Tokens {
            input: 100_000,
            ..Tokens::default()
        };
        let long = Tokens {
            input: 100_001,
            ..Tokens::default()
        };
        assert_eq!(cost_micros("claude-haiku-5-5", short), 10_000);
        assert_eq!(cost_micros("claude-haiku-5-5", long), 50_001);
    }

    #[test]
    fn unknown_and_dated_models_are_priced_conservatively() {
        assert_eq!(
            lookup("claude-opus-5-5-20261001").unwrap().id,
            "claude-opus-5-5"
        );
        assert_eq!(lookup("claude-opus-5").unwrap().id, "claude-opus-5");
        assert!(lookup("claude-opus-5-5-extra").is_none());
        let tokens = Tokens {
            output: 1_000,
            ..Tokens::default()
        };
        assert_eq!(cost_micros("claude-unknown-9", tokens), 50_000);
    }

    #[test]
    fn worst_case_covers_fallback_attempts_and_bounds_the_output_allowance() {
        let opus = lookup("claude-opus-5-5").unwrap();
        // Prompt written at $5 plus a fallback prompt at $6.25; output at $25.
        assert_eq!(worst_case_micros(opus, 1_000, 1_000, true), 36_250);
        assert_eq!(worst_case_micros(opus, 1_000, 1_000, false), 25_000);
        let allowance = affordable_max_tokens(opus, 1_000, 36_250, 8_192, true);
        assert_eq!(allowance, 1_000);
        assert_eq!(affordable_max_tokens(opus, 1_000, 1_000, 8_192, true), 0);
        assert_eq!(affordable_max_tokens(opus, 0, i64::MAX, 8_192, true), 8_192);
    }

    #[test]
    fn only_current_models_are_selectable() {
        let ids = selectable_models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"]
        );
        assert!(selectable("claude-opus-5").is_none());
    }
}
