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

/// Ledger key id used to attribute completions authenticated by the shared
/// admin key, so that spend shows up in usage reporting instead of vanishing.
const ADMIN_KEY_ATTRIBUTION_ID: &str = "key_admin";

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

/// The gateway's per-request deadline, from `request_timeout_ms` or the
/// checked-in default.
///
/// Single source of truth: the axum `TimeoutLayer` and the provider HTTP client
/// both derive from this, so the transport gives up at the same point the
/// request layer does. Previously the client had no timeout at all, leaving
/// callers outside the request stack -- notably the spawned startup catalog
/// refresh -- able to wait on a hung upstream indefinitely.
fn request_timeout(config: &GatewayConfig) -> Duration {
    Duration::from_millis(
        config
            .extra
            .get("request_timeout_ms")
            .and_then(Value::as_u64)
            .filter(|timeout| *timeout > 0)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
    )
}

/// Build the upstream provider's config, including the transport timeout.
///
/// `provider_preferences` is applied by the caller, which can propagate a
/// serialisation failure.
fn provider_client_config(config: &GatewayConfig, api_key: &str) -> ProviderConfig {
    let deadline_ms = request_timeout(config).as_millis() as u64;
    ProviderConfig {
        base_url: config.provider.base_url.clone(),
        api_key: Some(api_key.to_owned()),
        // Rounded *up*, and never zero. `as_secs` would floor instead, which
        // makes the transport give up before the request layer does -- and for
        // a sub-second deadline, flooring to 0 is worse still: reqwest reads a
        // zero timeout as "fail immediately", so every request would error.
        timeout_seconds: Some(deadline_ms.div_ceil(1000).max(1)),
        ..Default::default()
    }
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
    let mut provider_config = provider_client_config(&config, &api_key);
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
        auth: Arc::new(
            auth::AuthManager::new(std::path::PathBuf::from(&auth_path)).map_err(|e| {
                anyhow::anyhow!(
                    "failed to load the API key store at {auth_path}: {e}. \
                     Refusing to start rather than booting with no keys."
                )
            })?,
        ),
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
        model: &str,
        status: Option<axum::http::StatusCode>,
    ) {
        if !self.catalog.enabled() {
            return;
        }
        if matches!(status, Some(status) if status.is_success()) {
            self.catalog.report_success(tier, model);
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
        if let Some(new_model) = self.catalog.report_failure(tier, model, threshold) {
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
    let request_timeout = request_timeout(&state.config);
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
                // The shared admin key is deliberately usable on the data plane
                // so an operator can smoke-test without minting a key. It is
                // still real traffic against the operator's upstream account, so
                // it is attributed to a synthetic key rather than dropped:
                // leaving this `None` made `record_usage` a no-op, and the spend
                // was invisible in `/admin/usage/summary`.
                //
                // No rate limit, no budget and an empty tier allowlist, so the
                // quota and gating block below stays a no-op for it -- exactly
                // as it was when the key was `None`. Only attribution changes.
                authenticated_key = Some(auth::ApiKey {
                    id: ADMIN_KEY_ATTRIBUTION_ID.to_owned(),
                    key_hash: String::new(),
                    owner: "admin-key".to_owned(),
                    client: "admin-key".to_owned(),
                    created_at: unix_now(),
                    active: true,
                    allowed_tiers: Vec::new(),
                    rate_limit_rpm: None,
                    monthly_budget_usd: None,
                    expires_at: None,
                });
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
    //
    // Tool-bearing requests are excluded for the same reason, and the
    // exclusion has to be explicit because neither stage of the pipeline can
    // see a tool contract: `request_text_for_embedding` hashes only
    // `messages[].content`, and `QualityJudge::equivalent` compares only the
    // two last user messages. So a tool-less prompt and the identical prompt
    // with a tool attached are indistinguishable -- same embedding, and the
    // judge is asked the same question about the same text. Serving the
    // cached response then hands the client a body with no `tool_calls`, so
    // its agentic loop has nothing to dispatch. Because the cache answers
    // before classification, the tool-capable tier floor in `effective_tier`
    // is skipped too, so the cheap model would have served it anyway.
    //
    // Tool *history* counts too, not just a declared `tools` array: a client
    // resuming an agentic loop commonly replays the transcript without
    // re-declaring its tools, and `has_tool_history` treats exactly that as
    // agentic and floors the tier to Hard. Folding the tool-result text into
    // the embedding makes those look similar, so without this the same bug
    // reaches us one axis over.
    //
    // The exact-match cache still covers these requests -- `request_hash`
    // keeps `tools` and the full transcript in the key -- so only genuinely
    // different prompts miss.
    if state.config.cache.enabled
        && state.config.cache.semantic_enabled
        && request.response_format.is_none()
        && request.tools.as_ref().is_none_or(Vec::is_empty)
        && request.tool_choice.is_none()
        && !miser_policy::has_tool_history(&request)
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
    // Session continuity is scoped to the authenticated API key: the session
    // key is built from client-supplied data, so without the tenant prefix two
    // keys sharing a `user` (or a first message) would share a session, and
    // since a session only ever moves up in tier, one tenant's expensive
    // conversation would drag the other onto that tier.
    let session_tenant = authenticated_key
        .as_ref()
        .map(|key| key.id.as_str())
        .unwrap_or("-");
    if state.config.session.enabled {
        if let Some(key) = session::session_key(&request, session_tenant) {
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
        // A pre-flight floor, not a quality decision. This must not touch
        // `quality_escalations_total`: agentic traffic floors to Hard by
        // construction, so folding these in made that counter report the
        // judge as active on requests it never graded, and double-counted
        // the ones that genuinely were escalated.
        state.metrics.tier_floors_total.inc();
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
        if let Some(key) = session::session_key(&request, session_tenant) {
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
            state.report_upstream_outcome(effective_tier, &route.model, None);
            return Err(internal(error));
        }
    };
    let upstream_status = upstream.status();
    if !upstream_status.is_success() {
        state.metrics.upstream_errors_total.inc();
    }
    state.report_upstream_outcome(effective_tier, &route.model, Some(upstream_status));
    let selected_route = route.clone();
    if !stream_requested && upstream.status().is_success() {
        // Charge the first upstream call to the key's budget. An escalation
        // below issues a second real call and is charged again, so a
        // quality-gated request is never silently free.
        if let Some(key) = &authenticated_key {
            charge_budget(&state, key, estimated_call_tokens(&request));
        }
        // The status and pass-through headers of whichever response is
        // ultimately served. Renamed from `original_*` because a quality-gate
        // escalation replaces both along with the body.
        let mut settled_status = upstream.status();
        let mut settled_headers = safe_response_headers(upstream.headers());
        let payload = upstream.bytes().await.map_err(internal)?;
        // Captured now, before the quality gate can replace `payload` with an
        // escalated response. Every successful upstream call is billed, so
        // the discarded attempt's tokens must not be lost.
        let mut settled_usage = usage_tokens(&payload);
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
            // A tool-calling turn carries no prose, so there is nothing for
            // the judge to grade. Scoring the empty string against the prompt
            // failed every agentic turn and triggered a needless escalation.
            let tool_call_only = response_text.trim().is_empty()
                && parsed
                    .as_ref()
                    .and_then(|body| body["choices"][0]["message"]["tool_calls"].as_array())
                    .is_some_and(|calls| !calls.is_empty());
            let mut check = deterministic_quality(
                &request,
                &parsed.unwrap_or(Value::Null),
                &classification,
                &state.config.quality,
            );
            if !tool_call_only {
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
                            // Mirror the pre-flight gate: in `fixed` mode the
                            // configured [tiers.*].model is authoritative, so
                            // reading a persisted snapshot pin here silently
                            // overrode the operator's model choice on exactly
                            // the escalated (hardest) requests.
                            let escalated_model = if state.catalog.enabled() {
                                state
                                    .catalog
                                    .active_model(escalated_tier)
                                    .unwrap_or_else(|| next_route.model.clone())
                            } else {
                                next_route.model.clone()
                            };
                            let mut escalated_body =
                                serde_json::to_value(&request).map_err(internal)?;
                            escalated_body["model"] = Value::String(escalated_model.clone());
                            if let Ok(escalated_upstream) =
                                state.provider.forward(escalated_body, None).await
                            {
                                // The retry's status and headers are read
                                // before the success guard, not inside it: a
                                // failing retry previously fell out of this
                                // block entirely, so nothing about it was
                                // reported. The retry is a real upstream call
                                // against a *different* tier's active model, so
                                // without this the model the gateway escalated
                                // to could never accumulate failures and never
                                // fail over, however many escalating requests it
                                // broke -- every one of them burning a first
                                // attempt plus a failing escalation.
                                let escalated_status = escalated_upstream.status();
                                let escalated_headers =
                                    safe_response_headers(escalated_upstream.headers());
                                state.report_upstream_outcome(
                                    escalated_tier,
                                    &escalated_model,
                                    Some(escalated_status),
                                );
                                if escalated_status.is_success() {
                                    if let Ok(escalated_payload) = escalated_upstream.bytes().await
                                    {
                                        // The retry was really made and really
                                        // billed, whether or not its answer is
                                        // the one we end up serving, so charge
                                        // and accumulate it here rather than
                                        // inside the keep-or-discard branch.
                                        if let Some(key) = &authenticated_key {
                                            charge_budget(
                                                &state,
                                                key,
                                                estimated_call_tokens(&request),
                                            );
                                        }
                                        let (escalated_prompt, escalated_completion) =
                                            usage_tokens(&escalated_payload);
                                        settled_usage.0 += escalated_prompt;
                                        settled_usage.1 += escalated_completion;
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
                                            // The body is now the retry's, so the
                                            // response the client sees -- and the
                                            // pair both cache layers persist --
                                            // must describe the retry rather than
                                            // the discarded attempt.
                                            settled_status = escalated_status;
                                            settled_headers = escalated_headers;
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
        // Real usage from every upstream call this request made, attributed
        // to the key. `settled_usage` already sums a quality-gate escalation's
        // discarded attempt, so re-parsing the final payload here would drop
        // it and under-report the client's actual spend.
        record_usage(
            &state,
            authenticated_key.as_ref(),
            &selected_route.model,
            &requested_model,
            &format_tier(effective_tier),
            settled_usage.0,
            settled_usage.1,
            started.elapsed(),
            false,
            settled_status.as_u16(),
            &request_id,
        );
        state.cache.store(
            cache_key,
            payload.clone(),
            settled_status,
            settled_headers.clone(),
        );
        // Semantic cache gets the final (quality-gated, possibly escalated)
        // response so near-duplicate requests reuse the best answer.
        //
        // Skipped when this response contains `tool_calls`. The lookup guard
        // already refuses to serve tool-bearing *requests*, but the mirror of
        // that mattered too: a tool-calling body stored here can be replayed to
        // a plain, tool-free turn, and the client would act on a `tool_calls`
        // entry for tools it never declared. A response that cannot be served
        // to anything is not worth caching.
        let response_has_tool_calls = serde_json::from_slice::<Value>(&payload)
            .ok()
            .and_then(|body| {
                body["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .map(|calls| !calls.is_empty())
            })
            .unwrap_or(false);
        if state.config.cache.enabled
            && state.config.cache.semantic_enabled
            && !response_has_tool_calls
        {
            let embedding_text = semantic_cache::request_text_for_embedding(&body);
            state.semantic_cache.store(
                semantic_cache::embed_prompt(&embedding_text),
                embedding_text,
                payload.clone(),
                settled_status,
                settled_headers.clone(),
            );
        }
        let mut response = Response::builder().status(settled_status);
        for (name, value) in &settled_headers {
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
    // The stream is observed as it is relayed. Failover used to be decided from
    // the response headers alone, so a model that answered `200` and then
    // dropped the connection mid-SSE was recorded as a success every time: it
    // could never accumulate failures and so never failed over, and clients
    // kept receiving truncated streams from a model the failover logic was
    // supposed to have retired. Long generations and provider-side read
    // timeouts make mid-stream death the common streaming failure, not the rare
    // one.
    //
    // A promotion can only affect the *next* request, since these headers are
    // already on the wire by the time the body fails. That is the best
    // available once bytes have been sent.
    let stream_model = selected_route.model.clone();
    let report_state = Arc::clone(&state);
    let report_tier = effective_tier;
    let stream = futures_util::StreamExt::map(upstream.bytes_stream(), move |chunk| {
        if chunk.is_err() {
            report_state.report_upstream_outcome(report_tier, &stream_model, None);
        }
        chunk
    });
    // Streaming: token counts arrive inside the stream, so charge and record
    // the same monotonic estimate the non-streaming path uses. The charge is
    // what makes a monthly budget cap mean anything -- without it a client
    // could set `stream: true` and spend without limit.
    //
    // Guarded on a successful status, because this block is also the
    // non-streaming *error* path: a plain request whose upstream answered 4xx
    // or 5xx falls straight through to here. Charging those would let a client
    // drain its own monthly budget with requests the provider rejected, and
    // would keep a key 402'd for the rest of the month after a transient
    // upstream incident had already recovered.
    if status.is_success() {
        if let Some(key) = &authenticated_key {
            charge_budget(&state, key, estimated_call_tokens(&request));
        }
    }
    if let Some(key) = &authenticated_key {
        record_usage(
            &state,
            Some(key),
            &selected_route.model,
            &requested_model,
            &format_tier(effective_tier),
            0,
            estimated_call_tokens(&request) as u64,
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

/// `usage.prompt_tokens` / `usage.completion_tokens` from a provider payload,
/// defaulting to 0 when the provider reported no usage block.
fn usage_tokens(payload: &[u8]) -> (u64, u64) {
    serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|body| body.get("usage").cloned())
        .map(|usage| {
            (
                usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        })
        .unwrap_or((0, 0))
}

/// Conservative token estimate for one upstream call, used for budget
/// enforcement. Real usage is not available until the body has been read, and
/// a budget cap is a safety mechanism that should over- rather than
/// under-charge, so this mirrors the long-standing `max_tokens` convention.
fn estimated_call_tokens(request: &ChatCompletionRequest) -> f64 {
    request.max_tokens.unwrap_or(512) as f64
}

/// Charge one upstream call against a key's monthly budget. No-op unless the
/// operator configured a blended `price_per_1k_usd`.
///
/// Called once per *successful upstream call*, not once per client request: a
/// quality-gate escalation issues a second real call, and that money is spent
/// whether or not the escalated answer is the one served.
fn charge_budget(state: &AppState, key: &auth::ApiKey, tokens: f64) {
    if let Some(price) = state
        .config
        .extra
        .get("price_per_1k_usd")
        .and_then(Value::as_f64)
    {
        state.quotas.record_spend(&key.id, tokens / 1000.0 * price);
    }
}

/// Parse a reporting window query value ("24h" | "7d" | "30d" | "all")
/// into a lower-bound unix timestamp; `None` means no lower bound.
///
/// Only an explicit `all` returns `None`. Anything unrecognised used to fall
/// into the same arm, so a typo like `window=90d` silently became a full scan of
/// an append-only ledger that is never rotated -- the one input most likely to
/// hurt. Unknown values now fall back to the 30-day default and say so.
fn window_since(window: Option<&str>) -> Option<u64> {
    let now = unix_now();
    match window.unwrap_or("30d") {
        "24h" => Some(now.saturating_sub(24 * 3_600)),
        "7d" => Some(now.saturating_sub(7 * 86_400)),
        "30d" => Some(now.saturating_sub(30 * 86_400)),
        "all" => None,
        other => {
            tracing::warn!(
                window = other,
                "unrecognised usage window; falling back to 30d (`all` means no lower bound)"
            );
            Some(now.saturating_sub(30 * 86_400))
        }
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
            let rollups = state
                .usage
                .summarize_blocking(Some(since), None, None)
                .await;
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

/// Strictly read one optional admin-supplied field into the absent / clear /
/// set trichotomy that `AuthManager::update_key_quotas` expects.
///
/// `Value::as_*` cannot express that trichotomy: it maps a wrong-typed value to
/// `None`, which the store then assigns, so `{"monthly_budget_usd": "10.00"}`
/// -- a stringified number, exactly what a shell-quoted curl or a client that
/// stringifies numerics sends -- silently deleted the spend cap, and
/// `{"rate_limit_rpm": "60"}` deleted the rate limit, while the handler
/// answered `200 {"updated": true}`. A non-array `allowed_tiers` became an
/// empty allowlist, which the completions path reads as "every tier allowed".
/// These fields gate spend and access, so a value that is present but
/// unparseable is a client error rather than something to guess at. Parsing
/// into the target type also range-checks for free: an `rpm` above `u32::MAX`
/// used to truncate via `as u32`.
fn patch_field<T>(body: &Value, field: &str) -> Result<Option<Option<T>>, String>
where
    T: serde::de::DeserializeOwned,
{
    match body.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(value) => serde_json::from_value::<T>(value.clone())
            .map(|parsed| Some(Some(parsed)))
            .map_err(|error| format!("{field} is invalid: {error}")),
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
    // An explicit null clears the allowlist, which the completions path reads
    // as "every tier allowed" -- the same state an empty array denotes.
    let allowed_tiers = patch_field::<Vec<String>>(&body, "allowed_tiers")
        .map_err(|message| auth::json_error(&message, StatusCode::BAD_REQUEST))?
        .map(|inner| inner.unwrap_or_default());
    let rate_limit_rpm = patch_field::<u32>(&body, "rate_limit_rpm")
        .map_err(|message| auth::json_error(&message, StatusCode::BAD_REQUEST))?;
    let monthly_budget_usd = patch_field::<f64>(&body, "monthly_budget_usd")
        .map_err(|message| auth::json_error(&message, StatusCode::BAD_REQUEST))?;
    // `client` is a label rather than a control, and the store has no notion
    // of clearing it, so an explicit null resets it to the "-" placeholder via
    // `normalize_client` instead of erroring.
    let client = patch_field::<String>(&body, "client")
        .map_err(|message| auth::json_error(&message, StatusCode::BAD_REQUEST))?
        .map(|inner| inner.unwrap_or_default());
    // `active` and `expires_at` used to be ignored here while the API still
    // answered `{"updated": true}`, so revoking a key or setting an expiry
    // after creation was impossible without deleting the record -- which also
    // discards the key's usage history.
    let active = patch_field::<bool>(&body, "active")
        .map_err(|message| auth::json_error(&message, StatusCode::BAD_REQUEST))?
        .flatten();
    let expires_at = patch_field::<u64>(&body, "expires_at")
        .map_err(|message| auth::json_error(&message, StatusCode::BAD_REQUEST))?;
    let outcome = state
        .auth
        .update_key_quotas(
            &id,
            client,
            allowed_tiers,
            rate_limit_rpm,
            monthly_budget_usd,
        )
        .and_then(|()| state.auth.update_key_status(&id, active, expires_at));
    match outcome {
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
            // The key is gone, so its rate-limit window and accumulated spend
            // must go with it rather than lingering for the life of the process.
            state.quotas.forget(&id);
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
    // The one endpoint most in need of the blocking hop: with no `key_id`
    // filter it can aggregate the entire never-rotated ledger.
    let summary = state
        .usage
        .summarize_blocking(since, None, params.get("client").map(|c| c.to_owned()))
        .await;
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
    let summary = state
        .usage
        .summarize_blocking(since, Some(id.clone()), None)
        .await;
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
    let summary = state.usage.summarize_blocking(since, None, None).await;
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

    /// [`test_state_with_upstream`] with the config adjusted before the
    /// `AppState` is assembled. Needed for anything the state builds eagerly
    /// from config -- notably the catalog router, whose snapshot is loaded in
    /// `test_state_with_upstream` itself, so mutating `config.routing` after
    /// the fact would have no effect.
    fn test_state_with(
        admin_key: &str,
        base_url: String,
        tune: impl FnOnce(&mut GatewayConfig),
    ) -> AppState {
        test_state_tuned(admin_key, base_url, tune)
    }

    /// A per-call unique scratch file under the temp dir.
    ///
    /// Uniqueness comes from a process-wide atomic counter, NOT from a
    /// wall-clock timestamp. Tests share a process and run in parallel, and
    /// `SystemTime::now()` on a coarse-granularity clock (common in
    /// containers and VMs) returns the *same* nanosecond reading to two
    /// threads that ask within the same tick. That made two tests pick the
    /// same keys file, so a key created by one test turned up in the other
    /// test's `AuthManager` and an unrelated test intermittently failed with
    /// `401 invalid API key`. A counter is race-free by construction.
    fn unique_temp_path(prefix: &str, extension: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}_{}_{n}.{extension}", std::process::id()))
    }

    /// The keys/usage/audit files are only isolated if `unique_temp_path` is,
    /// so pin that guarantee directly instead of waiting for the suite to trip
    /// over a collision at random.
    #[test]
    fn scratch_paths_are_unique_under_concurrency() {
        use std::collections::HashSet;
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..64)
                        .map(|_| unique_temp_path("miser_test_keys", "json"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut seen = HashSet::new();
        for handle in handles {
            for path in handle.join().expect("scratch path thread") {
                assert!(
                    seen.insert(path.clone()),
                    "unique_temp_path handed out a duplicate: {}",
                    path.display()
                );
            }
        }
        assert_eq!(seen.len(), 8 * 64);
    }

    /// [`test_state`] pointed at a live upstream base_url, for
    /// end-to-end completions tests that exercise the real request path.
    fn test_state_with_upstream(admin_key: &str, base_url: String) -> AppState {
        test_state_tuned(admin_key, base_url, |_| {})
    }

    /// The real implementation behind the two helpers above.
    fn test_state_tuned(
        admin_key: &str,
        base_url: String,
        tune: impl FnOnce(&mut GatewayConfig),
    ) -> AppState {
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
        tune(&mut config);
        let keys_file = unique_temp_path("miser_test_keys", "json");
        let usage_file = unique_temp_path("miser_test_usage", "jsonl");
        let audit_file = unique_temp_path("miser_test_audit", "jsonl");
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
            auth: Arc::new(auth::AuthManager::new(keys_file).expect("fresh test key store loads")),
            quotas: Arc::new(auth::QuotaEnforcer::new()),
            usage: Arc::new(usage::UsageLedger::new(usage_file)),
            metrics: Arc::new(metrics::Metrics::new().unwrap()),
            audit: Arc::new(auth::AuditLog::new(audit_file)),
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
        spawn_mock_upstream_inner(
            delay,
            status,
            content_type,
            body_text,
            MockBehaviour::default(),
        )
        .await
    }

    /// As [`spawn_mock_upstream`], but every call from the `fail_from_call`-th
    /// onwards answers `503`. Lets a test make the *first* upstream call succeed
    /// and a later one -- a quality-gate escalation -- fail.
    async fn spawn_mock_upstream_failing_after(
        first_calls: u64,
        content_type: &'static str,
        body_text: Option<String>,
    ) -> MockUpstream {
        spawn_mock_upstream_inner(
            Duration::ZERO,
            StatusCode::OK,
            content_type,
            body_text,
            MockBehaviour {
                fail_from_call: Some(first_calls),
                tag_calls: false,
            },
        )
        .await
    }

    /// As [`spawn_mock_upstream`], but every reply also carries a distinct
    /// `x-request-id: mock-call-<n>`. That header is on the gateway's
    /// allow-list of pass-through response headers, so it makes the identity of
    /// the upstream call visible to the client -- which is how a test can tell
    /// whose headers accompany an escalated body.
    async fn spawn_mock_upstream_tagged(
        delay: Duration,
        status: StatusCode,
        content_type: &'static str,
        body_text: Option<String>,
    ) -> MockUpstream {
        spawn_mock_upstream_inner(
            delay,
            status,
            content_type,
            body_text,
            MockBehaviour {
                fail_from_call: None,
                tag_calls: true,
            },
        )
        .await
    }

    #[derive(Clone, Copy, Default)]
    struct MockBehaviour {
        /// Answer 503 from this call index onwards.
        fail_from_call: Option<u64>,
        /// Stamp a per-call `x-request-id`.
        tag_calls: bool,
    }

    async fn spawn_mock_upstream_inner(
        delay: Duration,
        status: StatusCode,
        content_type: &'static str,
        body_text: Option<String>,
        behaviour: MockBehaviour,
    ) -> MockUpstream {
        let requests: Arc<tokio::sync::Mutex<Vec<Value>>> = Arc::default();
        let call_index = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let fixed_body = body_text;
        let capture_for_handler = Arc::clone(&requests);
        let counter_for_handler = Arc::clone(&call_index);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let capture = Arc::clone(&capture_for_handler);
                let counter = Arc::clone(&counter_for_handler);
                let fixed_body = fixed_body.clone();
                async move {
                    let model = body
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let call = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    capture.lock().await.push(body);
                    tokio::time::sleep(delay).await;
                    if behaviour.fail_from_call.is_some_and(|from| call >= from) {
                        let payload = json!({
                            "error": {"message": "upstream unavailable", "code": 503}
                        })
                        .to_string();
                        let mut headers = axum::http::HeaderMap::new();
                        headers.insert(
                            axum::http::header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        );
                        return (StatusCode::SERVICE_UNAVAILABLE, headers, payload);
                    }
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
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert(
                        axum::http::header::CONTENT_TYPE,
                        HeaderValue::from_static(content_type),
                    );
                    if behaviour.tag_calls {
                        headers.insert(
                            "x-request-id",
                            HeaderValue::from_str(&format!("mock-call-{call}")).unwrap(),
                        );
                    }
                    (status, headers, text)
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

    /// A request that carries `tools` must never be answered from the
    /// semantic cache.
    ///
    /// The cache embeds only `messages[].content` and its equivalence judge
    /// compares only the last user message, so neither can see that two
    /// requests carry different tool contracts. A tool-less response holds no
    /// `tool_calls`, so serving it to an agentic request leaves the client's
    /// loop with nothing to dispatch -- and because the cache answers before
    /// classification, the tool-capable tier floor in `effective_tier` is
    /// never consulted either.
    #[tokio::test]
    async fn tool_bearing_request_is_not_served_from_the_semantic_cache() {
        let upstream = spawn_mock_upstream(
            Duration::from_millis(0),
            StatusCode::OK,
            "application/json",
            None,
        )
        .await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state.config.cache.semantic_enabled = true;
        let app = build_router(state);

        // Warm the semantic cache with a tool-less request.
        let tool_less = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what is the weather in Paris"}]
        });
        let (status, _, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(tool_less),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        // Byte-identical prompt text, but the client now offers a tool it
        // expects the model to call.
        let with_tool = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what is the weather in Paris"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    }
                }
            }]
        });
        let (status, headers, body) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(with_tool)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        assert_eq!(
            headers.get("x-miser-cache").and_then(|v| v.to_str().ok()),
            Some("miss"),
            "a tool-bearing request must not be answered from the semantic cache"
        );

        let requests = upstream.requests().await;
        assert_eq!(
            requests.len(),
            2,
            "the tool-bearing request must reach the upstream, not the cache"
        );
        assert!(
            requests[1].get("tools").is_some(),
            "the tool schema must be forwarded upstream"
        );
    }

    /// The mirror of the test above: excluding tool-bearing requests must not
    /// disable the semantic cache. This path had no end-to-end coverage at
    /// all, so it could have been broken -- or excluded wholesale by a
    /// regression -- without any test noticing.
    ///
    /// The two bodies differ only in whitespace, so `request_hash` (which
    /// keeps `messages` in the key) misses the exact-match cache, while
    /// `embed_prompt` normalizes the token bag and lands on cosine 1.0.
    #[tokio::test]
    async fn tool_less_rewrite_is_served_from_the_semantic_cache() {
        let upstream = spawn_mock_upstream(
            Duration::from_millis(0),
            StatusCode::OK,
            "application/json",
            None,
        )
        .await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state.config.cache.semantic_enabled = true;
        let app = build_router(state);

        let original = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what is the weather in paris"}]
        });
        let (status, _, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(original),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        let rewritten = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what  is  the  weather  in  paris"}]
        });
        let (status, headers, body) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(rewritten)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        assert_eq!(
            headers.get("x-miser-cache").and_then(|v| v.to_str().ok()),
            Some("hit-semantic"),
            "an equivalent tool-less rewrite must still be served from the cache"
        );
        assert_eq!(
            upstream.requests().await.len(),
            1,
            "the rewrite must not have reached the upstream"
        );
    }

    /// A pre-flight tier floor is not a quality escalation.
    ///
    /// `effective_tier` raises the tier for tools, low confidence,
    /// `response_format`, an agentic task, or tool history -- none of which
    /// involve the quality gate. Those requests used to increment
    /// `miser_quality_escalations_total`, so the counter meant "tier was
    /// raised" rather than "the Jev judge rejected a response", and
    /// double-counted the requests that genuinely were escalated. Agentic
    /// traffic floors to Hard by construction, so the metric was dominated by
    /// requests no judge ever saw.
    #[tokio::test]
    async fn tier_floor_does_not_count_as_a_quality_escalation() {
        let upstream = spawn_mock_upstream(
            Duration::from_millis(0),
            StatusCode::OK,
            "application/json",
            None,
        )
        .await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        // Keep the quality gate off so nothing can escalate for real: this
        // test is only about the floor path.
        state.config.quality.enabled = false;
        let app = build_router(state);

        // A trivial-sounding prompt that nevertheless carries a tool, so
        // policy floors the tier above the classifier's.
        let payload = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "noop"}}]
        });
        let (status, _, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        let (_, _, metrics) = send_raw(app, "GET", "/metrics", None, None).await;
        let metrics = String::from_utf8_lossy(&metrics);
        assert!(
            metrics.contains("miser_tier_floors_total 1"),
            "the tool floor should be recorded as a floor:\n{metrics}"
        );
        assert!(
            metrics.contains("miser_quality_escalations_total 0"),
            "a tier floor must not be reported as a quality escalation:\n{metrics}"
        );
    }

    /// A quality-gate escalation issues a *second* real upstream call, and
    /// both calls cost money. Neither was accounted for correctly:
    ///
    /// - The budget enforcer was charged once, before the gate ran, so the
    ///   escalated call -- the expensive one, at a higher tier -- was free.
    ///   The cap under-counted exactly when the gate was doing its job.
    /// - The usage ledger parsed `usage` from the *final* payload only, so
    ///   the discarded first call's tokens and cost vanished from per-key and
    ///   per-client spend rollups.
    ///
    /// Both calls are now charged, and the ledger sums their real usage
    /// while still writing one record per client request (so request counts
    /// stay correct).
    #[tokio::test]
    async fn escalated_request_charges_and_records_both_upstream_calls() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state
            .config
            .extra
            .insert("price_per_1k_usd".into(), json!(0.001));
        // Force the gate to reject and escalate. No judge is configured, so
        // `deterministic_quality` decides: the mock's "mock reply" scores
        // 0.65, which is below this threshold.
        state.config.quality.enabled = true;
        state.config.quality.minimum_score = 0.99;
        state.config.quality.escalate_on_failure = true;

        let (key_id, raw) = state
            .auth
            .create_key_full("escalating", "-", vec![], None, None, None)
            .unwrap();
        let quotas = Arc::clone(&state.quotas);
        let usage = Arc::clone(&state.usage);
        let app = build_router(state);

        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, headers, body) = send_raw(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            headers
                .get("x-miser-escalated")
                .and_then(|v| v.to_str().ok()),
            Some("true"),
            "the gate should have escalated this request"
        );
        assert_eq!(
            upstream.requests().await.len(),
            2,
            "an escalation must issue a second upstream call"
        );

        // The mock reports 3 prompt / 5 completion tokens per call, so both
        // calls together are 6 / 10. Recording only the escalated payload
        // would report half of that.
        let summary = usage.summarize(None, Some(&key_id), None);
        assert_eq!(
            summary.requests, 1,
            "one client request is one ledger record, even when it cost two calls"
        );
        assert_eq!(
            summary.prompt_tokens, 6,
            "the discarded first call's prompt tokens must still be billed"
        );
        assert_eq!(
            summary.completion_tokens, 10,
            "the discarded first call's completion tokens must still be billed"
        );

        // Each call is charged the conservative max_tokens estimate, so the
        // key's recorded spend must have doubled. A cap above one call's
        // charge but below two must therefore now read as exhausted.
        assert!(
            !quotas.check_budget(&key_id, 0.0008),
            "the escalated call was not charged to the budget"
        );
    }

    /// Streaming must not be a way to spend for free.
    ///
    /// Budget enforcement lived entirely inside the non-streaming branch, so a
    /// client that set `stream: true` was recorded in the usage ledger but
    /// never charged. A key with a `monthly_budget_usd` cap could therefore
    /// issue unlimited streaming requests and never exhaust it. The comment on
    /// the streaming `record_usage` call even claimed it recorded "the same
    /// monotonic estimate the budget enforcer uses", which was not true.
    #[tokio::test]
    async fn streaming_requests_are_charged_to_the_monthly_budget() {
        let sse = concat!(
            "data: {\"id\":\"mock-chunk\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let upstream = spawn_mock_upstream(
            Duration::ZERO,
            StatusCode::OK,
            "text/event-stream",
            Some(sse.into()),
        )
        .await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state
            .config
            .extra
            .insert("price_per_1k_usd".into(), json!(0.001));
        let raw = state
            .auth
            .create_key_with_quotas("streaming", "-", vec![], None, Some(0.0001), None)
            .unwrap();
        let app = build_router(state);
        let payload =
            json!({"model":"auto","stream":true,"messages":[{"role":"user","content":"hi"}]});

        let (first, body) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(first, StatusCode::OK, "{body}");

        let (second, body) = send(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(
            second,
            StatusCode::PAYMENT_REQUIRED,
            "a streaming request must be charged, so the cap is exhausted: {body}"
        );
        assert_eq!(
            upstream.requests().await.len(),
            1,
            "the budget-exhausted request must be rejected before the upstream"
        );
    }

    /// A session must not leak across API keys.
    ///
    /// The session tracker keeps the *highest* tier a conversation has seen and
    /// never lowers it, and its key was derived only from client-supplied data
    /// (`user`, else a hash of the first user message) with no tenant scoping.
    /// So two different API keys whose clients send the same `user` -- a
    /// routine thing for OpenAI-compatible clients to hardcode -- shared one
    /// session, and a cheap request on key B was served by the expensive tier
    /// key A had already pulled the session up to. That is both a cross-tenant
    /// cost leak and a way to hit a `403` for a tier key B is not allowed.
    #[tokio::test]
    async fn session_tier_does_not_leak_across_api_keys() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state.config.session.enabled = true;
        let (a_id, key_a) = state
            .auth
            .create_key_full("tenant-a", "-", vec![], None, None, None)
            .unwrap();
        let (_b_id, key_b) = state
            .auth
            .create_key_full("tenant-b", "-", vec![], None, None, None)
            .unwrap();
        let app = build_router(state);

        // Tenant A runs an expensive conversation under a shared `user` id.
        let expensive = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "@route:hard\ndesign a system"}],
            "user": "shared-client-id"
        });
        let (status, headers, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&key_a),
            Some(expensive),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            headers.get("x-miser-tier").and_then(|v| v.to_str().ok()),
            Some("hard"),
            "tenant A should have been routed to the hard tier"
        );

        // Tenant B sends a trivial request under the *same* `user` id.
        let trivial = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "user": "shared-client-id"
        });
        let (status, headers, body) = send_raw(
            app,
            "POST",
            "/v1/chat/completions",
            Some(&key_b),
            Some(trivial),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            headers.get("x-miser-tier").and_then(|v| v.to_str().ok()),
            Some("trivial"),
            "tenant B must not inherit tenant A's session tier (key_a={a_id})"
        );
    }

    /// An escalated response must ship the escalated call's own status and
    /// headers, not the discarded attempt's.
    ///
    /// The gateway snapshots `settled_status` / `settled_headers` before the
    /// quality gate runs and then swaps in the retry's body, but never refreshes
    /// the snapshot. So a client asking for a retry got the retry's bytes
    /// labelled with the first call's `x-request-id` and timing headers, and
    /// both cache layers stored that mismatched pair to replay later. Not
    /// protocol-breaking -- `content-length` is not on the pass-through
    /// allow-list, so axum recomputes it -- but the response stops describing
    /// itself, and the mislabelled pair is persisted.
    #[tokio::test]
    async fn escalated_response_ships_the_escalated_calls_headers() {
        let upstream =
            spawn_mock_upstream_tagged(Duration::ZERO, StatusCode::OK, "application/json", None)
                .await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state.config.quality.enabled = true;
        state.config.quality.minimum_score = 0.99;
        state.config.quality.escalate_on_failure = true;
        let app = build_router(state);

        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, headers, body) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(payload)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            headers
                .get("x-miser-escalated")
                .and_then(|v| v.to_str().ok()),
            Some("true"),
            "the gate should have escalated this request"
        );
        assert_eq!(upstream.requests().await.len(), 2, "expected a retry");
        assert_eq!(
            headers.get("x-request-id").and_then(|v| v.to_str().ok()),
            Some("mock-call-1"),
            "the body came from the second call, so its headers must too"
        );
    }

    /// A malformed `PATCH` must be rejected, never silently interpreted as
    /// "remove this restriction".
    ///
    /// The handler built `Option<Option<T>>` with `body.get(..).map(Value::as_*)`,
    /// so a value of the wrong JSON type produced `Some(None)`, and
    /// `update_key_quotas` assigns that inner `None` -- clearing the field --
    /// while the handler answers `200 {"updated": true}`. So
    /// `{"monthly_budget_usd": "10.00"}` (a stringified number, which is what a
    /// shell-quoted curl or a client that stringifies numerics sends) silently
    /// deleted the spend cap, and `{"rate_limit_rpm": "60"}` deleted the rate
    /// limit. `{"allowed_tiers": "hard"}` became an empty allowlist, which
    /// `completions_inner` reads as "all tiers allowed". Every one of these
    /// removed a cost or safety control and reported success.
    #[tokio::test]
    async fn update_key_rejects_wrongly_typed_restrictions() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let state = test_state_with_upstream("secret-admin", upstream.base_url.clone());
        let (id, _) = state
            .auth
            .create_key_full("typed", "-", vec![], Some(60), Some(10.0), None)
            .unwrap();
        let app = build_router(state);

        for (field, bad_value) in [
            ("monthly_budget_usd", json!("10.00")),
            ("rate_limit_rpm", json!("60")),
            ("allowed_tiers", json!("hard")),
        ] {
            let (status, _, body) = send_raw(
                app.clone(),
                "PATCH",
                &format!("/admin/keys/{id}"),
                Some("secret-admin"),
                Some(json!({ field: bad_value })),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{field} = {bad_value} must be rejected, got: {}",
                String::from_utf8_lossy(&body)
            );
        }

        // Nothing was silently dropped along the way.
        let (status, _, body) = send_raw(
            app,
            "GET",
            &format!("/admin/keys/{id}"),
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let key: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            key["monthly_budget_usd"],
            json!(10.0),
            "the spend cap must survive a rejected PATCH"
        );
        assert_eq!(
            key["rate_limit_rpm"],
            json!(60),
            "the rate limit must survive a rejected PATCH"
        );
    }

    /// An out-of-range `rate_limit_rpm` must be rejected too, not truncated.
    /// `4294967396 as u32` is `100`, so a typo silently produced a 100 rpm cap.
    #[tokio::test]
    async fn update_key_rejects_out_of_range_rate_limit() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let state = test_state_with_upstream("secret-admin", upstream.base_url.clone());
        let (id, _) = state
            .auth
            .create_key_full("range", "-", vec![], None, None, None)
            .unwrap();
        let app = build_router(state);

        let (status, _, body) = send_raw(
            app,
            "PATCH",
            &format!("/admin/keys/{id}"),
            Some("secret-admin"),
            Some(json!({"rate_limit_rpm": 4_294_967_396u64})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "an rpm above u32::MAX must be rejected, not truncated: {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// `PATCH` must be able to revoke a key and set an expiry. Both fields used
    /// to be ignored while the API answered `{"updated": true}`, so the only
    /// way to neutralise a leaked key was `DELETE` -- which discards the
    /// record along with the key's usage history.
    #[tokio::test]
    async fn update_key_can_revoke_and_set_expiry() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let state = test_state_with_upstream("secret-admin", upstream.base_url.clone());
        let (id, raw) = state
            .auth
            .create_key_full("owner", "-", vec![], None, None, None)
            .unwrap();
        let app = build_router(state);

        let completions = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(completions.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // Revoke: the key must be refused, and distinctly so.
        let (status, _, body) = send_raw(
            app.clone(),
            "PATCH",
            &format!("/admin/keys/{id}"),
            Some("secret-admin"),
            Some(json!({"active": false})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(completions.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a revoked key must be refused: {body}"
        );

        // Expiry, in the past so it is immediately effective.
        let (status, _, body) = send_raw(
            app.clone(),
            "PATCH",
            &format!("/admin/keys/{id}"),
            Some("secret-admin"),
            Some(json!({"expires_at": 1u64})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(completions),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "an expired key must be refused: {body}"
        );

        // Still revoked: expiry did not quietly restore it.
        let (status, _, body) = send_raw(
            app,
            "GET",
            &format!("/admin/keys/{id}"),
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let key: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(key["active"], json!(false), "still revoked");
        assert_eq!(key["expires_at"], json!(1), "expiry was recorded");
    }

    /// A failing quality-gate escalation must count against the catalog
    /// failover machinery.
    ///
    /// `report_upstream_outcome` was only ever called for the *first* attempt.
    /// The escalated retry is a real upstream call that can fail on its own --
    /// and it runs against a different, more expensive tier's active model --
    /// yet its outcome was discarded. So the model the gateway escalated *to*
    /// could never accumulate failures and could never fail over, no matter how
    /// many escalating requests it broke. Every one of those requests kept
    /// burning a first attempt plus a failing escalation against a model the
    /// failover logic was supposed to protect.
    #[tokio::test]
    async fn failed_escalation_is_reported_to_the_failover_counter() {
        // First call succeeds, the escalation (second call) fails with 503.
        let upstream = spawn_mock_upstream_failing_after(1, "application/json", None).await;
        let app = build_router(test_state_with(
            "secret-admin",
            upstream.base_url.clone(),
            |config| {
                config.routing.mode = miser_types::RoutingMode::Catalog;
                config.routing.failover_threshold = 1;
                // Without this the router loads the relative, gitignored
                // `catalog/models.json`, i.e. whatever snapshot happens to exist
                // in the working directory. That file is developer-local, so the
                // test would depend on the checkout and could promote real models
                // rather than the config seed.
                config.routing.cache_path = Some(
                    unique_temp_path("miser_test_catalog_seed", "json")
                        .display()
                        .to_string(),
                );
                config.quality.enabled = true;
                config.quality.minimum_score = 0.99;
                config.quality.escalate_on_failure = true;
            },
        ));

        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        // An admin key is configured, so the completions path is no longer in
        // open-access mode and the request must authenticate.
        let (status, _, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("secret-admin"),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            upstream.requests().await.len(),
            2,
            "the gate should have escalated, and the retry should have failed"
        );

        let (status, _, body) =
            send_raw(app, "GET", "/admin/catalog", Some("secret-admin"), None).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let catalog: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            catalog["enabled"],
            json!(true),
            "catalog routing must be on"
        );
        let simple = catalog["tiers"]
            .as_array()
            .expect("tiers array")
            .iter()
            .find(|tier| tier["tier"] == json!("simple"))
            .expect("simple tier present")
            .clone();
        assert_eq!(
            simple["consecutive_failures"],
            json!(1),
            "the escalated call's 503 must be counted against the escalated tier: {simple}"
        );
    }

    /// The provider HTTP client must have a timeout.
    ///
    /// `ProviderConfig` was built with only `base_url` and `api_key`, so
    /// `timeout_seconds` stayed `None` and reqwest was constructed with *no*
    /// timeout at all. Every upstream call was then bounded only by the axum
    /// `TimeoutLayer` -- which does not cover every caller: the startup catalog
    /// refresh is a spawned task outside the request stack, so a hung
    /// `/models` response could keep it waiting indefinitely, and an admin
    /// refresh held a concurrency permit until the 300s layer fired.
    #[test]
    fn provider_client_gets_a_timeout() {
        let base: GatewayConfig =
            toml::from_str(include_str!("../../../config/miser.toml")).unwrap();

        let default_config = provider_client_config(&base, "sk-test");
        assert_eq!(
            default_config.timeout_seconds,
            Some(DEFAULT_REQUEST_TIMEOUT_MS.div_ceil(1000)),
            "the provider must fall back to the gateway's request deadline"
        );

        // An explicit ceiling is honoured, and reaches the transport.
        let mut tuned = base.clone();
        tuned
            .extra
            .insert("request_timeout_ms".into(), json!(45_000u64));
        assert_eq!(
            provider_client_config(&tuned, "sk-test").timeout_seconds,
            Some(45)
        );

        // Sub-second deadlines must round *up*, never down: flooring makes the
        // transport give up before the request layer, and flooring all the way
        // to zero makes reqwest fail every request instantly.
        let mut fractional = base.clone();
        fractional
            .extra
            .insert("request_timeout_ms".into(), json!(2_500u64));
        assert_eq!(
            provider_client_config(&fractional, "sk-test").timeout_seconds,
            Some(3),
            "a 2.5s deadline must not become a 2s transport timeout"
        );
        let mut tiny = base.clone();
        tiny.extra
            .insert("request_timeout_ms".into(), json!(500u64));
        assert_eq!(
            provider_client_config(&tiny, "sk-test").timeout_seconds,
            Some(1),
            "a sub-second deadline must not floor to an immediate failure"
        );

        // A nonsense value must not produce a zero timeout, which reqwest
        // treats as "give up immediately".
        let mut broken = base.clone();
        broken
            .extra
            .insert("request_timeout_ms".into(), json!(0u64));
        assert_eq!(
            provider_client_config(&broken, "sk-test").timeout_seconds,
            Some(DEFAULT_REQUEST_TIMEOUT_MS.div_ceil(1000)),
            "a zero/negative ceiling must fall back, not fail every request instantly"
        );
    }

    /// An unrecognised `window` must not silently mean "no lower bound".
    ///
    /// `window_since` mapped every unknown value to `None`, so `window=90d` --
    /// or any typo -- became a full scan of an append-only ledger that is never
    /// rotated. Only an explicit `all` may do that.
    #[test]
    fn unknown_usage_window_falls_back_to_a_bounded_one() {
        // `window_since` reads the clock itself, so bracket rather than snapshot
        // it: a second boundary falling between the two calls would otherwise
        // fail the test.
        let before = unix_now();
        let day = window_since(Some("24h")).expect("24h is bounded");
        let after = unix_now();
        assert!(
            (before.saturating_sub(86_400)..=after.saturating_sub(86_400)).contains(&day),
            "24h should resolve to now-86400 within the bracket, got {day}"
        );
        // Ordering, not exact values: a shorter window has a *later* lower
        // bound, so 24h > 7d > 30d.
        let week = window_since(Some("7d")).expect("7d is bounded");
        let month = window_since(Some("30d")).expect("30d is bounded");
        assert!(day > week, "24h is a later lower bound than 7d");
        assert!(week > month, "7d is a later lower bound than 30d");
        assert_eq!(
            window_since(None),
            window_since(Some("30d")),
            "the default must be the same bounded 30d window"
        );
        assert_eq!(
            window_since(Some("all")),
            None,
            "only an explicit `all` may scan the whole ledger"
        );
        for unknown in ["90d", "forever", "", "30D", "-1"] {
            assert!(
                window_since(Some(unknown)).is_some(),
                "window={unknown:?} must fall back to a bounded window"
            );
        }
    }

    /// Deleting a key must release its quota state.
    ///
    /// `QuotaEnforcer`'s `windows` and `spend` maps only ever grew -- entries
    /// were inserted by `or_insert` and never removed. `delete_key` reaches
    /// `AuthManager`, which has no handle on the enforcer, so every key id that
    /// ever carried a rate limit or ever spent money left a permanent entry
    /// behind for the life of the process. On a long-lived gateway that issues
    /// per-tenant keys the maps grow monotonically, and a recycled id would
    /// inherit a stranger's spend and rate-limit window.
    #[tokio::test]
    async fn deleting_a_key_releases_its_quota_state() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("secret-admin", upstream.base_url.clone());
        state
            .config
            .extra
            .insert("price_per_1k_usd".into(), json!(0.001));
        let (id, raw) = state
            .auth
            .create_key_full("ephemeral", "-", vec![], None, Some(0.0001), None)
            .unwrap();
        let quotas = Arc::clone(&state.quotas);
        let app = build_router(state);

        // One request exhausts the (deliberately tiny) cap.
        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, body) = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some(&raw),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            !quotas.check_budget(&id, 0.0001),
            "the cap should be exhausted before the delete"
        );

        let (status, _, body) = send_raw(
            app,
            "DELETE",
            &format!("/admin/keys/{id}"),
            Some("secret-admin"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert!(
            quotas.check_budget(&id, 0.0001),
            "a deleted key must not keep its spend on the books forever"
        );
    }

    /// Traffic authenticated by the shared admin key must still be accounted
    /// for.
    ///
    /// The admin key is deliberately accepted on `/v1/chat/completions` so an
    /// operator can smoke-test without minting a key, but it left
    /// `authenticated_key` as `None`, and `record_usage` returns immediately on
    /// `None`. So every such request was served, billed to the operator's
    /// upstream account, and then vanished: `GET /admin/usage/summary` reported
    /// `requests: 0, cost_usd: 0.0` for all of it. The untracked spend was
    /// invisible in the very dashboard built to detect it.
    ///
    /// The quota and tier gates are unchanged in effect -- the synthetic key
    /// carries no rate limit, no budget and no tier allowlist, so all three are
    /// no-ops, exactly as they were when the key was `None`.
    #[tokio::test]
    async fn admin_key_traffic_is_attributed_in_the_usage_ledger() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("secret-admin", upstream.base_url.clone());
        state
            .config
            .extra
            .insert("price_per_1k_usd".into(), json!(0.001));
        let usage = Arc::clone(&state.usage);
        let app = build_router(state);

        let payload = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let (status, _, body) = send_raw(
            app,
            "POST",
            "/v1/chat/completions",
            Some("secret-admin"),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        let summary = usage.summarize(None, None, None);
        assert_eq!(
            summary.requests, 1,
            "admin-key traffic must appear in the ledger"
        );
        assert!(
            summary.cost_usd > 0.0,
            "admin-key traffic must contribute to reported cost, got {}",
            summary.cost_usd
        );
        assert_eq!(
            summary.by_key.len(),
            1,
            "it must be attributed somewhere queryable: {:?}",
            summary.by_key
        );
    }

    /// A stream that starts healthy and then dies must count as an upstream
    /// failure.
    ///
    /// Failover was decided from the response *headers* alone, so a model that
    /// reliably answered `200` and then dropped the connection mid-SSE was
    /// recorded as a success every time. It could never accumulate failures and
    /// so never failed over: clients kept receiving truncated streams from a
    /// model the failover logic was supposed to have retired. Long generations
    /// and provider-side read timeouts make exactly this the common streaming
    /// failure, not the rare one.
    ///
    /// The promotion necessarily lands after the headers are already on the
    /// wire, so it protects the *next* request rather than rescuing this one --
    /// which is the best available once bytes have been sent.
    #[tokio::test]
    async fn a_stream_that_dies_midway_is_counted_as_a_failure() {
        // Announce a healthy chunked SSE response, deliver one event, then hang
        // up without the terminating zero-length chunk. Chunked framing is what
        // makes that an error rather than a clean end-of-body: without a
        // `Content-Length`, HTTP treats connection close as a normal end.
        let event =
            "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n";
        let truncated_sse = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}",
            event.len(),
            event
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(truncated_sse.as_bytes()).await;
                let _ = sock.flush().await;
                // Close without `0\r\n\r\n`.
            }
        });

        let state = test_state_with("secret-admin", format!("http://{addr}"), |config| {
            config.routing.mode = miser_types::RoutingMode::Catalog;
            config.routing.failover_threshold = 1;
            // Without this the router loads the relative, gitignored
            // `catalog/models.json`, i.e. whatever snapshot happens to exist in
            // the working directory. That file is developer-local, so the test
            // would depend on the checkout and could promote real models rather
            // than the config seed.
            config.routing.cache_path = Some(
                unique_temp_path("miser_test_catalog_seed", "json")
                    .display()
                    .to_string(),
            );
        });
        let catalog = Arc::clone(&state.catalog);
        let app = build_router(state);

        let payload =
            json!({"model":"auto","stream":true,"messages":[{"role":"user","content":"hi"}]});
        // Not `send_raw`: it unwraps the collected body, and the whole point of
        // this test is that collecting the body fails.
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer secret-admin")
            .body(Body::from(payload.to_string()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "headers arrive before the failure"
        );
        // Draining surfaces the error to the client, and by then the failure
        // must have been counted.
        let _ = response.into_body().collect().await;

        // The body error surfaces while the client is reading, and by then the
        // failure must have been counted.
        let mut counted = false;
        for _ in 0..50 {
            if catalog.summary_json()["tiers"]
                .as_array()
                .is_some_and(|tiers| {
                    tiers
                        .iter()
                        .any(|tier| tier["consecutive_failures"] != json!(0))
                })
            {
                counted = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            counted,
            "a stream that dies mid-flight must be reported to the failover counter: {}",
            catalog.summary_json()
        );
    }

    /// A rejected upstream call must not be billed to the key's budget.
    ///
    /// The non-streaming error path falls through to the same block that
    /// charges for streaming, so the charge introduced there also fired for
    /// 4xx/5xx responses. A client could drain its own monthly cap with requests
    /// the provider refused, and a transient upstream incident would leave the
    /// key hard-`402` for the rest of the month long after the provider
    /// recovered -- which is the opposite of what a budget cap is for.
    #[tokio::test]
    async fn a_rejected_upstream_call_is_not_charged() {
        let upstream = spawn_mock_upstream_failing_after(0, "application/json", None).await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state
            .config
            .extra
            .insert("price_per_1k_usd".into(), json!(0.001));
        let (_id, raw) = state
            .auth
            .create_key_full("rejected", "-", vec![], None, Some(0.0001), None)
            .unwrap();
        let quotas = Arc::clone(&state.quotas);
        let app = build_router(state);

        for _ in 0..3 {
            let (status, body) = send(
                app.clone(),
                "POST",
                "/v1/chat/completions",
                Some(&raw),
                Some(json!({"model":"auto","messages":[{"role":"user","content":"hi"}]})),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "the upstream error must still pass through: {body}"
            );
        }
        assert!(
            quotas.check_budget(&_id, 0.0001),
            "requests the provider rejected must not count against the budget"
        );
    }

    /// Tool *history* must exclude a request from the semantic cache too, not
    /// just a declared `tools` array.
    ///
    /// A client resuming an agentic loop commonly replays the transcript
    /// without re-declaring its tools, so the guard on `request.tools` alone let
    /// the original bug through one axis over: the tool-result text is folded
    /// into the embedding, making the turn look similar, while
    /// `QualityJudge::equivalent` still only compares the two last user
    /// messages. The client would get a prose answer with no `tool_calls` on a
    /// turn it expects a call from -- and `has_tool_history`'s Hard floor would
    /// be skipped too, since the cache answers before `effective_tier`.
    #[tokio::test]
    async fn tool_history_is_not_served_from_the_semantic_cache() {
        let upstream =
            spawn_mock_upstream(Duration::ZERO, StatusCode::OK, "application/json", None).await;
        let mut state = test_state_with_upstream("", upstream.base_url.clone());
        state.config.cache.semantic_enabled = true;
        let app = build_router(state);

        // Turn 1: a plain question, no tools at all.
        let plain = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what is the weather in Paris"}]
        });
        let (status, _, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(plain),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        // Turn 2: the agentic loop resumes, replaying the transcript. Note the
        // absence of a `tools` array -- only the history marks it agentic.
        let resumed = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "what is the weather in Paris"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{}"}
                }]},
                // Empty content on purpose. The embedding is a bag of word
                // tokens over the joined message text, so a tool result
                // carrying real words adds tokens the first turn does not have
                // and drags the cosine below `similarity_threshold` -- at which
                // point this test would pass with the `has_tool_history` guard
                // deleted, and verify nothing. An empty result keeps the two
                // embeddings identical, so without the guard this turn really
                // would be served from the cache.
                {"role": "tool", "tool_call_id": "call_1", "content": ""}
            ]
        });
        let (status, headers, body) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(resumed)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            headers.get("x-miser-cache").and_then(|v| v.to_str().ok()),
            Some("miss"),
            "a turn carrying tool history must not be answered from the semantic cache"
        );
        assert_eq!(
            upstream.requests().await.len(),
            2,
            "the agentic turn must reach the upstream, not the cache"
        );
    }

    /// A tool-calling response must not be stored in the semantic cache.
    ///
    /// The lookup guard refuses to serve tool-bearing *requests*, but the store
    /// was unconditional. So a response carrying `tool_calls` was written and
    /// could later be replayed to a plain, tool-free turn — the mirror of the
    /// bug the guard fixed. The client would receive a `tool_calls` entry naming
    /// tools it never declared, and no tool result would ever arrive to
    /// continue the turn.
    #[tokio::test]
    async fn a_tool_calling_response_is_not_stored_in_the_semantic_cache() {
        // The mock has to vary its answer by request, or the second turn gets
        // the same body straight from upstream and the test proves nothing
        // about the cache. A tool-bearing request gets a `tool_calls` reply; a
        // tool-free one gets plain prose.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if request.contains("\"tools\"") {
                    json!({
                        "id": "mock-completion", "object": "chat.completion",
                        "model": "test/trivial",
                        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                            "role": "assistant", "content": Value::Null,
                            "tool_calls": [{"id": "call_1", "type": "function",
                                "function": {"name": "get_weather", "arguments": "{}"}}]
                        }}]
                    })
                } else {
                    json!({
                        "id": "mock-completion", "object": "chat.completion",
                        "model": "test/trivial",
                        "choices": [{"index": 0, "finish_reason": "stop", "message": {
                            "role": "assistant", "content": "It is 21C in Paris."
                        }}]
                    })
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes()).await;
            }
        });

        let mut state = test_state_with_upstream("", format!("http://{addr}"));
        state.config.cache.semantic_enabled = true;
        let app = build_router(state);

        // A tool-bearing request: the lookup guard lets it through to the
        // upstream, and the upstream answers with a tool call.
        let asking = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what is the weather in Paris"}],
            "tools": [{
                "type": "function",
                "function": {"name": "get_weather", "parameters": {"type": "object"}}
            }]
        });
        let (status, _, body) = send_raw(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            None,
            Some(asking),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let served: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            served["choices"][0]["message"]["tool_calls"].is_array(),
            "the upstream's tool call should have been relayed"
        );

        // A later tool-free turn with the same text. The embedding is built from
        // message content only, so this turn and the one above are *identical*
        // to the cache -- cosine 1.0, well past any threshold. Without the store
        // guard it is served the tool call.
        let plain = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "what is the weather in Paris"}]
        });
        let (status, headers, body) =
            send_raw(app, "POST", "/v1/chat/completions", None, Some(plain)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_ne!(
            headers.get("x-miser-cache").and_then(|v| v.to_str().ok()),
            Some("hit-semantic"),
            "a tool-free turn must not be served a cached tool call"
        );
        let served: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            served["choices"][0]["message"]["tool_calls"].is_null(),
            "a tool-free turn must never receive tool_calls"
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
