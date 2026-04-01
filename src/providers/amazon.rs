use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use tracing::{debug, info, warn};

use crate::error::ProviderError;
use crate::models::{Currency, PriceInfo, Product, ProductCondition, SearchQuery, TaxInfo, TaxRegime};
use crate::providers::{Provider, ProviderId};

pub struct Amazon {
    client: Client,
}

impl Default for Amazon {
    fn default() -> Self {
        Self {
            client: crate::scraping::build_impersonating_client(20),
        }
    }
}

impl Amazon {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Provider for Amazon {
    fn name(&self) -> &'static str {
        "Amazon BR"
    }

    fn id(&self) -> ProviderId {
        ProviderId::Amazon
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        Ok(parse_amazon_br_html(html, max_results))
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

        let products = parse_amazon_br_html(&html, query.max_results);

        info!(results = products.len(), "Amazon BR search complete");

        Ok(products)
    }
}

fn parse_amazon_br_html(html: &str, _max_results: usize) -> Vec<Product> {
    let document = Html::parse_document(html);

    let card_sel =
        Selector::parse("div[data-component-type='s-search-result']").unwrap();
    let title_sel = Selector::parse("h2 span").unwrap();
    let price_whole_sel = Selector::parse("span.a-price-whole").unwrap();
    let price_frac_sel = Selector::parse("span.a-price-fraction").unwrap();
    let link_sel = Selector::parse("h2 a.a-link-normal, h2 a[href*='/dp/'], a.s-underline-text").unwrap();
    let img_sel = Selector::parse("img.s-image").unwrap();
    let rating_sel = Selector::parse("span.a-icon-alt").unwrap();
    let review_sel = Selector::parse("a[href*='customerReviews'] span.a-size-base, a[href*='customerReviews'] span.a-size-small").unwrap();

    let mut products = Vec::new();

    for card in document.select(&card_sel).take(50) {
        let title = extract_amazon_title(&card, &title_sel, &img_sel);
        if title.is_empty() {
            continue;
        }

        let price = extract_amazon_price(&card, &price_whole_sel, &price_frac_sel);
        let link = extract_amazon_link(&card, &link_sel, "https://www.amazon.com.br");
        let image = extract_image(&card, &img_sel);
        let rating = extract_rating(&card, &rating_sel);
        let review_count = extract_review_count(&card, &review_sel);
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
            review_count,
            sold_count: None,
            domestic: true,
            fetched_at: Utc::now(),
            keepa: Vec::new(),
        });
    }

    products
}

fn extract_amazon_title(
    card: &scraper::ElementRef<'_>,
    title_sel: &Selector,
    img_sel: &Selector,
) -> String {
    let mut title = card
        .select(title_sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    // If title is too short (just brand name), try image alt for full product name
    if title.len() < 15 {
        if let Some(alt) = card.select(img_sel).next().and_then(|el| el.value().attr("alt")) {
            if alt.len() > title.len() {
                title = alt.trim().to_string();
            }
        }
    }

    title
}

fn extract_amazon_price(
    card: &scraper::ElementRef<'_>,
    whole_sel: &Selector,
    frac_sel: &Selector,
) -> Decimal {
    let whole = card
        .select(whole_sel)
        .next()
        .map(|el| {
            el.text()
                .collect::<String>()
                .replace(['.', ','], "")
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    let fraction = card
        .select(frac_sel)
        .next()
        .map_or_else(|| "00".to_string(), |el| el.text().collect::<String>().trim().to_string());

    if whole.is_empty() {
        Decimal::ZERO
    } else {
        format!("{whole}.{fraction}").parse().unwrap_or(Decimal::ZERO)
    }
}

fn extract_amazon_link(
    card: &scraper::ElementRef<'_>,
    link_sel: &Selector,
    base_url: &str,
) -> String {
    card.select(link_sel)
        .next()
        .and_then(|el| el.value().attr("href"))
        .map(|href| {
            if href.starts_with('/') {
                format!("{base_url}{href}")
            } else {
                href.to_string()
            }
        })
        .unwrap_or_default()
}

fn extract_image(card: &scraper::ElementRef<'_>, img_sel: &Selector) -> Option<String> {
    card.select(img_sel)
        .next()
        .and_then(|el| el.value().attr("src"))
        .map(std::string::ToString::to_string)
}

fn extract_rating(card: &scraper::ElementRef<'_>, rating_sel: &Selector) -> Option<f32> {
    card.select(rating_sel).next().and_then(|el| {
        let text = el.text().collect::<String>();
        text.split(' ')
            .next()
            .and_then(|s| s.replace(',', ".").parse::<f32>().ok())
    })
}

fn extract_review_count(card: &scraper::ElementRef<'_>, review_sel: &Selector) -> Option<u32> {
    card.select(review_sel).next().and_then(|el| {
        let text = el.text().collect::<String>();
        text.replace(['.', ','], "").trim().parse::<u32>().ok()
    })
}
