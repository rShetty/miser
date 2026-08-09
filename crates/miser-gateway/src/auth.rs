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
    StoreError,
    NotFound,
    AlreadyExists,
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
                return Ok(api_key.clone());
            }
        }
        Err(AuthError::InvalidKey)
    }

    pub fn create_key(&self, owner: &str) -> Result<String, AuthError> {
        let raw_key = format!("miser_{}", random_key());
        let id = format!("key_{}", &random_key()[..12]);
        let hash = sha256_hex(&raw_key["miser_".len()..]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let api_key = ApiKey {
            id,
            key_hash: hash,
            owner: owner.to_string(),
            created_at: now,
            active: true,
            allowed_tiers: vec![],
            rate_limit_rpm: None,
            monthly_budget_usd: None,
        };
        let mut store = self.store.lock().map_err(|_| AuthError::StoreError)?;
        store.keys.push(api_key);
        self.persist(&store)?;
        Ok(raw_key)
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
    let bytes = input.as_bytes();
    let mut hash: u64 = 14695981039346656037;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let mut result = String::with_capacity(64);
    let mut state = hash;
    for _ in 0..8 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        result.push_str(&format!("{:08x}", state));
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
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let chars: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut state = now as u64 ^ 0x9E3779B97F4A7C15;
    let mut result = String::with_capacity(43);
    for _ in 0..43 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        result.push(chars[(state % chars.len() as u64) as usize] as char);
    }
    result
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
