use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct Shopee {
    cdp_port: Option<u16>,
}

impl Shopee {
    pub fn new(cdp_port: Option<u16>) -> Self {
        Self { cdp_port }
    }
}

#[async_trait]
impl Provider for Shopee {
    fn name(&self) -> &str {
        "Shopee"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Shopee
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        let products = parse_shopee_html(html, max_results);
        if products.is_empty() {
            if let Some(json_products) = try_extract_shopee_json(html, max_results) {
                return Ok(json_products);
            }
        }
        Ok(products)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let encoded = urlencoding::encode(&query.query);

        let url = format!("https://shopee.com.br/search?keyword={encoded}");

        let cdp_port = self.cdp_port.ok_or_else(|| {
            ProviderError::Browser(
                "Shopee requires CDP connection to your real browser. \
                 Set cdp_port = 9222 in config and launch: chromium --remote-debugging-port=9222"
                    .into(),
            )
        })?;

        debug!(cdp_port, "Shopee: connecting to real browser via CDP");

        let html = crate::browser::fetch_via_cdp(&url, cdp_port)
            .await
            .map_err(|e| ProviderError::Browser(e))?;

        debug!(html_len = html.len(), "Shopee browser response");

        // Check if we got redirected to login
        if html.contains("buyer/login") || html.contains("verify/captcha") {
            warn!("Shopee redirected to login/captcha — try `pechincha login shopee` first");
            return Err(ProviderError::Auth("Shopee requires login. Run `pechincha login shopee` first.".into()));
        }

        // Parse the rendered HTML for product data
        // Shopee renders product cards with data in the DOM
        let products = parse_shopee_html(&html, query.max_results);

        if products.is_empty() {
            // Try to extract from embedded JSON in scripts
            if let Some(json_products) = try_extract_shopee_json(&html, query.max_results) {
                info!(results = json_products.len(), "Shopee search complete (JSON)");
                return Ok(json_products);
            }
            warn!("Shopee returned no results from browser render");
        } else {
            info!(results = products.len(), "Shopee search complete (HTML)");
        }

        Ok(products)
    }
}

fn parse_shopee_html(html: &str, max_results: usize) -> Vec<Product> {
    // Shopee uses Tailwind CSS with product links like: /Product-Name-i.SHOPID.ITEMID
    // Parse using regex on the raw HTML since CSS selectors are obfuscated
    let mut products = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Find all product links: href="/Product-Name-i.SHOPID.ITEMID"
    let link_re = regex_lite::Regex::new(r#"href="(/[^"]*-i\.(\d+)\.(\d+)[^"]*)"#).unwrap();

    for cap in link_re.captures_iter(html) {
        if products.len() >= max_results {
            break;
        }

        let href = cap.get(1).unwrap().as_str();
        let _shop_id = cap.get(2).unwrap().as_str();
        let item_id = cap.get(3).unwrap().as_str();

        if seen.contains(item_id) {
            continue;
        }
        seen.insert(item_id.to_string());

        // Find the <a> tag containing this link and extract text content
        let link_pos = cap.get(0).unwrap().start();
        let a_start = html[..link_pos].rfind("<a ").unwrap_or(link_pos);
        let a_end = html[link_pos..].find("</a>").map(|i| link_pos + i + 4).unwrap_or(link_pos);
        let a_tag = &html[a_start..a_end];

        // Extract all text nodes from the <a> tag
        let text_re = regex_lite::Regex::new(r">([^<]+)<").unwrap();
        let texts: Vec<String> = text_re
            .captures_iter(a_tag)
            .filter_map(|c| {
                let t = c.get(1).unwrap().as_str().trim().to_string();
                if t.is_empty() || t.len() < 2 { None } else { Some(t) }
            })
            .collect();

        // Title: longest text that's not a price/percentage/sales count
        let title = texts
            .iter()
            .filter(|t| !t.starts_with("R$") && !t.starts_with('-') && !t.contains("vendido") && t.len() > 10)
            .max_by_key(|t| t.len())
            .cloned()
            .unwrap_or_default();

        if title.is_empty() {
            continue;
        }

        // Price: find "R$" text node followed by a number node
        let mut price = Decimal::ZERO;
        for (i, t) in texts.iter().enumerate() {
            if t == "R$" {
                if let Some(next) = texts.get(i + 1) {
                    price = parse_shopee_price(next);
                    if price > Decimal::ZERO {
                        break;
                    }
                }
            }
            // Also try "R$X.XXX,XX" in a single node
            if t.starts_with("R$") && t.len() > 2 {
                price = parse_shopee_price(&t[2..]);
                if price > Decimal::ZERO {
                    break;
                }
            }
        }

        if price == Decimal::ZERO {
            continue;
        }

        // Image: find img src in the <a> tag
        let img_re = regex_lite::Regex::new(r#"src="(https://[^"]*susercontent[^"]*)"#).unwrap();
        let image = img_re.captures(a_tag).map(|c| c.get(1).unwrap().as_str().to_string());

        // Sold count
        let sold = texts.iter()
            .find(|t| t.contains("vendido"))
            .and_then(|t| {
                t.split_whitespace().next()
                    .and_then(|n| n.replace('.', "").parse::<u32>().ok())
            });

        let url = format!("https://shopee.com.br{href}");

        products.push(Product {
            provider: ProviderId::Shopee,
            platform_id: item_id.to_string(),
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
                    remessa_conforme: true,
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
            sold_count: sold,
            domestic: true,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    products
}

fn try_extract_shopee_json(html: &str, _max_results: usize) -> Option<Vec<Product>> {
    // Shopee sometimes embeds search data in window.__INITIAL_STATE__ or similar
    let markers = ["window.__INITIAL_STATE__", "window.__data__"];

    for marker in markers {
        if let Some(start) = html.find(marker) {
            let eq = html[start..].find('=')?;
            let json_start = start + eq + 1;
            let trimmed = html[json_start..].trim_start();
            if !trimmed.starts_with('{') {
                continue;
            }
            // Try to parse
            let mut depth = 0;
            let mut end = 0;
            for (i, c) in trimmed.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end == 0 {
                continue;
            }
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&trimmed[..end]) {
                // Look for items array in the data
                if let Some(items) = find_items_array(&data) {
                    let mut products = Vec::new();
                    for item in items.iter().take(50) {
                        let name = item["name"]
                            .as_str()
                            .or_else(|| item["item_basic"]["name"].as_str())
                            .unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }

                        let raw_price = item["price"]
                            .as_i64()
                            .or_else(|| item["item_basic"]["price"].as_i64())
                            .unwrap_or(0);
                        let price = Decimal::from(raw_price) / Decimal::from(100_000);
                        if price == Decimal::ZERO {
                            continue;
                        }

                        let item_id = item["itemid"]
                            .as_i64()
                            .or_else(|| item["item_basic"]["itemid"].as_i64())
                            .unwrap_or(0);
                        let shop_id = item["shopid"]
                            .as_i64()
                            .or_else(|| item["item_basic"]["shopid"].as_i64())
                            .unwrap_or(0);

                        products.push(Product {
                            provider: ProviderId::Shopee,
                            platform_id: item_id.to_string(),
                            title: name.to_string(),
                            normalized_title: None,
                            url: format!("https://shopee.com.br/product/{shop_id}/{item_id}"),
                            image_url: None,
                            price: PriceInfo {
                                listed_price: price,
                                currency: Currency::BRL,
                                price_brl: price,
                                shipping_cost: None,
                                tax: TaxInfo {
                                    remessa_conforme: true,
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
                    if !products.is_empty() {
                        return Some(products);
                    }
                }
            }
        }
    }
    None
}

fn find_items_array(data: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    // Recursively search for an "items" array in the JSON
    if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
        if !items.is_empty() {
            return Some(items);
        }
    }
    if let Some(obj) = data.as_object() {
        for (_key, value) in obj {
            if let Some(items) = find_items_array(value) {
                return Some(items);
            }
        }
    }
    None
}

fn parse_shopee_price(text: &str) -> Decimal {
    // Shopee displays prices like "R$1.234,56" or "R$ 1.234"
    let cleaned: String = text
        .replace("R$", "")
        .replace(" ", "")
        .replace(".", "")
        .replace(",", ".");
    cleaned.trim().parse().unwrap_or(Decimal::ZERO)
}
