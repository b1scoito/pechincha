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
        eprintln!("  Searching {} providers...", active.len());
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
        let (mut all_products, errors) = if let Some(cdp_port) = self.cdp_port {
            self.search_cdp(cdp_port, &active, query).await
        } else {
            self.search_wreq(&active, query).await
        };

        let exchange_rate = exchange_rate_future.await;

        // For Amazon US products: fetch real shipping + import charges from detail pages.
        // Only fetch for products without a price (Keepa will handle MSRP/pricing).
        // Limit to 3 concurrent to avoid overwhelming the browser.
        if let Some(cdp_port) = self.cdp_port {
            let amz_us_products: Vec<(usize, String)> = all_products
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.provider == ProviderId::AmazonUS
                        && !p.url.is_empty()
                        && p.price.listed_price == Decimal::ZERO // Only fetch if price missing
                })
                .map(|(i, p)| (i, p.url.clone()))
                .take(3) // Limit concurrent detail page fetches
                .collect();

            if !amz_us_products.is_empty() {
                eprintln!("  Fetching Amazon US details...");
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

        // For AliExpress products: fetch exact tax from product detail pages.
        // AliExpress shows "R$X em impostos estimados" on product pages.
        // Fetch for the top 2 relevant AliExpress products.
        if let Some(cdp_port) = self.cdp_port {
            let ali_products: Vec<(usize, String)> = all_products
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.provider == ProviderId::AliExpress
                        && !p.url.is_empty()
                        && is_relevant(&p.title, &query.query)
                })
                .take(2)
                .map(|(i, p)| (i, p.url.clone()))
                .collect();

            if !ali_products.is_empty() {
                eprintln!("  Fetching AliExpress taxes...");
                for (idx, url) in ali_products {
                    if let Some(tax) = crate::cdp::fetch_aliexpress_tax(cdp_port, &url).await {
                        all_products[idx].price.tax.import_tax = Some(tax);
                        all_products[idx].price.tax.total_tax = tax;
                        all_products[idx].price.tax.taxes_included = true; // Now we have the real tax
                        all_products[idx].price.tax.tax_regime = TaxRegime::RemessaConformeOver50;
                    }
                }
            }
        }

        // Keepa price intelligence — fetch international Amazon prices.
        // Pick the best ASIN (actual product, not accessories) by selecting
        // the highest-priced product with the best title relevance match.
        if let Some(cdp_port) = self.cdp_port {
            // Pick the best ASIN: score by title match quality + review count.
            // The actual product (e.g. "Dyson V15 Detect Cordless Vacuum") has the
            // query terms at the START of the title. Accessories have them buried
            // ("Filtro compatível com Dyson V15 V11 V10...").
            let mut seen_asins = std::collections::HashSet::new();
            let mut candidates: Vec<(String, u32, u32, Decimal, u8, String)> = all_products
                .iter()
                .filter(|p| {
                    (p.provider == ProviderId::Amazon || p.provider == ProviderId::AmazonUS)
                        && !p.platform_id.is_empty()
                        && p.platform_id.len() == 10
                        && seen_asins.insert(p.platform_id.clone())
                        && is_relevant(&p.title, &query.query)
                })
                .map(|p| {
                    let domain = if p.provider == ProviderId::Amazon {
                        crate::keepa::DOMAIN_BR
                    } else {
                        crate::keepa::DOMAIN_US
                    };
                    let reviews = p.review_count.unwrap_or(0);
                    let title_score = title_match_score(&p.title, &query.query);
                    (p.platform_id.clone(), title_score, reviews, p.price.listed_price, domain, p.title.clone())
                })
                .collect();

            // Sort by: title score desc, then reviews desc, then price desc
            candidates.sort_by(|a, b| {
                b.1.cmp(&a.1) // title match score (higher = better match)
                    .then(b.2.cmp(&a.2)) // reviews (more = more popular)
                    .then(b.3.cmp(&a.3)) // price (higher = actual product)
            });

            // Log top candidates for debugging
            for (i, (asin, score, reviews, _price, domain, title)) in candidates.iter().take(3).enumerate() {
                debug!(
                    rank = i + 1,
                    asin = %asin,
                    score = score,
                    reviews = reviews,
                    domain = domain,
                    title = %truncate_str(title, 60),
                    "Keepa candidate"
                );
            }

            // Take the single best ASIN — best title match with most reviews.
            // Skip if the best candidate has a very low title score (likely wrong product).
            let min_keepa_score = 50u32;
            if let Some((best_asin, score, reviews, price, domain, title)) = candidates.first() {
                if *score < min_keepa_score {
                    info!(
                        asin = %best_asin,
                        title_score = score,
                        title = %truncate_str(title, 50),
                        "Skipping Keepa — best candidate score too low"
                    );
                } else {
                info!(
                    asin = %best_asin,
                    title_score = score,
                    reviews = reviews,
                    price = %price,
                    domain = %domain,
                    title = %truncate_str(title, 50),
                    "Keepa target ASIN"
                );
                eprintln!("  Fetching Keepa prices...");

                let insights = tokio::time::timeout(
                    std::time::Duration::from_secs(35), // Must be > inner 25s deadline
                    crate::keepa::fetch_keepa_comparison(cdp_port, best_asin, *domain)
                ).await.unwrap_or_default();

                // Fallback: if comparison failed (<=1 domain), try single US fetch
                let insights = if insights.len() <= 1 {
                    warn!("Keepa comparison got {} domains, trying single US fetch", insights.len());
                    let single = tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        crate::keepa::fetch_keepa_data(cdp_port, best_asin, crate::keepa::DOMAIN_US)
                    ).await.ok().flatten();
                    let mut combined = insights;
                    if let Some(insight) = single {
                        if !combined.iter().any(|k| k.domain == insight.domain) {
                            combined.push(insight);
                        }
                    }
                    combined
                } else {
                    insights
                };

                if !insights.is_empty() {
                    let us_msrp = insights.iter()
                        .find(|k| k.domain == crate::keepa::DOMAIN_US)
                        .and_then(|k| k.msrp());

                    if let Some(msrp) = us_msrp {
                        info!(asin = %best_asin, msrp = %msrp, domains = insights.len(), "Keepa MSRP");
                    }

                    // Attach Keepa data to ALL Amazon products with this ASIN
                    for product in all_products.iter_mut() {
                        if product.platform_id != *best_asin { continue; }

                        product.keepa = insights.clone();

                        if let Some(msrp) = us_msrp {
                            product.price.original_price = Some(msrp);
                        }

                        let own_domain = if product.provider == ProviderId::Amazon {
                            crate::keepa::DOMAIN_BR
                        } else {
                            crate::keepa::DOMAIN_US
                        };
                        if let Some(own) = insights.iter().find(|k| k.domain == own_domain) {
                            if product.rating.is_none() {
                                product.rating = own.rating;
                            }
                            if product.review_count.is_none() {
                                product.review_count = own.review_count;
                            }
                        }
                    }
                    // Fallback: if Keepa didn't provide MSRP, fetch detail page for this ASIN
                    let has_msrp = all_products.iter().any(|p| {
                        p.platform_id == *best_asin && p.price.original_price.is_some()
                    });

                    if !has_msrp && *domain == crate::keepa::DOMAIN_US {
                        info!("No Keepa MSRP, fetching detail page for {}", best_asin);
                        let url = format!("https://www.amazon.com/dp/{}", best_asin);
                        if let Some(details) = crate::cdp::fetch_amazon_us_details(cdp_port, &url).await {
                            if let Some(msrp) = details.msrp {
                                info!(asin = %best_asin, msrp = %msrp, "MSRP from detail page");
                                for product in all_products.iter_mut() {
                                    if product.platform_id == *best_asin {
                                        product.price.original_price = Some(msrp);
                                    }
                                }
                            }
                        }
                    }
                }
            } // else (score >= min_keepa_score)
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
        all_products.retain(|p| is_relevant(&p.title, &query.query));

        // MSRP-based accessory filter: if we have a reference MSRP from Keepa,
        // products priced below 10% of it are almost certainly accessories.
        // E.g., MSRP $600 → filter out R$60 filters and R$130 accessories.
        // Only use Keepa-sourced MSRPs (USD, from Amazon products with Keepa data).
        let reference_msrp_brl: Option<Decimal> = all_products.iter()
            .filter(|p| !p.keepa.is_empty() || p.price.currency == Currency::USD)
            .filter_map(|p| p.price.original_price.map(|msrp| {
                if p.price.currency == Currency::USD || !p.keepa.is_empty() {
                    msrp * exchange_rate // Keepa MSRP is always in USD cents → dollars
                } else {
                    msrp
                }
            }))
            .max();

        if let Some(msrp_brl) = reference_msrp_brl {
            let before = all_products.len();

            // Two-tier accessory filter:
            // 1. Products below 15% of MSRP are definitely accessories (filters, brushes)
            let hard_min = msrp_brl * rust_decimal_macros::dec!(0.15);
            // 2. Products below 45% with accessory keywords in title are likely stands/holders/parts
            let soft_min = msrp_brl * rust_decimal_macros::dec!(0.45);

            all_products.retain(|p| {
                if p.price.total_cost < hard_min {
                    return false; // Definitely an accessory
                }
                if p.price.total_cost < soft_min && is_accessory_title(&p.title) {
                    return false; // Accessory keyword + low price = accessory
                }
                true
            });

            let filtered = before - all_products.len();
            if filtered > 0 {
                debug!(
                    filtered = filtered,
                    hard_min = %hard_min,
                    soft_min = %soft_min,
                    msrp = %msrp_brl,
                    "Filtered accessories by MSRP + title"
                );
            }
        }

        // Fallback: price-clustering filter when no MSRP is available.
        // If prices span >10x range AND there are accessory-titled items,
        // use the highest-priced product as reference and filter accessories below 20%.
        if reference_msrp_brl.is_none() && all_products.len() > 3 {
            let mut prices: Vec<Decimal> = all_products.iter().map(|p| p.price.total_cost).collect();
            prices.sort();
            let lowest = prices[0];
            let highest = prices[prices.len() - 1];

            if highest > lowest * Decimal::from(10) {
                // Huge price spread — likely accessories mixed with actual product
                let threshold = highest * rust_decimal_macros::dec!(0.15);
                let before = all_products.len();
                all_products.retain(|p| {
                    p.price.total_cost >= threshold || !is_accessory_title(&p.title)
                });
                let filtered = before - all_products.len();
                if filtered > 0 {
                    debug!(
                        filtered = filtered,
                        threshold = %threshold,
                        highest = %highest,
                        "Filtered accessories by price clustering"
                    );
                }
            }
        }

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

        // Deduplicate: same product from same platform listed multiple times.
        // After sorting by price, keep the cheapest listing for each unique product.
        let before_dedup = all_products.len();
        deduplicate_products(&mut all_products);
        let deduped = before_dedup - all_products.len();
        if deduped > 0 {
            debug!(removed = deduped, "Deduplicated products");
        }

        // Apply per-provider result limit AFTER filtering and sorting
        // This ensures the best N results per provider survive, not just the first N parsed
        if query.max_results < 50 {
            let max = query.max_results;
            let mut provider_counts = std::collections::HashMap::new();
            all_products.retain(|p| {
                let count = provider_counts.entry(p.provider).or_insert(0usize);
                *count += 1;
                *count <= max
            });
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
///
/// Simple, strict rule: ALL significant query tokens must appear in the title.
/// No partial matching, no thresholds, no core/non-core distinction.
/// Every word the user typed matters.
///
/// Handles: accent normalization, hyphen splitting, compound numbers (v15 = "v15" or "v 15").
fn is_relevant(title: &str, query: &str) -> bool {
    let stop_words: &[&str] = &[
        "de", "do", "da", "dos", "das", "para", "com", "sem", "por", "em", "no", "na",
        "the", "for", "with", "and", "or", "a", "an", "o", "e", "um", "uma",
    ];

    let normalize = |s: &str| -> String {
        s.to_lowercase()
            .replace('-', " ")
            .replace('_', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };

    let title_norm = normalize(title);
    let title_compact = title_norm.replace(' ', ""); // for compound matching: "v 15" → "v15"

    let query_tokens: Vec<String> = normalize(query)
        .split_whitespace()
        .filter(|t| t.len() > 1 && !stop_words.contains(t))
        .map(|s| s.to_string())
        .collect();

    if query_tokens.is_empty() {
        return true;
    }

    // ALL tokens must match — either as substring in title, or in compact form
    query_tokens.iter().all(|token| {
        title_norm.contains(token.as_str()) || title_compact.contains(token.as_str())
    })
}

/// Score how well a product title matches the search query.
/// Higher = better match. The actual product has query terms at the START of the title.
/// Accessories bury them in "compatible with X Y Z" lists.
fn title_match_score(title: &str, query: &str) -> u32 {
    let title_lower = title.to_lowercase();
    let query_lower = query.to_lowercase();

    let mut score = 0u32;

    // Bonus: query appears as an exact phrase in the title (strongest signal)
    if title_lower.contains(&query_lower) {
        score += 100;
    }

    // Bonus: query phrase appears in the first 60 chars (product name, not compat list)
    let title_start: String = title_lower.chars().take(60).collect();
    if title_start.contains(&query_lower) {
        score += 50;
    }

    // Check each query token — where does it appear in the title?
    let tokens: Vec<&str> = query_lower.split_whitespace().collect();
    for token in &tokens {
        if let Some(pos) = title_lower.find(token) {
            // Token found early in title = actual product, not accessory
            if pos < 30 { score += 20; }
            else if pos < 60 { score += 10; }
            else { score += 5; }
        }
    }

    // Penalty: accessories, bundles, and non-product items
    let penalty_words = [
        // Accessories
        "compativel", "compatível", "substituição", "substituicao",
        "filtro", "filter", "peças", "pecas", "acessório", "acessorio",
        "replacement", "attachment", "stand", "suporte", "bracket",
        // Bundles (we want the standalone product, not combos)
        "bundle", "combo", "kit de", "kit com", "soundbar",
        // Cases & covers
        "case", "capa", "película", "pelicula", "protetor",
    ];
    for word in &penalty_words {
        if title_lower.contains(word) {
            score = score.saturating_sub(30);
        }
    }

    score
}

/// Deduplicate products: remove near-identical listings from the same platform.
/// Products are considered duplicates when:
/// 1. Same platform + same platform_id (ASIN) — exact duplicate
/// 2. Same platform + normalized title matches — same product, different sellers
///
/// Already sorted by price, so first seen = cheapest = the one we keep.
fn deduplicate_products(products: &mut Vec<Product>) {
    let mut seen = std::collections::HashSet::new();

    products.retain(|p| {
        // Key 1: platform + platform_id (for Amazon ASINs)
        if !p.platform_id.is_empty() {
            let key = format!("{}:{}", p.provider, p.platform_id);
            if !seen.insert(key) {
                return false; // Duplicate ASIN on same platform
            }
        }

        // Key 2: platform + normalized title (for ML, OLX, etc.)
        let norm_title = normalize_for_dedup(&p.title);
        if !norm_title.is_empty() {
            let key = format!("{}:{}", p.provider, norm_title);
            if !seen.insert(key) {
                return false; // Duplicate title on same platform
            }
        }

        true
    });
}

/// Normalize a title for deduplication: lowercase, remove common suffixes,
/// strip punctuation, collapse whitespace.
fn normalize_for_dedup(title: &str) -> String {
    title.to_lowercase()
        .replace([':', '-', '(', ')', ',', '.', '!', '/', '|'], " ")
        // Remove common seller-specific suffixes
        .replace("bivolt", "")
        .replace("110v", "")
        .replace("220v", "")
        .replace("preto", "")
        .replace("black", "")
        .replace("branco", "")
        .replace("white", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check if a product title indicates it's an accessory, not the main product.
fn is_accessory_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    let accessory_patterns = [
        "stand", "suporte", "holder", "docking", "station", "organizer",
        "filtro", "filter", "replacement", "substituição", "substituicao",
        "brush", "escova", "roller", "rolo", "attachment", "acessório",
        "acessorio", "accessory", "accessories", "peças", "pecas",
        "cleanerhead", "battery", "bateria", "charger", "carregador",
        "hose", "mangueira", "tube", "tubo", "mount", "bracket",
        "compatível", "compativel", "compatible", "para aspirador",
        "ferramenta de", "tool for",
        "bundle", "combo", "soundbar", "kit de", "kit com",
        "case", "capa", "película", "pelicula", "protetor",
    ];
    accessory_patterns.iter().any(|pat| lower.contains(pat))
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}...", &s[..max.saturating_sub(3)]) }
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
