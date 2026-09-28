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
use std::io::Write as _;
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
    /// Serialises refreshes against each other.
    ///
    /// `state` is not enough: `commit` must persist *before* taking it, so that
    /// a failed write leaves routing untouched. Two refreshes could therefore
    /// both reach `persist` and write the same sibling `.tmp` file -- each
    /// truncating it and writing from offset 0, so the first to rename
    /// published a byte-level mix of both snapshots. The startup refresh task
    /// and the admin endpoint both refresh, so this is reachable. Held across
    /// the whole fetch-then-commit sequence, never across an `.await` that
    /// needs another lock.
    refresh_lock: Mutex<()>,
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
            refresh_lock: Mutex::new(()),
            state: Mutex::new(CatalogState {
                snapshot,
                active,
                failures: BTreeMap::new(),
            }),
        }
    }

    /// `MISER_CATALOG_FILE` overrides `routing.cache_path`.
    ///
    /// The environment variable is consulted *first*, because that is what
    /// "overridable via `MISER_CATALOG_FILE`" means. It used to be the
    /// fallback, so `cache_path` won whenever it was set — and the shipped
    /// `config/miser.toml` sets it, which made the documented override inert in
    /// the shipped deployment and therefore untestable by anyone following the
    /// docs.
    fn snapshot_path(routing: &RoutingConfig) -> PathBuf {
        if let Ok(path) = std::env::var("MISER_CATALOG_FILE") {
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
        if let Some(path) = &routing.cache_path {
            return PathBuf::from(shellexpand_home(path));
        }
        PathBuf::from("catalog/models.json")
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

    /// Record a success for `model` on `tier`.
    ///
    /// Ignored unless `model` is the tier's current active model. Requests are
    /// served concurrently, so a straggler success for a model the tier has
    /// already been promoted off must not clear the streak of the model that
    /// replaced it -- otherwise the replacement can never accumulate
    /// consecutive failures and so never fails over.
    pub fn report_success(&self, tier: ComplexityTier, model: &str) {
        if let Ok(mut state) = self.state.lock() {
            if state.active.get(&tier).map(String::as_str) == Some(model) {
                state.failures.remove(&tier);
            }
        }
    }

    /// Records an upstream failure for `model` on `tier`. When consecutive
    /// failures reach `threshold`, promotes the next candidate and returns the
    /// new active model.
    ///
    /// Ignored unless `model` is the tier's current active model: a late
    /// failure for a model already promoted off says nothing about its
    /// replacement and must not advance the replacement's streak.
    pub fn report_failure(
        &self,
        tier: ComplexityTier,
        model: &str,
        threshold: u32,
    ) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        if state.active.get(&tier).map(String::as_str) != Some(model) {
            return None;
        }
        // Resolve the successor *before* touching the counter. A tier whose
        // active model is absent from its own candidate list has no successor,
        // and incrementing first would leave a count that only ever grows and
        // can never be cleared or promoted past -- an unbounded, permanently
        // un-clearable figure that `summary_json` would report.
        let count = state.failures.entry(tier).or_insert(0);
        *count += 1;
        if *count < threshold.max(1) {
            return None;
        }
        // Resolve the successor only once the threshold is crossed.
        //
        // `position` is `None` when the active model is absent from its own
        // candidate list, rather than a `usize::MAX` sentinel that the increment
        // would overflow -- a panic here would be raised while the state mutex is
        // held, poisoning it, so `active_model` would return `None` forever and
        // catalog routing would be silently dead for the life of the process.
        let position = state
            .active
            .get(&tier)
            .zip(state.snapshot.pins.get(&tier))
            .and_then(|(current, pin)| pin.candidates.iter().position(|m| m == current));
        let Some(position) = position else {
            // An inconsistent snapshot: this tier's failures cannot promote
            // anything, so a count here would only grow and never be cleared.
            state.failures.insert(tier, 0);
            return None;
        };
        let Some(next) = state
            .snapshot
            .pins
            .get(&tier)
            .and_then(|pin| pin.candidates.get(position + 1))
            .cloned()
        else {
            // The candidate list is exhausted. The failure is real, so the count
            // is kept -- it still reports the tier's health, and a success or a
            // refresh clears it.
            return None;
        };
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
        // Taken *after* the fetch, so the guard is never held across an await
        // (which would make the handler future non-`Send`). Concurrent refreshes
        // therefore fetch in parallel and serialise only on the write, which is
        // where the hazard is: `commit` has to persist before taking `state`, so
        // that a failed write leaves routing untouched, and without this lock
        // two refreshes would write the same sibling `.tmp` -- each truncating
        // it and writing from offset 0, so the first to rename published a
        // byte-level mix of both snapshots. The startup refresh task and the
        // admin endpoint both refresh, so this is reachable.
        let _refresh = self
            .refresh_lock
            .lock()
            .map_err(|_| "catalog refresh lock poisoned".to_owned())?;
        let models = parse_catalog(&data);
        if models.is_empty() {
            // A 2xx body with no usable `data` array would otherwise persist a
            // snapshot with no pins and an empty model->tier split, wiping the
            // state the contract says survives until a real refresh. The
            // previous pins stay in force and nothing is written.
            return Err("catalog fetch returned no models; keeping the current pins".to_owned());
        }
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
        // ...but the *candidate list* it kept was validated against the
        // catalog this refresh just superseded. Carrying it forward verbatim
        // let `report_failure` promote a successor that this refresh proved is
        // not in the catalog at all — a delisted or delisted-by-filter id. And
        // because a 404 from a delisted model is classed as a *client* error,
        // `report_failure` is never called again, so the tier stayed wedged on
        // the stale pin with no route out. Keeping the pin is deliberate; the
        // stale candidates are not.
        for pin in snapshot.pins.values_mut() {
            pin.candidates
                .retain(|id| snapshot.model_tiers.contains_key(id));
        }
        snapshot.fetched_at = Some(unix_now());
        snapshot.source = "openrouter".to_owned();

        let changed: Vec<(ComplexityTier, String, String)> = self.commit(&snapshot)?;

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

    /// Persist the snapshot, then swap the in-memory state.
    ///
    /// The order is load-bearing. Swapping first meant a failed write returned
    /// an error the endpoint reported as `catalog_refresh/failure` with a 502,
    /// while live traffic was already being served from the new pins and every
    /// failure counter had been cleared -- and the next restart would silently
    /// revert to the pins still on disk. Persisting first means a failure
    /// changes nothing observable.
    fn commit(
        &self,
        snapshot: &CatalogSnapshot,
    ) -> Result<Vec<(ComplexityTier, String, String)>, String> {
        self.persist(snapshot)?;
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
        Ok(changed)
    }

    /// Write the snapshot atomically.
    ///
    /// `fs::write` is `File::create` (O_TRUNC) plus `write_all`, and callers
    /// reach it after releasing the state lock -- so two overlapping refreshes
    /// (the startup task and the admin endpoint both refresh) each truncated and
    /// wrote from offset 0 and interleaved into invalid JSON, and a crash or a
    /// full disk did the same. Either way the next boot read a corrupt file and
    /// silently re-derived every pin from config, contradicting the stability
    /// contract. A sibling temp file, flushed and renamed, means a reader only
    /// ever sees the old snapshot or the new one. The temp file must share a
    /// directory with the target for `rename` to be atomic.
    fn persist(&self, snapshot: &CatalogSnapshot) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create catalog dir: {error}"))?;
        }
        let data = serde_json::to_string_pretty(snapshot)
            .map_err(|error| format!("failed to encode snapshot: {error}"))?;
        let tmp = self.path.with_file_name(format!(
            "{}.tmp",
            self.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "models.json".to_string())
        ));
        let write = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        if let Err(error) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("failed to write catalog snapshot: {error}"));
        }
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
                let kept = pool
                    .iter()
                    .find(|model| model.id == previous.model)
                    .unwrap();
                let current_price = kept.input_price_per_m;
                let best = pin_choice(pool, tier, filters);
                // A pin that cannot do the tier's job is not a pin worth
                // keeping, whatever the price. `prefer_reasoning_pin` is
                // enforced inside `pin_choice` and in the candidate loop, but
                // the hysteresis arm used to keep the incumbent purely on
                // price, so a Reasoning pin left over from before the flag was
                // set -- or from a pool that had no reasoning model at the
                // time -- survived refreshes indefinitely: it could never be
                // migrated away, because hysteresis only ever moves to a
                // *cheaper* model and a capable one costs more. The tier then
                // served reasoning traffic with a model that cannot reason,
                // which is the under-route direction ROUTING.md calls the
                // expensive one, and the only escape was a three-failure
                // failover streak.
                let incumbent_is_disqualified = keeps_reasoning_pin(tier, filters, kept);
                let migrates = best.id != previous.model
                    && best.input_price_per_m
                        <= current_price * (1.0 - routing.switch_saving_ratio.max(0.0) as f64);
                if incumbent_is_disqualified || migrates {
                    best
                } else {
                    kept
                }
            }
            _ => pin_choice(pool, tier, filters),
        };
        let mut candidates: Vec<String> = Vec::with_capacity(MAX_CANDIDATES);
        candidates.push(chosen.id.clone());
        // A reasoning-tier failover target must itself be reasoning-capable.
        // `pin_choice` returns the cheapest *reasoning-capable* model, which is
        // generally not the cheapest in the band -- every cheaper model is
        // non-reasoning by construction -- so building the list from the whole
        // price-sorted pool made `candidates[1]` guaranteed non-reasoning. The
        // tier meant for reasoning traffic would degrade on the first upstream
        // failure streak. When the pool holds no other reasoning model, the
        // list is left as just the pin, which keeps "a success never demotes"
        // and means failover simply does not fire rather than firing onto
        // something that cannot reason.
        for model in pool {
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
            if model.id == chosen.id {
                continue;
            }
            if keeps_reasoning_pin(tier, filters, model) {
                continue;
            }
            candidates.push(model.id.clone());
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
    } else if input_price_per_m <= bands.hard_max {
        ComplexityTier::Hard
    } else if input_price_per_m <= bands.reasoning_max {
        ComplexityTier::Reasoning
    } else {
        // Above every band: the strongest tier, which is also the only one that
        // can serve a model priced out of all of them.
        ComplexityTier::Reasoning
    }
}

/// The default pin for a candidate pool: cheapest model, except the
/// reasoning tier prefers a reasoning-capable model when one exists.
/// Whether `filters` requires the Reasoning tier to use a reasoning-capable
/// model.
///
/// One predicate, shared by the pin choice, the candidate list and the
/// hysteresis arm. They previously each re-derived it, and the hysteresis arm
/// omitted it -- which is precisely how a non-reasoning pin could outlive the
/// condition that made it invalid.
fn keeps_reasoning_pin(
    tier: ComplexityTier,
    filters: &miser_types::RoutingFilters,
    model: &CatalogModel,
) -> bool {
    tier == ComplexityTier::Reasoning && filters.prefer_reasoning_pin && !model.reasoning
}

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
    // A price that is present but not a finite number ("NaN", "inf", "1e400")
    // means the provider is telling us nothing usable about this model, so the
    // model is dropped rather than given a substitute price. Both substitutes
    // are wrong in a way that matters: coercing to 0.0 puts it in the cheapest
    // band, where `pin_choice` then picks it as the *cheapest* pin and first
    // failover target, while leaving NaN in place makes every band comparison
    // false so it falls into `Hard` and, once pinned, can never be migrated
    // away because `best <= current * (1 - ratio)` is false forever. An absent
    // price is different and stays lenient at 0.0 -- that is a genuinely free
    // model, already gated by `allow_free`.
    //
    // "Present but unreadable" has to mean the same thing whatever JSON type
    // carried it. The test only covered a *string* that parsed to a non-finite
    // number, so a JSON number, `null`, `{"usd":0.15}` or `"see pricing page"`
    // all slipped past and were recorded as $0.00/M -- which, with
    // `allow_free = true`, made a paid model the cheapest candidate in its
    // band. One extractor, so the two questions cannot disagree again.
    let price_per_token = |key: &str| -> Option<f64> {
        match pricing.get(key)? {
            Value::String(text) => text.parse::<f64>().ok(),
            Value::Number(number) => number.as_f64(),
            _ => None,
        }
    };
    // Finiteness is checked on the *per-million* figure, not the per-token one.
    // The scaling by 1e6 can itself overflow: a per-token price of -1.797e308
    // (`f64::MIN`) is perfectly finite, and `f64::MIN * 1e6` is `-inf`. Every
    // band comparison is then false while `input_price_per_m <= 0.0` is true,
    // so the model reads as *free*: banded Trivial, and pinned as the cheapest
    // thing in the catalog. Checking the raw value rather than the scaled one is
    // how a finite-looking price became the cheapest model.
    let price_per_million = |key: &str| -> Option<f64> {
        let scaled = price_per_token(key)? * 1_000_000.0;
        scaled.is_finite().then_some(scaled)
    };
    let unpriceable = |key: &str| -> bool {
        // Absent is a free model, not an unreadable one.
        pricing
            .get(key)
            .is_some_and(|_| price_per_million(key).is_none())
    };
    if unpriceable("prompt") || unpriceable("completion") {
        tracing::warn!(
            model = %id,
            "dropping catalog entry with an unreadable or non-finite price"
        );
        return None;
    }
    let price = |key: &str| -> f64 {
        // Unreachable for a present key: `unpriceable` already returned.
        price_per_million(key).unwrap_or(0.0)
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
        // The corrected ladder: Hard sits *below* Reasoning in price, so it is
        // reached first. It used to be the "above everything" fallthrough, which
        // put it above Reasoning and so below it in strength -- the inversion
        // P4b pins.
        assert_eq!(band_tier(0.65, &bands), ComplexityTier::Hard);
        assert_eq!(band_tier(1.4, &bands), ComplexityTier::Hard);
        assert_eq!(band_tier(1.5, &bands), ComplexityTier::Reasoning);
        assert_eq!(band_tier(3.0, &bands), ComplexityTier::Reasoning);
        // Above every band there is nothing stronger to promote to, so the top
        // tier absorbs the overflow rather than a weaker one.
        assert_eq!(band_tier(500.0, &bands), ComplexityTier::Reasoning);
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
        let models = vec![no_tools, small_context, image_only, model("ok/model", 1.8)];
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
        // Both models sit in the Reasoning band, which begins above
        // `hard_max` (1.4) in the corrected ladder.
        let mut reasoning_model = model("smart/model", 1.8);
        reasoning_model.reasoning = true;
        let models = vec![model("dumb/model", 1.5), reasoning_model];
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
            refresh_lock: Mutex::new(()),
            state: Mutex::new(CatalogState {
                snapshot,
                active: BTreeMap::from([(ComplexityTier::Simple, "pin/model".to_owned())]),
                failures: BTreeMap::new(),
            }),
        };
        // Below threshold: no promotion.
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "pin/model", 3),
            None
        );
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "pin/model", 3),
            None
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("pin/model".to_owned())
        );
        // Third failure crosses the threshold.
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "pin/model", 3),
            Some("alt/model".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("alt/model".to_owned())
        );
        // Success keeps the promoted model (no demotion thrash) and resets
        // the counter.
        router.report_success(ComplexityTier::Simple, "alt/model");
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("alt/model".to_owned())
        );
        // A fresh failure cycle re-promotes from the promoted model's
        // position; the list is exhausted → None.
        router.report_failure(ComplexityTier::Simple, "alt/model", 1);
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "alt/model", 1),
            None
        );
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
            refresh_lock: Mutex::new(()),
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

    /// `temp_snapshot_path` mixes a monotonic counter with a wall-clock
    /// reading so it cannot collide, unlike either alone.
    fn temp_snapshot_path(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "miser_test_catalog_{}_{seq}_{label}.json",
            std::process::id()
        ))
    }

    /// The snapshot is written atomically, so a reader only ever sees a whole
    /// file and a failed write leaves nothing behind.
    ///
    /// `std::fs::write` is `File::create` (O_TRUNC) plus `write_all`, and the
    /// state lock was already released by the time it ran, so two overlapping
    /// refreshes -- the startup task and the admin endpoint both refresh -- each
    /// truncated and wrote from offset 0, interleaving into invalid JSON. A
    /// crash or a full disk did the same. Either way the next boot read a
    /// corrupt file and silently re-derived every pin from config.
    #[test]
    fn snapshot_is_written_atomically_and_leaves_no_temp_file() {
        let path = temp_snapshot_path("atomic");
        let mut router = router_with_pins(BTreeMap::new());
        router.path = path.clone();

        let snapshot = CatalogSnapshot {
            source: "test".to_owned(),
            pins: BTreeMap::from([(
                ComplexityTier::Simple,
                TierPin {
                    model: "s/pinned".to_owned(),
                    candidates: vec!["s/pinned".to_owned(), "s/next".to_owned()],
                },
            )]),
            ..Default::default()
        };
        router.commit(&snapshot).expect("commit succeeds");

        let text = std::fs::read_to_string(&path).expect("snapshot written");
        let reloaded: CatalogSnapshot =
            serde_json::from_str(&text).expect("the file on disk is whole, not torn");
        assert_eq!(reloaded.pins[&ComplexityTier::Simple].model, "s/pinned");
        assert!(
            !temp_sibling(&path).exists(),
            "no temp file may be left at {}",
            temp_sibling(&path).display()
        );
        let _ = std::fs::remove_file(&path);
    }

    fn temp_sibling(path: &std::path::Path) -> std::path::PathBuf {
        path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy()
        ))
    }

    /// If the snapshot cannot be written, live routing must not move.
    ///
    /// The old order swapped `active`, wiped the failure counters and installed
    /// the new snapshot *before* persisting, so a failed write returned an error
    /// that the endpoint reported as `catalog_refresh/failure` with a 502 --
    /// while traffic was already being served from the new pins and the next
    /// restart silently reverted to the old ones.
    #[test]
    fn failed_commit_leaves_live_routing_untouched() {
        // A directory where the snapshot belongs: the write cannot succeed.
        let path = temp_snapshot_path("commit_fail");
        std::fs::create_dir_all(&path).unwrap();

        let mut router = router_with_pins(BTreeMap::from([(
            ComplexityTier::Simple,
            TierPin {
                model: "s/original".to_owned(),
                candidates: vec!["s/original".to_owned()],
            },
        )]));
        router.path = path.clone();

        let replacement = CatalogSnapshot {
            source: "test".to_owned(),
            pins: BTreeMap::from([(
                ComplexityTier::Simple,
                TierPin {
                    model: "s/replacement".to_owned(),
                    candidates: vec!["s/replacement".to_owned()],
                },
            )]),
            ..Default::default()
        };
        assert!(
            router.commit(&replacement).is_err(),
            "an unwritable snapshot must surface the failure"
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple),
            Some("s/original".to_owned()),
            "a failed write must not change what traffic is routed to"
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    /// A snapshot whose active model is not among its own candidates must not
    /// panic.
    ///
    /// `position` fell back to `usize::MAX` and was then incremented, which
    /// overflows -- a panic in debug, a wrap to 0 in release. Worse, the panic
    /// happens while the state mutex is held, so it poisons the lock: every
    /// later `active_model` returns `None` and catalog routing is silently dead
    /// for the life of the process, and `refresh` can never recover it.
    /// `load_or_seed` accepts a snapshot verbatim, so a hand-edited or
    /// foreign-schema file reaches this path.
    #[test]
    fn failover_with_an_unknown_active_model_does_not_panic() {
        let router = router_with_pins(BTreeMap::from([(
            ComplexityTier::Simple,
            TierPin {
                model: "s/pinned".to_owned(),
                candidates: vec!["s/pinned".to_owned(), "s/next".to_owned()],
            },
        )]));
        // Simulate a snapshot whose active model is not in its candidate list.
        {
            let mut state = router.state.lock().unwrap();
            state
                .active
                .insert(ComplexityTier::Simple, "s/ghost".to_owned());
        }
        // Must return cleanly rather than overflowing, and the router must stay
        // usable afterwards.
        let promoted = router.report_failure(ComplexityTier::Simple, "s/ghost", 1);
        assert!(
            promoted.is_none(),
            "there is no successor to an unknown active model, got {promoted:?}"
        );
        assert!(
            router.active_model(ComplexityTier::Simple).is_some(),
            "the router must remain usable"
        );
        assert!(
            router.summary_json().get("error").is_none(),
            "the state lock must not be poisoned: {}",
            router.summary_json()
        );
    }

    /// Non-finite prices must be rejected, not banded.
    ///
    /// `"NaN".parse::<f64>()` is `Ok(NaN)`, and so are `"inf"` and `"1e400"`.
    /// Every band comparison is false for NaN, so such a model fell through to
    /// `Hard` -- the most expensive band -- and became a legal failover target.
    /// If one was ever a previous pin, the hysteresis test
    /// `best.price <= current_price * (1 - ratio)` is `x <= NaN`, i.e. false
    /// forever, so the garbage model was pinned permanently.
    #[test]
    fn non_finite_prices_drop_the_model() {
        for raw in ["NaN", "nan", "inf", "-inf", "Infinity", "1e400"] {
            let payload = json!({"data": [{
                "id": "x/bad",
                "pricing": {"prompt": raw, "completion": "0"},
                "context_length": 200000
            }]});
            assert!(
                parse_catalog(&payload).is_empty(),
                "{raw} must drop the model, not give it a substitute price"
            );
        }

        // The point of dropping rather than substituting: 0.0 would make the
        // entry the *cheapest* in the catalog, so `pin_choice` would pin it and
        // make it the first failover target. A genuinely free model is still
        // admitted at 0.0 -- a different case, already gated by `allow_free`.
        let free = json!({"data": [{
            "id": "x/free",
            "pricing": {"prompt": "0", "completion": "0"},
            "context_length": 200000
        }]});
        assert_eq!(
            parse_catalog(&free).first().map(|m| m.input_price_per_m),
            Some(0.0),
            "a genuinely free model keeps its 0.0 price"
        );
    }

    /// "Present but unreadable" must mean the same thing whatever JSON type
    /// carried the price. The non-finite test only covered strings, so a JSON
    /// number, `null`, an object, or a non-numeric string were all recorded as
    /// $0.00/M -- and with `allow_free = true` that made a paid model the
    /// *cheapest* candidate in its band, ahead of a genuinely cheap one.
    #[test]
    fn an_unreadable_price_is_dropped_whatever_json_type_carried_it() {
        for (label, pricing) in [
            ("null", json!({"prompt": Value::Null, "completion": "0"})),
            (
                "object",
                json!({"prompt": {"usd": 0.15}, "completion": "0"}),
            ),
            (
                "non-numeric string",
                json!({"prompt": "see pricing page", "completion": "0"}),
            ),
            ("array", json!({"prompt": [1, 2], "completion": "0"})),
            ("boolean", json!({"prompt": true, "completion": "0"})),
        ] {
            let payload = json!({"data": [{
                "id": "x/paid",
                "pricing": pricing,
                "context_length": 200000
            }]});
            assert!(
                parse_catalog(&payload).is_empty(),
                "a price delivered as a {label} is unreadable, not free"
            );
        }
    }

    /// The positive control: a JSON *number* is just as unambiguous as a
    /// numeric string, so it must be honoured rather than dropped or zeroed.
    #[test]
    fn a_numeric_json_price_is_honoured() {
        let payload = json!({"data": [{
            "id": "x/numeric",
            "pricing": {"prompt": 0.0000015, "completion": 0.000006},
            "context_length": 200000
        }]});
        let parsed = parse_catalog(&payload);
        assert_eq!(parsed.len(), 1, "a numeric price is readable");
        assert!(
            (parsed[0].input_price_per_m - 1.5).abs() < 1e-6,
            "got {}",
            parsed[0].input_price_per_m
        );
        assert!((parsed[0].output_price_per_m - 6.0).abs() < 1e-6);
    }

    /// Hysteresis must not be able to keep a pin that cannot do the tier's job.
    /// `prefer_reasoning_pin` is enforced in `pin_choice` and in the candidate
    /// list, but the "keep the incumbent" arm compared price only -- and since
    /// hysteresis only ever migrates to a *cheaper* model, and a reasoning-capable
    /// model costs more, a non-reasoning Reasoning pin could never be replaced
    /// by any refresh. Reasoning traffic then went to a model that cannot
    /// reason, and the only escape was a three-failure failover streak.
    #[test]
    fn hysteresis_does_not_keep_a_non_reasoning_pin_for_the_reasoning_tier() {
        // The Reasoning band begins above `hard_max` (1.4) in the corrected
        // ladder, so every price here is past it.
        let mut thinker = model("pool/thinker", 2.10);
        thinker.reasoning = true;
        let models = vec![model("seeded/non-reasoning", 1.80), thinker, {
            let mut other = model("pool/thinker-b", 2.20);
            other.reasoning = true;
            other
        }];
        // An incumbent pin left over from a pool that had no reasoning model.
        let previous = BTreeMap::from([(
            ComplexityTier::Reasoning,
            TierPin {
                model: "seeded/non-reasoning".to_owned(),
                candidates: vec!["seeded/non-reasoning".to_owned()],
            },
        )]);

        let snapshot = partition(&models, &routing(), &previous);
        let pin = snapshot.pins.get(&ComplexityTier::Reasoning).unwrap();
        assert_eq!(
            pin.model, "pool/thinker",
            "an incapable incumbent must not survive a refresh"
        );
        assert!(
            !pin.candidates.is_empty() && pin.candidates.iter().all(|id| id.contains("thinker")),
            "failover targets must be reasoning-capable too: {:?}",
            pin.candidates
        );
    }

    /// The control: hysteresis still does its job on a tier with no capability
    /// requirement, so the fix above did not disable pin stability everywhere.
    /// Prices inside the `Hard` band (0.36 < p <= 1.4), which has no
    /// `prefer_reasoning_pin` constraint.
    #[test]
    fn hysteresis_still_keeps_the_incumbent_when_it_is_qualified() {
        let models = vec![model("pool/incumbent", 1.20), model("pool/cheap", 1.00)];
        let previous = BTreeMap::from([(
            ComplexityTier::Hard,
            TierPin {
                model: "pool/incumbent".to_owned(),
                candidates: vec!["pool/incumbent".to_owned()],
            },
        )]);
        let snapshot = partition(&models, &routing(), &previous);
        assert_eq!(
            snapshot
                .pins
                .get(&ComplexityTier::Hard)
                .expect("hard tier must be pinned")
                .model,
            "pool/incumbent",
            "1.00 is not 25% cheaper than 1.20, so the pin must stand"
        );
    }

    /// The Reasoning tier's failover targets must themselves be
    /// reasoning-capable.
    ///
    /// `pin_choice` returns the cheapest *reasoning-capable* model, which is
    /// generally not the cheapest model in the band -- every cheaper model is
    /// non-reasoning by construction. The candidate list was then built from
    /// the whole price-sorted pool, so `candidates[1]`, the first failover
    /// target, was guaranteed to be a non-reasoning model. The tier meant for
    /// reasoning traffic silently degraded on the first upstream failure streak.
    #[test]
    fn reasoning_tier_failover_targets_are_reasoning_capable() {
        // All three land in the Reasoning band, which in the corrected ladder
        // is `hard_max < price` (1.4 < p).
        let mut models: Vec<CatalogModel> = vec![
            model("cheap/no-reason", 1.60),
            model("mid/no-reason", 1.80),
            model("thinker", 2.20),
        ];
        for candidate in &mut models {
            if candidate.id == "thinker" {
                candidate.reasoning = true;
            }
        }
        let snapshot = partition(&models, &routing(), &BTreeMap::new());
        let pin = snapshot
            .pins
            .get(&ComplexityTier::Reasoning)
            .expect("reasoning tier must be pinned");
        assert_eq!(pin.model, "thinker", "cheapest reasoning model is pinned");
        for candidate in &pin.candidates {
            assert_eq!(
                candidate, &"thinker",
                "a non-reasoning model became a reasoning failover target: {pin:?}"
            );
        }
    }

    /// The failure counter must track the model that is actually failing, not
    /// merely the tier.
    ///
    /// `report_success` cleared the counter by tier alone, so with several
    /// requests in flight against a tier a *late* success for the
    /// pre-promotion model wiped the streak of the model promoted in its place.
    /// `concurrency_limit` is 64 by default, so such stragglers are routine: the
    /// promoted model could fail indefinitely without ever accumulating
    /// `failover_threshold` *consecutive* failures, and so would never promote
    /// to the next candidate -- the failover machinery silently stopped
    /// protecting against a model that was already broken.
    #[test]
    fn outcomes_for_a_superseded_model_are_ignored() {
        let router = router_with_pins(BTreeMap::from([(
            ComplexityTier::Simple,
            TierPin {
                model: "s/a".to_owned(),
                candidates: vec!["s/a".to_owned(), "s/b".to_owned(), "s/c".to_owned()],
            },
        )]));

        // Two failures promote a -> b.
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/a", 2),
            None
        );
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/a", 2),
            Some("s/b".to_owned())
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple).as_deref(),
            Some("s/b")
        );

        // A straggler success for `a` must not clear b's streak, so b's next
        // two failures still promote.
        router.report_success(ComplexityTier::Simple, "s/a");
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/b", 2),
            None
        );
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/b", 2),
            Some("s/c".to_owned())
        );
    }

    /// A straggler *failure* for a superseded model must not be charged to its
    /// replacement, which is a different model with a different track record.
    #[test]
    fn failures_for_a_superseded_model_do_not_advance_the_replacement() {
        let router = router_with_pins(BTreeMap::from([(
            ComplexityTier::Simple,
            TierPin {
                model: "s/a".to_owned(),
                candidates: vec!["s/a".to_owned(), "s/b".to_owned()],
            },
        )]));

        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/a", 1),
            Some("s/b".to_owned())
        );
        // Two stragglers from `a` land after the promotion. With a threshold of
        // 2, counting them would promote b off the end of the list; they must
        // be dropped instead.
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/a", 2),
            None
        );
        assert_eq!(
            router.report_failure(ComplexityTier::Simple, "s/a", 2),
            None
        );
        assert_eq!(
            router.active_model(ComplexityTier::Simple).as_deref(),
            Some("s/b"),
            "b must not be promoted by failures that belonged to a"
        );
    }

    /// `refresh` must actually hold `refresh_lock` while it writes.
    ///
    /// The previous version of this test took the lock on the *test's own*
    /// thread and then called `commit` directly, so it exercised a file with
    /// exactly one writer and passed unchanged if the guard in `refresh` were
    /// deleted -- the same defect as the tautological catalog test fixed in
    /// 8d4adfd. This drives `refresh` itself, from a second thread that must
    /// park while the lock is held elsewhere.
    #[test]
    fn refresh_blocks_while_the_refresh_lock_is_held() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener as StdListener;
        use std::sync::atomic::{AtomicBool, Ordering};

        // A blocking loopback `/models` server, so no runtime is needed for the
        // mock side and only the refresher has to be async.
        let body = r#"{"data":[{"id":"s/only","pricing":{"prompt":"0.001","completion":"0.002"},"context_length":200000}]}"#;
        let listener = StdListener::bind("127.0.0.1:0").expect("bind mock /models");
        let addr = listener.local_addr().expect("mock addr");
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let provider = miser_provider::Provider::new(miser_provider::ProviderConfig {
            base_url: format!("http://{addr}"),
            api_key: Some("test".to_owned()),
            ..Default::default()
        })
        .expect("provider builds");

        let path = temp_snapshot_path("refresh_lock");
        let mut base = router_with_pins(BTreeMap::new());
        base.path = path.clone();
        let router = std::sync::Arc::new(base);

        // Hold the lock on another thread long enough to observe the refresher
        // parking on it.
        let held = std::sync::Arc::clone(&router);
        let holder = std::thread::spawn(move || {
            let _guard = held.refresh_lock.lock().expect("refresh lock");
            std::thread::sleep(std::time::Duration::from_millis(500));
        });

        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let refresher = {
            let router = std::sync::Arc::clone(&router);
            let finished = std::sync::Arc::clone(&finished);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                let _ = rt.block_on(router.refresh(&provider, &routing()));
                finished.store(true, Ordering::SeqCst);
            })
        };

        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            !finished.load(Ordering::SeqCst),
            "refresh must not complete while another caller holds the refresh lock"
        );

        holder.join().expect("lock holder");
        refresher.join().expect("refresher");
        assert!(
            finished.load(Ordering::SeqCst),
            "refresh must complete once the lock is released"
        );
        drop(server);
        let _ = std::fs::remove_file(&path);
    }

    /// A 2xx catalog response carrying no usable models must be rejected, with
    /// the previous pins left in force and nothing written to disk.
    ///
    /// `parse_catalog` yields no models for a body with no `data` array, and
    /// `partition` would then produce a snapshot with no pins and an empty
    /// model->tier split -- destroying the state the module's stability contract
    /// says survives until a real refresh, and clearing every tier's failure
    /// streak. Previously the guard was a `parse_catalog` sanity check that
    /// never touched `refresh`, so this behaviour had no coverage at all.
    #[tokio::test]
    async fn empty_catalog_payload_is_rejected_and_leaves_the_pins_alone() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let body = r#"{"object":"list"}"#;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes()).await;
            }
        });

        let path = temp_snapshot_path("empty_payload");
        let previous = CatalogSnapshot {
            source: "test".to_owned(),
            pins: BTreeMap::from([(
                ComplexityTier::Simple,
                TierPin {
                    model: "s/keep-me".to_owned(),
                    candidates: vec!["s/keep-me".to_owned()],
                },
            )]),
            model_tiers: BTreeMap::from([("s/keep-me".to_owned(), ComplexityTier::Simple)]),
            ..Default::default()
        };
        let router = CatalogRouter {
            path: path.clone(),
            enabled: true,
            refresh_lock: Mutex::new(()),
            state: Mutex::new(CatalogState {
                active: BTreeMap::from([(ComplexityTier::Simple, "s/keep-me".to_owned())]),
                failures: BTreeMap::from([(ComplexityTier::Simple, 2)]),
                snapshot: previous.clone(),
            }),
        };
        let provider = miser_provider::Provider::new(miser_provider::ProviderConfig {
            base_url: format!("http://{addr}"),
            api_key: Some("test".to_owned()),
            ..Default::default()
        })
        .expect("provider builds");

        let error = router
            .refresh(&provider, &routing())
            .await
            .expect_err("a catalog with no models must be refused");
        assert!(
            error.contains("no models"),
            "the error should say why: {error}"
        );

        // The pins stay in force, the failure streak is not wiped, and nothing
        // reached the disk.
        assert_eq!(
            router.active_model(ComplexityTier::Simple).as_deref(),
            Some("s/keep-me"),
            "the previous pin must survive a refused refresh"
        );
        let state = router.state.lock().unwrap();
        assert_eq!(
            state.failures.get(&ComplexityTier::Simple),
            Some(&2),
            "a refused refresh must not clear the failure counters"
        );
        assert_eq!(state.snapshot, previous, "in-memory snapshot is unchanged");
        drop(state);
        assert!(
            !path.exists(),
            "a refused refresh must not write a snapshot at all"
        );
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

    /// `MISER_CATALOG_FILE` is documented as an override of `cache_path`, so it
    /// has to win. It used to be consulted only as a fallback, which made it
    /// inert in the shipped deployment -- `config/miser.toml` sets
    /// `cache_path = "catalog/models.json"`, so an operator following the docs
    /// would set the variable and see no effect at all.
    #[test]
    fn the_catalog_file_env_var_overrides_the_configured_cache_path() {
        // Serialised: `set_var` is process-global, and the test binary is
        // multi-threaded.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let routing = RoutingConfig {
            cache_path: Some("catalog/models.json".into()),
            ..Default::default()
        };

        // Safety: guarded by the mutex above, and nothing else in this test
        // binary reads the variable.
        unsafe { std::env::set_var("MISER_CATALOG_FILE", "/tmp/from-env.json") };
        assert_eq!(
            CatalogRouter::snapshot_path(&routing),
            PathBuf::from("/tmp/from-env.json"),
            "the environment variable must win over cache_path"
        );

        // An empty value is not an override; fall through to the config.
        unsafe { std::env::set_var("MISER_CATALOG_FILE", "") };
        assert_eq!(
            CatalogRouter::snapshot_path(&routing),
            PathBuf::from("catalog/models.json")
        );

        unsafe { std::env::remove_var("MISER_CATALOG_FILE") };
        assert_eq!(
            CatalogRouter::snapshot_path(&routing),
            PathBuf::from("catalog/models.json"),
            "with no variable, cache_path is used"
        );
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
        assert_eq!(
            router.report_failure(ComplexityTier::Trivial, "pin/model", 3),
            None
        );
        router.report_success(ComplexityTier::Trivial, "pin/model");
        // The counter restarted, so one fresh failure must not promote.
        assert_eq!(
            router.report_failure(ComplexityTier::Trivial, "pin/model", 3),
            None
        );
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
            router.report_failure(ComplexityTier::Simple, "pin/model", 0),
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
        assert_eq!(band_tier(bands.hard_max, &bands), ComplexityTier::Hard);
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
            ComplexityTier::Hard
        );
        assert_eq!(
            band_tier(bands.hard_max * 1.01, &bands),
            ComplexityTier::Reasoning
        );
        // Past the top band the strongest tier absorbs the overflow, so the
        // ladder stays monotone rather than wrapping back down to a weaker tier.
        assert_eq!(
            band_tier(bands.reasoning_max * 1.01, &bands),
            ComplexityTier::Reasoning
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
        let good = model("good/model", 1.8);
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
