use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use tracing::{debug, info};

use crate::error::ProviderError;
use crate::models::{Currency, PriceInfo, Product, ProductCondition, SearchQuery, TaxInfo, TaxRegime};
use crate::providers::{Provider, ProviderId};

pub struct Ebay {
    client: wreq::Client,
}

impl Default for Ebay {
    fn default() -> Self {
        Self {
            client: crate::scraping::build_impersonating_client(15),
        }
    }
}

impl Ebay {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Provider for Ebay {
    fn name(&self) -> &'static str {
        "eBay"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Ebay
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        Ok(parse_ebay_html(html, max_results))
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let encoded = urlencoding::encode(&query.query);
        // LH_PrefLoc=3 = worldwide items, _sop=15 = price+shipping lowest first
        let url = format!(
            "https://www.ebay.com/sch/i.html?_nkw={encoded}&_sacat=0&LH_PrefLoc=3&_sop=15"
        );

        debug!(url = %url, "eBay search");

        let resp = self.client
            .get(&url)
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .send()
            .await?;

        let resp = resp.error_for_status()?;
        let html = resp.text().await?;
        debug!(html_len = html.len(), "eBay response");

        let products = parse_ebay_html(&html, query.max_results);
        info!(results = products.len(), "eBay search complete");

        Ok(products)
    }
}

fn parse_ebay_html(html: &str, max_results: usize) -> Vec<Product> {
    let document = Html::parse_document(html);
    let mut products = Vec::new();

    let card_sel = Selector::parse(".s-card, .s-item, li[data-viewport]").unwrap();
    let link_sel = Selector::parse("a[href*='/itm/'], a[href*='ebay.com/itm']").unwrap();
    let img_sel = Selector::parse("img[src*='ebayimg'], img[data-src*='ebayimg']").unwrap();

    let price_brl_re = regex_lite::Regex::new(r"R\$\s*([\d.]+,\d{2})").unwrap();
    let price_usd_re = regex_lite::Regex::new(r"US\s*\$\s*([\d,]+\.?\d*)").unwrap();
    let ship_brl_re = regex_lite::Regex::new(r"\+\s*R\$\s*([\d.]+,\d{2})\s*(?:de\s+)?(?:entrega|frete|envio)").unwrap();
    let ship_usd_re = regex_lite::Regex::new(r"\+\s*US\s*\$\s*([\d,]+\.?\d*)\s*(?:de\s+)?(?:shipping|entrega|frete)").unwrap();

    for card in document.select(&card_sel).take(max_results + 10) {
        if products.len() >= max_results { break; }

        let text = card.text().collect::<String>();
        if !is_valid_ebay_card(&text) { continue; }

        let title = extract_ebay_title(&card, &link_sel, &text);
        if title.is_empty() || title.len() < 10 { continue; }

        let Some((price, currency)) = extract_ebay_price(&text, &price_usd_re, &price_brl_re) else { continue };
        if price < Decimal::from(2) { continue; }
        if text.contains(" a R$") || text.contains(" to ") { continue; }

        let url = extract_ebay_url(&card, &link_sel);
        if url.is_empty() { continue; }

        let image = extract_ebay_image(&card, &img_sel);
        let text_lower = text.to_lowercase();
        let condition = detect_ebay_condition(&text_lower);
        let shipping = extract_ebay_shipping(&text, &text_lower, &ship_brl_re, &ship_usd_re);
        let platform_id = extract_ebay_item_id(&url);

        products.push(Product {
            provider: ProviderId::Ebay,
            platform_id,
            title,
            normalized_title: None,
            url,
            image_url: image,
            price: PriceInfo {
                listed_price: price,
                currency,
                price_brl: price, // Will be converted in search.rs for USD
                shipping_cost: shipping,
                tax: TaxInfo {
                    remessa_conforme: false,
                    taxes_included: false,
                    import_tax: None,
                    icms: None,
                    total_tax: Decimal::ZERO,
                    tax_regime: TaxRegime::Unknown,
                },
                total_cost: price + shipping.unwrap_or(Decimal::ZERO),
                original_price: None,
                installments: None,
            },
            seller: None,
            condition,
            rating: None,
            review_count: None,
            sold_count: None,
            domestic: false,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    info!(results = products.len(), "eBay parsed");
    products
}

fn is_valid_ebay_card(text: &str) -> bool {
    text.len() >= 30
        && !text.contains("Shop on eBay")
        && !text.contains("Resultados")
}

fn extract_ebay_title(
    card: &scraper::ElementRef<'_>,
    link_sel: &Selector,
    text: &str,
) -> String {
    card.select(link_sel).next()
        .and_then(|a| {
            a.value().attr("aria-label")
                .or_else(|| a.value().attr("title"))
                .map(|s| s.trim().to_string())
        })
        .or_else(|| {
            text.lines()
                .map(str::trim)
                .find(|l| l.len() > 15 && !l.starts_with("R$") && !l.starts_with("US"))
                .map(std::string::ToString::to_string)
        })
        .unwrap_or_default()
}

fn extract_ebay_price(
    text: &str,
    usd_re: &regex_lite::Regex,
    brl_re: &regex_lite::Regex,
) -> Option<(Decimal, Currency)> {
    usd_re.captures(text)
        .map(|cap| {
            let s = cap.get(1).unwrap().as_str().replace(',', "");
            let p = s.parse::<Decimal>().unwrap_or(Decimal::ZERO);
            (p, Currency::USD)
        })
        .or_else(|| {
            brl_re.captures(text).map(|cap| {
                let s = cap.get(1).unwrap().as_str().replace('.', "").replace(',', ".");
                let p = s.parse::<Decimal>().unwrap_or(Decimal::ZERO);
                (p, Currency::BRL)
            })
        })
}

fn extract_ebay_url(card: &scraper::ElementRef<'_>, link_sel: &Selector) -> String {
    card.select(link_sel).next()
        .and_then(|el| el.value().attr("href"))
        .map(std::string::ToString::to_string)
        .unwrap_or_default()
}

fn extract_ebay_image(card: &scraper::ElementRef<'_>, img_sel: &Selector) -> Option<String> {
    card.select(img_sel).next()
        .and_then(|el| el.value().attr("src").or_else(|| el.value().attr("data-src")))
        .map(std::string::ToString::to_string)
}

fn detect_ebay_condition(text_lower: &str) -> ProductCondition {
    if text_lower.contains("refurbished") || text_lower.contains("recondicionado") {
        ProductCondition::Refurbished
    } else if text_lower.contains("pre-owned") || text_lower.contains("usado") || text_lower.contains("seminovo") {
        ProductCondition::Used
    } else if text_lower.contains("brand new") || text_lower.contains("novo em folha") || text_lower.contains("novo") {
        ProductCondition::New
    } else {
        ProductCondition::Unknown
    }
}

fn extract_ebay_shipping(
    text: &str,
    text_lower: &str,
    ship_brl_re: &regex_lite::Regex,
    ship_usd_re: &regex_lite::Regex,
) -> Option<Decimal> {
    if text_lower.contains("frete grátis") || text_lower.contains("free shipping") {
        Some(Decimal::ZERO)
    } else if let Some(cap) = ship_brl_re.captures(text) {
        let s = cap.get(1).unwrap().as_str().replace('.', "").replace(',', ".");
        s.parse::<Decimal>().ok()
    } else if let Some(cap) = ship_usd_re.captures(text) {
        let s = cap.get(1).unwrap().as_str().replace(',', "");
        s.parse::<Decimal>().ok()
    } else {
        None
    }
}

fn extract_ebay_item_id(url: &str) -> String {
    url.split("/itm/")
        .nth(1)
        .and_then(|s| s.split('?').next())
        .unwrap_or("")
        .to_string()
}
