//! Prometheus metrics exposed at `/metrics`.
//!
//! All counters live in a private [`Registry`] so the gateway never
//! collides with collectors registered by other libraries in the same
//! process. Rendering uses the standard text exposition format.

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts, Registry, TextEncoder,
};

/// Route label recorded for the chat completions endpoint.
pub const COMPLETIONS_ROUTE: &str = "/v1/chat/completions";

/// Set of registered gateway metrics shared through `AppState`.
#[derive(Clone)]
pub struct Metrics {
    registry: Registry,
    /// Total requests by route and response status code.
    pub requests_total: IntCounterVec,
    /// Request latency in seconds by route.
    pub request_duration_seconds: HistogramVec,
    /// Requests served per effective classification tier.
    pub tier_requests_total: IntCounterVec,
    /// Exact-match response cache hits.
    pub cache_hits_total: IntCounter,
    /// Response cache misses.
    pub cache_misses_total: IntCounter,
    /// Requests escalated above the classifier's original tier.
    pub quality_escalations_total: IntCounter,
    /// Failed or errored upstream provider responses.
    pub upstream_errors_total: IntCounter,
}

impl Metrics {
    /// Creates and registers every gateway metric.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let requests_total = IntCounterVec::new(
            Opts::new(
                "miser_requests_total",
                "Total requests by route and HTTP status code.",
            ),
            &["route", "status"],
        )?;
        let request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "miser_request_duration_seconds",
                "Request latency in seconds by route.",
            ),
            &["route"],
        )?;
        let tier_requests_total = IntCounterVec::new(
            Opts::new(
                "miser_tier_requests_total",
                "Requests served per effective classification tier.",
            ),
            &["tier"],
        )?;
        let cache_hits_total =
            IntCounter::new("miser_cache_hits_total", "Exact-match response cache hits.")?;
        let cache_misses_total = IntCounter::new("miser_cache_misses_total", "Cache misses.")?;
        let quality_escalations_total = IntCounter::new(
            "miser_quality_escalations_total",
            "Requests escalated above the classifier's original tier.",
        )?;
        let upstream_errors_total = IntCounter::new(
            "miser_upstream_errors_total",
            "Failed or errored upstream provider responses.",
        )?;

        registry.register(Box::new(requests_total.clone()))?;
        registry.register(Box::new(request_duration_seconds.clone()))?;
        registry.register(Box::new(tier_requests_total.clone()))?;
        registry.register(Box::new(cache_hits_total.clone()))?;
        registry.register(Box::new(cache_misses_total.clone()))?;
        registry.register(Box::new(quality_escalations_total.clone()))?;
        registry.register(Box::new(upstream_errors_total.clone()))?;

        Ok(Self {
            registry,
            requests_total,
            request_duration_seconds,
            tier_requests_total,
            cache_hits_total,
            cache_misses_total,
            quality_escalations_total,
            upstream_errors_total,
        })
    }

    /// Renders every metric in the Prometheus text exposition format.
    pub fn render(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        encoder.encode(&self.registry.gather(), &mut buffer)?;
        Ok(String::from_utf8_lossy(&buffer).into_owned())
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new().expect("gateway metrics must register")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_exposes_all_metric_families() {
        let metrics = Metrics::new().unwrap();
        // Vector families only appear in the exposition once they have at
        // least one child series, so touch every family before rendering.
        metrics
            .requests_total
            .with_label_values(&[COMPLETIONS_ROUTE, "200"])
            .inc();
        metrics
            .request_duration_seconds
            .with_label_values(&[COMPLETIONS_ROUTE])
            .observe(0.01);
        metrics
            .tier_requests_total
            .with_label_values(&["trivial"])
            .inc();
        let text = metrics.render().unwrap();
        for family in [
            "# HELP miser_requests_total",
            "# HELP miser_request_duration_seconds",
            "# HELP miser_tier_requests_total",
            "# HELP miser_cache_hits_total",
            "# HELP miser_cache_misses_total",
            "# HELP miser_quality_escalations_total",
            "# HELP miser_upstream_errors_total",
        ] {
            assert!(text.contains(family), "missing {family} in:\n{text}");
        }
    }

    #[test]
    fn counters_increment_and_render_with_labels() {
        let metrics = Metrics::new().unwrap();
        metrics
            .requests_total
            .with_label_values(&[COMPLETIONS_ROUTE, "200"])
            .inc();
        metrics.cache_hits_total.inc();
        metrics.cache_misses_total.inc();
        metrics.cache_misses_total.inc();
        metrics
            .tier_requests_total
            .with_label_values(&["hard"])
            .inc();
        metrics.quality_escalations_total.inc();
        metrics.upstream_errors_total.inc();
        metrics
            .request_duration_seconds
            .with_label_values(&[COMPLETIONS_ROUTE])
            .observe(0.25);

        let text = metrics.render().unwrap();
        assert!(
            text.contains(concat!(
                "miser_requests_total{route=\"/v1/chat/completions\",",
                "status=\"200\"} 1"
            )),
            "request counter missing in:\n{text}"
        );
        assert!(text.contains("miser_cache_hits_total 1"), "{text}");
        assert!(text.contains("miser_cache_misses_total 2"), "{text}");
        assert!(
            text.contains("miser_tier_requests_total{tier=\"hard\"} 1"),
            "{text}"
        );
        assert!(text.contains("miser_quality_escalations_total 1"), "{text}");
        assert!(text.contains("miser_upstream_errors_total 1"), "{text}");
        assert!(
            text.contains(concat!(
                "miser_request_duration_seconds_bucket{route=\"/v1/chat/completions\",",
                "le=\"0.25\"} 1"
            )),
            "histogram bucket missing in:\n{text}"
        );
    }
}
