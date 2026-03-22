use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

use crate::config::PechinchaConfig;
use crate::currency::ExchangeRateService;
use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};
use crate::tax::TaxCalculator;

pub struct SearchOrchestrator {
    providers: Vec<Arc<dyn Provider>>,
    exchange_rate_service: ExchangeRateService,
    timeout: Duration,
    cdp_port: Option<u16>,
}

impl SearchOrchestrator {
    pub fn from_config(config: &PechinchaConfig) -> Self {
        let cdp_port = config.general.cdp_port;
        let mut providers: Vec<Arc<dyn Provider>> = Vec::new();

        // When CDP is available, ALL providers are enabled — they all benefit
        // from the real browser session (personalized prices, accurate shipping, etc.)
        let cdp_mode = cdp_port.is_some();

        if config.providers.mercadolivre.enabled || cdp_mode {
            providers.push(Arc::new(
                crate::providers::mercadolivre::MercadoLivre::new(),
            ));
        }
        if config.providers.aliexpress.enabled || cdp_mode {
            providers.push(Arc::new(
                crate::providers::aliexpress::AliExpress::new(cdp_port),
            ));
        }
        if config.providers.shopee.enabled || cdp_mode {
            providers.push(Arc::new(
                crate::providers::shopee::Shopee::new(cdp_port),
            ));
        }
        if config.providers.amazon.enabled || cdp_mode {
            providers.push(Arc::new(crate::providers::amazon::Amazon::new()));
        }
        if config.providers.kabum.enabled || cdp_mode {
            providers.push(Arc::new(crate::providers::kabum::Kabum::new()));
        }
        if config.providers.magalu.enabled || cdp_mode {
            providers.push(Arc::new(
                crate::providers::magalu::MagazineLuiza::new(),
            ));
        }
        if config.providers.amazon_us.enabled || cdp_mode {
            providers.push(Arc::new(
                crate::providers::amazon_us::AmazonUS::new(),
            ));
        }
        if config.providers.olx.enabled || cdp_mode {
            providers.push(Arc::new(crate::providers::olx::Olx::new()));
        }

        let http_client = crate::scraping::build_impersonating_client(10);

        Self {
            providers,
            exchange_rate_service: ExchangeRateService::new(http_client),
            timeout: Duration::from_secs(config.general.timeout_seconds),
            cdp_port,
        }
    }

    pub async fn search(&self, query: &SearchQuery) -> SearchResults {
        let start = Instant::now();

        // Filter providers by platform selection
        let active: Vec<_> = self
            .providers
            .iter()
            .filter(|p| {
                p.is_available()
                    && (query.platforms.is_empty()
                        || query.platforms.contains(&p.id()))
            })
            .cloned()
            .collect();

        if active.is_empty() {
            return SearchResults {
                products: Vec::new(),
                errors: vec![],
                query_time: start.elapsed(),
            };
        }

        let mode = if self.cdp_port.is_some() { "CDP" } else { "wreq" };
        info!(
            providers = active.len(),
            query = %query.query,
            mode = mode,
            "Searching across providers"
        );

        // Fetch exchange rate concurrently with provider searches
        let exchange_rate_future = self.exchange_rate_service.get_usd_brl();

        // CDP-first: if browser is available, fetch all pages concurrently via CDP
        // then let each provider parse its HTML
        let (mut all_products, mut errors) = if let Some(cdp_port) = self.cdp_port {
            self.search_cdp(cdp_port, &active, query).await
        } else {
            self.search_wreq(&active, query).await
        };

        let exchange_rate = exchange_rate_future.await;

        // For Amazon US products: fetch real shipping + import charges from detail pages
        if let Some(cdp_port) = self.cdp_port {
            let amz_us_products: Vec<(usize, String)> = all_products
                .iter()
                .enumerate()
                .filter(|(_, p)| p.provider == ProviderId::AmazonUS && !p.url.is_empty())
                .map(|(i, p)| (i, p.url.clone()))
                .collect();

            if !amz_us_products.is_empty() {
                info!(
                    count = amz_us_products.len(),
                    "Fetching Amazon US prices + shipping from detail pages"
                );

                // Fetch detail pages concurrently
                let mut handles = Vec::new();
                for (idx, url) in amz_us_products {
                    debug!(idx = idx, url = %url, "Visiting Amazon US detail page");
                    let handle = tokio::spawn(async move {
                        let details = crate::cdp::fetch_amazon_us_details(cdp_port, &url).await;
                        (idx, details)
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    if let Ok((idx, Some(details))) = handle.await {
                        // Fill in product price if it was missing from search results
                        if let Some(price) = details.product_price {
                            if all_products[idx].price.listed_price == Decimal::ZERO {
                                all_products[idx].price.listed_price = price;
                                all_products[idx].price.price_brl = price;
                                info!(idx = idx, price_usd = %price, "Got Amazon US product price");
                            }
                        }

                        // Store MSRP as original_price
                        if let Some(msrp) = details.msrp {
                            all_products[idx].price.original_price = Some(msrp);
                            info!(idx = idx, msrp_usd = %msrp, "Got Amazon US MSRP");
                        }

                        // Store seller info
                        if let Some(seller) = details.sold_by {
                            let ships = details.ships_from.unwrap_or_default();
                            info!(idx = idx, sold_by = %seller, ships_from = %ships, "Got Amazon US seller");
                            all_products[idx].seller = Some(crate::models::SellerInfo {
                                name: seller,
                                reputation: None,
                                official_store: false,
                            });
                        }

                        // Set real shipping + import charges
                        if let Some(ship_import) = details.shipping_import {
                            let ship_import_brl = ship_import * exchange_rate;
                            all_products[idx].price.shipping_cost = Some(ship_import_brl);
                            all_products[idx].price.tax = TaxInfo {
                                remessa_conforme: false,
                                taxes_included: true,
                                import_tax: None,
                                icms: None,
                                total_tax: Decimal::ZERO,
                                tax_regime: TaxRegime::InternationalStandard,
                            };
                            info!(
                                idx = idx,
                                shipping_usd = %ship_import,
                                shipping_brl = %ship_import_brl,
                                "Got Amazon US shipping + import"
                            );
                        }
                    }
                }

                // Remove Amazon US products that still have zero price (couldn't fetch)
                all_products.retain(|p| {
                    p.provider != ProviderId::AmazonUS || p.price.listed_price > Decimal::ZERO
                });
            }
        }

        // Apply tax calculations and currency conversion
        for product in &mut all_products {
            if product.price.currency == Currency::USD {
                product.price.price_brl =
                    product.price.listed_price * exchange_rate;
            }

            if !product.price.tax.taxes_included
                || product.price.tax.tax_regime == TaxRegime::Unknown
            {
                let price_usd = if product.price.currency == Currency::USD {
                    Some(product.price.listed_price)
                } else {
                    None
                };

                product.price.tax = TaxCalculator::calculate(
                    price_usd,
                    product.price.price_brl,
                    product.domestic,
                    product.price.tax.remessa_conforme,
                    product.price.tax.taxes_included,
                    exchange_rate,
                );
            }

            product.price.total_cost = product.price.price_brl
                + product.price.shipping_cost.unwrap_or(Decimal::ZERO)
                + product.price.tax.total_tax;
        }

        // Relevance filter: keep only products whose title matches the core search terms.
        // "RTX 4070" should match "Placa de Vídeo RTX 4070 12GB" but NOT "Fonte para Notebook RTX"
        all_products.retain(|p| is_relevant(&p.title, &query.query));

        // Filter by price range
        if let Some(min) = query.min_price {
            all_products.retain(|p| p.price.total_cost >= min);
        }
        if let Some(max) = query.max_price {
            all_products.retain(|p| p.price.total_cost <= max);
        }

        // Filter by condition
        if let Some(condition) = query.condition {
            all_products.retain(|p| {
                p.condition == condition || p.condition == ProductCondition::Unknown
            });
        }

        // Sort
        match query.sort {
            SortOrder::TotalCost | SortOrder::PriceAsc => {
                all_products.sort_by(|a, b| a.price.total_cost.cmp(&b.price.total_cost));
            }
            SortOrder::PriceDesc => {
                all_products.sort_by(|a, b| b.price.total_cost.cmp(&a.price.total_cost));
            }
            SortOrder::Rating => {
                all_products.sort_by(|a, b| {
                    b.rating
                        .unwrap_or(0.0)
                        .partial_cmp(&a.rating.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            SortOrder::Relevance => {}
        }

        let query_time = start.elapsed();
        info!(
            results = all_products.len(),
            errors = errors.len(),
            time_ms = query_time.as_millis(),
            mode = mode,
            "Search complete"
        );

        SearchResults {
            products: all_products,
            errors,
            query_time,
        }
    }

}

/// Check if a product title is relevant to the search query.
/// Uses a scoring system: each matching token adds a point.
/// Products must match at least 50% of significant tokens, AND
/// all "core" tokens (numbers like model numbers) must match.
fn is_relevant(title: &str, query: &str) -> bool {
    let stop_words = &[
        "de", "do", "da", "dos", "das", "para", "com", "sem", "por", "em", "no", "na",
        "the", "for", "with", "and", "or", "a", "an", "o", "e", "um", "uma",
    ];

    let title_lower = title.to_lowercase();

    let query_tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|t| t.len() > 1 && !stop_words.contains(&t.to_lowercase().as_str()))
        .collect();

    if query_tokens.is_empty() {
        return true;
    }

    // Core tokens: numbers and model identifiers (RTX, GTX, i7, etc.)
    // These MUST match — they identify the specific product
    let core_tokens: Vec<&str> = query_tokens
        .iter()
        .filter(|t| {
            let t = t.to_lowercase();
            t.chars().any(|c| c.is_ascii_digit()) // contains numbers: "4070", "128gb", "i7"
                || ["rtx", "gtx", "rx", "ryzen", "intel", "amd", "iphone", "galaxy", "dyson"]
                    .contains(&t.as_str())
        })
        .copied()
        .collect();

    // All core tokens must match
    let core_match = core_tokens
        .iter()
        .all(|token| title_lower.contains(&token.to_lowercase()));

    if !core_match {
        return false;
    }

    // For non-core tokens (brand names, descriptors), require at least 50% match
    let total = query_tokens.len();
    let matched = query_tokens
        .iter()
        .filter(|token| title_lower.contains(&token.to_lowercase()))
        .count();

    matched * 2 >= total // at least 50%
}

impl SearchOrchestrator {
    /// CDP mode: open all tabs concurrently in the real browser, extract HTML, parse.
    async fn search_cdp(
        &self,
        cdp_port: u16,
        active: &[Arc<dyn Provider>],
        query: &SearchQuery,
    ) -> (Vec<Product>, Vec<(ProviderId, ProviderError)>) {
        // Build URLs for all providers
        let requests: Vec<(ProviderId, String)> = active
            .iter()
            .map(|p| (p.id(), crate::cdp::search_url(p.id(), &query.query)))
            .collect();

        info!(tabs = requests.len(), "Opening CDP tabs concurrently");

        // Fetch all pages at once
        let results = crate::cdp::fetch_pages(cdp_port, requests).await;

        // Parse each provider's HTML
        let mut all_products = Vec::new();
        let mut errors = Vec::new();

        for (provider_id, html_result) in results {
            match html_result {
                Ok(html) => {
                    // Find the provider and let it parse the HTML
                    let provider = active.iter().find(|p| p.id() == provider_id);
                    if let Some(provider) = provider {
                        match provider.parse_html(&html, query.max_results) {
                            Ok(products) => {
                                info!(
                                    provider = %provider.name(),
                                    results = products.len(),
                                    "Parsed"
                                );
                                all_products.extend(products);
                            }
                            Err(e) => {
                                warn!(provider = %provider.name(), error = %e, "Parse failed");
                                errors.push((provider_id, e));
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(provider = %provider_id, error = %e, "CDP fetch failed");
                    errors.push((provider_id, e));
                }
            }
        }

        (all_products, errors)
    }

    /// wreq mode: each provider handles its own HTTP request (fallback when no browser).
    async fn search_wreq(
        &self,
        active: &[Arc<dyn Provider>],
        query: &SearchQuery,
    ) -> (Vec<Product>, Vec<(ProviderId, ProviderError)>) {
        let timeout = self.timeout;
        let query_clone = query.clone();

        let mut handles = Vec::new();
        for provider in active {
            let provider = provider.clone();
            let query = query_clone.clone();
            let handle = tokio::spawn(async move {
                let provider_name = provider.name().to_string();
                let provider_id = provider.id();
                debug!(provider = %provider_name, "Starting wreq search");

                let result =
                    tokio::time::timeout(timeout, provider.search(&query)).await;

                match result {
                    Ok(Ok(products)) => {
                        info!(provider = %provider_name, results = products.len(), "Done");
                        Ok((provider_id, products))
                    }
                    Ok(Err(e)) => {
                        warn!(provider = %provider_name, error = %e, "Failed");
                        Err((provider_id, e))
                    }
                    Err(_) => {
                        warn!(provider = %provider_name, "Timed out");
                        Err((provider_id, ProviderError::Timeout(timeout)))
                    }
                }
            });
            handles.push(handle);
        }

        let mut all_products = Vec::new();
        let mut errors = Vec::new();

        for handle in handles {
            match handle.await {
                Ok(Ok((_, products))) => all_products.extend(products),
                Ok(Err((id, e))) => errors.push((id, e)),
                Err(e) => {
                    error!(error = %e, "Task panicked");
                }
            }
        }

        (all_products, errors)
    }
}
