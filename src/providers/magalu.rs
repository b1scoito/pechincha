use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use tracing::{debug, info};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct MagazineLuiza {
    client: Client,
}

impl MagazineLuiza {
    pub fn new() -> Self {
        Self {
            client: crate::scraping::build_client_with_cookies(ProviderId::MagazineLuiza, 20),
        }
    }
}

#[async_trait]
impl Provider for MagazineLuiza {
    fn name(&self) -> &str {
        "Magazine Luiza"
    }

    fn id(&self) -> ProviderId {
        ProviderId::MagazineLuiza
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_next_data(html, max_results)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let encoded = urlencoding::encode(&query.query);
        let url = format!("https://www.magazineluiza.com.br/busca/{encoded}/");

        debug!(url = %url, "Magazine Luiza search");

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "pt-BR,pt;q=0.9")
            .header("Accept", "text/html,application/xhtml+xml")
            .send()
            .await?
            .error_for_status()?;

        let html = resp.text().await?;
        debug!(html_len = html.len(), "Magalu response");

        let products = parse_next_data(&html, query.max_results)?;

        info!(results = products.len(), "Magazine Luiza search complete");
        Ok(products)
    }
}

fn parse_next_data(html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let marker = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html
        .find(marker)
        .ok_or_else(|| ProviderError::Scraping("Magalu __NEXT_DATA__ not found".into()))?;
    let json_start = start + marker.len();
    let json_end = html[json_start..]
        .find("</script>")
        .ok_or_else(|| ProviderError::Scraping("Magalu __NEXT_DATA__ closing tag missing".into()))?;

    let data: serde_json::Value = serde_json::from_str(&html[json_start..json_start + json_end])
        .map_err(|e| ProviderError::Parse(format!("Magalu JSON parse error: {e}")))?;

    let items = data
        .pointer("/props/pageProps/data/search/products")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProviderError::Scraping("Magalu product list not found".into()))?;

    let mut products = Vec::new();

    for item in items.iter().take(max_results) {
        if item["available"].as_bool() == Some(false) {
            continue;
        }

        let title = item["title"].as_str().unwrap_or_default().to_string();
        if title.is_empty() {
            continue;
        }

        // bestPrice is the actual price (with discounts), fullPrice is without discount
        let price = item["price"]["bestPrice"]
            .as_str()
            .and_then(|s| s.parse::<Decimal>().ok())
            .or_else(|| {
                item["price"]["fullPrice"]
                    .as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
            })
            .unwrap_or(Decimal::ZERO);

        if price == Decimal::ZERO {
            continue;
        }

        let original_price = item["price"]["fullPrice"]
            .as_str()
            .and_then(|s| s.parse::<Decimal>().ok());

        let product_id = item["id"].as_str().unwrap_or("").to_string();

        // Build URL from product slug
        let product_url = item["url"]
            .as_str()
            .map(|u| {
                if u.starts_with('/') {
                    format!("https://www.magazineluiza.com.br{u}")
                } else {
                    u.to_string()
                }
            })
            .unwrap_or_else(|| {
                let slug = title
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '-' })
                    .collect::<String>();
                format!(
                    "https://www.magazineluiza.com.br/{}/{}/p/{}/",
                    slug, "s", product_id
                )
            });

        // Image URL has {w}x{h} placeholder
        let image = item["image"]
            .as_str()
            .map(|s| s.replace("{w}x{h}", "300x300"));

        let rating = item["rating"]["score"]
            .as_f64()
            .map(|r| r as f32)
            .filter(|r| *r > 0.0);

        let review_count = item["rating"]["count"]
            .as_u64()
            .map(|n| n as u32);

        let seller_name = item["seller"]["description"]
            .as_str()
            .unwrap_or("Magazine Luiza")
            .to_string();

        let installments = parse_installment(&item["installment"]);

        products.push(Product {
            provider: ProviderId::MagazineLuiza,
            platform_id: product_id,
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
                installments,
            },
            seller: Some(SellerInfo {
                name: seller_name,
                reputation: None,
                official_store: item["seller"]["category"].as_str() == Some("1p"),
            }),
            condition: ProductCondition::New,
            rating,
            review_count,
            sold_count: None,
            domestic: true,
            fetched_at: Utc::now(),
        });
    }

    Ok(products)
}

fn parse_installment(v: &serde_json::Value) -> Option<InstallmentInfo> {
    let count = v["quantity"].as_u64()? as u8;
    let amount: Decimal = v["amount"].as_str()?.parse().ok()?;
    let interest: Decimal = v["interest"].as_str()?.parse().ok()?;

    Some(InstallmentInfo {
        count,
        amount_per: amount,
        interest_free: interest == Decimal::ZERO,
    })
}
