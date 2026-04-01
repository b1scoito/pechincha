pub mod cache;
pub mod cdp;
pub mod config;
pub mod keepa;
pub mod currency;
pub mod display;
pub mod error;
pub mod history;
pub mod models;
pub mod providers;
pub mod scraping;
pub mod notify;
pub mod scoring;
pub mod search;
pub mod tax;
pub mod watch;

pub use config::PechinchaConfig;
pub use error::{PechinchaError, ProviderError};
pub use models::*;
pub use providers::{Provider, ProviderId};
pub use search::SearchOrchestrator;

/// Convenience function: one-shot search with default config.
///
/// # Errors
///
/// Returns [`PechinchaError::Config`] if the default config file cannot be loaded,
/// or [`PechinchaError::AllProvidersFailed`] if every provider returned an error
/// and no products were found.
pub async fn search(query: &str) -> Result<SearchResults, PechinchaError> {
    let config = PechinchaConfig::load(None).map_err(PechinchaError::Config)?;
    let orchestrator = SearchOrchestrator::from_config(&config);
    let results = orchestrator.search(&SearchQuery::simple(query)).await;

    if results.products.is_empty() && !results.errors.is_empty() {
        return Err(PechinchaError::AllProvidersFailed(results.errors));
    }

    Ok(results)
}
