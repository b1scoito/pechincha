use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use tracing::{debug, info};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct AmazonUS {
    client: Client,
}

impl AmazonUS {
    pub fn new() -> Self {
        Self {
            client: crate::scraping::build_client_with_cookies(ProviderId::AmazonUS, 20),
        }
    }
}

#[async_trait]
impl Provider for AmazonUS {
    fn name(&self) -> &str {
        "Amazon US"
    }

    fn id(&self) -> ProviderId {
        ProviderId::AmazonUS
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_amazon_us_html(html, max_results)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let encoded = urlencoding::encode(&query.query);
        // Use dp/shipping=BR filter to show items that ship to Brazil
        let url = format!("https://www.amazon.com/s?k={encoded}");

        debug!(url = %url, "Amazon US search");

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "en-US,en;q=0.9,pt-BR;q=0.8")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .send()
            .await?;

        if resp.status() == 503 || resp.status() == 429 {
            return Err(ProviderError::Scraping(format!(
                "Amazon US returned {}",
                resp.status()
            )));
        }

        let resp = resp.error_for_status()?;
        let html = resp.text().await?;
        debug!(html_len = html.len(), "Amazon US response");

        let products = parse_amazon_us_html(&html, query.max_results)?;

        info!(results = products.len(), "Amazon US search complete");
        Ok(products)
    }
}

fn parse_amazon_us_html(html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let document = Html::parse_document(html);

    let card_selector =
        Selector::parse("div[data-component-type='s-search-result']").unwrap();
    let title_selector = Selector::parse("h2 span").unwrap();
    let price_whole_selector = Selector::parse("span.a-price-whole").unwrap();
    let price_fraction_selector = Selector::parse("span.a-price-fraction").unwrap();
    let link_selector = Selector::parse("h2 a.a-link-normal, h2 a[href*='/dp/'], a.s-underline-text").unwrap();
    let img_selector = Selector::parse("img.s-image").unwrap();
    let rating_selector = Selector::parse("span.a-icon-alt").unwrap();

    let mut products = Vec::new();

    for card in document.select(&card_selector).take(max_results) {
        // Try h2 span first, fallback to image alt for full product name
        let mut title = card
            .select(&title_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        // If title is too short (just brand name like "ASUS"), try image alt
        if title.len() < 15 {
            if let Some(alt) = card.select(&img_selector).next().and_then(|el| el.value().attr("alt")) {
                if alt.len() > title.len() {
                    title = alt.trim().to_string();
                }
            }
        }

        if title.is_empty() {
            continue;
        }

        let asin = card.value().attr("data-asin").unwrap_or("").to_string();
        if asin.is_empty() {
            continue;
        }

        // Get link — skip sponsored ads (link="#" or empty)
        let link = card
            .select(&link_selector)
            .next()
            .and_then(|el| el.value().attr("href"))
            .filter(|href| *href != "#" && !href.is_empty())
            .map(|href| {
                if href.starts_with('/') {
                    format!("https://www.amazon.com{href}")
                } else {
                    href.to_string()
                }
            });

        // Skip sponsored ads — they have link="#" and no real product page
        let url = match link {
            Some(l) => l,
            None => format!("https://www.amazon.com/dp/{asin}"),
        };

        // Sponsored ads typically have "#" links — check if this is one
        let card_html = card.html();
        let is_sponsored = card_html.contains("Sponsored") || card_html.contains("AdHolder");
        if is_sponsored {
            continue;
        }

        // Price — may be empty for "See options" products (price fetched from detail page later)
        let price_whole = card
            .select(&price_whole_selector)
            .next()
            .map(|el| {
                el.text()
                    .collect::<String>()
                    .replace(',', "")
                    .replace('.', "")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();

        let price_fraction = card
            .select(&price_fraction_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_else(|| "00".to_string());

        let price_usd: Decimal = if price_whole.is_empty() {
            Decimal::ZERO // Will be fetched from detail page
        } else {
            format!("{price_whole}.{price_fraction}").parse().unwrap_or(Decimal::ZERO)
        };

        let image = card
            .select(&img_selector)
            .next()
            .and_then(|el| el.value().attr("src"))
            .map(|s| s.to_string());

        let rating = card.select(&rating_selector).next().and_then(|el| {
            let text = el.text().collect::<String>();
            text.split(' ')
                .next()
                .and_then(|s| s.parse::<f32>().ok())
        });

        // Price is in USD — BRL conversion will be done by the orchestrator
        // Tax: international purchase, NOT in Remessa Conforme (Amazon US is not enrolled)
        // Shipping to Brazil is usually $10-40+ or not available for all items
        products.push(Product {
            provider: ProviderId::AmazonUS,
            platform_id: asin,
            title,
            normalized_title: None,
            url,
            image_url: image,
            price: PriceInfo {
                listed_price: price_usd,
                currency: Currency::USD,
                price_brl: price_usd, // Will be converted by orchestrator using exchange rate
                shipping_cost: None,   // Unknown until checkout — varies by item
                tax: TaxInfo {
                    remessa_conforme: false,
                    taxes_included: false,
                    import_tax: None,
                    icms: None,
                    total_tax: Decimal::ZERO,
                    tax_regime: TaxRegime::InternationalStandard,
                },
                total_cost: price_usd, // Will be recalculated with taxes by orchestrator
                original_price: None,
                installments: None,
            },
            seller: None,
            condition: ProductCondition::New,
            rating,
            review_count: None,
            sold_count: None,
            domestic: false, // International — triggers tax calculation
            fetched_at: Utc::now(),
        });
    }

    Ok(products)
}
