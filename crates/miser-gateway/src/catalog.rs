//! Catalog routing: partitions the provider's model catalog into the five
//! complexity tiers by input price, pins one model per tier, and sticks to
//! it across requests.
//!
//! Stability contract:
//! - Pins change only on an explicit catalog refresh, and even then only
//!   when a candidate is at least `switch_saving_ratio` cheaper than the
//!   current pin (hysteresis against price noise).
//! - Repeated upstream failures on a tier's active model promote the next
//!   candidate (in-memory, until restart or the next refresh); a success
//!   never demotes back, so failover cannot thrash.
//! - The snapshot (pins + full model→tier split) persists to disk and is
//!   reloaded on startup, so restarts never re-decide anything.

use miser_provider::Provider;
use miser_types::{
    CatalogModel, CatalogSnapshot, ComplexityTier, GatewayConfig, RoutingConfig, TierPin,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// All tiers, in price-band order.
const ALL_TIERS: [ComplexityTier; 5] = [
    ComplexityTier::Trivial,
    ComplexityTier::Simple,
    ComplexityTier::Standard,
    ComplexityTier::Hard,
    ComplexityTier::Reasoning,
];

/// Cap on persisted failover candidates per tier.
const MAX_CANDIDATES: usize = 20;

pub struct CatalogRouter {
    path: PathBuf,
    enabled: bool,
    state: Mutex<CatalogState>,
}

struct CatalogState {
    snapshot: CatalogSnapshot,
    /// Current active model per tier: the pin, or a failover promotion.
    active: BTreeMap<ComplexityTier, String>,
    /// Consecutive upstream failures per tier's active model.
    failures: BTreeMap<ComplexityTier, u32>,
}

impl CatalogRouter {
    /// Loads the persisted snapshot, falling back to pins seeded from the
    /// fixed `[tiers.*].model` config entries. Never fails: a missing or
    /// corrupt snapshot degrades to the seed, matching the config's fixed
    /// routing.
    pub fn load_or_seed(config: &GatewayConfig) -> Self {
        let routing = config.routing.clone();
        let path = Self::snapshot_path(&routing);
        let enabled = routing.mode == miser_types::RoutingMode::Catalog;
        let snapshot = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<CatalogSnapshot>(&data).ok())
        {
            Some(snapshot) => {
                tracing::info!(
                    path = %path.display(),
                    fetched_at = ?snapshot.fetched_at,
                    "catalog snapshot loaded"
                );
                snapshot
            }
            None => {
                let snapshot = Self::seed_from_config(config);
                tracing::info!(
                    path = %path.display(),
                    "no catalog snapshot found; seeded tier pins from config"
                );
                snapshot
            }
        };
        let active = snapshot
            .pins
            .iter()
            .map(|(tier, pin)| (*tier, pin.model.clone()))
            .collect();
        Self {
            path,
            enabled,
            state: Mutex::new(CatalogState {
                snapshot,
                active,
                failures: BTreeMap::new(),
            }),
        }
    }

    fn snapshot_path(routing: &RoutingConfig) -> PathBuf {
        if let Some(path) = &routing.cache_path {
            return PathBuf::from(shellexpand_home(path));
        }
        std::env::var("MISER_CATALOG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("catalog/models.json"))
    }

    /// Deterministic first snapshot: pin each tier to its configured model
    /// so enabling catalog mode is a no-op until a refresh runs.
    fn seed_from_config(config: &GatewayConfig) -> CatalogSnapshot {
        let pins: BTreeMap<ComplexityTier, TierPin> = config
            .tiers
            .iter()
            .map(|(tier, route)| {
                (
                    *tier,
                    TierPin {
                        model: route.model.clone(),
                        candidates: vec![route.model.clone()],
                    },
                )
            })
            .collect();
        let model_tiers: BTreeMap<String, ComplexityTier> = if config.routing.seed_from_config {
            pins.iter()
                .map(|(tier, pin)| (pin.model.clone(), *tier))
                .collect()
        } else {
            BTreeMap::new()
        };
        CatalogSnapshot {
            fetched_at: None,
            source: "seed".to_owned(),
            pins,
            model_tiers,
            total_models: 0,
            unranked_models: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The model a request for `tier` should use right now. `None` means
    /// "no catalog decision" and the caller falls back to the config route.
    pub fn active_model(&self, tier: ComplexityTier) -> Option<String> {
        let state = self.state.lock().ok()?;
        state.active.get(&tier).cloned()
    }

    pub fn report_success(&self, tier: ComplexityTier) {
        if let Ok(mut state) = self.state.lock() {
            state.failures.remove(&tier);
        }
    }

    /// Records an upstream failure for the tier's active model. When
    /// consecutive failures reach `threshold`, promotes the next candidate
    /// and returns the new active model.
    pub fn report_failure(&self, tier: ComplexityTier, threshold: u32) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        let count = state.failures.entry(tier).or_insert(0);
        *count += 1;
        if *count < threshold.max(1) {
            return None;
        }
        let current = state.active.get(&tier).cloned()?;
        let pin = state.snapshot.pins.get(&tier)?;
        let position = pin
            .candidates
            .iter()
            .position(|model| model == &current)
            .unwrap_or(usize::MAX);
        let next = pin.candidates.get(position + 1)?.clone();
        state.active.insert(tier, next.clone());
        state.failures.insert(tier, 0);
        Some(next)
    }

    /// Fetches the live catalog, re-partitions it into tiers, applies pin
    /// hysteresis, persists the snapshot, and swaps the in-memory state.
    pub async fn refresh(
        &self,
        provider: &Provider,
        routing: &RoutingConfig,
    ) -> Result<Value, String> {
        let data = provider
            .list_models()
            .await
            .map_err(|error| format!("catalog fetch failed: {error}"))?;
        let models = parse_catalog(&data);
        let mut snapshot = partition(&models, routing, &self.current_pins());

        // A tier whose filtered pool is empty keeps its previous pin — the
        // gateway can still route to it even though this refresh saw no
        // viable candidates.
        {
            let state = self.state.lock().map_err(|_| "catalog lock poisoned")?;
            for tier in ALL_TIERS {
                if let Some(previous) = state.snapshot.pins.get(&tier) {
                    snapshot
                        .pins
                        .entry(tier)
                        .or_insert_with(|| previous.clone());
                }
            }
        }
        snapshot.fetched_at = Some(unix_now());
        snapshot.source = "openrouter".to_owned();

        let changed: Vec<(ComplexityTier, String, String)> = {
            let mut state = self.state.lock().map_err(|_| "catalog lock poisoned")?;
            let mut changed = Vec::new();
            for tier in ALL_TIERS {
                let old = state.snapshot.pins.get(&tier).map(|pin| pin.model.clone());
                if let Some(pin) = snapshot.pins.get(&tier) {
                    if old.as_deref() != Some(pin.model.as_str()) {
                        changed.push((tier, old.unwrap_or_default(), pin.model.clone()));
                    }
                }
                if let Some(pin) = snapshot.pins.get(&tier) {
                    state.active.insert(tier, pin.model.clone());
                } else {
                    state.active.remove(&tier);
                }
            }
            state.failures.clear();
            state.snapshot = snapshot.clone();
            changed
        };
        self.persist(&snapshot)?;

        let migrated: Vec<Value> = changed
            .iter()
            .map(|(tier, from, to)| json!({"tier": format_tier(*tier), "from": from, "to": to}))
            .collect();
        tracing::info!(
            migrated = ?changed,
            total = snapshot.total_models,
            "catalog refreshed"
        );
        Ok(json!({
            "status": "refreshed",
            "fetched_at": snapshot.fetched_at,
            "total_models": snapshot.total_models,
            "unranked_models": snapshot.unranked_models,
            "pins": summary_pins(&snapshot),
            "migrations": migrated,
        }))
    }

    fn current_pins(&self) -> BTreeMap<ComplexityTier, TierPin> {
        self.state
            .lock()
            .map(|state| state.snapshot.pins.clone())
            .unwrap_or_default()
    }

    /// Current routing state as JSON for the admin endpoint.
    pub fn summary_json(&self) -> Value {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return json!({"error": "catalog lock poisoned"}),
        };
        let tier_summaries = summary_pins(&state.snapshot)
            .into_iter()
            .map(|pin| {
                let mut pin = pin.clone();
                let tier = pin["tier"].as_str().unwrap_or_default().to_owned();
                pin["active"] = state
                    .active
                    .iter()
                    .find(|(t, _)| format_tier(**t) == tier)
                    .map(|(_, model)| json!(model))
                    .unwrap_or(Value::Null);
                pin["consecutive_failures"] = json!(
                    state
                        .failures
                        .iter()
                        .find(|(t, _)| format_tier(**t) == tier)
                        .map(|(_, count)| *count)
                        .unwrap_or(0)
                );
                pin
            })
            .collect::<Vec<_>>();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for tier in state.snapshot.model_tiers.values() {
            *counts.entry(format_tier(*tier)).or_insert(0) += 1;
        }
        json!({
            "enabled": self.enabled,
            "snapshot_path": self.path.display().to_string(),
            "source": state.snapshot.source,
            "fetched_at": state.snapshot.fetched_at,
            "total_models": state.snapshot.total_models,
            "unranked_models": state.snapshot.unranked_models,
            "tiers": tier_summaries,
            "tier_split_counts": counts,
        })
    }

    fn persist(&self, snapshot: &CatalogSnapshot) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create catalog dir: {error}"))?;
        }
        let data = serde_json::to_string_pretty(snapshot)
            .map_err(|error| format!("failed to encode snapshot: {error}"))?;
        std::fs::write(&self.path, data)
            .map_err(|error| format!("failed to write snapshot: {error}"))?;
        Ok(())
    }
}

/// Splits the catalog into the five tiers by input price and picks sticky
/// pins with hysteresis. Every model lands in `model_tiers` (the full
/// split); only filter-passing models become candidates.
pub fn partition(
    models: &[CatalogModel],
    routing: &RoutingConfig,
    previous_pins: &BTreeMap<ComplexityTier, TierPin>,
) -> CatalogSnapshot {
    let bands = &routing.bands;
    let filters = &routing.filters;

    let passes_filters = |model: &CatalogModel| -> bool {
        if !model.text_output {
            return false;
        }
        if filters.require_tools && !model.tools {
            return false;
        }
        if model.context_length < filters.min_context {
            return false;
        }
        let is_free = model.input_price_per_m <= 0.0 || model.id.ends_with(":free");
        if is_free && !filters.allow_free {
            return false;
        }
        if filters
            .deny_prefixes
            .iter()
            .any(|prefix| model.id.starts_with(prefix.as_str()))
        {
            return false;
        }
        true
    };

    let mut model_tiers: BTreeMap<String, ComplexityTier> = BTreeMap::new();
    let mut pools: BTreeMap<ComplexityTier, Vec<&CatalogModel>> = BTreeMap::new();
    let mut unranked = 0usize;
    for model in models {
        let tier = band_tier(model.input_price_per_m, bands);
        model_tiers.insert(model.id.clone(), tier);
        if passes_filters(model) {
            pools.entry(tier).or_default().push(model);
        } else {
            unranked += 1;
        }
    }

    let mut pins = BTreeMap::new();
    for tier in ALL_TIERS {
        let pool = pools.entry(tier).or_default();
        pool.sort_by(|a, b| {
            a.input_price_per_m
                .total_cmp(&b.input_price_per_m)
                .then(a.id.cmp(&b.id))
        });
        if pool.is_empty() {
            continue;
        }
        let chosen = match previous_pins.get(&tier) {
            Some(previous) if pool.iter().any(|model| model.id == previous.model) => {
                // Hysteresis: keep the existing pin unless the cheapest
                // candidate is meaningfully cheaper.
                let current_price = pool
                    .iter()
                    .find(|model| model.id == previous.model)
                    .map(|model| model.input_price_per_m)
                    .unwrap_or_default();
                let best = pin_choice(pool, tier, filters);
                if best.id != previous.model
                    && best.input_price_per_m
                        <= current_price * (1.0 - routing.switch_saving_ratio.max(0.0) as f64)
                {
                    best
                } else {
                    pool.iter()
                        .find(|model| model.id == previous.model)
                        .unwrap()
                }
            }
            _ => pin_choice(pool, tier, filters),
        };
        let mut candidates: Vec<String> = Vec::with_capacity(MAX_CANDIDATES);
        candidates.push(chosen.id.clone());
        for model in pool {
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
            if model.id != chosen.id {
                candidates.push(model.id.clone());
            }
        }
        pins.insert(
            tier,
            TierPin {
                model: chosen.id.clone(),
                candidates,
            },
        );
    }

    CatalogSnapshot {
        fetched_at: None,
        source: "openrouter".to_owned(),
        pins,
        model_tiers,
        total_models: models.len(),
        unranked_models: unranked,
    }
}

/// Price band → tier. Boundaries are inclusive maxima.
pub fn band_tier(input_price_per_m: f64, bands: &miser_types::RoutingBands) -> ComplexityTier {
    if input_price_per_m <= bands.trivial_max {
        ComplexityTier::Trivial
    } else if input_price_per_m <= bands.simple_max {
        ComplexityTier::Simple
    } else if input_price_per_m <= bands.standard_max {
        ComplexityTier::Standard
    } else if input_price_per_m <= bands.reasoning_max {
        ComplexityTier::Reasoning
    } else {
        ComplexityTier::Hard
    }
}

/// The default pin for a candidate pool: cheapest model, except the
/// reasoning tier prefers a reasoning-capable model when one exists.
fn pin_choice<'a>(
    pool: &[&'a CatalogModel],
    tier: ComplexityTier,
    filters: &miser_types::RoutingFilters,
) -> &'a CatalogModel {
    if tier == ComplexityTier::Reasoning && filters.prefer_reasoning_pin {
        if let Some(model) = pool.iter().copied().find(|model| model.reasoning) {
            return model;
        }
    }
    pool[0]
}

/// Normalizes the provider's `/models` payload into [`CatalogModel`]s.
pub fn parse_catalog(data: &Value) -> Vec<CatalogModel> {
    let entries = data
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    entries.iter().filter_map(parse_model).collect()
}

fn parse_model(entry: &Value) -> Option<CatalogModel> {
    let id = entry.get("id")?.as_str()?.to_owned();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_owned();
    let pricing = entry.get("pricing").cloned().unwrap_or(Value::Null);
    let price = |key: &str| -> f64 {
        pricing
            .get(key)
            .and_then(Value::as_str)
            .and_then(|raw| raw.parse::<f64>().ok())
            .map(|per_token| per_token * 1_000_000.0)
            .unwrap_or(0.0)
    };
    let context_length = entry
        .get("context_length")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let parameters: Vec<String> = entry
        .get("supported_parameters")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let text_output = entry
        .get("architecture")
        .and_then(|arch| arch.get("output_modalities"))
        .and_then(Value::as_array)
        .map(|items| items.iter().any(|item| item.as_str() == Some("text")))
        .unwrap_or(true);
    Some(CatalogModel {
        id,
        name,
        context_length,
        input_price_per_m: price("prompt"),
        output_price_per_m: price("completion"),
        tools: parameters.iter().any(|parameter| parameter == "tools"),
        reasoning: parameters
            .iter()
            .any(|parameter| parameter == "reasoning" || parameter == "include_reasoning"),
        text_output,
    })
}

fn summary_pins(snapshot: &CatalogSnapshot) -> Vec<Value> {
    ALL_TIERS
        .iter()
        .filter_map(|tier| {
            let pin = snapshot.pins.get(tier)?;
            Some(json!({
                "tier": format_tier(*tier),
                "pin": pin.model,
                "candidates": pin.candidates,
            }))
        })
        .collect()
}

fn format_tier(tier: ComplexityTier) -> String {
    serde_json::to_string(&tier)
        .unwrap_or_else(|_| format!("{tier:?}"))
        .trim_matches('"')
        .to_owned()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Expands a leading `~/` in a configured path.
fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use miser_types::{RoutingBands, RoutingMode};

    fn routing() -> RoutingConfig {
        RoutingConfig {
            mode: RoutingMode::Catalog,
            ..Default::default()
        }
    }

    fn model(id: &str, input_price: f64) -> CatalogModel {
        CatalogModel {
            id: id.to_owned(),
            name: id.to_owned(),
            context_length: 200_000,
            input_price_per_m: input_price,
            output_price_per_m: input_price * 4.0,
            tools: true,
            reasoning: false,
            text_output: true,
        }
    }

    #[test]
    fn bands_split_by_price() {
        let bands = RoutingBands::default();
        assert_eq!(band_tier(0.01, &bands), ComplexityTier::Trivial);
        assert_eq!(band_tier(0.035, &bands), ComplexityTier::Trivial);
        assert_eq!(band_tier(0.04, &bands), ComplexityTier::Simple);
        assert_eq!(band_tier(0.2, &bands), ComplexityTier::Standard);
        assert_eq!(band_tier(0.65, &bands), ComplexityTier::Reasoning);
        assert_eq!(band_tier(1.4, &bands), ComplexityTier::Reasoning);
        assert_eq!(band_tier(3.0, &bands), ComplexityTier::Hard);
    }

    #[test]
    fn every_model_is_ranked_into_a_tier() {
        let models = vec![
            model("cheap/free-ish", 0.0),
            model("mid/model", 0.2),
            model("frontier/model", 5.0),
        ];
        let snapshot = partition(&models, &routing(), &BTreeMap::new());
        assert_eq!(snapshot.model_tiers.len(), 3);
        assert_eq!(snapshot.total_models, 3);
        // The zero-priced model is banded into trivial but fails the
        // candidate filters (free models excluded by default).
        assert_eq!(snapshot.unranked_models, 1);
        // Zero price bands to trivial even though it cannot become a
        // candidate (free models are filtered out).
        assert_eq!(
            snapshot.model_tiers.get("cheap/free-ish"),
            Some(&ComplexityTier::Trivial)
        );
    }

    #[test]
    fn filters_exclude_non_candidates_but_keep_split() {
        let routing = routing();
        let mut no_tools = model("weak/model", 0.01);
        no_tools.tools = false;
        let mut small_context = model("small/model", 0.02);
        small_context.context_length = 8_192;
        let mut image_only = model("vision/model", 0.3);
        image_only.text_output = false;
        let models = vec![no_tools, small_context, image_only, model("ok/model", 0.5)];
        let snapshot = partition(&models, &routing, &BTreeMap::new());
        assert_eq!(snapshot.unranked_models, 3);
        assert_eq!(
            snapshot.pins.get(&ComplexityTier::Reasoning).unwrap().model,
            "ok/model"
        );
        // Filtered-out models still appear in the split.
        assert_eq!(
            snapshot.model_tiers.get("weak/model"),
            Some(&ComplexityTier::Trivial)
        );
    }

    #[test]
    fn seed_pin_is_kept_unless_candidate_is_meaningfully_cheaper() {
        let mut previous = BTreeMap::new();
        previous.insert(
            ComplexityTier::Trivial,
            TierPin {
                model: "anchor/model".to_owned(),
                candidates: vec!["anchor/model".to_owned()],
            },
        );
        // 10% cheaper: below the default 25% saving ratio → keep pin.
        let models = vec![model("anchor/model", 0.03), model("cheaper/model", 0.027)];
        let snapshot = partition(&models, &routing(), &previous);
        assert_eq!(
            snapshot.pins.get(&ComplexityTier::Trivial).unwrap().model,
            "anchor/model"
        );
        // 60% cheaper → migrate.
        let models = vec![model("anchor/model", 0.03), model("cheaper/model", 0.012)];
        let snapshot = partition(&models, &routing(), &previous);
        assert_eq!(
            snapshot.pins.get(&ComplexityTier::Trivial).unwrap().model,
            "cheaper/model"
        );
    }

    #[test]
    fn vanished_pin_falls_back_to_cheapest_candidate() {
        let mut previous = BTreeMap::new();
        previous.insert(
            ComplexityTier::Trivial,
            TierPin {
                model: "gone/model".to_owned(),
                candidates: vec!["gone/model".to_owned()],
            },
        );
        let models = vec![model("a/model", 0.01), model("b/model", 0.02)];
        let snapshot = partition(&models, &routing(), &previous);
        assert_eq!(
            snapshot.pins.get(&ComplexityTier::Trivial).unwrap().model,
            "a/model"
        );
    }

    #[test]
    fn reasoning_pin_prefers_reasoning_capable_model() {
        let mut reasoning_model = model("smart/model", 0.5);
        reasoning_model.reasoning = true;
        let models = vec![model("dumb/model", 0.4), reasoning_model];
        let snapshot = partition(&models, &routing(), &BTreeMap::new());
        assert_eq!(
            snapshot.pins.get(&ComplexityTier::Reasoning).unwrap().model,
            "smart/model"
        );
    }

    #[test]
    fn failover_promotes_next_candidate_after_threshold() {
        let snapshot = CatalogSnapshot {
            source: "test".to_owned(),
            pins: BTreeMap::from([(
                ComplexityTier::Simple,
                TierPin {
                    model: "pin/model".to_owned(),
                    candidates: vec!["pin/model".to_owned(), "alt/model".to_owned()],
                },
            )]),
            ..Default::default()
        };
        let router = CatalogRouter {
            path: PathBuf::from("/tmp/miser-catalog-test/never-written.json"),
            enabled: true,
            state: Mutex::new(CatalogState {
                snapshot,
                active: BTreeMap::from([(ComplexityTier::Simple, "pin/model".to_owned())]),
                failures: BTreeMap::new(),
            }),
        };
        // Below threshold: no promotion.
        assert_eq!(router.report_failure(ComplexityTier::Simple, 3), None);
        assert_eq!(router.report_failure(ComplexityTier::Simple, 3), None);
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("pin/model".to_owned())
        );
        // Third failure crosses the threshold.
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, 3),
            Some("alt/model".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("alt/model".to_owned())
        );
        // Success keeps the promoted model (no demotion thrash) and resets
        // the counter.
        router.report_success(ComplexityTier::Simple);
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("alt/model".to_owned())
        );
        // A fresh failure cycle re-promotes from the promoted model's
        // position; the list is exhausted → None.
        router.report_failure(ComplexityTier::Simple, 1);
        assert_eq!(router.report_failure(ComplexityTier::Simple, 1), None);
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("alt/model".to_owned())
        );
    }

    // --- helpers for the tests below ---

    fn router_with_pins(pins: BTreeMap<ComplexityTier, TierPin>) -> CatalogRouter {
        let active = pins
            .iter()
            .map(|(tier, pin)| (*tier, pin.model.clone()))
            .collect();
        CatalogRouter {
            path: PathBuf::from("/tmp/miser-catalog-unit-tests/never-written.json"),
            enabled: true,
            state: Mutex::new(CatalogState {
                snapshot: CatalogSnapshot {
                    source: "test".to_owned(),
                    pins,
                    ..Default::default()
                },
                active,
                failures: BTreeMap::new(),
            }),
        }
    }

    fn temp_snapshot_path(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "miser_test_catalog_{}_{}_{seq}_{label}.json",
            std::process::id(),
            nanos
        ))
    }

    fn gateway_config(cache_path: &std::path::Path, seed_split: bool) -> GatewayConfig {
        serde_json::from_value(json!({
            "classifier": {},
            "provider": {"api_key": "test"},
            "tiers": {
                "trivial": {"model": "t/model"},
                "simple": {"model": "s/model"},
                "standard": {"model": "std/model"},
                "hard": {"model": "h/model"},
                "reasoning": {"model": "r/model"}
            },
            "routing": {
                "mode": "catalog",
                "cache_path": cache_path.display().to_string(),
                "seed_from_config": seed_split
            }
        }))
        .expect("test gateway config parses")
    }

    #[test]
    fn success_resets_failure_counter_without_promotion() {
        let router = router_with_pins(BTreeMap::from([(
            ComplexityTier::Trivial,
            TierPin {
                model: "pin/model".to_owned(),
                candidates: vec!["pin/model".to_owned(), "alt/model".to_owned()],
            },
        )]));
        // One provider failure below the threshold, then a 2xx resets it.
        assert_eq!(router.report_failure(ComplexityTier::Trivial, 3), None);
        router.report_success(ComplexityTier::Trivial);
        // The counter restarted, so one fresh failure must not promote.
        assert_eq!(router.report_failure(ComplexityTier::Trivial, 3), None);
        assert_eq!(
            router.active_model(ComplexityTier::Trivial),
            Some("pin/model".to_owned())
        );
    }

    #[test]
    fn zero_threshold_promotes_on_first_failure() {
        let router = router_with_pins(BTreeMap::from([(
            ComplexityTier::Simple,
            TierPin {
                model: "pin/model".to_owned(),
                candidates: vec!["pin/model".to_owned(), "alt/model".to_owned()],
            },
        )]));
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, 0),
            Some("alt/model".to_owned()),
            "threshold is clamped to at least one failure"
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("alt/model".to_owned())
        );
    }

    #[test]
    fn switch_ratio_boundary_migrates_only_at_exact_saving() {
        let mut routing = routing();
        routing.switch_saving_ratio = 0.5;
        let previous = BTreeMap::from([(
            ComplexityTier::Simple,
            TierPin {
                model: "anchor/model".to_owned(),
                candidates: vec!["anchor/model".to_owned()],
            },
        )]);
        // Exactly 50% cheaper (0.08 → 0.04): the rule is <=, so migrate.
        let models = vec![model("anchor/model", 0.08), model("cheaper/model", 0.04)];
        let snapshot = partition(&models, &routing, &previous);
        assert_eq!(
            snapshot.pins[&ComplexityTier::Simple].model,
            "cheaper/model"
        );
        // Just under 50%: hysteresis keeps the pin.
        let models = vec![model("anchor/model", 0.08), model("barely/model", 0.0401)];
        let snapshot = partition(&models, &routing, &previous);
        assert_eq!(snapshot.pins[&ComplexityTier::Simple].model, "anchor/model");
        // A 25% saving is below a 0.5 ratio: no migration.
        let models = vec![model("anchor/model", 0.08), model("mid/model", 0.06)];
        let snapshot = partition(&models, &routing, &previous);
        assert_eq!(snapshot.pins[&ComplexityTier::Simple].model, "anchor/model");
    }

    #[test]
    fn band_boundaries_are_inclusive_maxima() {
        let bands = RoutingBands::default();
        assert_eq!(
            band_tier(bands.trivial_max, &bands),
            ComplexityTier::Trivial
        );
        assert_eq!(band_tier(bands.simple_max, &bands), ComplexityTier::Simple);
        assert_eq!(
            band_tier(bands.standard_max, &bands),
            ComplexityTier::Standard
        );
        assert_eq!(
            band_tier(bands.reasoning_max, &bands),
            ComplexityTier::Reasoning
        );
        assert_eq!(
            band_tier(bands.trivial_max * 1.01, &bands),
            ComplexityTier::Simple
        );
        assert_eq!(
            band_tier(bands.simple_max * 1.01, &bands),
            ComplexityTier::Standard
        );
        assert_eq!(
            band_tier(bands.standard_max * 1.01, &bands),
            ComplexityTier::Reasoning
        );
        assert_eq!(
            band_tier(bands.reasoning_max * 1.01, &bands),
            ComplexityTier::Hard
        );
    }

    #[test]
    fn seed_from_config_pins_configured_models_without_writing() {
        let path = temp_snapshot_path("seed");
        let config = gateway_config(&path, true);
        let router = CatalogRouter::load_or_seed(&config);
        assert!(router.enabled());
        assert_eq!(
            router.active_model(ComplexityTier::Trivial),
            Some("t/model".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("s/model".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Standard),
            Some("std/model".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Hard),
            Some("h/model".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Reasoning),
            Some("r/model".to_owned())
        );
        assert!(!path.exists(), "seeding must not write a snapshot file");
        // The seeded split ranks each configured model into its tier.
        let summary = router.summary_json();
        assert_eq!(summary["enabled"], json!(true));
        assert_eq!(summary["tier_split_counts"]["trivial"], json!(1));
    }

    #[test]
    fn seed_without_config_split_leaves_model_tiers_empty() {
        let path = temp_snapshot_path("seed-nosplit");
        let config = gateway_config(&path, false);
        let router = CatalogRouter::load_or_seed(&config);
        let summary = router.summary_json();
        assert!(
            summary["tier_split_counts"]
                .as_object()
                .expect("object")
                .is_empty(),
            "seed_from_config=false must not invent a tier split: {summary}"
        );
    }

    #[test]
    fn fixed_mode_disables_catalog_routing() {
        let path = temp_snapshot_path("fixed");
        let mut config = gateway_config(&path, true);
        config.routing.mode = RoutingMode::Fixed;
        assert!(!CatalogRouter::load_or_seed(&config).enabled());
    }

    #[test]
    fn load_or_seed_reloads_persisted_snapshot() {
        let path = temp_snapshot_path("reload");
        let snapshot = CatalogSnapshot {
            source: "openrouter".to_owned(),
            fetched_at: Some(1_767_225_600),
            pins: BTreeMap::from([(
                ComplexityTier::Simple,
                TierPin {
                    model: "persisted/model".to_owned(),
                    candidates: vec!["persisted/model".to_owned(), "backup/model".to_owned()],
                },
            )]),
            model_tiers: BTreeMap::from([("persisted/model".to_owned(), ComplexityTier::Simple)]),
            total_models: 1,
            unranked_models: 0,
        };
        std::fs::write(&path, serde_json::to_string(&snapshot).unwrap()).unwrap();
        let config = gateway_config(&path, true);
        let router = CatalogRouter::load_or_seed(&config);
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("persisted/model".to_owned()),
            "snapshot pin wins over the config seed"
        );
        // Tiers absent from the snapshot have no catalog decision, so the
        // caller falls back to the fixed config route.
        assert_eq!(router.active_model(ComplexityTier::Trivial), None);
        let summary = router.summary_json();
        assert_eq!(summary["source"], json!("openrouter"));
        assert_eq!(summary["fetched_at"], json!(1_767_225_600));
        assert_eq!(summary["tiers"][0]["pin"], json!("persisted/model"));
        assert_eq!(summary["tiers"][0]["active"], json!("persisted/model"));
        assert_eq!(summary["tiers"][0]["consecutive_failures"], json!(0));
    }

    #[test]
    fn load_or_seed_falls_back_to_seed_on_corrupt_snapshot() {
        let path = temp_snapshot_path("corrupt");
        std::fs::write(&path, "definitely not json").unwrap();
        let config = gateway_config(&path, true);
        let router = CatalogRouter::load_or_seed(&config);
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("s/model".to_owned()),
            "a corrupt snapshot degrades to the config seed"
        );
    }

    #[test]
    fn parse_catalog_normalizes_provider_payload() {
        let data = json!({
            "data": [
                {
                    "id": "vendor/model-a",
                    "name": "Model A",
                    "context_length": 131072,
                    "pricing": {"prompt": "0.0000015", "completion": "0.000002"},
                    "supported_parameters": ["tools", "reasoning"],
                    "architecture": {"output_modalities": ["text", "image"]}
                },
                {"id": "vendor/defaults"},
                {"name": "no id, skipped"}
            ]
        });
        let models = parse_catalog(&data);
        assert_eq!(models.len(), 2, "entries without an id are skipped");

        let a = &models[0];
        assert_eq!(a.id, "vendor/model-a");
        assert!(
            (a.input_price_per_m - 1.5).abs() < 1e-9,
            "per-token price × 1e6, got {}",
            a.input_price_per_m
        );
        assert!((a.output_price_per_m - 2.0).abs() < 1e-9);
        assert_eq!(a.context_length, 131_072);
        assert!(a.tools && a.reasoning && a.text_output);

        let b = &models[1];
        assert_eq!(b.id, "vendor/defaults");
        assert_eq!(b.name, "vendor/defaults", "missing name defaults to the id");
        assert_eq!(b.input_price_per_m, 0.0);
        assert!(!b.tools && !b.reasoning);
        assert!(
            b.text_output,
            "missing architecture defaults to text output"
        );

        assert!(
            parse_catalog(&json!({})).is_empty(),
            "missing data array yields no models"
        );
    }

    #[test]
    fn candidate_list_is_capped_at_max_candidates() {
        let models: Vec<CatalogModel> = (0..25)
            .map(|i| model(&format!("m{i:02}/model"), 0.01))
            .collect();
        let snapshot = partition(&models, &routing(), &BTreeMap::new());
        let pin = snapshot.pins.get(&ComplexityTier::Trivial).unwrap();
        assert_eq!(pin.candidates.len(), 20, "candidate list capped at 20");
        assert_eq!(
            pin.candidates[0], "m00/model",
            "cheapest pin first; equal prices tie-break by id"
        );
        assert!(pin.candidates.contains(&"m19/model".to_owned()));
        assert!(!pin.candidates.contains(&"m20/model".to_owned()));
    }

    #[test]
    fn relaxed_filters_readmit_models_except_deny_prefixes() {
        let mut routing = routing();
        routing.filters.require_tools = false;
        routing.filters.min_context = 8_192;
        routing.filters.allow_free = true;
        routing.filters.deny_prefixes = vec!["blocked/".to_owned()];

        let mut no_tools = model("notools/model", 0.01);
        no_tools.tools = false;
        let mut exact_context = model("exact/model", 0.02);
        exact_context.context_length = 8_192;
        let blocked = model("blocked/model", 0.03);
        let free_suffix = model("vendor/alt:free", 0.01);
        let good = model("good/model", 0.5);
        let models = vec![no_tools, exact_context, blocked, free_suffix, good];

        let snapshot = partition(&models, &routing, &BTreeMap::new());
        assert_eq!(
            snapshot.unranked_models, 1,
            "only the deny-prefixed model is filtered out"
        );
        assert_eq!(
            snapshot.model_tiers.get("blocked/model"),
            Some(&ComplexityTier::Trivial),
            "denied models still appear in the split"
        );
        // Trivial pool: notools(0.01), exact(0.02), free(0.01) — the
        // price tie between notools and vendor/alt:free breaks by id.
        assert_eq!(
            snapshot.pins[&ComplexityTier::Trivial].model,
            "notools/model"
        );
        assert_eq!(
            snapshot.pins[&ComplexityTier::Reasoning].model,
            "good/model"
        );
    }

    #[test]
    fn min_context_boundary_is_inclusive() {
        let mut under = model("under/model", 0.01);
        under.context_length = 65_535;
        let mut exact = model("exact/model", 0.02);
        exact.context_length = 65_536;
        let snapshot = partition(&[under, exact], &routing(), &BTreeMap::new());
        assert_eq!(
            snapshot.unranked_models, 1,
            "just under the default min_context is filtered"
        );
        assert_eq!(snapshot.pins[&ComplexityTier::Trivial].model, "exact/model");
    }
}
