use std::time::Duration;

use crate::providers::ProviderId;

#[derive(Debug, thiserror::Error)]
pub enum PechinchaError {
    #[error("Provider '{provider}' failed: {source}")]
    Provider {
        provider: ProviderId,
        #[source]
        source: ProviderError,
    },

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("No providers available for this search")]
    NoProviders,

    #[error("All providers failed")]
    AllProvidersFailed(Vec<(ProviderId, ProviderError)>),
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] wreq::Error),

    #[error("Failed to parse response: {0}")]
    Parse(String),

    #[error("Rate limited by {provider}")]
    RateLimited {
        provider: String,
        retry_after: Option<Duration>,
    },

    #[error("Authentication failed — check API credentials for {0}")]
    Auth(String),

    #[error("Provider returned no results")]
    NoResults,

    #[error("Request timed out after {0:?}")]
    Timeout(Duration),

    #[error("Scraping failed — site structure may have changed: {0}")]
    Scraping(String),

    #[error("Headless browser error: {0}")]
    Browser(String),
}
