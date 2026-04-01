use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

use crate::config::PechinchaConfig;
use crate::currency::ExchangeRateService;
use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};
use crate::scoring::{self, RelevanceScore, KEEPA_CANDIDATE_THRESHOLD, RELEVANT_THRESHOLD};
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
        // Google Shopping only when explicitly enabled — redundant with existing providers
        // and shows misleading pre-tax prices for imports.
        if config.providers.google_shopping.enabled {
            providers.push(Arc::new(
                crate::providers::google_shopping::GoogleShopping::new(),
            ));
        }
        if config.providers.ebay.enabled || cdp_mode {
            providers.push(Arc::new(crate::providers::ebay::Ebay::new()));
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
                        && p.price.listed_price == Decimal::ZERO
                        && scoring::tokens_match(&p.title, &query.query) // Don't fetch irrelevant products
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

        // Parallel detail page fetches: Amazon BR prices + AliExpress taxes concurrently.
        if let Some(cdp_port) = self.cdp_port {
            let amz_br_products: Vec<(usize, String)> = all_products
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.provider == ProviderId::Amazon
                        && !p.url.is_empty()
                        && p.price.listed_price == Decimal::ZERO
                        && scoring::tokens_match(&p.title, &query.query)
                })
                .take(3)
                .map(|(i, p)| (i, p.url.clone()))
                .collect();

            // Fetch AliExpress taxes for up to 3 products, then extrapolate the
            // tax rate to all other AliExpress products.
            let ali_products: Vec<(usize, String)> = all_products
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.provider == ProviderId::AliExpress
                        && !p.url.is_empty()
                        && scoring::tokens_match(&p.title, &query.query)
                })
                .take(3)
                .map(|(i, p)| (i, p.url.clone()))
                .collect();

            let has_work = !amz_br_products.is_empty() || !ali_products.is_empty();
            if has_work {
                eprintln!("  Fetching detail pages...");

                // Spawn all detail page fetches concurrently
                let mut br_handles = Vec::new();
                for (idx, url) in amz_br_products {
                    let handle = tokio::spawn(async move {
                        let price = crate::cdp::fetch_amazon_br_price(cdp_port, &url).await;
                        (idx, price)
                    });
                    br_handles.push(handle);
                }

                let mut ali_handles = Vec::new();
                for (idx, url) in ali_products {
                    let handle = tokio::spawn(async move {
                        let tax = crate::cdp::fetch_aliexpress_tax(cdp_port, &url).await;
                        (idx, tax)
                    });
                    ali_handles.push(handle);
                }

                // Collect Amazon BR results
                for handle in br_handles {
                    if let Ok((idx, Some(price))) = handle.await {
                        if price > Decimal::from(50) {
                            info!(idx = idx, price = %price, "Amazon BR price from detail page");
                            all_products[idx].price.listed_price = price;
                            all_products[idx].price.price_brl = price;
                            all_products[idx].price.total_cost = price;
                        } else {
                            warn!(idx = idx, price = %price, "Amazon BR price too low, skipping");
                        }
                    }
                }

                // Collect AliExpress tax results and compute average tax ratio
                let mut ali_tax_ratios: Vec<Decimal> = Vec::new();
                let mut fetched_indices = std::collections::HashSet::new();
                for handle in ali_handles {
                    if let Ok((idx, Some(tax))) = handle.await {
                        let listed = all_products[idx].price.listed_price;
                        if listed > Decimal::ZERO {
                            let ratio = tax / listed;
                            ali_tax_ratios.push(ratio);
                            info!(idx = idx, tax = %tax, ratio = %ratio, "AliExpress tax fetched");
                        }
                        fetched_indices.insert(idx);
                        all_products[idx].price.tax.import_tax = Some(tax);
                        all_products[idx].price.tax.total_tax = tax;
                        all_products[idx].price.tax.taxes_included = true;
                        all_products[idx].price.tax.tax_regime = TaxRegime::RemessaConformeOver50;
                    }
                }

                // Apply average tax ratio to ALL remaining AliExpress products
                // (regardless of taxes_included flag — search results never include tax)
                if !ali_tax_ratios.is_empty() {
                    let avg_ratio = ali_tax_ratios.iter().sum::<Decimal>()
                        / Decimal::from(ali_tax_ratios.len() as u32);
                    info!(avg_ratio = %avg_ratio, samples = ali_tax_ratios.len(), "AliExpress tax ratio");

                    for (i, product) in all_products.iter_mut().enumerate() {
                        if product.provider == ProviderId::AliExpress
                            && !fetched_indices.contains(&i)
                        {
                            let estimated_tax = product.price.listed_price * avg_ratio;
                            product.price.tax.import_tax = Some(estimated_tax);
                            product.price.tax.total_tax = estimated_tax;
                            product.price.tax.taxes_included = true;
                            product.price.tax.tax_regime = TaxRegime::RemessaConformeOver50;
                            debug!(
                                title = %truncate_str(&product.title, 40),
                                tax = %estimated_tax,
                                "AliExpress tax extrapolated from ratio"
                            );
                        }
                    }
                }
            }

            // Don't remove zero-price BR products yet — Keepa may fill the price later
        }

        // Keepa price intelligence — fetch international Amazon prices.
        // Pick the best ASIN (actual product, not accessories) by selecting
        // the highest-priced product with the best title relevance match.
        if let Some(cdp_port) = self.cdp_port {
            // Pick the best ASIN using signal-based scoring.
            let mut seen_asins = std::collections::HashSet::new();
            let mut candidates: Vec<(String, f64, u32, Decimal, u8, String)> = all_products
                .iter()
                .filter(|p| {
                    (p.provider == ProviderId::Amazon || p.provider == ProviderId::AmazonUS)
                        && !p.platform_id.is_empty()
                        && p.platform_id.len() == 10
                        && scoring::tokens_match(&p.title, &query.query)
                })
                .filter(|p| seen_asins.insert(p.platform_id.clone()))
                .map(|p| {
                    let domain = if p.provider == ProviderId::Amazon {
                        crate::keepa::DOMAIN_BR
                    } else {
                        crate::keepa::DOMAIN_US
                    };
                    let reviews = p.review_count.unwrap_or(0);
                    let score = scoring::score_product(&p.title, &query.query, 0.5, 1.0);
                    (p.platform_id.clone(), score.total, reviews, p.price.listed_price, domain, p.title.clone())
                })
                .collect();

            // Sort by relevance score desc, then reviews, then price
            candidates.sort_by(|a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.2.cmp(&a.2))
                    .then(b.3.cmp(&a.3))
            });

            // Log top candidates
            for (i, (asin, score, reviews, _price, domain, title)) in candidates.iter().take(5).enumerate() {
                debug!(
                    rank = i + 1,
                    asin = %asin,
                    score = format!("{:.2}", score),
                    reviews = reviews,
                    domain = domain,
                    title = %truncate_str(title, 60),
                    "Keepa candidate"
                );
            }

            // Take the best candidate above the threshold
            if let Some((best_asin, score, reviews, price, domain, title)) = candidates.first() {
                if *score < KEEPA_CANDIDATE_THRESHOLD {
                    info!(
                        asin = %best_asin,
                        score = format!("{:.2}", score),
                        title = %truncate_str(title, 50),
                        "Skipping Keepa — best candidate score too low"
                    );
                } else {
                info!(
                    asin = %best_asin,
                    score = format!("{:.2}", score),
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

                // Track which ASIN the insights came from (may change on fallback)
                let mut effective_asin = best_asin.clone();

                // Fallback: if comparison failed (<=1 domain), try a US ASIN.
                // The original ASIN may be from Amazon BR and not exist on Keepa's
                // US database.  Find the best US candidate from the list instead.
                let insights = if insights.len() <= 1 {
                    let us_fallback_asin = candidates.iter()
                        .find(|(asin, _, _, _, domain, _)| {
                            *domain == crate::keepa::DOMAIN_US && asin != best_asin
                        })
                        .map(|(asin, ..)| asin.clone());

                    if let Some(ref us_asin) = us_fallback_asin {
                        warn!(
                            br_asin = %best_asin,
                            us_asin = %us_asin,
                            "Keepa comparison got {} domains, falling back to US ASIN",
                            insights.len()
                        );
                        let us_insights = tokio::time::timeout(
                            std::time::Duration::from_secs(35),
                            crate::keepa::fetch_keepa_comparison(cdp_port, us_asin, crate::keepa::DOMAIN_US)
                        ).await.unwrap_or_default();
                        if us_insights.len() > insights.len() {
                            effective_asin = us_asin.clone();
                            us_insights
                        } else {
                            insights
                        }
                    } else {
                        warn!("Keepa comparison got {} domains, no US fallback available", insights.len());
                        // Last resort: try the same ASIN on US domain
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
                    }
                } else {
                    insights
                };

                if !insights.is_empty() {
                    // Find MSRP: use median across all domains (converted to USD).
                    // Single-domain MSRP can be misleading (e.g. JPY ¥396 misconverted).
                    // Median of all available MSRPs gives a robust reference.
                    let msrp = {
                        let mut msrp_usd_values: Vec<Decimal> = insights.iter()
                            .filter_map(|k| k.msrp_usd())
                            .filter(|&m| m > Decimal::ZERO)
                            .collect();
                        msrp_usd_values.sort();
                        if msrp_usd_values.is_empty() {
                            None
                        } else if msrp_usd_values.len() == 1 {
                            Some(msrp_usd_values[0])
                        } else {
                            // Median
                            let mid = msrp_usd_values.len() / 2;
                            Some(msrp_usd_values[mid])
                        }
                    };

                    // MSRP sanity check: compare against the median price of actual
                    // search results. If MSRP is <10% of median, we probably picked
                    // an accessory ASIN — discard the MSRP to avoid corrupting filters.
                    let msrp = msrp.and_then(|m| {
                        let mut result_prices: Vec<Decimal> = all_products.iter()
                            .filter(|p| p.price.listed_price > Decimal::ZERO && scoring::tokens_match(&p.title, &query.query))
                            .map(|p| p.price.listed_price)
                            .collect();
                        result_prices.sort();
                        let median = if result_prices.is_empty() {
                            Decimal::ZERO
                        } else {
                            result_prices[result_prices.len() / 2]
                        };
                        // Convert MSRP to BRL for comparison
                        let msrp_brl = m * exchange_rate;
                        if median > Decimal::ZERO && msrp_brl < median * rust_decimal_macros::dec!(0.10) {
                            warn!(
                                msrp = %m,
                                msrp_brl = %msrp_brl,
                                median = %median,
                                "Keepa MSRP suspiciously low vs median — discarding"
                            );
                            None
                        } else {
                            info!(asin = %effective_asin, msrp = %m, domains = insights.len(), "Keepa MSRP");
                            Some(m)
                        }
                    });

                    // Attach Keepa data to products matching the effective ASIN
                    // (could be the original BR ASIN or the US fallback)
                    for product in all_products.iter_mut() {
                        if product.platform_id != effective_asin
                            && product.platform_id != *best_asin
                        {
                            continue;
                        }

                        product.keepa = insights.clone();

                        if let Some(m) = msrp {
                            product.price.original_price = Some(m);
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
                    // Fallback: if Keepa didn't provide MSRP, fetch detail page
                    let has_msrp = all_products.iter().any(|p| {
                        (p.platform_id == effective_asin || p.platform_id == *best_asin)
                            && p.price.original_price.is_some()
                    });

                    if !has_msrp {
                        let detail_asin = if effective_asin != *best_asin {
                            &effective_asin
                        } else {
                            best_asin
                        };
                        info!("No Keepa MSRP, fetching detail page for {}", detail_asin);
                        let url = format!("https://www.amazon.com/dp/{}", detail_asin);
                        if let Some(details) = crate::cdp::fetch_amazon_us_details(cdp_port, &url).await {
                            if let Some(msrp) = details.msrp {
                                info!(asin = %detail_asin, msrp = %msrp, "MSRP from detail page");
                                for product in all_products.iter_mut() {
                                    if product.platform_id == *detail_asin
                                        || product.platform_id == *best_asin
                                    {
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

        // Fill missing prices from Keepa buy_box data.
        // This handles Amazon BR products with "Ver opções de compra" where
        // detail page extraction failed but Keepa has the real price.
        for product in all_products.iter_mut() {
            if product.price.listed_price == Decimal::ZERO && !product.keepa.is_empty() {
                let own_domain = if product.provider == ProviderId::Amazon {
                    crate::keepa::DOMAIN_BR
                } else {
                    crate::keepa::DOMAIN_US
                };
                if let Some(keepa_price) = product.keepa.iter()
                    .find(|k| k.domain == own_domain)
                    .and_then(|k| k.best_new_price())
                {
                    info!(
                        asin = %product.platform_id,
                        price = %keepa_price,
                        "Filled price from Keepa buy_box"
                    );
                    product.price.listed_price = keepa_price;
                    product.price.price_brl = keepa_price;
                }
            }
        }

        // Remove products that still have zero price after all enrichment
        all_products.retain(|p| p.price.listed_price > Decimal::ZERO);

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

        // ── Signal-based relevance scoring ──────────────────────────────────
        // Combines title structure, price clustering, and string similarity
        // into a single 0.0–1.0 score. Replaces hard-coded word lists and
        // magic MSRP/price thresholds.
        {
            // Step 1: token pre-filter (cheap — removes obviously irrelevant results)
            all_products.retain(|p| scoring::tokens_match(&p.title, &query.query));

            // Step 2: compute price cluster scores, anchored by MSRP when available
            let msrp_brl: Option<f64> = all_products.iter()
                .filter(|p| !p.keepa.is_empty())
                .filter_map(|p| p.price.original_price)
                .next()
                .map(|msrp_usd| {
                    let brl = msrp_usd * exchange_rate;
                    brl.to_string().parse::<f64>().unwrap_or(0.0)
                });
            if let Some(ref m) = msrp_brl {
                debug!(msrp_brl = m, "Using MSRP anchor for price clustering");
            }
            let prices: Vec<Decimal> = all_products.iter().map(|p| p.price.total_cost).collect();
            let (cluster_scores, gap_ratio) = scoring::price_cluster_scores(&prices, msrp_brl);

            // Step 3: score each product and filter
            let before = all_products.len();
            let scored: Vec<(usize, RelevanceScore)> = all_products
                .iter()
                .enumerate()
                .zip(cluster_scores.iter())
                .map(|((i, p), &cs)| {
                    let score = scoring::score_product(&p.title, &query.query, cs, gap_ratio);
                    (i, score)
                })
                .collect();

            // Log low-scoring products for debugging
            for (i, score) in &scored {
                if score.total < RELEVANT_THRESHOLD {
                    debug!(
                        title = %truncate_str(&all_products[*i].title, 50),
                        score = format!("{:.2}", score.total),
                        structure = format!("{:.2}", score.title_structure),
                        cluster = format!("{:.2}", score.price_cluster),
                        similarity = format!("{:.2}", score.string_similarity),
                        "Filtered by relevance score"
                    );
                }
            }

            // Keep only indices that pass the threshold
            let keep: std::collections::HashSet<usize> = scored
                .iter()
                .filter(|(_, s)| s.total >= RELEVANT_THRESHOLD)
                .map(|(i, _)| *i)
                .collect();

            let mut idx = 0;
            all_products.retain(|_| {
                let result = keep.contains(&idx);
                idx += 1;
                result
            });

            let filtered = before - all_products.len();
            if filtered > 0 {
                debug!(filtered = filtered, threshold = RELEVANT_THRESHOLD, "Signal-based filtering");
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

        // Clean up any leaked CDP tabs
        if let Some(cdp_port) = self.cdp_port {
            crate::cdp::cleanup_tabs(cdp_port).await;
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

// Old is_relevant, title_match_score, is_accessory_title removed.
// All relevance logic now lives in src/scoring.rs using signal-based scoring.

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


fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = max.saturating_sub(3);
        let boundary = s.floor_char_boundary(end);
        format!("{}...", &s[..boundary])
    }
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
