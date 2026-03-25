use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct AliExpress {
    cdp_port: Option<u16>,
}

impl AliExpress {
    pub fn new(cdp_port: Option<u16>) -> Self {
        Self { cdp_port }
    }
}

#[async_trait]
impl Provider for AliExpress {
    fn name(&self) -> &str {
        "AliExpress"
    }

    fn id(&self) -> ProviderId {
        ProviderId::AliExpress
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        if let Some(products) = try_extract_dida_data(html, max_results) {
            if !products.is_empty() {
                return Ok(products);
            }
        }
        let products = scrape_rendered_html(html, max_results);
        Ok(products)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let search_term = query.query.replace(' ', "+");
        let url = format!(
            "https://pt.aliexpress.com/w/wholesale-{}.html",
            urlencoding::encode(&search_term)
        );
        let max = query.max_results;

        debug!(url = %url, "AliExpress search (headless browser)");

        let cdp_port = self.cdp_port.ok_or_else(|| {
            ProviderError::Browser(
                "AliExpress requires CDP connection to your real browser. \
                 Set cdp_port = 9222 in config and launch: chromium --remote-debugging-port=9222"
                    .into(),
            )
        })?;

        debug!(cdp_port, "AliExpress: connecting to real browser via CDP");

        let html = crate::browser::fetch_via_cdp(&url, cdp_port)
            .await
            .map_err(|e| ProviderError::Browser(e))?;

        debug!(html_len = html.len(), "AliExpress browser response");

        // Check for captcha/block page
        if html.contains("unusual traffic") || html.contains("captcha") {
            warn!("AliExpress showing captcha — try `pechincha login ali` first");
            return Err(ProviderError::Auth(
                "AliExpress captcha detected. Run `pechincha login ali` first.".into(),
            ));
        }

        // Try to extract from _dida_config_ (embedded in SSR script tag)
        if let Some(products) = try_extract_dida_data(&html, max) {
            if !products.is_empty() {
                info!(results = products.len(), "AliExpress (dida data)");
                return Ok(products);
            }
        }

        // Fallback: parse rendered HTML links
        let products = scrape_rendered_html(&html, max);
        if !products.is_empty() {
            info!(results = products.len(), "AliExpress (HTML)");
            return Ok(products);
        }

        warn!("AliExpress returned no extractable data");
        Ok(Vec::new())
    }
}

fn try_extract_dida_data(html: &str, _max_results: usize) -> Option<Vec<Product>> {
    let marker = "window._dida_config_";
    let start = html.find(marker)?;
    let eq = html[start..].find('=')?;
    let json_start = start + eq + 1;
    let trimmed = html[json_start..].trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }

    let mut depth = 0;
    let mut end = 0;
    for (i, &b) in trimmed.as_bytes().iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
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
        return None;
    }

    let config: serde_json::Value = serde_json::from_str(&trimmed[..end]).ok()?;
    let init_data = config.get("_init_data_")?.get("data")?.get("data")?;

    let mut products = Vec::new();
    if let Some(obj) = init_data.as_object() {
        for (_key, value) in obj {
            if let Some(item_list) = value
                .pointer("/fields/mods/itemList/content")
                .and_then(|v| v.as_array())
            {
                for item in item_list.iter().take(50) {
                    if let Some(p) = parse_ali_item(item) {
                        products.push(p);
                    }
                }
                if !products.is_empty() {
                    return Some(products);
                }
            }
        }
    }

    if products.is_empty() {
        None
    } else {
        Some(products)
    }
}

fn parse_ali_item(item: &serde_json::Value) -> Option<Product> {
    let title = item["title"]["displayTitle"]
        .as_str()
        .or_else(|| item["title"]["text"].as_str())?
        .to_string();

    let price_str = item["prices"]["salePrice"]["formattedPrice"]
        .as_str()
        .or_else(|| item["prices"]["salePrice"]["minPrice"].as_str())
        .unwrap_or("0");

    let price = parse_ali_price(price_str);
    if price == Decimal::ZERO {
        return None;
    }

    let product_id = item["productId"].as_str().unwrap_or("").to_string();
    let url = if !product_id.is_empty() {
        format!("https://pt.aliexpress.com/item/{product_id}.html")
    } else {
        String::new()
    };

    let image = item["image"]["imgUrl"].as_str().map(|s| {
        if s.starts_with("//") {
            format!("https:{s}")
        } else {
            s.to_string()
        }
    });

    let rating = item["evaluation"]["starRating"]
        .as_str()
        .and_then(|s| s.parse::<f32>().ok());

    let (currency, price_brl) = if price_str.contains("R$") {
        (Currency::BRL, price)
    } else {
        (Currency::USD, price)
    };

    Some(Product {
        provider: ProviderId::AliExpress,
        platform_id: product_id,
        title,
        normalized_title: None,
        url,
        image_url: image,
        price: PriceInfo {
            listed_price: price,
            currency,
            price_brl,
            shipping_cost: None,
            tax: TaxInfo {
                remessa_conforme: true,
                taxes_included: true,
                import_tax: None,
                icms: None,
                total_tax: Decimal::ZERO,
                tax_regime: TaxRegime::Unknown,
            },
            total_cost: price_brl,
            original_price: None,
            installments: None,
        },
        seller: None,
        condition: ProductCondition::New,
        rating,
        review_count: None,
        sold_count: None,
        domestic: false,
        fetched_at: Utc::now(),
        keepa: Vec::new(),
    })
}

fn scrape_rendered_html(html: &str, max_results: usize) -> Vec<Product> {
    // AliExpress uses obfuscated CSS classes. Parse using regex on raw HTML.
    // Card structure: <div class="... card-out-wrapper">
    //   <a class="... search-card-item" href="//pt.aliexpress.com/item/ID.html...">
    //   Title in <img alt="..."> or as text node
    //   Price split: "R$" + "853" + "99" or "R$1.911,33"
    let mut products = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Find card wrappers containing product links
    let card_re = regex_lite::Regex::new(r"card-out-wrapper").unwrap();
    let item_re = regex_lite::Regex::new(r#"item/(\d+)\.html"#).unwrap();
    let img_alt_re = regex_lite::Regex::new(r#"alt="([^"]{15,200})""#).unwrap();
    let img_src_re = regex_lite::Regex::new(r#"src="(//ae-pic[^"]+)""#).unwrap();
    let price_full_re = regex_lite::Regex::new(r"R\$([\d.,]+)").unwrap();

    let card_positions: Vec<usize> = card_re.find_iter(html).map(|m| m.start()).collect();

    for (i, &pos) in card_positions.iter().enumerate() {
        if products.len() >= max_results {
            break;
        }

        // Card content goes from this position to the next card (or +3000 chars)
        let card_end = card_positions.get(i + 1).copied().unwrap_or(pos + 4000).min(pos + 4000);
        let card = &html[pos..card_end];

        // Extract product ID
        let product_id = match item_re.captures(card) {
            Some(cap) => {
                let id = cap.get(1).unwrap().as_str().to_string();
                if seen.contains(&id) { continue; }
                seen.insert(id.clone());
                id
            }
            None => continue,
        };

        // Title from img alt attribute (most reliable)
        let title = img_alt_re.captures(card)
            .map(|c| c.get(1).unwrap().as_str().to_string())
            .unwrap_or_default();

        if title.is_empty() || title.len() < 10 {
            continue;
        }

        // Price: find first "R$X.XXX,XX" pattern (full price in one node)
        // or construct from separate "R$" + integer + cents nodes
        let price = if let Some(cap) = price_full_re.captures(card) {
            parse_ali_price(cap.get(1).unwrap().as_str())
        } else {
            Decimal::ZERO
        };

        if price == Decimal::ZERO {
            continue;
        }

        // Image
        let image = img_src_re.captures(card)
            .map(|c| format!("https:{}", c.get(1).unwrap().as_str()));

        let url = format!("https://pt.aliexpress.com/item/{product_id}.html");

        products.push(Product {
            provider: ProviderId::AliExpress,
            platform_id: product_id,
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
                    tax_regime: TaxRegime::Unknown,
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
            domestic: false,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    products
}

fn parse_ali_price(text: &str) -> Decimal {
    let cleaned: String = text
        .replace("R$", "")
        .replace("US$", "")
        .replace("$", "")
        .replace(" ", "");

    if cleaned.contains(',') {
        let normalized = cleaned.replace('.', "").replace(',', ".");
        normalized.trim().parse().unwrap_or(Decimal::ZERO)
    } else {
        cleaned.trim().parse().unwrap_or(Decimal::ZERO)
    }
}
