use async_trait::async_trait;
use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use tracing::{debug, warn};

use crate::error::ProviderError;
use crate::models::*;
use crate::providers::{Provider, ProviderId};

pub struct MercadoLivre {
    client: Client,
}

impl MercadoLivre {
    pub fn new() -> Self {
        Self {
            client: crate::scraping::build_client_with_cookies(ProviderId::MercadoLivre, 15),
        }
    }

    async fn search_scraping(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        let search_term = query.query.replace(' ', "-");
        let url = format!("https://lista.mercadolivre.com.br/{search_term}");

        debug!(url = %url, "Mercado Livre scraping search");

        let resp = self
            .client
            .get(&url)
            .header("Accept-Language", "pt-BR,pt;q=0.9,en;q=0.8")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .header("Accept-Encoding", "gzip, deflate, br")
            .send()
            .await?
            .error_for_status()?;
        let html = resp.text().await?;

        let products = parse_ml_html(&html, query.max_results)?;

        if products.is_empty() {
            warn!("Mercado Livre scraping returned 0 results — selectors may be outdated");
        }

        Ok(products)
    }
}

fn parse_ml_html(html: &str, _max_results: usize) -> Result<Vec<Product>, ProviderError> {
    let document = Html::parse_document(html);

    let card_selector = Selector::parse("li.ui-search-layout__item").unwrap();
    let title_selector = Selector::parse("a.poly-component__title").unwrap();
    let price_selector =
        Selector::parse(".poly-price__current .andes-money-amount__fraction").unwrap();
    let img_selector = Selector::parse("img.poly-component__picture").unwrap();
    let shipping_selector = Selector::parse(".poly-component__shipping").unwrap();

    let mut products = Vec::new();

    for card in document.select(&card_selector).take(50) {
        let title_el = match card.select(&title_selector).next() {
            Some(el) => el,
            None => continue,
        };

        let title = title_el.text().collect::<String>();
        if title.is_empty() {
            continue;
        }

        let link = title_el
            .value()
            .attr("href")
            .unwrap_or("")
            .to_string();

        let price_str = card
            .select(&price_selector)
            .next()
            .map(|el| el.text().collect::<String>())
            .unwrap_or_default()
            .replace('.', "");

        let price: Decimal = price_str.trim().parse().unwrap_or(Decimal::ZERO);
        if price == Decimal::ZERO {
            continue;
        }

        let image = card
            .select(&img_selector)
            .next()
            .and_then(|el| el.value().attr("src").or(el.value().attr("data-src")))
            .map(|s| s.to_string());

        let shipping_text = card
            .select(&shipping_selector)
            .next()
            .map(|el| el.text().collect::<String>())
            .unwrap_or_default();

        let shipping_cost = if shipping_text.to_lowercase().contains("grátis") {
            Some(Decimal::ZERO)
        } else {
            None
        };

        // Detect international (cross-border trade) listings
        let card_html = card.html();
        let is_international = card_html.contains("Internacional")
            || card_html.contains("poly-component__cbt");
        let domestic = !is_international;

        let tax_regime = if is_international {
            TaxRegime::Unknown // Will be calculated by tax engine
        } else {
            TaxRegime::Domestic
        };

        products.push(Product {
            provider: ProviderId::MercadoLivre,
            platform_id: String::new(),
            title,
            normalized_title: None,
            url: link,
            image_url: image,
            price: PriceInfo {
                listed_price: price,
                currency: Currency::BRL,
                price_brl: price,
                shipping_cost,
                tax: TaxInfo {
                    remessa_conforme: is_international, // ML international uses Remessa Conforme
                    taxes_included: !is_international,  // Domestic: taxes included. International: may not be
                    import_tax: None,
                    icms: None,
                    total_tax: Decimal::ZERO,
                    tax_regime,
                },
                total_cost: price + shipping_cost.unwrap_or(Decimal::ZERO),
                original_price: None,
                installments: None,
            },
            seller: None,
            condition: ProductCondition::Unknown,
            rating: None,
            review_count: None,
            sold_count: None,
            domestic,
            fetched_at: Utc::now(),
        });
    }

    Ok(products)
}

#[async_trait]
impl Provider for MercadoLivre {
    fn name(&self) -> &str {
        "Mercado Livre"
    }

    fn id(&self) -> ProviderId {
        ProviderId::MercadoLivre
    }

    fn is_available(&self) -> bool {
        true
    }

    fn parse_html(&self, html: &str, max_results: usize) -> Result<Vec<Product>, ProviderError> {
        parse_ml_html(html, max_results)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError> {
        self.search_scraping(query).await
    }
}
