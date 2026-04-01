//! Price history tracking — records prices over time and detects changes.
//! Stores one JSON-lines file per product in ~/.local/share/pechincha/history/

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::models::Product;
use crate::providers::ProviderId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceEntry {
    pub timestamp: DateTime<Utc>,
    pub total_cost: Decimal,
    pub listed_price: Decimal,
    pub provider: ProviderId,
}

#[derive(Debug, Clone)]
pub struct PriceChange {
    pub previous: Decimal,
    pub current: Decimal,
    pub pct_change: f64,
    pub days_ago: u32,
}

pub struct PriceTracker {
    data_dir: PathBuf,
}

impl Default for PriceTracker {
    fn default() -> Self {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("pechincha")
            .join("history");
        Self { data_dir }
    }
}

impl PriceTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn product_key(product: &Product) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Key by provider + platform_id (most unique), fallback to title hash
        if product.platform_id.is_empty() {
            let mut hasher = DefaultHasher::new();
            product.provider.to_string().hash(&mut hasher);
            product.title.to_lowercase().hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        } else {
            format!("{}_{}", product.provider, product.platform_id)
        }
    }

    fn history_path(&self, product: &Product) -> PathBuf {
        self.data_dir.join(format!("{}.jsonl", Self::product_key(product)))
    }

    /// Record current price for a product.
    pub fn record(&self, product: &Product) {
        use std::io::Write;

        if product.price.total_cost == Decimal::ZERO {
            return;
        }

        if let Err(e) = std::fs::create_dir_all(&self.data_dir) {
            debug!(error = %e, "Failed to create history dir");
            return;
        }

        let entry = PriceEntry {
            timestamp: Utc::now(),
            total_cost: product.price.total_cost,
            listed_price: product.price.listed_price,
            provider: product.provider,
        };

        let path = self.history_path(product);
        let Ok(line) = serde_json::to_string(&entry) else {
            return;
        };

        // Append to JSONL file
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Record prices for all products in a search result.
    pub fn record_all(&self, products: &[Product]) {
        for product in products {
            self.record(product);
        }
    }

    /// Get the most recent previous price entry (before today).
    #[must_use]
    pub fn get_previous(&self, product: &Product) -> Option<PriceEntry> {
        let path = self.history_path(product);
        let data = std::fs::read_to_string(path).ok()?;

        let today = Utc::now().date_naive();

        // Read all entries, find the latest one that's before today
        data.lines()
            .filter_map(|line| serde_json::from_str::<PriceEntry>(line).ok())
            .filter(|e| e.timestamp.date_naive() < today)
            .max_by_key(|e| e.timestamp)
    }

    /// Compute price change for a product relative to its last known price.
    #[must_use]
    pub fn price_change(&self, product: &Product) -> Option<PriceChange> {
        let previous_entry = self.get_previous(product)?;
        let previous = previous_entry.total_cost;
        let current = product.price.total_cost;

        if previous == Decimal::ZERO {
            return None;
        }

        let pct_change = ((current - previous) * Decimal::from(100)) / previous;
        let days_ago = u32::try_from((Utc::now() - previous_entry.timestamp).num_days().max(1)).unwrap_or(0);

        Some(PriceChange {
            previous,
            current,
            pct_change: pct_change.to_string().parse().unwrap_or(0.0),
            days_ago,
        })
    }
}
