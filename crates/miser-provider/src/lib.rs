use reqwest::{
    Client, Response, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::time::Duration;
use thiserror::Error;

const SAFE_RESPONSE_HEADERS: [&str; 5] = [
    "content-type",
    "cache-control",
    "content-encoding",
    "x-request-id",
    "openrouter-processing-time",
];

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("invalid provider URL: {0}")]
    Url(#[from] reqwest::Error),
    #[error("upstream returned {status}: {body}")]
    Upstream { status: StatusCode, body: String },
    #[error("invalid JSON response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("classifier response did not contain JSON content")]
    MissingClassifierContent,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider_preferences: Option<Value>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Clone)]
pub struct Provider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: Option<String>,
    provider_preferences: Option<Value>,
}

pub type OpenRouterProvider = Provider;

impl Provider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let mut builder = Client::builder();
        if let Some(seconds) = config.timeout_seconds {
            builder = builder.timeout(Duration::from_secs(seconds));
        }
        Ok(Self {
            client: builder.build()?,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            api_key: config.api_key,
            model: config.model,
            provider_preferences: config.provider_preferences,
        })
    }

    pub fn with_client(config: ProviderConfig, client: Client) -> Self {
        Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            api_key: config.api_key,
            model: config.model,
            provider_preferences: config.provider_preferences,
        }
    }

    pub fn rewrite_body(&self, mut body: Value, requested_model: Option<&str>) -> Value {
        if let Value::Object(ref mut object) = body {
            if let Some(model) = self.model.as_deref().or(requested_model) {
                object.insert("model".to_owned(), Value::String(model.to_owned()));
            }
            if let Some(preferences) = &self.provider_preferences {
                merge_provider_preferences(object, preferences);
            }
        }
        body
    }

    pub async fn forward(
        &self,
        body: Value,
        requested_model: Option<&str>,
    ) -> Result<Response, ProviderError> {
        let request = self
            .client
            .post(format!("{}/chat/completions", self.base_url));
        let response = self
            .authorized(request)
            .json(&self.rewrite_body(body, requested_model))
            .send()
            .await?;
        Ok(response)
    }

    pub async fn list_models(&self) -> Result<Value, ProviderError> {
        let response = self
            .authorized(self.client.get(format!("{}/models", self.base_url)))
            .send()
            .await?;
        parse_json_response(response).await
    }

    pub async fn chat_json<T: DeserializeOwned>(&self, body: Value) -> Result<T, ProviderError> {
        let response = self
            .authorized(
                self.client
                    .post(format!("{}/chat/completions", self.base_url)),
            )
            .json(&body)
            .send()
            .await?;
        let payload: Value = parse_json_response(response).await?;
        let content = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or(ProviderError::MissingClassifierContent)?;
        Ok(serde_json::from_str(content)?)
    }

    pub async fn classifier_chat<T: DeserializeOwned>(
        &self,
        messages: Vec<Value>,
        model: &str,
    ) -> Result<T, ProviderError> {
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": 0,
            "response_format": { "type": "json_object" }
        });
        self.chat_json(body).await
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }
}

fn merge_provider_preferences(object: &mut Map<String, Value>, preferences: &Value) {
    let Some(preferences) = preferences.as_object() else {
        object.insert("provider".to_owned(), preferences.clone());
        return;
    };
    let provider = object
        .entry("provider")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(existing) = provider {
        for (key, value) in preferences {
            existing.insert(key.clone(), value.clone());
        }
    } else {
        *provider = Value::Object(preferences.clone());
    }
}

async fn parse_json_response(response: Response) -> Result<Value, ProviderError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(ProviderError::Upstream { status, body });
    }
    Ok(serde_json::from_str(&body)?)
}

pub fn safe_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut safe = HeaderMap::new();
    for name in SAFE_RESPONSE_HEADERS {
        let name = HeaderName::from_static(name);
        if let Some(value) = headers.get(&name) {
            safe.insert(name, value.clone());
        }
    }
    safe
}

pub fn content_type_json() -> HeaderValue {
    HeaderValue::from_static("application/json")
}

pub fn authorization_header(key: &str) -> Result<HeaderValue, reqwest::header::InvalidHeaderValue> {
    HeaderValue::from_str(&format!("Bearer {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    #[test]
    fn rewrites_model_and_preserves_arbitrary_fields() {
        let provider = Provider::with_client(
            ProviderConfig {
                base_url: "http://localhost".into(),
                model: Some("target/model".into()),
                provider_preferences: Some(
                    serde_json::json!({"sort": "price", "allow_fallbacks": true}),
                ),
                ..Default::default()
            },
            Client::new(),
        );
        let body = provider.rewrite_body(serde_json::json!({"model":"old", "stream":true, "metadata":{"x":1}, "provider":{"sort":"latency"}}), None);
        assert_eq!(body["model"], "target/model");
        assert_eq!(body["metadata"]["x"], 1);
        assert_eq!(body["provider"]["sort"], "price");
        assert_eq!(body["provider"]["allow_fallbacks"], true);
    }

    #[test]
    fn optional_auth_is_only_added_when_configured() {
        let provider = Provider::with_client(
            ProviderConfig {
                base_url: "http://localhost".into(),
                ..Default::default()
            },
            Client::new(),
        );
        let request = provider
            .authorized(Client::new().get("http://localhost"))
            .build()
            .unwrap();
        assert!(request.headers().get(AUTHORIZATION).is_none());
        assert_eq!(
            content_type_json(),
            HeaderValue::from_static("application/json")
        );
    }

    #[test]
    fn filters_response_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream"),
        );
        headers.insert("set-cookie", HeaderValue::from_static("secret"));
        assert!(safe_response_headers(&headers).contains_key("content-type"));
        assert!(!safe_response_headers(&headers).contains_key("set-cookie"));
    }
}

/// `parse_json_response` decides whether an upstream body is an answer or an
/// error, and three mutants survived because nothing exercised it:
///
/// - `delete !` on `if !status.is_success()` inverts the check, so a 500 is
///   parsed as JSON and a 200 is reported as `Upstream`. The error path and the
///   success path would be exactly swapped.
/// - replacing `response.status()`
/// - replacing the whole body with an error
///
/// A unit test because the function is private and `reqwest::Response` is
/// constructible from an `http::Response` without a network.
#[cfg(test)]
mod parse_tests {
    use super::*;
    use http::{Response as HttpResponse, StatusCode};

    /// A `reqwest::Response` with a given status and body, built in memory.
    fn response(status: u16, body: &str) -> Response {
        let built = HttpResponse::builder()
            .status(status)
            .body(body.to_string())
            .expect("valid response");
        Response::from(built)
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(fut)
    }

    /// A 2xx body is the answer, and it is returned verbatim.
    #[test]
    fn a_success_status_yields_the_parsed_body() {
        let parsed = block_on(parse_json_response(response(
            200,
            r#"{"id":"ok","nested":{"n":1}}"#,
        )))
        .expect("a 2xx with valid JSON is not an error");
        assert_eq!(parsed["id"], "ok");
        assert_eq!(parsed["nested"]["n"], 1);
    }

    /// A non-2xx becomes `ProviderError::Upstream`, carrying the status and the
    /// body. This is the assertion that `delete !` would break: with the
    /// negation removed, a 500 would be parsed as JSON and a 200 would be
    /// reported as an upstream failure.
    #[test]
    fn a_failure_status_becomes_an_upstream_error_with_the_body() {
        let err = block_on(parse_json_response(response(500, r#"{"error":"boom"}"#)))
            .expect_err("a 5xx must not be returned as a successful answer");
        match err {
            ProviderError::Upstream { status, body } => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(
                    body.contains("boom"),
                    "the upstream body should be preserved for diagnosis, got {body:?}"
                );
            }
            other => panic!("expected ProviderError::Upstream, got {other:?}"),
        }
    }

    /// 4xx is an error too -- not just 5xx. `is_success()` covers 2xx, so a 404
    /// must not be mistaken for an answer.
    #[test]
    fn a_client_error_status_is_also_an_upstream_error() {
        let err =
            block_on(parse_json_response(response(404, "not found"))).expect_err("404 is an error");
        match err {
            ProviderError::Upstream { status, .. } => {
                assert_eq!(status, StatusCode::NOT_FOUND);
            }
            other => panic!("expected ProviderError::Upstream, got {other:?}"),
        }
    }

    /// A 2xx whose body is not JSON is a *parse* failure, not an upstream
    /// status failure. An HTML error page served with a 200 must not be
    /// silently treated as `Upstream` -- that would report the wrong class of
    /// problem and lose the distinction between "the provider said no" and "the
    /// provider said something we cannot read".
    #[test]
    fn a_success_status_with_a_non_json_body_is_a_parse_error() {
        let err = block_on(parse_json_response(response(200, "<html>gateway</html>")))
            .expect_err("not JSON");
        assert!(
            !matches!(err, ProviderError::Upstream { .. }),
            "a malformed 2xx body is a parse failure, not an upstream status failure: {err:?}"
        );
    }
}
