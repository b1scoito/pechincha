//! Price watch — monitors products and alerts when prices drop below a threshold.
//! Watches are stored in ~/.config/pechincha/watches.json

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::PechinchaConfig;
use crate::models::SearchQuery;
use crate::providers::ProviderId;
use crate::search::SearchOrchestrator;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    pub id: u32,
    pub query: String,
    pub max_price: Decimal,
    pub platforms: Vec<ProviderId>,
    pub created_at: DateTime<Utc>,
    pub last_checked: Option<DateTime<Utc>>,
    pub last_best_price: Option<Decimal>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WatchStore {
    pub watches: Vec<Watch>,
    #[serde(default)]
    next_id: u32,
}

impl WatchStore {
    fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pechincha")
            .join("watches.json")
    }

    #[must_use]
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn add(&mut self, query: String, max_price: Decimal, platforms: Vec<ProviderId>) -> &Watch {
        self.next_id += 1;
        self.watches.push(Watch {
            id: self.next_id,
            query,
            max_price,
            platforms,
            created_at: Utc::now(),
            last_checked: None,
            last_best_price: None,
        });
        self.save();
        self.watches.last().unwrap()
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.watches.len();
        self.watches.retain(|w| w.id != id);
        let removed = self.watches.len() < before;
        if removed { self.save(); }
        removed
    }

    pub fn list(&self) {
        if self.watches.is_empty() {
            println!("  No active watches.");
            return;
        }
        for w in &self.watches {
            let last = w.last_checked
                .map_or_else(|| "never".to_string(), |t| t.format("%Y-%m-%d %H:%M").to_string());
            let best = w.last_best_price
                .map_or_else(|| "-".to_string(), |p| format!("R$ {p}"));
            println!(
                "  #{:<3} {:30} below R$ {:<10} last: {} best: {}",
                w.id, w.query, w.max_price, last, best
            );
        }
    }
}

/// Check all watches and send notifications for price drops.
pub async fn check_all(config: &PechinchaConfig) {
    let mut store = WatchStore::load();
    if store.watches.is_empty() {
        println!("  No watches to check.");
        return;
    }

    let orchestrator = SearchOrchestrator::from_config(config);

    for watch in &mut store.watches {
        eprintln!("  Checking: {} (below R$ {})", watch.query, watch.max_price);

        let query = SearchQuery {
            query: watch.query.clone(),
            max_results: 5,
            min_price: None,
            max_price: None,
            condition: None,
            sort: crate::models::SortOrder::TotalCost,
            platforms: watch.platforms.clone(),
        };

        let results = orchestrator.search(&query).await;
        watch.last_checked = Some(Utc::now());

        if let Some(best) = results.products.first() {
            let price = best.price.total_cost;
            watch.last_best_price = Some(price);

            if price <= watch.max_price {
                info!(
                    query = %watch.query,
                    price = %price,
                    target = %watch.max_price,
                    "Price alert triggered!"
                );
                crate::notify::send(
                    &format!("Pechincha: {}", watch.query),
                    &format!(
                        "R$ {} at {} (target: R$ {})",
                        price,
                        best.provider,
                        watch.max_price
                    ),
                );
            } else {
                eprintln!(
                    "  Best: R$ {} at {} (target: R$ {})",
                    price, best.provider, watch.max_price
                );
            }
        } else {
            eprintln!("  No results found.");
        }
    }

    store.save();
}
