//! Append-only per-request usage ledger and aggregation.
//!
//! Every chat completion (including cache hits) appends one
//! [`UsageRecord`] line to a JSONL file. Aggregation walks the file and
//! buckets by model, tier, key, client and day — the same shape as
//! OpenRouter's per-key activity view.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// One settled gateway request, attributed to its API key and client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Unix timestamp (seconds).
    pub ts: u64,
    pub key_id: String,
    /// Client/application label from the key ("-" when unset).
    pub client: String,
    pub model: String,
    /// Model as requested by the client; "auto" routes through
    /// classification. Kept separate from `model`, which is the model that
    /// actually served the request.
    #[serde(default)]
    pub requested_model: String,
    pub tier: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
    /// True when served from the exact-match response cache.
    pub cached: bool,
    pub status: u16,
    pub request_id: String,
}

/// Aggregate rollups over a window, serialised straight into the admin API.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UsageSummary {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
    pub by_model: BTreeMap<String, ModelUsage>,
    pub by_tier: BTreeMap<String, u64>,
    pub by_key: BTreeMap<String, KeyUsage>,
    pub by_client: BTreeMap<String, ClientUsage>,
    /// Requests per UTC day (YYYY-MM-DD), oldest first.
    pub by_day: BTreeMap<String, DayUsage>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ModelUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct KeyUsage {
    pub client: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClientUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DayUsage {
    pub requests: u64,
    pub cost_usd: f64,
}

pub struct UsageLedger {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl UsageLedger {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            path,
            write_lock: Mutex::new(()),
        }
    }

    /// Append one settled request. Never panics; failures are best-effort
    /// analytics (the response has already been produced).
    pub fn record(&self, record: &UsageRecord) {
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        let _guard = self.write_lock.lock();
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Aggregate every recorded request in the window defined by the
    /// optional filters. `since_ts` filters by time; the other two filter
    /// by attribution.
    pub fn summarize(
        &self,
        since_ts: Option<u64>,
        key_id: Option<&str>,
        client: Option<&str>,
    ) -> UsageSummary {
        let mut summary = UsageSummary::default();
        let Ok(text) = fs::read_to_string(&self.path) else {
            return summary;
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(record) = serde_json::from_str::<UsageRecord>(line) else {
                continue;
            };
            if let Some(since) = since_ts {
                if record.ts < since {
                    continue;
                }
            }
            if let Some(want) = key_id {
                if record.key_id != want {
                    continue;
                }
            }
            if let Some(want) = client {
                if record.client != want {
                    continue;
                }
            }
            summary.requests += 1;
            summary.prompt_tokens += record.prompt_tokens;
            summary.completion_tokens += record.completion_tokens;
            summary.cost_usd += record.cost_usd;

            let model = summary.by_model.entry(record.model.clone()).or_default();
            model.requests += 1;
            model.prompt_tokens += record.prompt_tokens;
            model.completion_tokens += record.completion_tokens;
            model.cost_usd += record.cost_usd;

            *summary.by_tier.entry(record.tier.clone()).or_default() += 1;

            let key = summary.by_key.entry(record.key_id.clone()).or_default();
            key.client = record.client.clone();
            key.requests += 1;
            key.prompt_tokens += record.prompt_tokens;
            key.completion_tokens += record.completion_tokens;
            key.cost_usd += record.cost_usd;

            let client_agg = summary.by_client.entry(record.client.clone()).or_default();
            client_agg.requests += 1;
            client_agg.prompt_tokens += record.prompt_tokens;
            client_agg.completion_tokens += record.completion_tokens;
            client_agg.cost_usd += record.cost_usd;

            let day = day_of(record.ts).unwrap_or_else(|| "unknown".into());
            let day_agg = summary.by_day.entry(day).or_default();
            day_agg.requests += 1;
            day_agg.cost_usd += record.cost_usd;
        }
        // Round costs so JSON output stays readable.
        summary.cost_usd = round2(summary.cost_usd);
        for v in summary.by_model.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        for v in summary.by_key.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        for v in summary.by_client.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        for v in summary.by_day.values_mut() {
            v.cost_usd = round2(v.cost_usd);
        }
        summary
    }
}

/// UTC calendar day (YYYY-MM-DD) for a unix timestamp.
fn day_of(ts: u64) -> Option<String> {
    let days = ts / 86_400;
    // Days since epoch to civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "miser_test_usage_{}_{}_{seq}_{label}.jsonl",
            std::process::id(),
            nanos
        ))
    }

    fn record(
        ts: u64,
        key_id: &str,
        client: &str,
        model: &str,
        requested: &str,
        cost: f64,
    ) -> UsageRecord {
        UsageRecord {
            ts,
            key_id: key_id.into(),
            client: client.into(),
            model: model.into(),
            requested_model: requested.into(),
            tier: "standard".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            cost_usd: cost,
            latency_ms: 10,
            cached: false,
            status: 200,
            request_id: format!("r-{ts}-{key_id}-{model}"),
        }
    }

    #[test]
    fn summarize_aggregates_totals_and_attribution() {
        let path = temp_path("aggregate");
        let ledger = UsageLedger::new(path);
        ledger.record(&record(1000, "k1", "cli", "m/served", "auto", 0.111));
        ledger.record(&record(1001, "k2", "web", "m/served", "m/served", 0.111));
        ledger.record(&record(1002, "k1", "cli", "m/other", "auto", 1.0));

        let summary = ledger.summarize(None, None, None);
        assert_eq!(summary.requests, 3);
        assert_eq!(summary.prompt_tokens, 300);
        assert_eq!(summary.completion_tokens, 150);
        assert!(
            (summary.cost_usd - 1.22).abs() < 1e-9,
            "cost rounded to 2dp: {}",
            summary.cost_usd
        );

        // Attribution uses the serving model; requested_model is carried on
        // the record but never merged into by_model buckets.
        assert_eq!(summary.by_model["m/served"].requests, 2);
        assert_eq!(summary.by_model["m/served"].prompt_tokens, 200);
        assert_eq!(summary.by_model["m/other"].requests, 1);
        assert_eq!(summary.by_tier["standard"], 3);

        let k1 = &summary.by_key["k1"];
        assert_eq!(k1.client, "cli");
        assert_eq!(k1.requests, 2);
        assert_eq!(k1.prompt_tokens, 200);
        assert_eq!(summary.by_key["k2"].client, "web");

        assert_eq!(summary.by_client["cli"].requests, 2);
        assert_eq!(summary.by_client["web"].requests, 1);
    }

    #[test]
    fn summarize_filters_by_since_key_and_client() {
        let path = temp_path("filters");
        let ledger = UsageLedger::new(path);
        ledger.record(&record(1000, "k1", "cli", "m", "m", 0.5));
        ledger.record(&record(2000, "k1", "cli", "m", "m", 0.5));
        ledger.record(&record(3000, "k2", "cli", "m", "m", 0.5));
        ledger.record(&record(4000, "k1", "web", "m", "m", 0.5));

        assert_eq!(
            ledger.summarize(Some(2000), None, None).requests,
            3,
            "since excludes older"
        );
        assert_eq!(
            ledger.summarize(None, Some("k1"), None).requests,
            3,
            "key filter"
        );
        assert_eq!(
            ledger.summarize(None, None, Some("web")).requests,
            1,
            "client filter"
        );
        assert_eq!(
            ledger
                .summarize(Some(2000), Some("k1"), Some("cli"))
                .requests,
            1,
            "filters combine"
        );
    }

    #[test]
    fn summarize_tolerates_missing_and_corrupt_files() {
        let path = temp_path("corrupt");
        // Missing file: default summary, not a panic.
        assert_eq!(
            UsageLedger::new(path.clone())
                .summarize(None, None, None)
                .requests,
            0
        );

        std::fs::write(
            &path,
            "{\"ts\":1,\"key_id\":\"k\",\"client\":\"c\",\"model\":\"m\",\
             \"requested_model\":\"m\",\"tier\":\"trivial\",\"prompt_tokens\":1,\
             \"completion_tokens\":1,\"cost_usd\":0.0,\"latency_ms\":1,\"cached\":false,\
             \"status\":200,\"request_id\":\"ok\"}\n\
             this line is not json\n\
             {\"ts\":2\n",
        )
        .unwrap();
        let summary = UsageLedger::new(path).summarize(None, None, None);
        assert_eq!(
            summary.requests, 1,
            "corrupt lines skipped, valid ones counted"
        );
    }

    #[test]
    fn records_append_across_ledger_instances_and_create_parent_dirs() {
        let nested = temp_path("append").with_extension("d").join("usage.jsonl");
        {
            let first = UsageLedger::new(nested.clone());
            first.record(&record(1, "k", "cli", "m", "m", 0.1));
        }
        assert!(nested.exists(), "record creates missing parent directories");
        let second = UsageLedger::new(nested.clone());
        second.record(&record(2, "k", "cli", "m", "m", 0.1));
        let summary = second.summarize(None, None, None);
        assert_eq!(summary.requests, 2, "append-only ledger survives re-open");
        assert_eq!(summary.by_key["k"].requests, 2);
    }

    #[test]
    fn summarize_buckets_by_utc_calendar_day() {
        let path = temp_path("days");
        let ledger = UsageLedger::new(path);
        // 2026-01-01T00:00:00Z, 2026-01-01T23:59:59Z, 2026-01-02T00:00:00Z.
        let day_start: u64 = 1_767_225_600;
        ledger.record(&record(day_start, "k", "c", "m", "m", 0.25));
        ledger.record(&record(day_start + 86_399, "k", "c", "m", "m", 0.25));
        ledger.record(&record(day_start + 86_400, "k", "c", "m", "m", 1.0));

        let summary = ledger.summarize(None, None, None);
        let days: Vec<String> = summary.by_day.keys().cloned().collect();
        assert_eq!(
            days,
            vec!["2026-01-01", "2026-01-02"],
            "UTC days, oldest first"
        );
        assert_eq!(summary.by_day["2026-01-01"].requests, 2);
        assert!((summary.by_day["2026-01-01"].cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(summary.by_day["2026-01-02"].requests, 1);
    }
}
