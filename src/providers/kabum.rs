use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use tracing::{debug, warn};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct Kabum {
    client: Client,
}

impl Kabum {
    pub fn new() -> Self {
        Self {
            client: crate::scraping::build_impersonating_client(20),
        }
    }
}

#[async_trait]
impl Provider for Kabum {
    fn name(&self) -> &str {
        "Kabum"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Kabum
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_next_data(html, max_results)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let search_term = query.query.replace(' ', "-");
        let url = format!("https://www.kabum.com.br/busca/{search_term}");

        debug!(url = %url, "Kabum search");

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "pt-BR,pt;q=0.9")
            .header("Accept", "text/html,application/xhtml+xml")
            .send()
            .await?
            .error_for_status()?;
        let html = resp.text().await?;

        // Kabum uses Next.js — product data is in __NEXT_DATA__ JSON
        let products = parse_next_data(&html, query.max_results)?;

        if products.is_empty() {
            warn!("Kabum returned 0 results");
        }

        Ok(products)
    }
}

fn parse_next_data(html: &str, _max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let marker = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html
        .find(marker)
        .ok_or_else(|| ProviderError::Scraping("__NEXT_DATA__ not found".into()))?;
    let json_start = start + marker.len();
    let json_end = html[json_start..]
        .find("</script>")
        .ok_or_else(|| ProviderError::Scraping("__NEXT_DATA__ closing tag not found".into()))?;
    let json_str = &html[json_start..json_start + json_end];

    let data: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| ProviderError::Parse(format!("__NEXT_DATA__ JSON parse error: {e}")))?;

    let items = data
        .pointer("/props/pageProps/data/catalogServer/data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProviderError::Scraping("Kabum catalog data not found in __NEXT_DATA__".into()))?;

    let mut products = Vec::new();

    for item in items.iter().take(50) {
        let name = item["name"].as_str().unwrap_or_default().to_string();
        if name.is_empty() {
            continue;
        }

        let price = item["priceWithDiscount"]
            .as_f64()
            .or_else(|| item["price"].as_f64())
            .and_then(|p| Decimal::try_from(p).ok())
            .unwrap_or(Decimal::ZERO);

        if price == Decimal::ZERO {
            continue;
        }

        let original_price = item["oldPrice"]
            .as_f64()
            .and_then(|p| Decimal::try_from(p).ok());

        let code = item["code"].as_u64().unwrap_or(0);
        let friendly_name = item["friendlyName"].as_str().unwrap_or("");
        let product_url = format!("https://www.kabum.com.br/produto/{code}/{friendly_name}");

        let image = item["images"]
            .as_array()
            .and_then(|imgs| imgs.first())
            .and_then(|img| img.as_str())
            .map(|s| s.to_string());

        let rating = item["averageScore"]
            .as_f64()
            .map(|r| r as f32)
            .filter(|r| *r > 0.0);

        let installments_str = item["maxInstallment"].as_str().unwrap_or("");
        let installments = parse_kabum_installments(installments_str);

        let manufacturer = item["manufacturer"]["name"]
            .as_str()
            .unwrap_or("Unknown")
            .to_string();

        products.push(Product {
            provider: ProviderId::Kabum,
            platform_id: code.to_string(),
            title: name,
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
                name: manufacturer,
                reputation: None,
                official_store: false,
            }),
            condition: ProductCondition::New,
            rating,
            review_count: None,
            sold_count: None,
            domestic: true,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    Ok(products)
}

fn parse_kabum_installments(text: &str) -> Option<InstallmentInfo> {
    // Format: "10x de R$ 1.249,90" or "12x de R$ 499,91"
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.splitn(2, "x de R$ ").collect();
    if parts.len() != 2 {
        return None;
    }
    let count: u8 = parts[0].trim().parse().ok()?;
    let amount_str = parts[1].replace('.', "").replace(',', ".");
    let amount: Decimal = amount_str.trim().parse().ok()?;

    Some(InstallmentInfo {
        count,
        amount_per: amount,
        interest_free: true, // Kabum typically shows interest-free installments
    })
}
