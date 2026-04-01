use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use tracing::{debug, info};
use regex_lite;

use crate::error::ProviderError;
use crate::models::{Currency, InstallmentInfo, PriceInfo, Product, ProductCondition, SearchQuery, SellerInfo, TaxInfo, TaxRegime};
use crate::providers::{Provider, ProviderId};

pub struct MagazineLuiza {
    client: Client,
}

impl Default for MagazineLuiza {
    fn default() -> Self {
        Self {
            client: crate::scraping::build_impersonating_client(20),
        }
    }
}

impl MagazineLuiza {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
#[allow(clippy::unnecessary_literal_bound)]
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

#[allow(clippy::too_many_lines)]
fn parse_next_data(html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let marker = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let Some(start) = html.find(marker) else {
        // Fallback: try HTML scraping when __NEXT_DATA__ is absent
        debug!("Magalu __NEXT_DATA__ not found, trying HTML fallback");
        return parse_magalu_html(html, max_results);
    };
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

    for item in items.iter().take(50) {
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
            .map_or_else(
                || {
                    let slug = title
                        .to_lowercase()
                        .chars()
                        .map(|c| if c.is_alphanumeric() { c } else { '-' })
                        .collect::<String>();
                    format!(
                        "https://www.magazineluiza.com.br/{slug}/s/p/{product_id}/",
                    )
                },
                |u| {
                    if u.starts_with('/') {
                        format!("https://www.magazineluiza.com.br{u}")
                    } else {
                        u.to_string()
                    }
                },
            );

        // Image URL has {w}x{h} placeholder
        let image = item["image"]
            .as_str()
            .map(|s| s.replace("{w}x{h}", "300x300"));

        #[allow(clippy::cast_possible_truncation)]
        let rating = item["rating"]["score"]
            .as_f64()
            .map(|r| r as f32)
            .filter(|r| *r > 0.0);

        #[allow(clippy::cast_possible_truncation)]
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
            keepa: Vec::new(),
        });
    }

    Ok(products)
}

/// Fallback HTML parser for Magalu when `__NEXT_DATA__` is not available.
/// Extracts products from rendered HTML using product card patterns.
#[allow(clippy::unnecessary_wraps)]
fn parse_magalu_html(html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let document = scraper::Html::parse_document(html);
    let mut products = Vec::new();

    // Magalu product cards: try multiple selector strategies
    let card_sel = scraper::Selector::parse(
        "[data-testid='product-card'], a[href*='/p/'], li[class*='product']"
    ).unwrap();
    let title_sel = scraper::Selector::parse("h2, [data-testid='product-title'], p").unwrap();
    let link_sel = scraper::Selector::parse("a[href*='/p/']").unwrap();

    let price_re = regex_lite::Regex::new(r"R\$\s*([\d.]+,\d{2})").unwrap();

    let mut seen_urls = std::collections::HashSet::new();

    for card in document.select(&card_sel).take(max_results * 3) {
        if products.len() >= max_results { break; }

        let text = card.text().collect::<String>();
        if text.len() < 20 { continue; }

        // Title: first h2/p with substantial text
        let title = card.select(&title_sel)
            .find_map(|el| {
                let t = el.text().collect::<String>().trim().to_string();
                if t.len() > 15 { Some(t) } else { None }
            })
            .unwrap_or_default();

        if title.is_empty() { continue; }

        // Price
        let price = price_re.captures(&text)
            .map_or(Decimal::ZERO, |cap| {
                let s = cap.get(1).unwrap().as_str().replace('.', "").replace(',', ".");
                s.parse::<Decimal>().unwrap_or(Decimal::ZERO)
            });

        if price == Decimal::ZERO { continue; }

        // URL
        let url = card.select(&link_sel).next()
            .or_else(|| {
                // If the card itself is a link
                if card.value().name() == "a" { Some(card) } else { None }
            })
            .and_then(|el| el.value().attr("href"))
            .map(|href| {
                if href.starts_with('/') {
                    format!("https://www.magazineluiza.com.br{href}")
                } else {
                    href.to_string()
                }
            })
            .unwrap_or_default();

        if url.is_empty() || !seen_urls.insert(url.clone()) { continue; }

        products.push(Product {
            provider: ProviderId::MagazineLuiza,
            platform_id: String::new(),
            title,
            normalized_title: None,
            url,
            image_url: None,
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
            rating: None,
            review_count: None,
            sold_count: None,
            domestic: true,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    info!(results = products.len(), "Magalu HTML fallback parsed");
    Ok(products)
}

#[allow(clippy::cast_possible_truncation)]
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
