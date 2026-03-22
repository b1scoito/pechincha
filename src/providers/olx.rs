use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use tracing::{debug, info};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct Olx {
    client: Client,
}

impl Olx {
    pub fn new() -> Self {
        Self {
            client: crate::scraping::build_client_with_cookies(ProviderId::Olx, 15),
        }
    }
}

#[async_trait]
impl Provider for Olx {
    fn name(&self) -> &str {
        "OLX"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Olx
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_next_data(html, max_results)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let encoded = urlencoding::encode(&query.query);
        let url = format!("https://www.olx.com.br/brasil?q={encoded}");

        debug!(url = %url, "OLX search");

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "pt-BR,pt;q=0.9")
            .header("Accept", "text/html,application/xhtml+xml")
            .send()
            .await?
            .error_for_status()?;

        let html = resp.text().await?;
        debug!(html_len = html.len(), "OLX response");

        let products = parse_next_data(&html, query.max_results)?;
        info!(results = products.len(), "OLX search complete");
        Ok(products)
    }
}

fn parse_next_data(html: &str, _max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let marker = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html
        .find(marker)
        .ok_or_else(|| ProviderError::Scraping("OLX __NEXT_DATA__ not found".into()))?;
    let json_start = start + marker.len();
    let json_end = html[json_start..]
        .find("</script>")
        .ok_or_else(|| ProviderError::Scraping("OLX __NEXT_DATA__ closing tag missing".into()))?;

    let data: serde_json::Value = serde_json::from_str(&html[json_start..json_start + json_end])
        .map_err(|e| ProviderError::Parse(format!("OLX JSON parse error: {e}")))?;

    let ads = data
        .pointer("/props/pageProps/ads")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProviderError::Scraping("OLX ads list not found".into()))?;

    let mut products = Vec::new();

    for ad in ads.iter().take(50) {
        let title = ad["subject"]
            .as_str()
            .or_else(|| ad["title"].as_str())
            .unwrap_or_default()
            .to_string();

        if title.is_empty() {
            continue;
        }

        // priceValue is a string like "3500" (already in cents? no, it's the actual value)
        let price = ad["priceValue"]
            .as_str()
            .and_then(|s| s.parse::<Decimal>().ok())
            .or_else(|| {
                // Fallback: parse "price" which may be "R$ 3.500"
                ad["price"]
                    .as_str()
                    .and_then(|s| parse_brl_price(s))
            })
            .unwrap_or(Decimal::ZERO);

        if price == Decimal::ZERO {
            continue;
        }

        let original_price = ad["oldPrice"]
            .as_str()
            .and_then(|s| s.parse::<Decimal>().ok());

        let product_url = ad["friendlyUrl"]
            .as_str()
            .or_else(|| ad["url"].as_str())
            .unwrap_or("")
            .to_string();

        let image = ad["images"]
            .as_array()
            .and_then(|imgs| imgs.first())
            .and_then(|img| {
                img["original"]
                    .as_str()
                    .or_else(|| img["originalWebp"].as_str())
            })
            .map(|s| s.to_string());

        let list_id = ad["listId"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_default();

        let seller_name = ad["user"]["displayName"]
            .as_str()
            .unwrap_or("Particular")
            .to_string();

        let is_professional = ad["professionalAd"].as_bool().unwrap_or(false);

        // OLX items are typically used
        let condition = if title.to_lowercase().contains("novo")
            || title.to_lowercase().contains("lacrado")
        {
            ProductCondition::New
        } else {
            ProductCondition::Used
        };

        products.push(Product {
            provider: ProviderId::Olx,
            platform_id: list_id,
            title,
            normalized_title: None,
            url: product_url,
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
                original_price,
                installments: None,
            },
            seller: Some(SellerInfo {
                name: seller_name,
                reputation: None,
                official_store: is_professional,
            }),
            condition,
            rating: None,
            review_count: None,
            sold_count: None,
            domestic: true,
            fetched_at: Utc::now(),
        });
    }

    Ok(products)
}

fn parse_brl_price(text: &str) -> Option<Decimal> {
    let cleaned: String = text
        .replace("R$", "")
        .replace(" ", "")
        .replace(".", "")
        .replace(",", ".");
    cleaned.trim().parse().ok()
}
