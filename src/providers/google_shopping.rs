use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use regex_lite::Regex;
use scraper::{Html, Selector};
use tracing::{debug, info};

use crate::error::ProviderError;
use crate::models::{Currency, PriceInfo, Product, ProductCondition, SearchQuery, SellerInfo, TaxInfo, TaxRegime};
use crate::providers::{Provider, ProviderId};

pub struct GoogleShopping;

impl Default for GoogleShopping {
    fn default() -> Self {
        Self
    }
}

impl GoogleShopping {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Provider for GoogleShopping {
    fn name(&self) -> &'static str {
        "Google Shopping"
    }

    fn id(&self) -> ProviderId {
        ProviderId::GoogleShopping
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        Ok(parse_google_shopping_html(html, max_results))
    }

    async fn search(&self, _query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        Err(ProviderError::Browser(
            "Google Shopping requires CDP. Launch browser with --remote-debugging-port=9222".into(),
        ))
    }
}

fn parse_google_shopping_html(html: &str, max_results: usize) -> Vec<Product> {
    let document = Html::parse_document(html);
    let mut products = Vec::new();
    let mut seen_titles = std::collections::HashSet::new();

    let price_re = Regex::new(r"R\$\s*([\d.]+,\d{2})").unwrap();
    let h3_sel = Selector::parse("h3").unwrap();
    let link_sel = Selector::parse("a[href]").unwrap();

    for h3 in document.select(&h3_sel) {
        if products.len() >= max_results { break; }

        let title = h3.text().collect::<String>().trim().to_string();
        if !is_valid_gs_title(&title) { continue; }

        let title_lower = title.to_lowercase();
        if !seen_titles.insert(title_lower) { continue; }

        let (card_text, card_html) = walk_up_to_card(&h3);
        if card_text.is_empty() { continue; }

        let price = price_re.captures(&card_text)
            .map_or(Decimal::ZERO, |cap| parse_brl_price(cap.get(1).unwrap().as_str()));
        if price == Decimal::ZERO { continue; }

        let url = extract_gs_store_url(&card_html, &link_sel);
        let seller = extract_store_name(&card_text).map(|name| SellerInfo {
            name,
            reputation: None,
            official_store: false,
        });

        products.push(build_gs_product(title, price, url, seller));
    }

    // Fallback: regex-based extraction from raw HTML
    if products.is_empty() {
        debug!("Google Shopping h3 parsing found 0 results, trying regex fallback");
        products = parse_google_shopping_regex(html, max_results);
    }

    info!(results = products.len(), "Google Shopping parsed");
    products
}

fn is_valid_gs_title(title: &str) -> bool {
    title.len() >= 15
        && !title.contains("patrocinado")
        && !title.contains("Sobre esse")
        && !title.contains("Avaliações")
        && !title.contains("Mais opções")
        && !title.contains("avaliações")
}

fn walk_up_to_card(h3: &scraper::ElementRef<'_>) -> (String, String) {
    let mut card_text = String::new();
    let mut card_html = String::new();
    let mut node = h3.parent();

    for _ in 0..5 {
        if let Some(n) = node {
            if let Some(el_ref) = scraper::ElementRef::wrap(n) {
                let text: String = el_ref.text().collect();
                let html_content = el_ref.html();
                if text.contains("R$") && el_ref.value().name() == "div" {
                    return (text, html_content);
                }
                card_text = text;
                card_html = html_content;
            }
            node = n.parent();
        } else {
            break;
        }
    }

    (card_text, card_html)
}

fn extract_gs_store_url(card_html: &str, link_sel: &Selector) -> String {
    let card_doc = Html::parse_fragment(card_html);
    card_doc.select(link_sel)
        .filter_map(|a| a.value().attr("href"))
        .find(|href| {
            href.contains("mercadolivre") || href.contains("amazon")
                || href.contains("magazineluiza") || href.contains("kabum")
                || href.contains("shopee") || href.contains("aliexpress")
                || href.contains("/shopping/product/")
                || (href.starts_with("http") && !href.contains("google.com"))
        })
        .map(std::string::ToString::to_string)
        .unwrap_or_default()
}

fn build_gs_product(title: String, price: Decimal, url: String, seller: Option<SellerInfo>) -> Product {
    Product {
        provider: ProviderId::GoogleShopping,
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
        seller,
        condition: ProductCondition::New,
        rating: None,
        review_count: None,
        sold_count: None,
        domestic: true,
        fetched_at: Utc::now(),
        keepa: Vec::new(),
    }
}

/// Extract store name from card text by looking for known patterns.
fn extract_store_name(text: &str) -> Option<String> {
    let stores = [
        "Mercado Livre", "Amazon", "Magazine Luiza", "Kabum",
        "Shopee", "AliExpress", "Americanas", "Casas Bahia",
        "Ponto", "Submarino", "Carrefour", "Extra",
    ];
    for store in &stores {
        if text.contains(store) {
            return Some(store.to_string());
        }
    }
    // Try "De STORE" pattern
    let re = Regex::new(r"(?:De |de )([A-Z][a-zA-Z .]+)").ok()?;
    re.captures(text)
        .map(|cap| cap.get(1).unwrap().as_str().trim().to_string())
}

/// Fallback: find <h3> titles near R$ prices in raw HTML using regex.
fn parse_google_shopping_regex(html: &str, max_results: usize) -> Vec<Product> {
    let mut products = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Find h3 tags with their content
    let h3_re = Regex::new(r"<h3[^>]*>([^<]{15,200})</h3>").unwrap();
    let price_re = Regex::new(r"R\$\s*([\d.]+,\d{2})").unwrap();
    let link_re = Regex::new(r#"href="(https?://(?:www\.)?(?:mercadolivre|amazon|magazineluiza|kabum)[^"]+)"#).unwrap();

    for cap in h3_re.captures_iter(html) {
        if products.len() >= max_results { break; }

        let title = cap.get(1).unwrap().as_str().trim().to_string();
        if title.len() < 15 { continue; }
        if title.contains("patrocinado") || title.contains("Avaliações") { continue; }

        let title_lower = title.to_lowercase();
        if !seen.insert(title_lower) { continue; }

        // Look forward up to 500 chars for price
        let h3_end = cap.get(0).unwrap().end();
        let search_end = (h3_end + 500).min(html.len());
        // Ensure char boundary
        let mut end = search_end;
        while end < html.len() && !html.is_char_boundary(end) { end += 1; }
        let after = &html[h3_end..end];

        let price = price_re.captures(after)
            .map_or(Decimal::ZERO, |c| parse_brl_price(c.get(1).unwrap().as_str()));

        if price == Decimal::ZERO { continue; }

        // Look backward for a store link
        let search_start = cap.get(0).unwrap().start().saturating_sub(500);
        let mut start = search_start;
        while start > 0 && !html.is_char_boundary(start) { start += 1; }
        let before = &html[start..cap.get(0).unwrap().start()];
        let url = link_re.captures(before)
            .map(|c| c.get(1).unwrap().as_str().to_string())
            .unwrap_or_default();

        products.push(Product {
            provider: ProviderId::GoogleShopping,
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

    products
}

fn parse_brl_price(text: &str) -> Decimal {
    let cleaned = text.replace('.', "").replace(',', ".");
    cleaned.trim().parse().unwrap_or(Decimal::ZERO)
}
