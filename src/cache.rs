//! File-based response cache for search results.
//! Stores serialized products in ~/.cache/pechincha/ with a configurable TTL.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::models::{Product, SearchQuery};

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    /// Unix timestamp when the cache was written
    created_at: u64,
    products: Vec<Product>,
}

pub struct SearchCache {
    ttl: Duration,
    cache_dir: PathBuf,
}

impl SearchCache {
    #[must_use]
    pub fn new(ttl_minutes: u64) -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("pechincha");
        Self {
            ttl: Duration::from_secs(ttl_minutes * 60),
            cache_dir,
        }
    }

    fn cache_key(query: &SearchQuery) -> String {
        let mut hasher = DefaultHasher::new();
        query.query.to_lowercase().trim().hash(&mut hasher);
        // Include sort and platforms in the key
        format!("{:?}", query.sort).hash(&mut hasher);
        let mut platform_names: Vec<String> = query.platforms.iter().map(std::string::ToString::to_string).collect();
        platform_names.sort();
        for name in &platform_names {
            name.hash(&mut hasher);
        }
        format!("{:016x}.json", hasher.finish())
    }

    fn cache_path(&self, query: &SearchQuery) -> PathBuf {
        self.cache_dir.join(Self::cache_key(query))
    }

    /// Try to get cached results. Returns None if cache miss or expired.
    pub fn get(&self, query: &SearchQuery) -> Option<Vec<Product>> {
        let path = self.cache_path(query);
        let data = std::fs::read_to_string(&path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&data).ok()?;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if now - entry.created_at > self.ttl.as_secs() {
            debug!(path = %path.display(), "Cache expired");
            let _ = std::fs::remove_file(&path);
            return None;
        }

        info!(
            results = entry.products.len(),
            age_secs = now - entry.created_at,
            "Cache hit"
        );
        Some(entry.products)
    }

    /// Store products in cache.
    pub fn put(&self, query: &SearchQuery, products: &[Product]) {
        if products.is_empty() {
            return;
        }

        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            debug!(error = %e, "Failed to create cache dir");
            return;
        }

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entry = CacheEntry {
            created_at: now,
            products: products.to_vec(),
        };

        let path = self.cache_path(query);
        match serde_json::to_string(&entry) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    debug!(error = %e, "Failed to write cache");
                } else {
                    debug!(path = %path.display(), results = products.len(), "Cached results");
                }
            }
            Err(e) => debug!(error = %e, "Failed to serialize cache"),
        }
    }

    /// Clear all cached results.
    pub fn clear(&self) {
        if self.cache_dir.exists() {
            let _ = std::fs::remove_dir_all(&self.cache_dir);
            info!("Cache cleared");
        }
    }
}
