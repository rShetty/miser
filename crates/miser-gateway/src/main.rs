mod auth;
mod cache;
mod session;
mod validate;

use axum::{
    Json, Router,
    error_handling::HandleErrorLayer,
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    response::Response,
    routing::{delete, get, patch, post},
};
use clap::Parser;
use miser_classifier::Classifier;
use miser_policy::PolicyEngine;
use miser_provider::{Provider, ProviderConfig, safe_response_headers};
use miser_types::{ChatCompletionRequest, ComplexityTier, GatewayConfig};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
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
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

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
    admin_key: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
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
    let state = AppState {
        classifier: Arc::new(Classifier::new(config.classifier.clone())?),
        policy: PolicyEngine::new(config.clone()),
        provider: Provider::new(provider_config)?,
        cache: Arc::new(cache::ResponseCache::new(10000, 300)),
        session: Arc::new(session::SessionTracker::new(
            config.session.max_entries,
            config.session.ttl_seconds,
        )),
        auth: Arc::new(auth::AuthManager::new(std::path::PathBuf::from(auth_path))),
        quotas: Arc::new(auth::QuotaEnforcer::new()),
        admin_key,
        config,
    };
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
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(completions))
        .route("/admin/keys", post(create_key))
        .route("/admin/keys", get(list_keys))
        .route("/admin/keys/{id}", get(get_key))
        .route("/admin/keys/{id}", patch(update_key))
        .route("/admin/keys/{id}", delete(delete_key))
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

async fn completions(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, Json<Value>)> {
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
    let body = serde_json::to_value(&request).map_err(internal)?;
    let cache_key = cache::request_hash(&body);
    if let Some((cached_body, cached_status, cached_headers)) = state.cache.get(cache_key) {
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
    let route = state
        .policy
        .select(&request, &classification)
        .map_err(internal)?;
    let effective_tier = state.policy.effective_tier(&request, &classification);
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
    let stream_requested = request.stream.unwrap_or(false);
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
    let upstream = state
        .provider
        .forward(body.clone(), None)
        .await
        .map_err(internal)?;
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
        state.cache.store(
            cache_key,
            payload.clone(),
            original_status,
            original_headers.clone(),
        );
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
    match state.auth.create_key_with_quotas(
        owner,
        allowed_tiers,
        rate_limit_rpm,
        monthly_budget_usd,
    ) {
        Ok(raw_key) => Ok(Json(json!({
            "key": raw_key,
            "message": "Store this key securely. It will not be shown again."
        }))),
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
        Ok(keys) => Ok(Json(json!({"keys": keys}))),
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
    match state
        .auth
        .update_key_quotas(&id, allowed_tiers, rate_limit_rpm, monthly_budget_usd)
    {
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
        Ok(_) => Ok(Json(json!({"message": "key deleted"}))),
        Err(auth::AuthError::NotFound) => {
            Err(auth::json_error("key not found", StatusCode::NOT_FOUND))
        }
        Err(_) => Err(auth::json_error(
            "failed to delete key",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state(admin_key: &str) -> AppState {
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
                base_url: "http://127.0.0.1:9".to_string(),
                api_key: Some("test".to_string()),
                ..Default::default()
            })
            .unwrap(),
            cache: Arc::new(cache::ResponseCache::new(100, 60)),
            session: Arc::new(session::SessionTracker::new(100, 60)),
            auth: Arc::new(auth::AuthManager::new(keys_file)),
            quotas: Arc::new(auth::QuotaEnforcer::new()),
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
            .create_key_with_quotas("tester", vec![], None, None)
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
            .create_key_with_quotas("limited", vec![], Some(1), None)
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
            .create_key_with_quotas("gated", vec!["hard".to_string()], None, None)
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
