pub mod cdp;
pub mod config;
pub mod keepa;
pub mod currency;
pub mod display;
pub mod error;
pub mod models;
pub mod providers;
pub mod scraping;
pub mod search;
pub mod tax;

pub use config::PechinchaConfig;
pub use error::{PechinchaError, ProviderError};
pub use models::*;
pub use providers::{Provider, ProviderId};
pub use search::SearchOrchestrator;

/// Convenience function: one-shot search with default config.
pub async fn search(query: &str) -> Result<SearchResults, PechinchaError> {
    let config = PechinchaConfig::load(None).map_err(PechinchaError::Config)?;
    let orchestrator = SearchOrchestrator::from_config(&config);
    let results = orchestrator.search(&SearchQuery::simple(query)).await;

    if results.products.is_empty() && !results.errors.is_empty() {
        return Err(PechinchaError::AllProvidersFailed(results.errors));
    }

    Ok(results)
}
