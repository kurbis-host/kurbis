//! Models the gateway offers and the prices usage is charged at.
//!
//! Prices are micro-USD per million tokens, so one token of a model listed at
//! `$4 / MTok` costs exactly 4 micro-USD. Costs round up: the ledger never
//! undercharges a budget. Offered models take the prices the deployment
//! configures, else the published prices of the Claude models listed here; a
//! model with neither is not charged.

use crate::{
    config::{AiModelEntry, AiProvider},
    contracts::ai::AiModel,
};

/// Standard prices of one prompt-size tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prices {
    pub input: i64,
    /// Five-minute cache writes; the gateway only writes ephemeral caches.
    pub cache_write: i64,
    pub cache_read: i64,
    pub output: i64,
}

impl Prices {
    /// Prices known only as input and output rates: cached prompt tokens are
    /// charged as input.
    const fn flat(input: i64, output: i64) -> Self {
        Self {
            input,
            cache_write: input,
            cache_read: input,
            output,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pricing {
    prices: Prices,
    /// Prompts above this many tokens are charged at these prices.
    long_prompt: Option<(u64, Prices)>,
}

impl Pricing {
    const fn standard(prices: Prices) -> Self {
        Self {
            prices,
            long_prompt: None,
        }
    }

    fn for_prompt(&self, prompt_tokens: u64) -> Prices {
        match self.long_prompt {
            Some((threshold, prices)) if prompt_tokens > threshold => prices,
            _ => self.prices,
        }
    }
}

/// A Claude model whose prices and Messages API features the gateway knows.
struct Claude {
    id: &'static str,
    name: &'static str,
    pricing: Pricing,
    /// Accepts `fallbacks: "default"`; refused requests then rerun on one of
    /// these models.
    fallback_models: &'static [&'static str],
    /// Returns progress updates between tool calls as thinking blocks with
    /// `thinking.display: "updates"`.
    progress_updates: bool,
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

/// Charged for an attempt on a model that is neither offered nor listed here
/// (the most expensive tier).
const UNKNOWN_MODEL: Pricing = Pricing::standard(Prices {
    input: usd(10, 0),
    cache_write: usd(12, 50),
    cache_read: usd(1, 0),
    output: usd(50, 0),
});

const CLAUDE: &[Claude] = &[
    Claude {
        id: "claude-opus-5-5",
        name: "Claude Opus 5.5",
        pricing: Pricing::standard(Prices {
            input: usd(4, 0),
            cache_write: usd(5, 0),
            cache_read: usd(0, 20),
            output: usd(20, 0),
        }),
        fallback_models: &["claude-opus-5", "claude-opus-4-8"],
        progress_updates: true,
    },
    Claude {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5",
        pricing: Pricing::standard(Prices {
            input: usd(2, 0),
            cache_write: usd(2, 50),
            cache_read: usd(0, 10),
            output: usd(10, 0),
        }),
        fallback_models: &["claude-sonnet-5"],
        progress_updates: true,
    },
    Claude {
        id: "claude-haiku-5-5",
        name: "Claude Haiku 5.5",
        pricing: Pricing {
            prices: Prices {
                input: usd(0, 10),
                cache_write: 125_000,
                cache_read: usd(0, 1),
                output: usd(0, 50),
            },
            long_prompt: Some((
                100_000,
                Prices {
                    input: usd(0, 50),
                    cache_write: 625_000,
                    cache_read: usd(0, 5),
                    output: usd(2, 50),
                },
            )),
        },
        fallback_models: &[],
        progress_updates: false,
    },
    Claude {
        id: "claude-opus-5",
        name: "Claude Opus 5",
        pricing: Pricing::standard(OPUS_5_PRICES),
        fallback_models: &[],
        progress_updates: false,
    },
    Claude {
        id: "claude-opus-4-8",
        name: "Claude Opus 4.8",
        pricing: Pricing::standard(OPUS_5_PRICES),
        fallback_models: &[],
        progress_updates: false,
    },
    Claude {
        id: "claude-sonnet-5",
        name: "Claude Sonnet 5",
        pricing: Pricing::standard(Prices {
            input: usd(2, 0),
            cache_write: usd(2, 50),
            cache_read: usd(0, 20),
            output: usd(10, 0),
        }),
        fallback_models: &[],
        progress_updates: false,
    },
];

/// Whether `model` names `id` or a dated snapshot of it
/// (`claude-haiku-4-5-20251001`).
fn names(model: &str, id: &str) -> bool {
    model == id
        || model
            .strip_prefix(id)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
}

fn claude(model: &str) -> Option<&'static Claude> {
    CLAUDE.iter().find(|claude| names(model, claude.id))
}

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

/// A model server owners can choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub name: String,
    pricing: Pricing,
    /// Models the Messages API may rerun a refused request on.
    pub fallback_models: &'static [&'static str],
    /// Whether the Messages API reports progress as thinking blocks.
    pub progress_updates: bool,
}

/// The models a deployment offers; the first is the default.
#[derive(Debug, Clone)]
pub struct Catalog {
    models: Vec<Model>,
}

impl Catalog {
    /// `entries` must not be empty; the configuration guarantees it.
    pub fn new(provider: AiProvider, entries: &[AiModelEntry]) -> Self {
        // Fallbacks and progress updates are Messages API features.
        let messages_api = provider == AiProvider::Anthropic;
        let models = entries
            .iter()
            .map(|entry| {
                let claude = claude(&entry.id);
                Model {
                    id: entry.id.clone(),
                    name: claude.map_or_else(|| entry.id.clone(), |claude| claude.name.to_owned()),
                    pricing: match (entry.prices, claude) {
                        (Some((input, output)), _) => {
                            Pricing::standard(Prices::flat(input, output))
                        }
                        (None, Some(claude)) => claude.pricing,
                        (None, None) => Pricing::standard(Prices::flat(0, 0)),
                    },
                    fallback_models: claude
                        .filter(|_| messages_api)
                        .map(|claude| claude.fallback_models)
                        .unwrap_or_default(),
                    progress_updates: messages_api
                        && claude.is_some_and(|claude| claude.progress_updates),
                }
            })
            .collect::<Vec<_>>();
        assert!(!models.is_empty(), "a catalog offers at least one model");
        Self { models }
    }

    pub fn default_model(&self) -> &Model {
        &self.models[0]
    }

    pub fn offered(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|model| model.id == id)
    }

    /// The offered models as the panel and the plugin list them.
    pub fn models(&self) -> Vec<AiModel> {
        self.models
            .iter()
            .map(|model| AiModel {
                id: model.id.clone(),
                name: model.name.clone(),
                input_micros_per_mtok: model.pricing.prices.input,
                output_micros_per_mtok: model.pricing.prices.output,
            })
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Model> {
        self.models.iter()
    }

    fn pricing(&self, model: &str) -> Pricing {
        self.models
            .iter()
            .find(|offered| names(model, &offered.id))
            .map(|offered| offered.pricing)
            .or_else(|| claude(model).map(|claude| claude.pricing))
            .unwrap_or(UNKNOWN_MODEL)
    }

    /// Exact cost of one recorded attempt in micro-USD.
    pub fn cost_micros(&self, model: &str, tokens: Tokens) -> i64 {
        let prices = self.pricing(model).for_prompt(tokens.prompt());
        micros(
            charge(tokens.input, prices.input)
                + charge(tokens.cache_creation, prices.cache_write)
                + charge(tokens.cache_read, prices.cache_read)
                + charge(tokens.output, prices.output),
        )
    }

    /// Highest input and output prices any attempt of a request to `model`
    /// can be charged at, including server-side fallback attempts.
    fn worst_prices(&self, model: &Model, prompt_tokens: u64) -> (i64, i64) {
        let prices = model.pricing.for_prompt(prompt_tokens);
        // A refused attempt can be charged for its prompt before the fallback
        // model reads the prompt again and writes the answer.
        let (fallback_input, fallback_output) = model
            .fallback_models
            .iter()
            .map(|fallback| self.pricing(fallback).for_prompt(prompt_tokens))
            .fold((0, 0), |(input, output), fallback| {
                (input.max(fallback.cache_write), output.max(fallback.output))
            });
        (
            prices.cache_write + fallback_input,
            prices.output.max(fallback_output),
        )
    }

    /// Worst-case cost of one request: every prompt token written to the cache
    /// and the full output allowance spent.
    pub fn worst_case_micros(&self, model: &Model, prompt_tokens: u64, max_tokens: u32) -> i64 {
        let (input, output) = self.worst_prices(model, prompt_tokens);
        micros(charge(prompt_tokens, input) + charge(u64::from(max_tokens), output))
    }

    /// Largest output allowance whose worst case still fits in `available`
    /// micro-USD, capped at `ceiling`.
    pub fn affordable_max_tokens(
        &self,
        model: &Model,
        prompt_tokens: u64,
        available: i64,
        ceiling: u32,
    ) -> u32 {
        let (input, output) = self.worst_prices(model, prompt_tokens);
        let remaining = i128::from(available) * 1_000_000 - charge(prompt_tokens, input);
        if remaining < 0 {
            return 0;
        }
        if output == 0 {
            return ceiling;
        }
        u32::try_from(remaining / i128::from(output))
            .unwrap_or(u32::MAX)
            .min(ceiling)
    }
}

fn charge(tokens: u64, micros_per_mtok: i64) -> i128 {
    i128::from(tokens) * i128::from(micros_per_mtok)
}

fn micros(scaled: i128) -> i64 {
    // Round up to whole micro-USD and saturate instead of overflowing.
    i64::try_from((scaled + 999_999) / 1_000_000).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(provider: AiProvider, entries: &[(&str, Option<(i64, i64)>)]) -> Catalog {
        let entries = entries
            .iter()
            .map(|(id, prices)| AiModelEntry {
                id: (*id).to_owned(),
                prices: *prices,
            })
            .collect::<Vec<_>>();
        Catalog::new(provider, &entries)
    }

    fn claude_catalog() -> Catalog {
        catalog(
            AiProvider::Anthropic,
            &[
                ("claude-opus-5-5", None),
                ("claude-sonnet-5-5", None),
                ("claude-haiku-5-5", None),
            ],
        )
    }

    #[test]
    fn prices_match_the_published_rates() {
        let opus = claude("claude-opus-5-5").unwrap().pricing.prices;
        assert_eq!(opus.input, 4_000_000);
        assert_eq!(opus.cache_read, 200_000);
        let sonnet = claude("claude-sonnet-5-5").unwrap().pricing.prices;
        assert_eq!(sonnet.cache_write, 2_500_000);
        assert_eq!(sonnet.cache_read, 100_000);
        let haiku = claude("claude-haiku-5-5").unwrap().pricing;
        assert_eq!(haiku.prices.cache_write, 125_000);
        assert_eq!(haiku.long_prompt.unwrap().1.cache_write, 625_000);
    }

    #[test]
    fn costs_are_exact_and_round_up() {
        let catalog = claude_catalog();
        let tokens = Tokens {
            input: 1_000,
            output: 500,
            cache_creation: 2_000,
            cache_read: 10_000,
        };
        // 1000*4 + 2000*5 + 10000*0.2 + 500*20 micro-USD.
        assert_eq!(catalog.cost_micros("claude-opus-5-5", tokens), 26_000);
        let one_token = Tokens {
            cache_read: 1,
            ..Tokens::default()
        };
        assert_eq!(catalog.cost_micros("claude-haiku-5-5", one_token), 1);
        assert_eq!(catalog.cost_micros("claude-opus-5-5", Tokens::default()), 0);
    }

    #[test]
    fn haiku_prompts_over_the_threshold_use_long_prompt_prices() {
        let catalog = claude_catalog();
        let short = Tokens {
            input: 100_000,
            ..Tokens::default()
        };
        let long = Tokens {
            input: 100_001,
            ..Tokens::default()
        };
        assert_eq!(catalog.cost_micros("claude-haiku-5-5", short), 10_000);
        assert_eq!(catalog.cost_micros("claude-haiku-5-5", long), 50_001);
    }

    #[test]
    fn fallback_dated_and_unknown_models_are_priced_conservatively() {
        let catalog = claude_catalog();
        let tokens = Tokens {
            output: 1_000,
            ..Tokens::default()
        };
        assert_eq!(
            catalog.cost_micros("claude-opus-5-5-20261001", tokens),
            20_000
        );
        assert_eq!(catalog.cost_micros("claude-opus-5", tokens), 25_000);
        assert_eq!(catalog.cost_micros("claude-opus-5-5-extra", tokens), 50_000);
        assert_eq!(catalog.cost_micros("claude-unknown-9", tokens), 50_000);
    }

    #[test]
    fn worst_case_covers_fallback_attempts_and_bounds_the_output_allowance() {
        let catalog = claude_catalog();
        let opus = catalog.default_model();
        // Prompt written at $5 plus a fallback prompt at $6.25; output at $25.
        assert_eq!(catalog.worst_case_micros(opus, 1_000, 1_000), 36_250);
        assert_eq!(
            catalog.affordable_max_tokens(opus, 1_000, 36_250, 8_192),
            1_000
        );
        assert_eq!(catalog.affordable_max_tokens(opus, 1_000, 1_000, 8_192), 0);
        assert_eq!(
            catalog.affordable_max_tokens(opus, 0, i64::MAX, 8_192),
            8_192
        );
        let haiku = catalog.offered("claude-haiku-5-5").unwrap();
        assert!(haiku.fallback_models.is_empty());
        assert_eq!(catalog.worst_case_micros(haiku, 1_000, 1_000), 625);
    }

    #[test]
    fn claude_models_offered_through_another_api_keep_their_prices_only() {
        let catalog = catalog(
            AiProvider::OpenAi,
            &[
                ("claude-opus-5-5", None),
                ("gpt-oss-120b", Some((150_000, 600_000))),
                ("llama3.1:8b", None),
            ],
        );
        let opus = catalog.default_model();
        assert_eq!(opus.name, "Claude Opus 5.5");
        assert!(opus.fallback_models.is_empty());
        assert!(!opus.progress_updates);
        // Without fallbacks only the model's own cache-write and output rates count.
        assert_eq!(catalog.worst_case_micros(opus, 1_000, 1_000), 25_000);

        let gpt = catalog.offered("gpt-oss-120b").unwrap();
        assert_eq!(gpt.name, "gpt-oss-120b");
        let tokens = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_creation: 0,
            cache_read: 1_000_000,
        };
        assert_eq!(catalog.cost_micros("gpt-oss-120b", tokens), 900_000);

        let llama = catalog.offered("llama3.1:8b").unwrap();
        assert_eq!(catalog.cost_micros("llama3.1:8b", tokens), 0);
        assert_eq!(catalog.worst_case_micros(llama, 50_000, 8_192), 0);
        assert_eq!(
            catalog.affordable_max_tokens(llama, 50_000, 0, 8_192),
            8_192
        );
        assert_eq!(catalog.affordable_max_tokens(llama, 50_000, -1, 8_192), 0);

        let listed = catalog.models();
        assert_eq!(
            listed
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["claude-opus-5-5", "gpt-oss-120b", "llama3.1:8b"]
        );
        assert_eq!(listed[1].input_micros_per_mtok, 150_000);
        assert_eq!(listed[2].output_micros_per_mtok, 0);
        assert!(catalog.offered("claude-sonnet-5-5").is_none());
    }

    #[test]
    fn configured_prices_replace_published_ones() {
        let catalog = catalog(AiProvider::Anthropic, &[("claude-opus-5-5", Some((0, 0)))]);
        let opus = catalog.default_model();
        assert_eq!(opus.fallback_models, ["claude-opus-5", "claude-opus-4-8"]);
        let tokens = Tokens {
            output: 1_000,
            ..Tokens::default()
        };
        assert_eq!(catalog.cost_micros("claude-opus-5-5", tokens), 0);
        // Fallback attempts still run on models charged at their own prices.
        assert_eq!(catalog.cost_micros("claude-opus-4-8", tokens), 25_000);
    }
}
