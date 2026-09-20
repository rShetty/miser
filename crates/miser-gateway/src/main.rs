mod auth;
mod cache;
mod catalog;
mod judge;
mod metrics;
mod semantic_cache;
mod session;
mod usage;
mod validate;

use axum::{
    Json, Router,
    error_handling::HandleErrorLayer,
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use clap::Parser;
use miser_classifier::Classifier;
use miser_policy::PolicyEngine;
use miser_policy::quality::{QualityScore, deterministic_quality};
use miser_provider::{Provider, ProviderConfig, safe_response_headers};
use miser_types::{ChatCompletionRequest, ComplexityTier, GatewayConfig, TierModelRouteConfig};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tower::{ServiceBuilder, limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use uuid::Uuid;
use validate::validate_config;

/// Default cap on in-flight requests when `concurrency_limit` is absent
/// from the config.
const DEFAULT_CONCURRENCY_LIMIT: usize = 64;
/// Default per-request timeout when `request_timeout_ms` is absent from
/// the config.
///
/// Regression guard: 30s 408'd legitimate non-streaming completions.
/// Measured hard-tier generations ran 8-30s upstream alone, before
/// classification and the quality judge add their latency, so real
/// traffic routinely crossed 30s and died with
/// `{"error":{"message":"request timed out"}}`. LLM latencies justify
/// minutes, not seconds; operators can still lower it per deployment.
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 300_000;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "config/miser.toml")]
    config: String,
}

#[derive(Clone)]
struct AppState {
    config: GatewayConfig,
    classifier: Arc<Classifier>,
    policy: PolicyEngine,
    provider: Provider,
    cache: Arc<cache::ResponseCache>,
    session: Arc<session::SessionTracker>,
    auth: Arc<auth::AuthManager>,
    quotas: Arc<auth::QuotaEnforcer>,
    usage: Arc<usage::UsageLedger>,
    metrics: Arc<metrics::Metrics>,
    audit: Arc<auth::AuditLog>,
    catalog: Arc<catalog::CatalogRouter>,
    semantic_cache: Arc<semantic_cache::SemanticCache>,
    quality_judge: Option<judge::QualityJudge>,
    admin_key: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let json_logs = std::env::var("RUST_LOG_FORMAT").as_deref() == Ok("json");
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env());
    if json_logs {
        subscriber.json().init();
    } else {
        subscriber.init();
    }
    let args = Args::parse();
    let config: GatewayConfig = toml::from_str(&tokio::fs::read_to_string(&args.config).await?)?;
    validate_config(&config).map_err(|error| anyhow::anyhow!("{error}"))?;
    let api_key = std::env::var(
        config
            .provider
            .extra
            .get("api_key_env")
            .and_then(Value::as_str)
            .unwrap_or("OPENROUTER_API_KEY"),
    )
    .unwrap_or_else(|_| config.provider.api_key.clone());
    if api_key.is_empty() {
        anyhow::bail!("provider API key is required");
    }
    let mut provider_config = ProviderConfig {
        base_url: config.provider.base_url.clone(),
        api_key: Some(api_key),
        ..Default::default()
    };
    provider_config.provider_preferences = config
        .provider
        .provider_preferences
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;
    let admin_key = std::env::var("MISER_ADMIN_KEY").unwrap_or_else(|_| {
        config
            .extra
            .get("admin_key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });
    let auth_path =
        std::env::var("MISER_KEYS_FILE").unwrap_or_else(|_| "/etc/miser/keys.json".to_string());
    let mut classifier_config = config.classifier.clone();
    if classifier_config
        .jev
        .api_key
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        if let Ok(jev_key) = std::env::var("JEV_API_KEY") {
            if !jev_key.is_empty() {
                classifier_config.jev.api_key = Some(jev_key);
            }
        }
    }
    if classifier_config.mode == miser_types::ClassifierMode::Jev
        && classifier_config
            .jev
            .api_key
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    {
        tracing::warn!(
            "classifier.mode is 'jev' but no JEV_API_KEY is configured; \
             every classification will fall back to the heuristic until a key is set"
        );
    }
    // The quality judge shares the Jev evaluation endpoint; when the
    // quality config has no key of its own, fall back to the classifier's
    // (env-resolved) key rather than silently disabling the judge.
    let quality_judge = config
        .quality
        .judge
        .as_ref()
        .filter(|judge| judge.enabled && !judge.base_url.is_empty() && !judge.model.is_empty())
        .map(|judge| {
            let api_key = judge
                .api_key
                .clone()
                .filter(|key| !key.is_empty())
                .or_else(|| classifier_config.jev.api_key.clone())
                .unwrap_or_default();
            judge::QualityJudge::new(judge, api_key)
        });
    let state = AppState {
        classifier: Arc::new(Classifier::new(classifier_config)?),
        policy: PolicyEngine::new(config.clone()),
        provider: Provider::new(provider_config)?,
        cache: Arc::new(cache::ResponseCache::new(10000, 300)),
        session: Arc::new(session::SessionTracker::new(
            config.session.max_entries,
            config.session.ttl_seconds,
        )),
        auth: Arc::new(auth::AuthManager::new(std::path::PathBuf::from(auth_path))),
        quotas: Arc::new(auth::QuotaEnforcer::new()),
        usage: Arc::new(usage::UsageLedger::new(std::path::PathBuf::from(
            std::env::var("MISER_USAGE_FILE")
                .unwrap_or_else(|_| "/var/lib/miser/usage.jsonl".to_string()),
        ))),
        metrics: Arc::new(metrics::Metrics::new()?),
        audit: Arc::new(auth::AuditLog::new(std::path::PathBuf::from(
            std::env::var("MISER_AUDIT_FILE")
                .unwrap_or_else(|_| "/var/lib/miser/audit.jsonl".to_string()),
        ))),
        catalog: Arc::new(catalog::CatalogRouter::load_or_seed(&config)),
        semantic_cache: Arc::new(semantic_cache::SemanticCache::new(
            config.cache.max_entries,
            300,
            config.cache.semantic_candidate_threshold,
        )),
        quality_judge,
        admin_key,
        config,
    };
    if state.catalog.enabled() && state.config.routing.refresh_on_start {
        let refresh_state = state.clone();
        tokio::spawn(async move {
            match refresh_state
                .catalog
                .refresh(&refresh_state.provider, &refresh_state.config.routing)
                .await
            {
                Ok(_) => tracing::info!("catalog refreshed at startup"),
                Err(error) => tracing::warn!(
                    error = %error,
                    "startup catalog refresh failed; persisted or seeded pins in use"
                ),
            }
        });
    }
    let address = format!("{}:{}", state.config.host, state.config.port);
    let app = build_router(state);
    let listener = TcpListener::bind(&address).await?;
    tracing::info!(address = %address, "miser gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("miser gateway shutdown complete");
    Ok(())
}

/// Resolves when SIGTERM or SIGINT is received so `with_graceful_shutdown`
/// can stop accepting connections and drain in-flight requests.
#[cfg(unix)]
async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("failed to install SIGINT handler");
    wait_for_shutdown_signal(&mut terminate, &mut interrupt).await;
}

#[cfg(unix)]
async fn wait_for_shutdown_signal(
    terminate: &mut tokio::signal::unix::Signal,
    interrupt: &mut tokio::signal::unix::Signal,
) {
    tokio::select! {
        _ = terminate.recv() => {
            tracing::info!(signal = "SIGTERM", "shutdown signal received, draining in-flight requests");
        }
        _ = interrupt.recv() => {
            tracing::info!(signal = "SIGINT", "shutdown signal received, draining in-flight requests");
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!(
        signal = "SIGINT",
        "shutdown signal received, draining in-flight requests"
    );
}

impl AppState {
    /// Feeds the upstream outcome back into the catalog router. Transport
    /// errors (`None`) and 5xx/429 responses count as provider failures;
    /// client errors (4xx) are the caller's fault and never trigger
    /// failover; 2xx resets the failure counter. A promoted failover model
    /// is sticky until restart or the next catalog refresh.
    fn report_upstream_outcome(
        &self,
        tier: ComplexityTier,
        status: Option<axum::http::StatusCode>,
    ) {
        if !self.catalog.enabled() {
            return;
        }
        if matches!(status, Some(status) if status.is_success()) {
            self.catalog.report_success(tier);
            return;
        }
        let is_provider_failure = match status {
            None => true,
            Some(status) => status.is_server_error() || status.as_u16() == 429,
        };
        if !is_provider_failure {
            return;
        }
        let threshold = self.config.routing.failover_threshold;
        if let Some(new_model) = self.catalog.report_failure(tier, threshold) {
            self.metrics.catalog_failovers_total.inc();
            tracing::warn!(
                tier = %format_tier(tier),
                new_model = %new_model,
                threshold,
                "upstream provider failures exceeded threshold; failing over to next catalog candidate"
            );
        }
    }

    /// Resolve the acting admin identity for audit records: the admin key
    /// id when the caller presented a valid key, else the shared admin key
    /// fingerprint.
    fn admin_actor(&self, headers: &axum::http::HeaderMap) -> String {
        if let Some(bearer) = auth::extract_bearer(headers) {
            if let Ok(key) = self.auth.validate(&bearer) {
                return format!("key:{}", key.id);
            }
            if auth::admin_auth(headers, &self.admin_key) && !self.admin_key.is_empty() {
                return "admin-key".to_string();
            }
        }
        "unknown".to_string()
    }
}

fn build_router(state: AppState) -> Router {
    let concurrency_limit = state
        .config
        .extra
        .get("concurrency_limit")
        .and_then(Value::as_u64)
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_CONCURRENCY_LIMIT as u64) as usize;
    let request_timeout = Duration::from_millis(
        state
            .config
            .extra
            .get("request_timeout_ms")
            .and_then(Value::as_u64)
            .filter(|timeout| *timeout > 0)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
    );
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics_endpoint))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(completions))
        .route("/admin/keys", post(create_key))
        .route("/admin/keys", get(list_keys))
        .route("/admin/keys/{id}", get(get_key))
        .route("/admin/keys/{id}", patch(update_key))
        .route("/admin/audit/verify", get(verify_audit))
        .route("/admin/keys/{id}", delete(delete_key))
        .route("/admin/keys/{id}/rotate", post(rotate_key))
        .route("/admin/usage/summary", get(usage_summary))
        .route("/admin/usage/keys/{id}", get(usage_key_detail))
        .route("/admin/usage/clients", get(usage_clients))
        .route("/admin/catalog", get(catalog_status))
        .route("/admin/catalog/refresh", post(refresh_catalog))
        .with_state(Arc::new(state))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(ConcurrencyLimitLayer::new(concurrency_limit))
        // The timeout sits outside the concurrency limit, so the deadline
        // covers both waiting for a permit and handling the request.
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: axum::BoxError| async {
                    handle_layer_error(error)
                }))
                .layer(TimeoutLayer::new(request_timeout)),
        )
}

/// Turns tower layer errors (per-request timeouts) into JSON responses.
fn handle_layer_error(error: axum::BoxError) -> (StatusCode, Json<Value>) {
    if error.is::<tower::timeout::error::Elapsed>() {
        auth::json_error("request timed out", StatusCode::REQUEST_TIMEOUT)
    } else {
        tracing::error!(error = %error, "request failed in middleware stack");
        auth::json_error("internal server error", StatusCode::INTERNAL_SERVER_ERROR)
    }
}

async fn live() -> Json<Value> {
    Json(json!({"status":"ok","service":"miser"}))
}

async fn ready(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"status":"ready","routes":state.config.tiers.len()}))
}

async fn models(State(state): State<Arc<AppState>>) -> Json<Value> {
    let models = state
        .provider
        .list_models()
        .await
        .unwrap_or_else(|_| json!({"data":[]}));
    Json(models)
}

/// Serves all gateway metrics in the Prometheus text exposition format.
async fn metrics_endpoint(State(state): State<Arc<AppState>>) -> Response {
    match state.metrics.render() {
        Ok(body) => (
            [(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to encode metrics");
            (StatusCode::INTERNAL_SERVER_ERROR, "metrics encoding failed").into_response()
        }
    }
}

/// Wraps the completions handler so every outcome — success or typed
/// error — is counted by route/status and observed in the latency
/// histogram.
async fn completions(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    request: Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let start = Instant::now();
    let result = completions_inner(State(state.clone()), headers, request).await;
    let status = match &result {
        Ok(response) => response.status(),
        Err((status, _)) => *status,
    };
    state
        .metrics
        .requests_total
        .with_label_values(&[metrics::COMPLETIONS_ROUTE, &status.as_u16().to_string()])
        .inc();
    state
        .metrics
        .request_duration_seconds
        .with_label_values(&[metrics::COMPLETIONS_ROUTE])
        .observe(start.elapsed().as_secs_f64());
    result
}

async fn completions_inner(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let started = Instant::now();
    let bearer = auth::extract_bearer(&headers).unwrap_or_default();
    let mut authenticated_key: Option<auth::ApiKey> = None;
    if state.admin_key.is_empty() && state.auth.list_keys().map(|k| k.is_empty()).unwrap_or(true) {
        // No auth configured — open access for initial setup
    } else {
        match state.auth.validate(&bearer) {
            Ok(api_key) => authenticated_key = Some(api_key),
            Err(auth::AuthError::Inactive) => {
                return Err(auth::json_error("API key inactive", StatusCode::FORBIDDEN));
            }
            Err(auth::AuthError::Expired) => {
                return Err(auth::json_error("API key expired", StatusCode::FORBIDDEN));
            }
            Err(_) => {
                if !auth::admin_auth(&headers, &state.admin_key) {
                    return Err(auth::json_error(
                        "invalid API key",
                        StatusCode::UNAUTHORIZED,
                    ));
                }
            }
        }
    }
    if let Some(key) = &authenticated_key {
        if let Some(rpm) = key.rate_limit_rpm {
            if !state.quotas.check_rate_limit(&key.id, rpm) {
                return Err(auth::json_error(
                    "rate limit exceeded for this API key",
                    StatusCode::TOO_MANY_REQUESTS,
                ));
            }
        }
        if let Some(cap) = key.monthly_budget_usd {
            if !state.quotas.check_budget(&key.id, cap) {
                return Err(auth::json_error(
                    "monthly budget exhausted for this API key",
                    StatusCode::PAYMENT_REQUIRED,
                ));
            }
        }
    }
    let request_id = Uuid::new_v4().to_string();
    // Capture the client's requested model before routing overwrites
    // request.model; the usage ledger records both.
    let requested_model = request.model.clone();
    let stream_requested = request.stream.unwrap_or(false);
    let body = serde_json::to_value(&request).map_err(internal)?;
    let cache_key = cache::request_hash(&body);
    if let Some((cached_body, cached_status, cached_headers)) = state.cache.get(cache_key) {
        state.metrics.cache_hits_total.inc();
        record_usage(
            &state,
            authenticated_key.as_ref(),
            &request.model,
            &requested_model,
            "-",
            0,
            0,
            started.elapsed(),
            true,
            cached_status.as_u16(),
            &request_id,
        );
        let mut response = Response::builder().status(cached_status);
        for (name, value) in &cached_headers {
            response = response.header(name, value);
        }
        return response
            .header(
                "x-miser-request-id",
                HeaderValue::from_str(&request_id).unwrap(),
            )
            .header("x-miser-cache", HeaderValue::from_static("hit-exact"))
            .body(axum::body::Body::from(cached_body))
            .map_err(internal);
    }
    // Semantic cache: embedding retrieval flags a candidate, the Jev
    // equivalence judge decides whether the cached response actually
    // answers this request. Two-stage, so bag-of-words similarity alone
    // never serves an answer. Structured-output requests are excluded:
    // a cached response built for a different response_format contract
    // could break the client's parser.
    if state.config.cache.enabled
        && state.config.cache.semantic_enabled
        && request.response_format.is_none()
        && !stream_requested
    {
        let embedding_text = semantic_cache::request_text_for_embedding(&body);
        let candidate = state
            .semantic_cache
            .lookup(&semantic_cache::embed_prompt(&embedding_text));
        if let Some(hit) = candidate {
            let new_prompt = judge::last_user_text(&request);
            let validated = match state.quality_judge.as_ref() {
                Some(judge) => judge.equivalent(&new_prompt, &hit.prompt_text).await == Some(true),
                // No judge configured: fall back to near-duplicate text
                // only, mirroring the exact-match safety bar.
                None => hit.similarity >= state.config.cache.similarity_threshold,
            };
            if validated {
                state.metrics.semantic_hits_total.inc();
                record_usage(
                    &state,
                    authenticated_key.as_ref(),
                    &request.model,
                    &requested_model,
                    "-",
                    0,
                    0,
                    started.elapsed(),
                    true,
                    hit.status.as_u16(),
                    &request_id,
                );
                let mut response = Response::builder().status(hit.status);
                for (name, value) in &hit.headers {
                    response = response.header(name, value);
                }
                return response
                    .header(
                        "x-miser-request-id",
                        HeaderValue::from_str(&request_id).unwrap(),
                    )
                    .header("x-miser-cache", HeaderValue::from_static("hit-semantic"))
                    .header(
                        "x-miser-semantic-similarity",
                        HeaderValue::from_str(&format!("{:.3}", hit.similarity)).unwrap(),
                    )
                    .body(axum::body::Body::from(hit.body))
                    .map_err(internal);
            }
        }
    }
    state.metrics.cache_misses_total.inc();
    let mut classification = state
        .classifier
        .classify(&request)
        .await
        .map_err(internal)?;
    if state.config.session.enabled {
        if let Some(key) = session::session_key(&request) {
            if let Some(session_tier) = state.session.get(&key) {
                if session_tier > classification.tier {
                    classification.tier = session_tier;
                    classification.reasons.push("session-continuity".into());
                }
            }
        }
    }
    let mut route = state
        .policy
        .select(&request, &classification)
        .map_err(internal)?;
    let effective_tier = state.policy.effective_tier(&request, &classification);
    // Catalog routing: swap the tier route's model for the tier's pinned
    // (or failover-promoted) model. Params (max_tokens, temperature) stay
    // from the config route; only the model id is catalog-driven.
    if state.catalog.enabled() {
        if let Some(model) = state.catalog.active_model(effective_tier) {
            route.model = model;
        }
    }
    state
        .metrics
        .tier_requests_total
        .with_label_values(&[&format_tier(effective_tier)])
        .inc();
    if effective_tier > classification.tier {
        state.metrics.quality_escalations_total.inc();
    }
    // Per-key tier gating: an empty allowlist means all tiers are allowed.
    if let Some(key) = &authenticated_key {
        if !key.allowed_tiers.is_empty() {
            let tier_name = format_tier(effective_tier);
            if !key
                .allowed_tiers
                .iter()
                .any(|t| t.eq_ignore_ascii_case(&tier_name))
            {
                return Err(auth::json_error(
                    format!("tier '{tier_name}' is not allowed for this API key").as_str(),
                    StatusCode::FORBIDDEN,
                ));
            }
        }
    }
    if state.config.session.enabled {
        if let Some(key) = session::session_key(&request) {
            state.session.update(&key, effective_tier);
        }
    }
    request.model = route.model.clone();
    if request.max_tokens.is_none() {
        if let Some(max_tokens) = route.max_tokens {
            request.max_tokens = Some(max_tokens);
        }
    }
    if request.temperature.is_none() {
        if let Some(temperature) = route.temperature {
            request.temperature = Some(temperature);
        }
    }
    let body = serde_json::to_value(&request).map_err(internal)?;
    let upstream = match state.provider.forward(body.clone(), None).await {
        Ok(upstream) => upstream,
        Err(error) => {
            state.metrics.upstream_errors_total.inc();
            state.report_upstream_outcome(effective_tier, None);
            return Err(internal(error));
        }
    };
    let upstream_status = upstream.status();
    if !upstream_status.is_success() {
        state.metrics.upstream_errors_total.inc();
    }
    state.report_upstream_outcome(effective_tier, Some(upstream_status));
    let selected_route = route.clone();
    if !stream_requested && upstream.status().is_success() {
        // Record estimated spend for per-key budget enforcement when the
        // operator configured a blended price (USD per 1k tokens).
        if let Some(key) = &authenticated_key {
            if let Some(price) = state
                .config
                .extra
                .get("price_per_1k_usd")
                .and_then(Value::as_f64)
            {
                // Usage is parsed from the buffered payload below; a cheap
                // estimate from max_tokens keeps accounting monotonic even
                // when usage fields are absent.
                let est_tokens = request.max_tokens.unwrap_or(512) as f64;
                state
                    .quotas
                    .record_spend(&key.id, est_tokens / 1000.0 * price);
            }
        }
        let original_status = upstream.status();
        let original_headers = safe_response_headers(upstream.headers());
        let payload = upstream.bytes().await.map_err(internal)?;
        // Quality gate: deterministic checks first, then the Jev judge
        // when configured ("Jev decides"), then one bounded escalation to
        // the next tier when the score is below threshold. The better of
        // the two responses is returned and cached.
        let mut payload = payload;
        let mut selected_route = selected_route;
        let mut effective_tier = effective_tier;
        let mut escalated = false;
        if state.config.quality.enabled {
            let parsed = serde_json::from_slice::<Value>(&payload).ok();
            let response_text = parsed
                .as_ref()
                .and_then(|body| body["choices"][0]["message"]["content"].as_str())
                .unwrap_or_default()
                .to_owned();
            let mut check = deterministic_quality(
                &request,
                &parsed.unwrap_or(Value::Null),
                &classification,
                &state.config.quality,
            );
            if let Some(judge) = state.quality_judge.as_ref() {
                if let Some(score) = judge
                    .score(&judge::last_user_text(&request), &response_text)
                    .await
                {
                    check = QualityScore {
                        score,
                        passed: score >= state.config.quality.minimum_score,
                        reason: "jev-judge",
                    };
                }
            }
            if !check.passed && state.config.quality.escalate_on_failure {
                if let Some(escalated_tier) = state.policy.escalated_tier(&request, &classification)
                {
                    // Tier gating mirrors the pre-flight gate: never hand a
                    // key a tier it is not allowed to see.
                    let tier_allowed = authenticated_key
                        .as_ref()
                        .map(|key| {
                            key.allowed_tiers.is_empty()
                                || key
                                    .allowed_tiers
                                    .iter()
                                    .any(|t| t.eq_ignore_ascii_case(&format_tier(escalated_tier)))
                        })
                        .unwrap_or(true);
                    if tier_allowed {
                        if let Some(next_route) =
                            state.policy.next(&request, &classification).ok().flatten()
                        {
                            let escalated_model = state
                                .catalog
                                .active_model(escalated_tier)
                                .unwrap_or_else(|| next_route.model.clone());
                            let mut escalated_body =
                                serde_json::to_value(&request).map_err(internal)?;
                            escalated_body["model"] = Value::String(escalated_model.clone());
                            if let Ok(escalated_upstream) =
                                state.provider.forward(escalated_body, None).await
                            {
                                if escalated_upstream.status().is_success() {
                                    if let Ok(escalated_payload) = escalated_upstream.bytes().await
                                    {
                                        let escalated_score = if let Some(judge) =
                                            state.quality_judge.as_ref()
                                        {
                                            let escalated_text =
                                                serde_json::from_slice::<Value>(&escalated_payload)
                                                    .ok()
                                                    .and_then(|body| {
                                                        body["choices"][0]["message"]["content"]
                                                            .as_str()
                                                            .map(str::to_owned)
                                                    })
                                                    .unwrap_or_default();
                                            judge
                                                .score(
                                                    &judge::last_user_text(&request),
                                                    &escalated_text,
                                                )
                                                .await
                                        } else {
                                            None
                                        };
                                        // Keep the escalated response unless the
                                        // judge scored it strictly worse.
                                        if escalated_score
                                            .map(|score| score >= check.score)
                                            .unwrap_or(true)
                                        {
                                            payload = escalated_payload;
                                            selected_route = TierModelRouteConfig {
                                                model: escalated_model,
                                                ..next_route
                                            };
                                            effective_tier = escalated_tier;
                                            escalated = true;
                                            state.metrics.quality_escalations_total.inc();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if escalated {
                // The escalated response carries the usage, so the
                // deterministic/empty markers from the first attempt must
                // not leak into the returned headers below.
                tracing::debug!(
                    request_id = %request_id,
                    tier = %format_tier(effective_tier),
                    score = %check.score,
                    "quality gate escalated response"
                );
            }
        }
        // Real usage from the provider response, attributed to the key.
        let usage = serde_json::from_slice::<Value>(&payload)
            .ok()
            .and_then(|body| body.get("usage").cloned())
            .unwrap_or(Value::Null);
        record_usage(
            &state,
            authenticated_key.as_ref(),
            &selected_route.model,
            &requested_model,
            &format_tier(effective_tier),
            usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            started.elapsed(),
            false,
            original_status.as_u16(),
            &request_id,
        );
        state.cache.store(
            cache_key,
            payload.clone(),
            original_status,
            original_headers.clone(),
        );
        // Semantic cache gets the final (quality-gated, possibly escalated)
        // response so near-duplicate requests reuse the best answer.
        if state.config.cache.enabled && state.config.cache.semantic_enabled {
            let embedding_text = semantic_cache::request_text_for_embedding(&body);
            state.semantic_cache.store(
                semantic_cache::embed_prompt(&embedding_text),
                embedding_text,
                payload.clone(),
                original_status,
                original_headers.clone(),
            );
        }
        let mut response = Response::builder().status(original_status);
        for (name, value) in &original_headers {
            response = response.header(name, value);
        }
        return response
            .header(
                "x-miser-request-id",
                HeaderValue::from_str(&request_id).unwrap(),
            )
            .header("x-miser-cache", HeaderValue::from_static("miss"))
            .header(
                "x-miser-escalated",
                HeaderValue::from_static(if escalated { "true" } else { "false" }),
            )
            .header(
                "x-miser-tier",
                HeaderValue::from_str(&format_tier(effective_tier)).unwrap(),
            )
            .header(
                "x-miser-model",
                HeaderValue::from_str(&selected_route.model).unwrap(),
            )
            .header(
                "x-miser-classifier",
                HeaderValue::from_str(&classification.classifier).unwrap(),
            )
            .header(
                "x-miser-confidence",
                HeaderValue::from_str(&classification.confidence.to_string()).unwrap(),
            )
            .body(axum::body::Body::from(payload))
            .map_err(internal);
    }
    let status = upstream.status();
    let safe_headers = safe_response_headers(upstream.headers());
    let stream = upstream.bytes_stream();
    // Streaming: token counts arrive inside the stream, so record the same
    // monotonic estimate the budget enforcer uses.
    if authenticated_key.is_some() {
        record_usage(
            &state,
            authenticated_key.as_ref(),
            &selected_route.model,
            &requested_model,
            &format_tier(effective_tier),
            0,
            request.max_tokens.unwrap_or(512) as u64,
            started.elapsed(),
            false,
            status.as_u16(),
            &request_id,
        );
    }
    let mut response = Response::builder().status(status);
    for (name, value) in &safe_headers {
        response = response.header(name, value);
    }
    response = response
        .header(
            "x-miser-request-id",
            HeaderValue::from_str(&request_id).unwrap(),
        )
        .header("x-miser-cache", HeaderValue::from_static("miss"))
        .header(
            "x-miser-tier",
            HeaderValue::from_str(&format_tier(effective_tier)).unwrap(),
        )
        .header(
            "x-miser-model",
            HeaderValue::from_str(&selected_route.model).unwrap(),
        )
        .header(
            "x-miser-classifier",
            HeaderValue::from_str(&classification.classifier).unwrap(),
        )
        .header(
            "x-miser-confidence",
            HeaderValue::from_str(&classification.confidence.to_string()).unwrap(),
        );
    response
        .body(axum::body::Body::from_stream(stream))
        .map_err(internal)
}

fn format_tier(tier: ComplexityTier) -> String {
    serde_json::to_string(&tier)
        .unwrap()
        .trim_matches('"')
        .to_owned()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Attribute one settled request to its key in the usage ledger. Cost uses
/// the same blended `price_per_1k_usd` the budget enforcer applies. Calls
/// without an authenticated key (admin/anonymous setup) are not recorded.
#[allow(clippy::too_many_arguments)]
fn record_usage(
    state: &AppState,
    key: Option<&auth::ApiKey>,
    model: &str,
    requested_model: &str,
    tier: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    latency: Duration,
    cached: bool,
    status: u16,
    request_id: &str,
) {
    let Some(key) = key else { return };
    let total_tokens = (prompt_tokens + completion_tokens) as f64;
    let cost_usd = state
        .config
        .extra
        .get("price_per_1k_usd")
        .and_then(Value::as_f64)
        .map(|price| total_tokens / 1000.0 * price)
        .unwrap_or(0.0);
    state.usage.record(&usage::UsageRecord {
        ts: unix_now(),
        key_id: key.id.clone(),
        client: key.client.clone(),
        model: model.to_string(),
        requested_model: requested_model.to_string(),
        tier: tier.to_string(),
        prompt_tokens,
        completion_tokens,
        cost_usd,
        latency_ms: latency.as_millis() as u64,
        cached,
        status,
        request_id: request_id.to_string(),
    });
}

/// Parse a reporting window query value ("24h" | "7d" | "30d" | "all")
/// into a lower-bound unix timestamp; `None` means no lower bound.
fn window_since(window: Option<&str>) -> Option<u64> {
    let now = unix_now();
    match window.unwrap_or("30d") {
        "24h" => Some(now.saturating_sub(86_400)),
        "7d" => Some(now.saturating_sub(7 * 86_400)),
        "30d" => Some(now.saturating_sub(30 * 86_400)),
        _ => None,
    }
}
fn internal<E: std::fmt::Display>(error: E) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error":{"message":error.to_string()}})),
    )
}

async fn create_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    let owner = body
        .get("owner")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let client = body.get("client").and_then(Value::as_str).unwrap_or("");
    let allowed_tiers: Vec<String> = body
        .get("allowed_tiers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let rate_limit_rpm = body
        .get("rate_limit_rpm")
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    let monthly_budget_usd = body.get("monthly_budget_usd").and_then(Value::as_f64);
    let expires_at = body.get("expires_at").and_then(Value::as_u64);
    match state.auth.create_key_full(
        owner,
        client,
        allowed_tiers,
        rate_limit_rpm,
        monthly_budget_usd,
        expires_at,
    ) {
        Ok((key_id, raw_key)) => {
            let actor = state.admin_actor(&headers);
            let _ = state
                .audit
                .append_outcome(&actor, "create_key", owner, "success");
            Ok(Json(json!({
                "id": key_id,
                "key": raw_key,
                "message": "Store this key securely. It will not be shown again."
            })))
        }
        Err(_) => Err(auth::json_error(
            "failed to create key",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn list_keys(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    match state.auth.list_keys() {
        Ok(keys) => {
            // Attach a 30-day usage rollup per key (OpenRouter-style).
            let since = unix_now().saturating_sub(30 * 86_400);
            let rollups = state.usage.summarize(Some(since), None, None);
            let enriched: Vec<Value> = keys
                .iter()
                .map(|k| {
                    let mut v = serde_json::to_value(k).unwrap_or(Value::Null);
                    let rollup = rollups.by_key.get(&k.id);
                    v["usage_30d"] = json!({
                        "requests": rollup.map(|u| u.requests).unwrap_or(0),
                        "prompt_tokens": rollup.map(|u| u.prompt_tokens).unwrap_or(0),
                        "completion_tokens": rollup.map(|u| u.completion_tokens).unwrap_or(0),
                        "cost_usd": rollup.map(|u| u.cost_usd).unwrap_or(0.0),
                    });
                    v
                })
                .collect();
            Ok(Json(json!({"keys": enriched})))
        }
        Err(_) => Err(auth::json_error(
            "failed to list keys",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn get_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    match state.auth.list_keys() {
        Ok(keys) => {
            if let Some(key) = keys.into_iter().find(|k| k.id == id) {
                Ok(Json(json!(key)))
            } else {
                Err(auth::json_error("key not found", StatusCode::NOT_FOUND))
            }
        }
        Err(_) => Err(auth::json_error(
            "failed to get key",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn update_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    let allowed_tiers = body.get("allowed_tiers").map(|v| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    });
    let rate_limit_rpm = body
        .get("rate_limit_rpm")
        .map(|v| v.as_u64().map(|n| n as u32));
    let monthly_budget_usd = body.get("monthly_budget_usd").map(|v| v.as_f64());
    let client = body
        .get("client")
        .and_then(Value::as_str)
        .map(str::to_string);
    match state.auth.update_key_quotas(
        &id,
        client,
        allowed_tiers,
        rate_limit_rpm,
        monthly_budget_usd,
    ) {
        Ok(()) => Ok(Json(json!({"id": id, "updated": true}))),
        Err(auth::AuthError::NotFound) => {
            Err(auth::json_error("key not found", StatusCode::NOT_FOUND))
        }
        Err(_) => Err(auth::json_error(
            "failed to update key",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// Verify the admin audit hash chain.
async fn verify_audit(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    match state.audit.verify_chain() {
        Ok(count) => Ok(Json(json!({"valid": true, "entries": count}))),
        Err(e) => Ok(Json(json!({"valid": false, "error": e}))),
    }
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    match state.auth.delete_key(&id) {
        Ok(_) => {
            let actor = state.admin_actor(&headers);
            let _ = state
                .audit
                .append_outcome(&actor, "delete_key", &id, "success");
            Ok(Json(json!({"message": "key deleted"})))
        }
        Err(auth::AuthError::NotFound) => {
            Err(auth::json_error("key not found", StatusCode::NOT_FOUND))
        }
        Err(_) => Err(auth::json_error(
            "failed to delete key",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// Rotates a key: issues a fresh secret, returned exactly once in this
/// response, and invalidates the previous secret immediately.
async fn rotate_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    match state.auth.rotate_key(&id) {
        Ok(raw_key) => {
            let actor = state.admin_actor(&headers);
            let _ = state
                .audit
                .append_outcome(&actor, "rotate_key", &id, "success");
            Ok(Json(json!({
                "key": raw_key,
                "message": "Store this key securely. The previous key is now invalid."
            })))
        }
        Err(auth::AuthError::NotFound) => {
            Err(auth::json_error("key not found", StatusCode::NOT_FOUND))
        }
        Err(_) => Err(auth::json_error(
            "failed to rotate key",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// Aggregate usage across all keys (OpenRouter-style activity dashboard).
/// Query: `window=24h|7d|30d|all` (default 30d), optional `client=`.
async fn usage_summary(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    let since = window_since(params.get("window").map(String::as_str));
    let summary = state
        .usage
        .summarize(since, None, params.get("client").map(String::as_str));
    Ok(Json(serde_json::to_value(summary).unwrap_or(Value::Null)))
}

/// Per-key usage detail: summary filtered to one key over a window.
async fn usage_key_detail(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    let known = state
        .auth
        .list_keys()
        .map(|keys| keys.iter().any(|k| k.id == id))
        .unwrap_or(false);
    if !known {
        return Err(auth::json_error("key not found", StatusCode::NOT_FOUND));
    }
    let since = window_since(params.get("window").map(String::as_str));
    let summary = state.usage.summarize(since, Some(&id), None);
    Ok(Json(serde_json::to_value(summary).unwrap_or(Value::Null)))
}

/// Per-client attribution rollup — which client/app consumed what.
async fn usage_clients(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    let since = window_since(params.get("window").map(String::as_str));
    let summary = state.usage.summarize(since, None, None);
    let clients: Vec<Value> = summary
        .by_client
        .iter()
        .map(|(client, agg)| {
            json!({
                "client": client,
                "requests": agg.requests,
                "prompt_tokens": agg.prompt_tokens,
                "completion_tokens": agg.completion_tokens,
                "cost_usd": agg.cost_usd,
            })
        })
        .collect();
    Ok(Json(
        json!({"window": params.get("window").cloned().unwrap_or_else(|| "30d".into()), "clients": clients}),
    ))
}

/// Current catalog routing state: pins, active (possibly failover-promoted)
/// models, failure counters, and the full model→tier split counts.
async fn catalog_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    Ok(Json(state.catalog.summary_json()))
}

/// Re-fetches the provider catalog, re-partitions tiers, applies pin
/// hysteresis, persists the snapshot, and swaps it in live.
async fn refresh_catalog(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !auth::admin_auth(&headers, &state.admin_key) {
        return Err(auth::json_error(
            "admin access required",
            StatusCode::UNAUTHORIZED,
        ));
    }
    let actor = state.admin_actor(&headers);
    match state
        .catalog
        .refresh(&state.provider, &state.config.routing)
        .await
    {
        Ok(summary) => {
            let _ = state
                .audit
                .append_outcome(&actor, "catalog_refresh", "openrouter", "success");
            Ok(Json(summary))
        }
        Err(error) => {
            let _ = state
                .audit
                .append_outcome(&actor, "catalog_refresh", "openrouter", "failure");
            Err(auth::json_error(&error, StatusCode::BAD_GATEWAY))
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state(admin_key: &str) -> AppState {
        test_state_with_upstream(admin_key, "http://127.0.0.1:9".to_string())
    }

    /// [`test_state`] pointed at a live upstream base_url, for
    /// end-to-end completions tests that exercise the real request path.
    fn test_state_with_upstream(admin_key: &str, base_url: String) -> AppState {
        let mut config: GatewayConfig = serde_json::from_value(json!({
            "host": "127.0.0.1",
            "port": 0,
            "classifier": {"mode": "heuristic"},
            "provider": {"api_key": "test-key"},
            "tiers": {
                "trivial": {"model": "test/trivial"},
                "simple": {"model": "test/simple"},
                "standard": {"model": "test/standard"},
                "hard": {"model": "test/hard"},
                "reasoning": {"model": "test/reasoning"}
            },
            "session": {"enabled": false, "ttl_seconds": 60, "max_entries": 10}
        }))
        .expect("test config parses");
        config.session.enabled = false;
        let keys_file = std::env::temp_dir().join(format!(
            "miser_test_keys_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        AppState {
            classifier: Arc::new(Classifier::new(config.classifier.clone()).unwrap()),
            policy: PolicyEngine::new(config.clone()),
            provider: Provider::new(ProviderConfig {
                base_url,
                api_key: Some("test".to_string()),
                ..Default::default()
            })
            .unwrap(),
            cache: Arc::new(cache::ResponseCache::new(100, 60)),
            session: Arc::new(session::SessionTracker::new(100, 60)),
            auth: Arc::new(auth::AuthManager::new(keys_file)),
            quotas: Arc::new(auth::QuotaEnforcer::new()),
            usage: Arc::new(usage::UsageLedger::new(std::env::temp_dir().join(format!(
                "miser_test_usage_{}_{}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))),
            metrics: Arc::new(metrics::Metrics::new().unwrap()),
            audit: Arc::new(auth::AuditLog::new(std::env::temp_dir().join(format!(
                "miser_test_audit_{}_{}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))),
            catalog: Arc::new(catalog::CatalogRouter::load_or_seed(&config)),
            semantic_cache: Arc::new(semantic_cache::SemanticCache::new(
                config.cache.max_entries,
                300,
                config.cache.semantic_candidate_threshold,
            )),
            quality_judge: None,
            admin_key: admin_key.to_string(),
            config,
        }
    }

    async fn send(
        app: Router,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        if let Some(token) = bearer {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        if body.is_some() {
            builder = builder.header("Content-Type", "application/json");
        }
        let request = match body {
            Some(b) => builder.body(Body::from(b.to_string())).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let resp = app.oneshot(request).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes).unwrap_or(json!({}))
        };
        (status, json)
    }

    #[tokio::test]
    async fn health_endpoints_are_public() {
        let app = build_router(test_state(""));
        for uri in ["/health/live", "/health/ready"] {
            let (status, _) = send(app.clone(), "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
        }
    }

    /// Like [`send`] but returns the raw response for non-JSON
    /// endpoints such as `/metrics`.
    async fn send_text(app: Router, method: &str, uri: &str) -> (StatusCode, String, String) {
        let request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(request).await.unwrap();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            content_type,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    /// Like [`send`] but returns status, headers, and the raw body — for
    /// streaming (SSE) responses and header assertions.
    async fn send_raw(
        app: Router,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        if let Some(token) = bearer {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        if body.is_some() {
            builder = builder.header("Content-Type", "application/json");
        }
        let request = match body {
            Some(b) => builder.body(Body::from(b.to_string())).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let resp = app.oneshot(request).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes.to_vec())
    }

    struct MockUpstream {
        base_url: String,
        requests: Arc<tokio::sync::Mutex<Vec<Value>>>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl MockUpstream {
        async fn requests(&self) -> Vec<Value> {
            self.requests.lock().await.clone()
        }
    }

    impl Drop for MockUpstream {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    /// Local mock `/chat/completions` upstream. Captures every request
    /// body and replies after `delay`: the fixed
    /// (`content_type`, `body_text`) pair when given, otherwise an
    /// OpenAI-style completion echoing the request's `model` — so
    /// assertions can verify the gateway's tier-model rewrite round-trip.
    async fn spawn_mock_upstream(
        delay: Duration,
        status: StatusCode,
        content_type: &'static str,
        body_text: Option<String>,
    ) -> MockUpstream {
        let requests: Arc<tokio::sync::Mutex<Vec<Value>>> = Arc::default();
        let fixed_body = body_text;
        let capture_for_handler = Arc::clone(&requests);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let capture = Arc::clone(&capture_for_handler);
                let fixed_body = fixed_body.clone();
                async move {
                    let model = body
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    capture.lock().await.push(body);
                    tokio::time::sleep(delay).await;
                    let text = fixed_body.unwrap_or_else(|| {
                        serde_json::to_string(&json!({
                            "id": "mock-completion",
                            "object": "chat.completion",
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "finish_reason": "stop",
                                "message": {"role": "assistant", "content": "mock reply"}
                            }],
                            "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8}
                        }))
                        .unwrap()
                    });
                    (
                        status,
                        [(
                            axum::http::header::CONTENT_TYPE,
                            HeaderValue::from_static(content_type),
                        )],
                        text,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let models_app = app.route(
            "/models",
            get(|| async { Json(json!({"data":[{"id":"mock-a"},{"id":"mock-b"}]})) }),
        );
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, models_app).await;
        });
        MockUpstream {
            base_url: format!("http://{addr}"),
            requests,
            handle,
        }
    }

    /// Regression: the checked-in 30s default `request_timeout_ms` 408'd
    /// legitimate non-streaming completions. Measured hard-tier
    /// generations ran 8-30s upstream alone, before classification and
    /// the quality judge added more latency, so real `model: "auto"`
    /// traffic routinely crossed 30s and died with
    /// `{"error":{"message":"request timed out"}}`. The default must
    /// leave headroom for slow providers; deployments still lower it via
    /// `request_timeout_ms`.
    // The assert is intentionally constant: it pins a compile-time
    // constant, which is exactly the regression it guards.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn default_request_timeout_leaves_room_for_slow_generation() {
        assert!(
            DEFAULT_REQUEST_TIMEOUT_MS >= 120_000,
            "DEFAULT_REQUEST_TIMEOUT_MS = {DEFAULT_REQUEST_TIMEOUT_MS}ms is low enough to 408 real LLM completions"
        );
    }

    /// End-to-end `auto` completion against a live slow upstream: the
    /// tier model is rewritten into the upstream request, the routing
    /// headers and served model agree, and a multi-second generation
    /// completes under the default timeout instead of 408ing.
    #[tokio::test]
    async fn auto_completion_survives_slow_generation_end_to_end() {
        let upstream = spawn_mock_upstream(
            Duration::from_millis(1500),
            StatusCode::OK,
            "application/json",
            None,
        )
        .await;
        let app = build_router(test_state_with_upstream("", upstream.base_url.clone()));
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hello"}]});

        let started = Instant::now();
        let (status, headers, bytes) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(payload)).await;
        let elapsed = started.elapsed();

        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(
            elapsed >= Duration::from_millis(1500),
            "returned in {elapsed:?}; the slow upstream was not waited out"
        );

        let body: Value = serde_json::from_slice(&bytes).expect("200 completion body must be JSON");
        let served_model = body["model"].as_str().unwrap_or_default().to_owned();
        assert!(
            served_model.starts_with("test/"),
            "served model should be the tier model, got {served_model:?}"
        );
        assert_eq!(
            headers.get("x-miser-model").and_then(|v| v.to_str().ok()),
            Some(served_model.as_str()),
            "x-miser-model must agree with the served model"
        );
        assert!(
            headers.get("x-miser-classifier").is_some(),
            "routing diagnostics headers are part of the response contract"
        );

        let requests = upstream.requests().await;
        assert_eq!(requests.len(), 1, "upstream should see exactly one request");
        assert_eq!(
            requests[0].get("model").and_then(Value::as_str),
            Some(served_model.as_str()),
            "upstream must receive the tier model, never 'auto'"
        );
    }

    /// A configured `request_timeout_ms` must still bound the request and
    /// surface the typed 408 JSON error rather than hang.
    #[tokio::test]
    async fn configured_request_timeout_enforced_with_typed_408() {
        let upstream = spawn_mock_upstream(
            Duration::from_secs(3),
            StatusCode::OK,
            "application/json",
            None,
        )
        .await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state
            .config
            .extra
            .insert("request_timeout_ms".into(), json!(500));
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hello"}]});

        let started = Instant::now();
        let (status, _, bytes) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(payload)).await;
        let elapsed = started.elapsed();

        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert!(
            elapsed < Duration::from_secs(3),
            "took {elapsed:?}; the timeout did not fire before the upstream replied"
        );
        let body: Value = serde_json::from_slice(&bytes).expect("408 body must be JSON");
        assert_eq!(body["error"]["message"], "request timed out");
    }

    /// Streaming passthrough: an `auto` streaming request relays the
    /// upstream SSE stream and carries the routing headers.
    #[tokio::test]
    async fn auto_streaming_relays_upstream_events() {
        let sse = concat!(
            "data: {\"id\":\"mock-chunk\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"id\":\"mock-chunk\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let upstream = spawn_mock_upstream(
            Duration::ZERO,
            StatusCode::OK,
            "text/event-stream",
            Some(sse.into()),
        )
        .await;
        let app = build_router(test_state_with_upstream("", upstream.base_url.clone()));
        let payload =
            json!({"model":"auto","stream":true,"messages":[{"role":"user","content":"hello"}]});

        let (status, headers, bytes) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(payload)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get("content-type").and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "stream content type must pass through"
        );
        let requests = upstream.requests().await;
        assert_eq!(requests.len(), 1);
        let tier_model = requests[0]["model"].as_str().unwrap_or_default();
        assert!(tier_model.starts_with("test/"));
        assert_eq!(
            headers.get("x-miser-model").and_then(|v| v.to_str().ok()),
            Some(tier_model),
            "x-miser-model must name the tier model the stream was routed to"
        );
        let body = String::from_utf8(bytes).unwrap();
        assert!(
            body.contains("\"delta\":{\"content\":\"Hello\"}"),
            "stream body must relay upstream events:\n{body}"
        );
        assert!(body.contains("data: [DONE]"));
    }

    /// The exact-match response cache: an identical repeat request is
    /// served from cache (`x-miser-cache: hit-exact`) without touching
    /// the upstream again.
    #[tokio::test]
    async fn identical_request_is_served_from_exact_cache() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let app = build_router(test_state_with_upstream("", upstream.base_url.clone()));
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});

        let (first_status, first_headers, first_bytes) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(payload.clone()),
        )
        .await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(
            first_headers
                .get("x-miser-cache")
                .and_then(|v| v.to_str().ok()),
            Some("miss")
        );

        let (second_status, second_headers, second_bytes) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(payload)).await;
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(
            second_headers
                .get("x-miser-cache")
                .and_then(|v| v.to_str().ok()),
            Some("hit-exact"),
            "an identical repeat request must be served from cache"
        );
        assert_eq!(first_bytes, second_bytes, "cached body must be identical");

        let requests = upstream.requests().await;
        assert_eq!(
            requests.len(),
            1,
            "the upstream must see only the first request; the replay is served from cache"
        );
    }

    /// Session continuity: once a conversation has been routed to a
    /// higher tier, a trivial follow-up in the same conversation stays on
    /// that tier instead of being downgraded.
    #[tokio::test]
    async fn session_continuity_keeps_conversation_on_higher_tier() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state.config.session.enabled = true;
        let app = build_router(state);

        // Tools bump the effective tier; the session stores it.
        let tool_request = json!({
            "model":"auto",
            "user":"cont-1",
            "messages":[{"role":"user","content":"hi"}],
            "tools":[{"type":"function","function":{"name":"shell","description":"run a shell command"}}]
        });
        let (status, headers, _) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(tool_request),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let elevated_model = headers
            .get("x-miser-model")
            .and_then(|v| v.to_str().ok())
            .expect("x-miser-model on the first response")
            .to_string();
        assert!(elevated_model.starts_with("test/"), "{elevated_model}");

        // A trivial follow-up in the same conversation must stay on the
        // stored tier instead of being downgraded to trivial.
        let follow_up = json!({
            "model":"auto",
            "user":"cont-1",
            "messages":[{"role":"user","content":"hi"}]
        });
        let (status, headers, _) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(follow_up)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get("x-miser-model").and_then(|v| v.to_str().ok()),
            Some(elevated_model.as_str()),
            "session continuity must keep the conversation on the stored tier"
        );
    }

    /// A key's monthly budget cap is enforced after real spend is
    /// recorded: the first request succeeds, the next is rejected with
    /// 402 before reaching the upstream.
    #[tokio::test]
    async fn monthly_budget_exhaustion_returns_402() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        // Blended price turns tokens into recorded spend.
        state
            .config
            .extra
            .insert("price_per_1k_usd".into(), json!(0.001));
        let raw = state
            .auth
            .create_key_with_quotas("budgeted", "-", vec![], None, Some(0.0001), None)
            .unwrap();
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});

        let (first, _) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(first, StatusCode::OK);

        let (second, body) = send(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(second, StatusCode::PAYMENT_REQUIRED, "{body}");
        assert_eq!(
            upstream.requests().await.len(),
            1,
            "the budget-exhausted request must be rejected before the upstream"
        );
    }

    /// Upstream failures pass through to the client with their status
    /// and error body, plus the routing diagnostics headers.
    #[tokio::test]
    async fn upstream_error_is_passed_through_with_routing_headers() {
        let upstream = spawn_mock_upstream(
            Duration::ZERO,
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            Some(r#"{"error":{"message":"upstream exploded"}}"#.into()),
        )
        .await;
        let app = build_router(test_state_with_upstream("", upstream.base_url.clone()));
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});

        let (status, headers, bytes) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(payload)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&bytes).expect("upstream error body must be relayed");
        assert_eq!(body["error"]["message"], "upstream exploded");
        assert!(
            headers.get("x-miser-model").is_some(),
            "routing diagnostics must accompany the relayed error"
        );
    }

    /// Usage attribution through the real completions path: the ledger
    /// must attribute the request to its key and bucket the SERVED model
    /// (never the requested "auto") in the summary.
    #[tokio::test]
    async fn completions_record_usage_with_served_model_and_key() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let state = test_state_with_upstream("secret-admin", upstream.base_url.clone());
        let raw = state
            .auth
            .create_key_with_quotas("attributed", "cli-app", vec![], None, None, None)
            .unwrap();
        let key_id = state
            .auth
            .list_keys()
            .unwrap()
            .into_iter()
            .find(|k| k.owner == "attributed")
            .unwrap()
            .id;
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});

        let (status, _, bytes) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );

        let (status, body) = send(
            app,
            "GET",
            "/admin/usage/summary?window=all",
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["requests"], 1, "{body}");
        let served = body["by_model"]
            .as_object()
            .expect("by_model present")
            .keys()
            .map(|k| k.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            served,
            vec!["test/trivial"],
            "by_model must bucket the served tier model, got {served:?}"
        );
        assert!(
            body["by_key"][&key_id].is_object(),
            "the request must be attributed to its key: {body}"
        );
    }

    /// The `/v1/models` endpoint relays the upstream model list.
    #[tokio::test]
    async fn models_endpoint_relays_upstream_catalog() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let app = build_router(test_state_with_upstream("", upstream.base_url.clone()));
        let (status, body) = send(app, "GET", "/v1/models", None, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"][0]["id"], "mock-a", "{body}");
    }

    /// Drift guard: the shipped `config/miser.toml` must keep parsing
    /// into `GatewayConfig` and passing validation. Renamed fields, new
    /// required keys, or invalid values fail here before any deployment.
    #[test]
    fn shipped_config_parses_and_validates() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/miser.toml");
        let raw = std::fs::read_to_string(path).expect("shipped config/miser.toml is present");
        let config: GatewayConfig = toml::from_str(&raw).expect("shipped config parses");
        assert_eq!(
            validate_config(&config),
            Ok(()),
            "shipped config must pass validation"
        );
    }

    #[tokio::test]
    async fn metrics_endpoint_renders_and_counters_increment() {
        let state = test_state("");
        // Seed one key so the gateway enforces authentication instead of
        // falling back to open access.
        state
            .auth
            .create_key_with_quotas("metrics", "-", vec![], None, None, None)
            .unwrap();
        let app = build_router(state);
        let (status, content_type, body) = send_text(app.clone(), "GET", "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "text/plain; version=0.0.4");
        // Scalar families always render; vector families appear once used.
        assert!(body.contains("# HELP miser_cache_hits_total"), "{body}");
        assert!(
            !body.contains("miser_requests_total{route=\"/v1/chat/completions\""),
            "counter should be absent before traffic:\n{body}"
        );

        // A rejected completions request must bump the route/status counter.
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, _) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("miser_bogus"),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _, body) = send_text(app, "GET", "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(concat!(
                "miser_requests_total{route=\"/v1/chat/completions\",",
                "status=\"401\"} 1"
            )),
            "counter did not increment:\n{body}"
        );
        assert!(
            body.contains("# HELP miser_request_duration_seconds"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn admin_endpoints_require_admin_key() {
        let app = build_router(test_state("secret-admin"));
        let (status, _) = send(
            app.clone(),
            "POST",
            "/admin/keys",
            None,
            Some(json!({"owner":"x"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = send(
            app.clone(),
            "POST",
            "/admin/keys",
            Some("wrong"),
            Some(json!({"owner":"x"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        let (status, body) = send(
            app,
            "POST",
            "/admin/keys",
            Some("secret-admin"),
            Some(json!({"owner":"x"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["key"].as_str().unwrap().starts_with("miser_"));
    }

    #[tokio::test]
    async fn completions_reject_invalid_and_inactive_keys() {
        let state = test_state("");
        // Seed one valid key.
        let raw = state
            .auth
            .create_key_with_quotas("tester", "-", vec![], None, None, None)
            .unwrap();
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, _) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("miser_bogus"),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Valid key passes auth (upstream is unreachable → expect 502, not 401).
        let (status, _) = send(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn rate_limit_enforcement_returns_429() {
        let state = test_state("");
        let raw = state
            .auth
            .create_key_with_quotas("limited", "-", vec![], Some(1), None, None)
            .unwrap();
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        // First request consumes the window; upstream failure (502) comes after quota pass.
        let (first, _) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(first, StatusCode::BAD_GATEWAY);
        let (second, body) = send(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(second, StatusCode::TOO_MANY_REQUESTS, "{body}");
    }

    #[tokio::test]
    async fn tier_gating_returns_403_for_disallowed_tier() {
        let state = test_state("");
        let raw = state
            .auth
            .create_key_with_quotas("gated", "-", vec!["hard".to_string()], None, None, None)
            .unwrap();
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, body) = send(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not allowed")
        );
    }

    /// Expired keys are rejected with 403, and admin rotation issues a new
    /// secret once while the old secret stops authenticating immediately.
    #[tokio::test]
    async fn usage_endpoints_gate_on_admin_and_report_attribution() {
        let state = Arc::new(test_state("secret-admin"));
        state.usage.record(&usage::UsageRecord {
            ts: unix_now(),
            key_id: "key_test".into(),
            client: "cli-app".into(),
            model: "test/hard".into(),
            requested_model: "auto".into(),
            tier: "hard".into(),
            prompt_tokens: 10,
            completion_tokens: 20,
            cost_usd: 0.03,
            latency_ms: 5,
            cached: false,
            status: 200,
            request_id: "r1".into(),
        });
        let app = build_router((*state).clone());

        // Admin gate: unauthenticated access is rejected on every route.
        for uri in ["/admin/usage/summary", "/admin/usage/clients"] {
            let (status, _) = send(app.clone(), "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        }

        // Summary reflects the attributed record.
        let (status, body) = send(
            app.clone(),
            "GET",
            "/admin/usage/summary?window=all",
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["requests"], 1, "{body}");
        assert_eq!(body["prompt_tokens"], 10);
        assert_eq!(body["completion_tokens"], 20);
        assert_eq!(body["by_model"]["test/hard"]["requests"], 1);
        assert_eq!(body["by_key"]["key_test"]["client"], "cli-app");
        assert_eq!(body["by_client"]["cli-app"]["requests"], 1);
        assert_eq!(body["by_day"].as_object().map(|d| d.len()), Some(1));

        // Client rollup lists the client bucket.
        let (status, body) = send(
            app.clone(),
            "GET",
            "/admin/usage/clients?window=all",
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["clients"][0]["client"], "cli-app");
        assert_eq!(body["clients"][0]["cost_usd"], 0.03);

        // Per-key detail 404s for unknown ids…
        let (status, _) = send(
            app.clone(),
            "GET",
            "/admin/usage/keys/key_missing",
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // …and works for a real key, including create → list → detail flow.
        let (status, body) = send(
            app.clone(),
            "POST",
            "/admin/keys",
            Some("secret-admin"),
            Some(json!({"owner": "o", "client": "cli2"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["id"].as_str().is_some(), "create must return id");
        let key_id = body["id"].as_str().unwrap().to_string();
        let (status, body) = send(
            app,
            "GET",
            &format!("/admin/usage/keys/{key_id}"),
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["requests"], 0);
    }

    /// Expired keys are rejected with 403, and admin rotation issues a new
    /// secret once while the old secret stops authenticating immediately.
    #[tokio::test]
    async fn expired_keys_rejected_and_rotation_invalidates_old_secret() {
        let state = test_state("secret-admin");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let expired_raw = state
            .auth
            .create_key_with_quotas("stale", "-", vec![], None, None, Some(now - 10))
            .unwrap();
        let active_raw = state
            .auth
            .create_key_with_quotas("current", "-", vec![], None, None, None)
            .unwrap();
        let active_id = state
            .auth
            .list_keys()
            .unwrap()
            .into_iter()
            .find(|k| k.owner == "current")
            .unwrap()
            .id;
        let app = build_router(state);
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});

        // Expired key → 403 with an explicit message.
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&expired_raw),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("expired"),
            "{body}"
        );

        // Rotation requires the admin key.
        let (status, _) = send(
            app.clone(),
            "POST",
            &format!("/admin/keys/{active_id}/rotate"),
            Some(&active_raw),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Admin rotation returns a fresh one-time secret.
        let (status, body) = send(
            app.clone(),
            "POST",
            &format!("/admin/keys/{active_id}/rotate"),
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let rotated_raw = body["key"]
            .as_str()
            .expect("rotated key in response")
            .to_string();
        assert!(rotated_raw.starts_with("miser_"));
        assert_ne!(rotated_raw, active_raw);

        // The old secret is dead; the new one passes auth (502 = upstream).
        let (status, _) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&active_raw),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&rotated_raw),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    /// Creating a key with a future `expires_at` via the admin API works
    /// and the raw secret is returned once.
    #[tokio::test]
    async fn create_key_accepts_optional_expiry() {
        let state = test_state("secret-admin");
        let app = build_router(state);
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        let (status, body) = send(
            app,
            "POST",
            "/admin/keys",
            Some("secret-admin"),
            Some(json!({"owner":"temporary","expires_at": expires_at})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["key"].as_str().unwrap().starts_with("miser_"));
    }

    /// Delivering SIGTERM to our own process must complete the shutdown
    /// future handed to `with_graceful_shutdown`.
    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_completes_shutdown_signal_future() {
        use std::time::Duration;

        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .expect("install SIGINT handler");
        let shutdown = std::pin::pin!(wait_for_shutdown_signal(&mut terminate, &mut interrupt));
        // Give the signal handler a moment to register before delivery.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .expect("failed to run kill");
        assert!(status.success(), "failed to deliver SIGTERM");
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .expect("SIGTERM should complete the shutdown future");
    }
}
