use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

use crate::config::PechinchaConfig;
use crate::currency::ExchangeRateService;
use crate::error::ProviderError;
use crate::models::{
    Currency, Product, ProductCondition, SearchQuery, SearchResults,
    SortOrder, TaxInfo, TaxRegime,
};
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
    #[must_use]
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

        let active_providers = self.active_providers(query);
        if active_providers.is_empty() {
            return SearchResults {
                products: Vec::new(),
                errors: vec![],
                query_time: start.elapsed(),
            };
        }

        let transport_mode = if self.cdp_port.is_some() { "CDP" } else { "wreq" };
        eprintln!("  Searching {} providers...", active_providers.len());
        info!(
            providers = active_providers.len(),
            query = %query.query,
            mode = transport_mode,
            "Searching across providers"
        );

        // Fetch exchange rate concurrently with provider searches
        let exchange_rate_future = self.exchange_rate_service.get_usd_brl();

        let (mut products, provider_errors) = self.fetch_results(&active_providers, query).await;
        let usd_to_brl = exchange_rate_future.await;

        // CDP-only enrichment phases
        if let Some(port) = self.cdp_port {
            self.enrich_amazon_us_details(port, &mut products, query, usd_to_brl).await;
            self.enrich_detail_pages(port, &mut products, query).await;
            Self::enrich_keepa(port, &mut products, query, usd_to_brl).await;
        }

        Self::fill_keepa_prices(&mut products);
        products.retain(|p| p.price.listed_price > Decimal::ZERO);

        Self::apply_taxes(&mut products, usd_to_brl);
        Self::apply_scoring(&mut products, query, usd_to_brl);
        Self::post_filter(&mut products, query);

        // Clean up any leaked CDP tabs
        if let Some(port) = self.cdp_port {
            crate::cdp::cleanup_tabs(port).await;
        }

        let query_time = start.elapsed();
        info!(
            results = products.len(),
            errors = provider_errors.len(),
            time_ms = query_time.as_millis(),
            mode = transport_mode,
            "Search complete"
        );

        SearchResults {
            products,
            errors: provider_errors,
            query_time,
        }
    }

    /// Filter providers by platform selection and availability.
    fn active_providers(&self, query: &SearchQuery) -> Vec<Arc<dyn Provider>> {
        self.providers
            .iter()
            .filter(|p| {
                p.is_available()
                    && (query.platforms.is_empty()
                        || query.platforms.contains(&p.id()))
            })
            .cloned()
            .collect()
    }

    /// Dispatch search to CDP or wreq transport.
    async fn fetch_results(
        &self,
        active_providers: &[Arc<dyn Provider>],
        query: &SearchQuery,
    ) -> (Vec<Product>, Vec<(ProviderId, ProviderError)>) {
        if let Some(port) = self.cdp_port {
            self.search_cdp(port, active_providers, query).await
        } else {
            self.search_wreq(active_providers, query).await
        }
    }

    /// Fetch Amazon US detail pages for products missing prices.
    async fn enrich_amazon_us_details(
        &self,
        port: u16,
        products: &mut Vec<Product>,
        query: &SearchQuery,
        usd_to_brl: Decimal,
    ) {
        let us_detail_targets: Vec<(usize, String)> = products
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.provider == ProviderId::AmazonUS
                    && !p.url.is_empty()
                    && p.price.listed_price == Decimal::ZERO
                    && scoring::tokens_match(&p.title, &query.query)
            })
            .map(|(i, p)| (i, p.url.clone()))
            .take(3)
            .collect();

        if us_detail_targets.is_empty() {
            return;
        }

        eprintln!("  Fetching Amazon US details...");
        info!(
            count = us_detail_targets.len(),
            "Fetching Amazon US prices + shipping from detail pages"
        );

        let mut handles = Vec::new();
        for (idx, url) in us_detail_targets {
            debug!(idx = idx, url = %url, "Visiting Amazon US detail page");
            let handle = tokio::spawn(async move {
                let details = crate::cdp::fetch_amazon_us_details(port, &url).await;
                (idx, details)
            });
            handles.push(handle);
        }

        for handle in handles {
            if let Ok((idx, Some(details))) = handle.await {
                if let Some(listed_price) = details.product_price {
                    if products[idx].price.listed_price == Decimal::ZERO {
                        products[idx].price.listed_price = listed_price;
                        products[idx].price.price_brl = listed_price;
                        info!(idx = idx, price_usd = %listed_price, "Got Amazon US product price");
                    }
                }

                if let Some(msrp_value) = details.msrp {
                    products[idx].price.original_price = Some(msrp_value);
                    info!(idx = idx, msrp_usd = %msrp_value, "Got Amazon US MSRP");
                }

                if let Some(seller_name) = details.sold_by {
                    let ships = details.ships_from.unwrap_or_default();
                    info!(idx = idx, sold_by = %seller_name, ships_from = %ships, "Got Amazon US seller");
                    products[idx].seller = Some(crate::models::SellerInfo {
                        name: seller_name,
                        reputation: None,
                        official_store: false,
                    });
                }

                if let Some(shipping_and_import) = details.shipping_import {
                    let shipping_brl = shipping_and_import * usd_to_brl;
                    products[idx].price.shipping_cost = Some(shipping_brl);
                    products[idx].price.tax = TaxInfo {
                        remessa_conforme: false,
                        taxes_included: true,
                        import_tax: None,
                        icms: None,
                        total_tax: Decimal::ZERO,
                        tax_regime: TaxRegime::InternationalStandard,
                    };
                    info!(
                        idx = idx,
                        shipping_usd = %shipping_and_import,
                        shipping_brl = %shipping_brl,
                        "Got Amazon US shipping + import"
                    );
                }
            }
        }

        // Remove Amazon US products that still have zero price
        products.retain(|p| {
            p.provider != ProviderId::AmazonUS || p.price.listed_price > Decimal::ZERO
        });
    }

    /// Fetch Amazon BR prices and `AliExpress` taxes from detail pages.
    async fn enrich_detail_pages(
        &self,
        port: u16,
        products: &mut [Product],
        query: &SearchQuery,
    ) {
        let br_detail_targets: Vec<(usize, String)> = products
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

        let ali_tax_targets: Vec<(usize, String)> = products
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

        if br_detail_targets.is_empty() && ali_tax_targets.is_empty() {
            return;
        }

        eprintln!("  Fetching detail pages...");

        let mut br_handles = Vec::new();
        for (idx, url) in br_detail_targets {
            let handle = tokio::spawn(async move {
                let fetched_price = crate::cdp::fetch_amazon_br_price(port, &url).await;
                (idx, fetched_price)
            });
            br_handles.push(handle);
        }

        let mut ali_handles = Vec::new();
        for (idx, url) in ali_tax_targets {
            let handle = tokio::spawn(async move {
                let tax = crate::cdp::fetch_aliexpress_tax(port, &url).await;
                (idx, tax)
            });
            ali_handles.push(handle);
        }

        // Collect Amazon BR results
        for handle in br_handles {
            if let Ok((idx, Some(fetched_price))) = handle.await {
                if fetched_price > Decimal::from(50) {
                    info!(idx = idx, price = %fetched_price, "Amazon BR price from detail page");
                    products[idx].price.listed_price = fetched_price;
                    products[idx].price.price_brl = fetched_price;
                    products[idx].price.total_cost = fetched_price;
                } else {
                    warn!(idx = idx, price = %fetched_price, "Amazon BR price too low, skipping");
                }
            }
        }

        // Collect AliExpress tax results and compute average tax ratio
        let mut ali_tax_ratios: Vec<Decimal> = Vec::new();
        let mut fetched_tax_indices = std::collections::HashSet::new();
        for handle in ali_handles {
            if let Ok((idx, Some(tax))) = handle.await {
                let listed = products[idx].price.listed_price;
                if listed > Decimal::ZERO {
                    let ratio = tax / listed;
                    ali_tax_ratios.push(ratio);
                    info!(idx = idx, tax = %tax, ratio = %ratio, "AliExpress tax fetched");
                }
                fetched_tax_indices.insert(idx);
                products[idx].price.tax.import_tax = Some(tax);
                products[idx].price.tax.total_tax = tax;
                products[idx].price.tax.taxes_included = true;
                products[idx].price.tax.tax_regime = TaxRegime::RemessaConformeOver50;
            }
        }

        // Apply average tax ratio to ALL remaining AliExpress products
        if !ali_tax_ratios.is_empty() {
            let avg_ratio = ali_tax_ratios.iter().sum::<Decimal>()
                / Decimal::from(u32::try_from(ali_tax_ratios.len()).unwrap_or(0));
            info!(avg_ratio = %avg_ratio, samples = ali_tax_ratios.len(), "AliExpress tax ratio");

            for (i, product) in products.iter_mut().enumerate() {
                if product.provider == ProviderId::AliExpress
                    && !fetched_tax_indices.contains(&i)
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

    /// Fetch Keepa price intelligence for the best ASIN candidate.
    async fn enrich_keepa(
        port: u16,
        products: &mut [Product],
        query: &SearchQuery,
        usd_to_brl: Decimal,
    ) {
        let asin_candidates = Self::rank_keepa_candidates(products, query);

        let Some((top_asin, top_score, top_reviews, top_price, top_domain, top_title)) = asin_candidates.first() else {
            return;
        };

        if *top_score < KEEPA_CANDIDATE_THRESHOLD {
            info!(
                asin = %top_asin,
                score = format!("{:.2}", top_score),
                title = %truncate_str(top_title, 50),
                "Skipping Keepa — best candidate score too low"
            );
            return;
        }

        info!(
            asin = %top_asin,
            score = format!("{:.2}", top_score),
            reviews = top_reviews,
            price = %top_price,
            domain = %top_domain,
            title = %truncate_str(top_title, 50),
            "Keepa target ASIN"
        );
        eprintln!("  Fetching Keepa prices...");

        let (insights, resolved_asin) =
            Self::fetch_keepa_with_fallback(port, top_asin, *top_domain, &asin_candidates).await;

        if insights.is_empty() {
            return;
        }

        let msrp = Self::compute_keepa_msrp(&insights, products, query, usd_to_brl, &resolved_asin);
        Self::attach_keepa_data(products, &insights, msrp, &resolved_asin, top_asin);

        // Fallback: if Keepa didn't provide MSRP, fetch detail page
        let has_msrp = products.iter().any(|p| {
            (p.platform_id == resolved_asin || p.platform_id == *top_asin)
                && p.price.original_price.is_some()
        });

        if !has_msrp {
            Self::fetch_msrp_from_detail_page(port, products, &resolved_asin, top_asin).await;
        }
    }

    /// Build and rank ASIN candidates for Keepa enrichment.
    fn rank_keepa_candidates(
        products: &[Product],
        query: &SearchQuery,
    ) -> Vec<(String, f64, u32, Decimal, u8, String)> {
        let mut seen_asins = std::collections::HashSet::new();
        let mut candidates: Vec<(String, f64, u32, Decimal, u8, String)> = products
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
                let relevance = scoring::score_product(&p.title, &query.query, 0.5, 1.0);
                (p.platform_id.clone(), relevance.total, reviews, p.price.listed_price, domain, p.title.clone())
            })
            .collect();

        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                .then(b.2.cmp(&a.2))
                .then(b.3.cmp(&a.3))
        });

        for (rank, (asin, relevance_score, reviews, _price, domain, title)) in candidates.iter().take(5).enumerate() {
            debug!(
                rank = rank + 1,
                asin = %asin,
                score = format!("{:.2}", relevance_score),
                reviews = reviews,
                domain = domain,
                title = %truncate_str(title, 60),
                "Keepa candidate"
            );
        }

        candidates
    }

    /// Fetch Keepa data with automatic fallback to US ASIN if needed.
    /// Returns the insights and the resolved ASIN (may differ from the original).
    async fn fetch_keepa_with_fallback(
        port: u16,
        top_asin: &str,
        top_domain: u8,
        candidates: &[(String, f64, u32, Decimal, u8, String)],
    ) -> (Vec<crate::keepa::KeepaInsight>, String) {
        let insights = tokio::time::timeout(
            std::time::Duration::from_secs(35),
            crate::keepa::fetch_keepa_comparison(port, top_asin, top_domain)
        ).await.unwrap_or_default();

        if insights.len() > 1 {
            return (insights, top_asin.to_string());
        }

        // Try a different US ASIN
        let us_fallback = candidates.iter()
            .find(|(asin, _, _, _, domain, _)| {
                *domain == crate::keepa::DOMAIN_US && asin != top_asin
            })
            .map(|(asin, ..)| asin.clone());

        if let Some(ref fallback_asin) = us_fallback {
            warn!(
                br_asin = %top_asin,
                us_asin = %fallback_asin,
                "Keepa comparison got {} domains, falling back to US ASIN",
                insights.len()
            );
            let fallback_insights = tokio::time::timeout(
                std::time::Duration::from_secs(35),
                crate::keepa::fetch_keepa_comparison(port, fallback_asin, crate::keepa::DOMAIN_US)
            ).await.unwrap_or_default();
            if fallback_insights.len() > insights.len() {
                return (fallback_insights, fallback_asin.clone());
            }
            return (insights, top_asin.to_string());
        }

        warn!("Keepa comparison got {} domains, no US fallback available", insights.len());
        // Last resort: try the same ASIN on US domain
        let single = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            crate::keepa::fetch_keepa_data(port, top_asin, crate::keepa::DOMAIN_US)
        ).await.ok().flatten();
        let mut combined = insights;
        if let Some(insight) = single {
            if !combined.iter().any(|k| k.domain == insight.domain) {
                combined.push(insight);
            }
        }
        (combined, top_asin.to_string())
    }

    /// Attach Keepa insights and MSRP to matching products.
    fn attach_keepa_data(
        products: &mut [Product],
        insights: &[crate::keepa::KeepaInsight],
        msrp: Option<Decimal>,
        resolved_asin: &str,
        original_asin: &str,
    ) {
        for product in products.iter_mut() {
            if product.platform_id != resolved_asin
                && product.platform_id != original_asin
            {
                continue;
            }

            product.keepa = insights.to_vec();

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
    }

    /// Fetch MSRP from Amazon US detail page when Keepa didn't provide one.
    async fn fetch_msrp_from_detail_page(
        port: u16,
        products: &mut [Product],
        resolved_asin: &str,
        original_asin: &str,
    ) {
        let detail_target = if resolved_asin == original_asin {
            original_asin
        } else {
            resolved_asin
        };
        info!("No Keepa MSRP, fetching detail page for {detail_target}");
        let url = format!("https://www.amazon.com/dp/{detail_target}");
        if let Some(details) = crate::cdp::fetch_amazon_us_details(port, &url).await {
            if let Some(msrp_value) = details.msrp {
                info!(asin = %detail_target, msrp = %msrp_value, "MSRP from detail page");
                for product in products.iter_mut() {
                    if product.platform_id == detail_target
                        || product.platform_id == original_asin
                    {
                        product.price.original_price = Some(msrp_value);
                    }
                }
            }
        }
    }

    /// Compute validated MSRP from Keepa insights (median across domains).
    fn compute_keepa_msrp(
        insights: &[crate::keepa::KeepaInsight],
        products: &[Product],
        query: &SearchQuery,
        usd_to_brl: Decimal,
        resolved_asin: &str,
    ) -> Option<Decimal> {
        let mut msrp_usd_values: Vec<Decimal> = insights.iter()
            .filter_map(crate::keepa::KeepaInsight::msrp_usd)
            .filter(|&m| m > Decimal::ZERO)
            .collect();
        msrp_usd_values.sort();

        let raw_msrp = if msrp_usd_values.is_empty() {
            None
        } else if msrp_usd_values.len() == 1 {
            Some(msrp_usd_values[0])
        } else {
            let mid = msrp_usd_values.len() / 2;
            Some(msrp_usd_values[mid])
        };

        // Sanity check: compare against median price of search results
        raw_msrp.and_then(|m| {
            let mut listing_prices: Vec<Decimal> = products.iter()
                .filter(|p| p.price.listed_price > Decimal::ZERO && scoring::tokens_match(&p.title, &query.query))
                .map(|p| p.price.listed_price)
                .collect();
            listing_prices.sort();
            let median_listing = if listing_prices.is_empty() {
                Decimal::ZERO
            } else {
                listing_prices[listing_prices.len() / 2]
            };
            let msrp_in_brl = m * usd_to_brl;
            if median_listing > Decimal::ZERO && msrp_in_brl < median_listing * rust_decimal_macros::dec!(0.10) {
                warn!(
                    msrp = %m,
                    msrp_brl = %msrp_in_brl,
                    median = %median_listing,
                    "Keepa MSRP suspiciously low vs median — discarding"
                );
                None
            } else {
                info!(asin = %resolved_asin, msrp = %m, domains = insights.len(), "Keepa MSRP");
                Some(m)
            }
        })
    }

    /// Fill missing prices from Keepa `buy_box` data.
    fn fill_keepa_prices(products: &mut [Product]) {
        for product in products.iter_mut() {
            if product.price.listed_price == Decimal::ZERO && !product.keepa.is_empty() {
                let own_domain = if product.provider == ProviderId::Amazon {
                    crate::keepa::DOMAIN_BR
                } else {
                    crate::keepa::DOMAIN_US
                };
                if let Some(keepa_price) = product.keepa.iter()
                    .find(|k| k.domain == own_domain)
                    .and_then(crate::keepa::KeepaInsight::best_new_price)
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
    }

    /// Apply tax calculations and currency conversion to all products.
    fn apply_taxes(products: &mut [Product], usd_to_brl: Decimal) {
        for product in products.iter_mut() {
            if product.price.currency == Currency::USD {
                product.price.price_brl =
                    product.price.listed_price * usd_to_brl;
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
                    usd_to_brl,
                );
            }

            product.price.total_cost = product.price.price_brl
                + product.price.shipping_cost.unwrap_or(Decimal::ZERO)
                + product.price.tax.total_tax;
        }
    }

    /// Signal-based relevance scoring: token filter + price clustering + combined score.
    fn apply_scoring(products: &mut Vec<Product>, query: &SearchQuery, usd_to_brl: Decimal) {
        // Step 1: token pre-filter (cheap)
        products.retain(|p| scoring::tokens_match(&p.title, &query.query));

        // Step 2: compute price cluster scores, anchored by MSRP when available
        let msrp_brl_value: Option<f64> = products.iter()
            .filter(|p| !p.keepa.is_empty())
            .find_map(|p| p.price.original_price)
            .map(|msrp_usd| {
                let brl = msrp_usd * usd_to_brl;
                brl.to_string().parse::<f64>().unwrap_or(0.0)
            });
        if let Some(ref m) = msrp_brl_value {
            debug!(msrp_brl = m, "Using MSRP anchor for price clustering");
        }
        let total_costs: Vec<Decimal> = products.iter().map(|p| p.price.total_cost).collect();
        let (cluster_scores, gap_ratio) = scoring::price_cluster_scores(&total_costs, msrp_brl_value);

        // Step 3: score each product and filter
        let before = products.len();
        let scored: Vec<(usize, RelevanceScore)> = products
            .iter()
            .enumerate()
            .zip(cluster_scores.iter())
            .map(|((i, p), &cs)| {
                let relevance = scoring::score_product(&p.title, &query.query, cs, gap_ratio);
                (i, relevance)
            })
            .collect();

        for (i, relevance) in &scored {
            if relevance.total < RELEVANT_THRESHOLD {
                debug!(
                    title = %truncate_str(&products[*i].title, 50),
                    score = format!("{:.2}", relevance.total),
                    structure = format!("{:.2}", relevance.title_structure),
                    cluster = format!("{:.2}", relevance.price_cluster),
                    similarity = format!("{:.2}", relevance.string_similarity),
                    "Filtered by relevance score"
                );
            }
        }

        let passing_indices: std::collections::HashSet<usize> = scored
            .iter()
            .filter(|(_, s)| s.total >= RELEVANT_THRESHOLD)
            .map(|(i, _)| *i)
            .collect();

        let mut idx = 0;
        products.retain(|_| {
            let passes = passing_indices.contains(&idx);
            idx += 1;
            passes
        });

        let filtered = before - products.len();
        if filtered > 0 {
            debug!(filtered = filtered, threshold = RELEVANT_THRESHOLD, "Signal-based filtering");
        }
    }

    /// Post-processing: price range filter, condition filter, sort, dedup, limit.
    fn post_filter(products: &mut Vec<Product>, query: &SearchQuery) {
        if let Some(min) = query.min_price {
            products.retain(|p| p.price.total_cost >= min);
        }
        if let Some(max) = query.max_price {
            products.retain(|p| p.price.total_cost <= max);
        }

        if let Some(condition) = query.condition {
            products.retain(|p| {
                p.condition == condition || p.condition == ProductCondition::Unknown
            });
        }

        match query.sort {
            SortOrder::TotalCost | SortOrder::PriceAsc => {
                products.sort_by_key(|p| p.price.total_cost);
            }
            SortOrder::PriceDesc => {
                products.sort_by_key(|p| std::cmp::Reverse(p.price.total_cost));
            }
            SortOrder::Rating => {
                products.sort_by(|a, b| {
                    b.rating
                        .unwrap_or(0.0)
                        .partial_cmp(&a.rating.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            SortOrder::Relevance => {}
        }

        let before_dedup = products.len();
        deduplicate_products(products);
        let deduped = before_dedup - products.len();
        if deduped > 0 {
            debug!(removed = deduped, "Deduplicated products");
        }

        if query.max_results < 50 {
            let max = query.max_results;
            let mut provider_counts = std::collections::HashMap::new();
            products.retain(|p| {
                let count = provider_counts.entry(p.provider).or_insert(0usize);
                *count += 1;
                *count <= max
            });
        }
    }

}

// Old is_relevant, title_match_score, is_accessory_title removed.
// All relevance logic now lives in src/scoring.rs using signal-based scoring.

/// Deduplicate products: remove near-identical listings from the same platform.
/// Products are considered duplicates when:
/// 1. Same platform + same `platform_id` (ASIN) — exact duplicate
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
