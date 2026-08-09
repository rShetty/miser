use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode},
    response::Response,
    routing::{get, post},
};
use clap::Parser;
use miser_classifier::Classifier;
use miser_policy::PolicyEngine;
use miser_provider::{Provider, ProviderConfig, safe_response_headers};
use miser_types::{ChatCompletionRequest, ComplexityTier, GatewayConfig};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use uuid::Uuid;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
    let args = Args::parse();
    let config: GatewayConfig = toml::from_str(&tokio::fs::read_to_string(&args.config).await?)?;
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
    let state = AppState {
        classifier: Arc::new(Classifier::new(config.classifier.clone())?),
        policy: PolicyEngine::new(config.clone()),
        provider: Provider::new(provider_config)?,
        config,
    };
    let address = format!("{}:{}", state.config.host, state.config.port);
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(completions))
        .with_state(Arc::new(state))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());
    let listener = TcpListener::bind(&address).await?;
    tracing::info!(address = %address, "miser gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
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
    if let Some(expected) = &state.config.api_key {
        let supplied = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if supplied != format!("Bearer {expected}") {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":{"message":"unauthorized"}})),
            ));
        }
    }
    let request_id = Uuid::new_v4().to_string();
    let classification = state
        .classifier
        .classify(&request)
        .await
        .map_err(internal)?;
    let route = state.policy.select(&classification).map_err(internal)?;
    request.model = route.model.clone();
    if let Some(max_tokens) = route.max_tokens {
        request.max_tokens = Some(max_tokens);
    }
    if let Some(temperature) = route.temperature {
        request.temperature = Some(temperature);
    }
    let body = serde_json::to_value(request).map_err(internal)?;
    let upstream = state.provider.forward(body, None).await.map_err(internal)?;
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
        .header(
            "x-miser-tier",
            HeaderValue::from_str(&format_tier(classification.tier)).unwrap(),
        )
        .header(
            "x-miser-model",
            HeaderValue::from_str(&route.model).unwrap(),
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
