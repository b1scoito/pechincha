use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use tracing::{debug, info, warn};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct Amazon {
    client: Client,
}

impl Amazon {
    pub fn new() -> Self {
        Self {
            client: crate::scraping::build_client_with_cookies(ProviderId::Amazon, 20),
        }
    }
}

#[async_trait]
impl Provider for Amazon {
    fn name(&self) -> &str {
        "Amazon BR"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Amazon
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_amazon_br_html(html, max_results)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let encoded = urlencoding::encode(&query.query);
        let url = format!("https://www.amazon.com.br/s?k={encoded}");

        debug!(url = %url, "Amazon BR search");

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "pt-BR,pt;q=0.9,en;q=0.8")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .send()
            .await?;

        if resp.status() == 503 || resp.status() == 429 {
            warn!("Amazon returned {} — anti-bot active", resp.status());
            return Err(ProviderError::Scraping(
                format!("Amazon returned {}", resp.status()),
            ));
        }

        let resp = resp.error_for_status()?;
        let html = resp.text().await?;
        debug!(html_len = html.len(), "Amazon response");

        let products = parse_amazon_br_html(&html, query.max_results)?;

        info!(results = products.len(), "Amazon BR search complete");

        Ok(products)
    }
}

fn parse_amazon_br_html(html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
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
        let mut title = card
            .select(&title_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        // If title is too short (just brand name), try image alt for full product name
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

        let price_whole = card
            .select(&price_whole_selector)
            .next()
            .map(|el| {
                el.text()
                    .collect::<String>()
                    .replace('.', "")
                    .replace(',', "")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();

        let price_fraction = card
            .select(&price_fraction_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_else(|| "00".to_string());

        if price_whole.is_empty() {
            continue;
        }

        let price_str = format!("{price_whole}.{price_fraction}");
        let price: Decimal = price_str.parse().unwrap_or(Decimal::ZERO);

        if price == Decimal::ZERO {
            continue;
        }

        let link = card
            .select(&link_selector)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|href| {
                if href.starts_with('/') {
                    format!("https://www.amazon.com.br{href}")
                } else {
                    href.to_string()
                }
            })
            .unwrap_or_default();

        let image = card
            .select(&img_selector)
            .next()
            .and_then(|el| el.value().attr("src"))
            .map(|s| s.to_string());

        let rating = card.select(&rating_selector).next().and_then(|el| {
            let text = el.text().collect::<String>();
            text.split(' ')
                .next()
                .and_then(|s| s.replace(',', ".").parse::<f32>().ok())
        });

        let asin = card.value().attr("data-asin").unwrap_or("").to_string();

        // Fallback: construct URL from ASIN if link selector didn't match
        let url = if link.is_empty() && !asin.is_empty() {
            format!("https://www.amazon.com.br/dp/{asin}")
        } else {
            link
        };

        products.push(Product {
            provider: ProviderId::Amazon,
            platform_id: asin,
            title,
            normalized_title: None,
            url,
            image_url: image,
            price: PriceInfo {
                listed_price: price,
                currency: Currency::BRL,
                price_brl: price,
                shipping_cost: None,
                tax: TaxInfo {
                    remessa_conforme: false,
                    taxes_included: true,
                    import_tax: None,
                    icms: None,
                    total_tax: Decimal::ZERO,
                    tax_regime: TaxRegime::Domestic,
                },
                total_cost: price,
                original_price: None,
                installments: None,
            },
            seller: None,
            condition: ProductCondition::New,
            rating,
            review_count: None,
            sold_count: None,
            domestic: true,
            fetched_at: Utc::now(),
        });
    }

    Ok(products)
}
