use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub key_hash: String,
    pub owner: String,
    pub created_at: u64,
    pub active: bool,
    #[serde(default)]
    pub allowed_tiers: Vec<String>,
    #[serde(default)]
    pub rate_limit_rpm: Option<u32>,
    #[serde(default)]
    pub monthly_budget_usd: Option<f64>,
    /// Unix timestamp (seconds) after which the key is rejected. `None`
    /// means the key never expires.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KeyStore {
    keys: Vec<ApiKey>,
}

pub struct AuthManager {
    store: Mutex<KeyStore>,
    path: PathBuf,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum AuthError {
    InvalidKey,
    Inactive,
    Expired,
    StoreError,
    NotFound,
    AlreadyExists,
}

/// Current Unix time in seconds; `0` if the clock is before the epoch.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AuthManager {
    pub fn new(path: PathBuf) -> Self {
        let store = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(text) => serde_json::from_str(&text).unwrap_or(KeyStore { keys: vec![] }),
                Err(_) => KeyStore { keys: vec![] },
            }
        } else {
            KeyStore { keys: vec![] }
        };
        Self {
            store: Mutex::new(store),
            path,
        }
    }

    pub fn validate(&self, bearer: &str) -> Result<ApiKey, AuthError> {
        let key = bearer.strip_prefix("miser_").unwrap_or(bearer);
        let hash = sha256_hex(key);
        let store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        for api_key in &store.keys {
            if api_key.active && constant_time_eq(&api_key.key_hash, &hash) {
                if let Some(expires_at) = api_key.expires_at {
                    if unix_now() >= expires_at {
                        return Err(AuthError::Expired);
                    }
                }
                return Ok(api_key.clone());
            }
        }
        Err(AuthError::InvalidKey)
    }

    /// Create a key with per-key quotas enforced by the gateway.
    pub fn create_key_with_quotas(
        &self,
        owner: &str,
        allowed_tiers: Vec<String>,
        rate_limit_rpm: Option<u32>,
        monthly_budget_usd: Option<f64>,
        expires_at: Option<u64>,
    ) -> Result<String, AuthError> {
        let raw_key = format!("miser_{}", random_key());
        let id = format!("key_{}", &random_key()[..12]);
        let hash = sha256_hex(&raw_key["miser_".len()..]);
        let now = unix_now();
        let api_key = ApiKey {
            id,
            key_hash: hash,
            owner: owner.to_string(),
            created_at: now,
            active: true,
            allowed_tiers,
            rate_limit_rpm,
            monthly_budget_usd,
            expires_at,
        };
        let mut store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        store.keys.push(api_key);
        self.persist(&store)?;
        Ok(raw_key)
    }

    /// Rotate a key: replaces its secret with a freshly generated one and
    /// returns the new raw key. The old secret stops working immediately
    /// (no grace period); owner, quotas, tier allowlist and expiry are
    /// preserved. The raw key is visible only in this return value.
    pub fn rotate_key(&self, id: &str) -> Result<String, AuthError> {
        let raw_key = format!("miser_{}", random_key());
        let hash = sha256_hex(&raw_key["miser_".len()..]);
        let mut store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        let key = store
            .keys
            .iter_mut()
            .find(|k| k.id == id)
            .ok_or(AuthError::NotFound)?;
        key.key_hash = hash;
        key.active = true;
        self.persist(&store)?;
        Ok(raw_key)
    }

    /// Update quota fields on an existing key.
    pub fn update_key_quotas(
        &self,
        id: &str,
        allowed_tiers: Option<Vec<String>>,
        rate_limit_rpm: Option<Option<u32>>,
        monthly_budget_usd: Option<Option<f64>>,
    ) -> Result<(), AuthError> {
        let mut store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        let key = store
            .keys
            .iter_mut()
            .find(|k| k.id == id)
            .ok_or(AuthError::NotFound)?;
        if let Some(tiers) = allowed_tiers {
            key.allowed_tiers = tiers;
        }
        if let Some(rpm) = rate_limit_rpm {
            key.rate_limit_rpm = rpm;
        }
        if let Some(budget) = monthly_budget_usd {
            key.monthly_budget_usd = budget;
        }
        self.persist(&store)
    }

    pub fn list_keys(&self) -> Result<Vec<ApiKey>, AuthError> {
        let store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        Ok(store
            .keys
            .iter()
            .map(|k| ApiKey {
                key_hash: String::new(),
                ..k.clone()
            })
            .collect())
    }

    #[allow(dead_code)]
    pub fn revoke_key(&self, id: &str) -> Result<(), AuthError> {
        let mut store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        let key = store
            .keys
            .iter_mut()
            .find(|k| k.id == id)
            .ok_or(AuthError::NotFound)?;
        key.active = false;
        self.persist(&store)
    }

    pub fn delete_key(&self, id: &str) -> Result<(), AuthError> {
        let mut store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        let before = store.keys.len();
        store.keys.retain(|k| k.id != id);
        if store.keys.len() == before {
            return Err(AuthError::NotFound);
        }
        self.persist(&store)
    }

    fn persist(&self, store: &KeyStore) -> Result<(), AuthError> {
        let text = serde_json::to_string_pretty(store).map_err(|_| AuthError::StoreError)?;
        let mut file = fs::File::create(&self.path).map_err(|_| AuthError::StoreError)?;
        file.write_all(text.as_bytes())
            .map_err(|_| AuthError::StoreError)?;
        Ok(())
    }
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let mut result = String::with_capacity(64);
    for byte in digest {
        result.push_str(&format!("{:02x}", byte));
    }
    result
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn random_key() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    use rand::rand_core::UnwrapErr;
    use rand::rngs::SysRng;
    // Alphanumeric is uniformly distributed over [A-Za-z0-9] and SysRng is the
    // operating system CSPRNG, so keys are unpredictable and unbiased.
    Alphanumeric.sample_string(&mut UnwrapErr(SysRng), 43)
}

pub fn extract_bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

pub fn admin_auth(headers: &axum::http::HeaderMap, admin_key: &str) -> bool {
    if admin_key.is_empty() {
        return false;
    }
    extract_bearer(headers)
        .map(|b| constant_time_eq(&b, admin_key))
        .unwrap_or(false)
}

pub fn json_error(
    msg: &str,
    status: axum::http::StatusCode,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    (status, axum::Json(json!({"error": {"message": msg}})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        // FIPS 180-4 test vectors.
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_is_64_lowercase_hex_chars() {
        let digest = sha256_hex("miser_test_key");
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }

    #[test]
    fn sha256_differs_for_similar_inputs() {
        assert_ne!(sha256_hex("miser_key1"), sha256_hex("miser_key2"));
    }

    #[test]
    fn random_keys_have_expected_length_and_alphabet() {
        for _ in 0..32 {
            let key = random_key();
            assert_eq!(key.len(), 43);
            assert!(
                key.bytes().all(|b| b.is_ascii_alphanumeric()),
                "key must be alphanumeric, got {key}"
            );
        }
    }

    #[test]
    fn consecutive_random_keys_differ() {
        for _ in 0..16 {
            assert_ne!(random_key(), random_key());
        }
    }

    #[test]
    fn random_keys_are_unique_across_many_samples() {
        let keys: std::collections::HashSet<String> = (0..256).map(|_| random_key()).collect();
        assert_eq!(keys.len(), 256, "CSPRNG keys must not collide");
    }

    fn temp_store(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "miser_auth_test_{tag}_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn expired_key_is_rejected_by_validate() {
        let manager = AuthManager::new(temp_store("expired"));
        let now = unix_now();
        let raw = manager
            .create_key_with_quotas("legacy", vec![], None, None, Some(now.saturating_sub(10)))
            .unwrap();
        assert!(matches!(manager.validate(&raw), Err(AuthError::Expired)));
    }

    #[test]
    fn key_expiring_in_the_future_is_accepted() {
        let manager = AuthManager::new(temp_store("future"));
        let raw = manager
            .create_key_with_quotas("fresh", vec![], None, None, Some(unix_now() + 3_600))
            .unwrap();
        let validated = manager.validate(&raw).unwrap();
        assert_eq!(validated.owner, "fresh");
        assert_eq!(validated.expires_at, Some(unix_now() + 3_600));
    }

    #[test]
    fn key_without_expiry_never_expires() {
        let manager = AuthManager::new(temp_store("noexpiry"));
        let raw = manager
            .create_key_with_quotas("steady", vec![], None, None, None)
            .unwrap();
        assert!(manager.validate(&raw).is_ok());
        let listed = manager.list_keys().unwrap();
        assert!(listed[0].expires_at.is_none());
    }

    #[test]
    fn rotation_replaces_secret_and_invalidates_old_key() {
        let manager = AuthManager::new(temp_store("rotate"));
        let old_raw = manager
            .create_key_with_quotas("rotating", vec!["hard".into()], Some(60), None, None)
            .unwrap();
        let id = manager.list_keys().unwrap()[0].id.clone();

        let new_raw = manager.rotate_key(&id).unwrap();
        assert_ne!(old_raw, new_raw);
        assert!(new_raw.starts_with("miser_"));
        // The old secret stops working immediately.
        assert!(matches!(
            manager.validate(&old_raw),
            Err(AuthError::InvalidKey)
        ));
        // The new secret authenticates and keeps the key's attributes.
        let validated = manager.validate(&new_raw).unwrap();
        assert_eq!(validated.id, id);
        assert_eq!(validated.owner, "rotating");
        assert_eq!(validated.allowed_tiers, vec!["hard".to_string()]);
        assert_eq!(validated.rate_limit_rpm, Some(60));
        assert!(validated.active);
    }

    #[test]
    fn rotation_survives_restart_from_disk() {
        let path = temp_store("rotate_persist");
        let manager = AuthManager::new(path.clone());
        let old_raw = manager
            .create_key_with_quotas("persisted", vec![], None, None, None)
            .unwrap();
        let id = manager.list_keys().unwrap()[0].id.clone();
        let new_raw = manager.rotate_key(&id).unwrap();

        let restarted = AuthManager::new(path);
        assert!(matches!(
            restarted.validate(&old_raw),
            Err(AuthError::InvalidKey)
        ));
        assert_eq!(restarted.validate(&new_raw).unwrap().id, id);
    }

    #[test]
    fn rotation_of_unknown_key_fails() {
        let manager = AuthManager::new(temp_store("rotate_missing"));
        assert!(matches!(
            manager.rotate_key("key_does_not_exist"),
            Err(AuthError::NotFound)
        ));
    }
}

/// Per-key quota enforcement: fixed-window rate limiting and monthly
/// budget tracking.
///
/// Budget semantics: spend is estimated from usage tokens multiplied by
/// `price_per_1k_tokens` (a conservative blended rate the operator
/// configures). When no price is configured, budget caps are not enforced —
/// rate limits and tier gating remain active.
pub struct QuotaEnforcer {
    /// key_id -> (window_start_minute, request_count)
    windows: Mutex<std::collections::HashMap<String, (u64, u32)>>,
    /// key_id -> (year_month, accumulated_usd)
    spend: Mutex<std::collections::HashMap<String, (u32, f64)>>,
}

impl Default for QuotaEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaEnforcer {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(std::collections::HashMap::new()),
            spend: Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn current_minute() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() / 60)
            .unwrap_or(0)
    }

    fn current_month() -> u32 {
        // Approximate month index from epoch seconds (30.44-day months).
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (secs / 2_629_800) as u32
    }

    /// Fixed-window RPM check. Returns `false` when the request should be
    /// rejected with 429.
    pub fn check_rate_limit(&self, key_id: &str, rpm: u32) -> bool {
        let minute = Self::current_minute();
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let entry = windows.entry(key_id.to_string()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        if entry.1 >= rpm {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// Returns `false` when the monthly budget cap would be exceeded.
    pub fn check_budget(&self, key_id: &str, cap_usd: f64) -> bool {
        let month = Self::current_month();
        let spend = self.spend.lock().unwrap_or_else(|e| e.into_inner());
        match spend.get(key_id) {
            Some((m, total)) if *m == month => *total < cap_usd,
            _ => true,
        }
    }

    /// Accumulate estimated cost for a key in the current month.
    pub fn record_spend(&self, key_id: &str, amount_usd: f64) {
        let month = Self::current_month();
        let mut spend = self.spend.lock().unwrap_or_else(|e| e.into_inner());
        let entry = spend.entry(key_id.to_string()).or_insert((month, 0.0));
        if entry.0 != month {
            *entry = (month, 0.0);
        }
        entry.1 += amount_usd;
    }
}

#[cfg(test)]
mod quota_tests {
    use super::*;

    #[test]
    fn rate_limit_blocks_after_threshold() {
        let q = QuotaEnforcer::new();
        for _ in 0..3 {
            assert!(q.check_rate_limit("k", 3));
        }
        assert!(!q.check_rate_limit("k", 3));
        // Independent per key.
        assert!(q.check_rate_limit("other", 3));
    }

    #[test]
    fn budget_blocks_only_when_exceeded() {
        let q = QuotaEnforcer::new();
        assert!(q.check_budget("k", 1.0));
        q.record_spend("k", 0.5);
        assert!(q.check_budget("k", 1.0));
        q.record_spend("k", 0.75);
        assert!(!q.check_budget("k", 1.0));
    }
}
