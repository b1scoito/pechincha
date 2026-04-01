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
#[allow(clippy::unnecessary_literal_bound)]
impl Provider for Ebay {
    fn name(&self) -> &str {
        "eBay"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Ebay
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_ebay_html(html, max_results)
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

        let products = parse_ebay_html(&html, query.max_results)?;
        info!(results = products.len(), "eBay search complete");

        Ok(products)
    }
}

#[allow(clippy::too_many_lines, clippy::unnecessary_wraps)]
fn parse_ebay_html(html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let document = Html::parse_document(html);
    let mut products = Vec::new();

    // eBay 2024+ uses .s-card for product cards (replaced .s-item)
    let card_sel = Selector::parse(".s-card, .s-item, li[data-viewport]").unwrap();
    let link_sel = Selector::parse("a[href*='/itm/'], a[href*='ebay.com/itm']").unwrap();
    let img_sel = Selector::parse("img[src*='ebayimg'], img[data-src*='ebayimg']").unwrap();

    let brl_re = regex_lite::Regex::new(r"R\$\s*([\d.]+,\d{2})").unwrap();
    let usd_re = regex_lite::Regex::new(r"US\s*\$\s*([\d,]+\.?\d*)").unwrap();
    // Shipping in BRL: "+R$ 443,91 de frete" / "+ R$ 6.513,01 entrega"
    let ship_brl_re = regex_lite::Regex::new(r"\+\s*R\$\s*([\d.]+,\d{2})\s*(?:de\s+)?(?:entrega|frete|envio)").unwrap();
    // Shipping in USD: "+US $85.69 shipping" / "+US $1,230.99 de frete"
    let ship_usd_re = regex_lite::Regex::new(r"\+\s*US\s*\$\s*([\d,]+\.?\d*)\s*(?:de\s+)?(?:shipping|entrega|frete)").unwrap();

    for card in document.select(&card_sel).take(max_results + 10) {
        if products.len() >= max_results { break; }

        let text = card.text().collect::<String>();

        // Skip non-product cards
        if text.len() < 30 { continue; }
        if text.contains("Shop on eBay") || text.contains("Resultados") { continue; }

        // Title: first substantial line of text, or from link title
        let title = card.select(&link_sel).next()
            .and_then(|a| {
                // Try aria-label or title attribute first
                a.value().attr("aria-label")
                    .or_else(|| a.value().attr("title"))
                    .map(|s| s.trim().to_string())
            })
            .or_else(|| {
                // Fallback: first line of card text
                text.lines()
                    .map(str::trim)
                    .find(|l| l.len() > 15 && !l.starts_with("R$") && !l.starts_with("US"))
                    .map(std::string::ToString::to_string)
            })
            .unwrap_or_default();

        if title.is_empty() || title.len() < 10 { continue; }

        // Price: prefer USD, fallback to BRL
        let (price, currency) = if let Some(cap) = usd_re.captures(&text) {
            let s = cap.get(1).unwrap().as_str().replace(',', "");
            let p = s.parse::<Decimal>().unwrap_or(Decimal::ZERO);
            (p, Currency::USD)
        } else if let Some(cap) = brl_re.captures(&text) {
            let s = cap.get(1).unwrap().as_str().replace('.', "").replace(',', ".");
            let p = s.parse::<Decimal>().unwrap_or(Decimal::ZERO);
            (p, Currency::BRL)
        } else {
            continue;
        };

        // Skip items under $2 / R$10
        if price < Decimal::from(2) { continue; }
        // Skip price ranges ("R$ 256,39 a R$ 1.564,49") — these are multi-variant listings
        if text.contains(" a R$") || text.contains(" to ") { continue; }

        // URL
        let url = card.select(&link_sel).next()
            .and_then(|el| el.value().attr("href"))
            .map(std::string::ToString::to_string)
            .unwrap_or_default();

        if url.is_empty() { continue; }

        // Image
        let image = card.select(&img_sel).next()
            .and_then(|el| el.value().attr("src").or_else(|| el.value().attr("data-src")))
            .map(std::string::ToString::to_string);

        // Condition from card text
        let text_lower = text.to_lowercase();
        let condition = if text_lower.contains("refurbished") || text_lower.contains("recondicionado") {
            ProductCondition::Refurbished
        } else if text_lower.contains("pre-owned") || text_lower.contains("usado") || text_lower.contains("seminovo") {
            ProductCondition::Used
        } else if text_lower.contains("brand new") || text_lower.contains("novo em folha") || text_lower.contains("novo") {
            ProductCondition::New
        } else {
            ProductCondition::Unknown
        };

        // Shipping from text: extract cost or detect free shipping
        let shipping = if text_lower.contains("frete grátis") || text_lower.contains("free shipping") {
            Some(Decimal::ZERO)
        } else if let Some(cap) = ship_brl_re.captures(&text) {
            let s = cap.get(1).unwrap().as_str().replace('.', "").replace(',', ".");
            s.parse::<Decimal>().ok()
        } else if let Some(cap) = ship_usd_re.captures(&text) {
            let s = cap.get(1).unwrap().as_str().replace(',', "");
            // Store USD shipping — will be converted alongside the price in search.rs
            s.parse::<Decimal>().ok()
        } else {
            None
        };

        // Extract eBay item ID from URL
        let platform_id = url.split("/itm/")
            .nth(1)
            .and_then(|s| s.split('?').next())
            .unwrap_or("")
            .to_string();

        // ebay.com is the US site — all items ship internationally to Brazil.
        // Even when eBay shows BRL-converted prices for Brazilian visitors,
        // these are still US-based sellers and subject to import taxes.
        let is_domestic = false;

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
                    taxes_included: is_domestic,
                    import_tax: None,
                    icms: None,
                    total_tax: Decimal::ZERO,
                    tax_regime: if is_domestic { TaxRegime::Domestic } else { TaxRegime::Unknown },
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
            domestic: is_domestic,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    info!(results = products.len(), "eBay parsed");
    Ok(products)
}
