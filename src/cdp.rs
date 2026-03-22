//! CDP-first search engine. Opens all provider tabs concurrently in the user's
//! real browser, waits for JS to render, extracts HTML from each.

use chaser_oxide::{Browser, Handler};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;
use tracing::{debug, error, info, warn};

use crate::error::ProviderError;
use crate::providers::ProviderId;

/// How long to wait for initial page render after navigation.
const RENDER_WAIT: Duration = Duration::from_secs(5);
/// How long to wait after scrolling for more content to load.
const SCROLL_WAIT: Duration = Duration::from_secs(2);

/// Shared browser connection — connected once, reused for all searches.
static BROWSER: OnceCell<Arc<Browser>> = OnceCell::const_new();

/// Connect to the browser (or reuse existing connection).
async fn get_browser(cdp_port: u16) -> Result<Arc<Browser>, String> {
    BROWSER
        .get_or_try_init(|| async {
            let url = format!("http://127.0.0.1:{cdp_port}");
            let (browser, mut handler) = Browser::connect(&url).await.map_err(|e| {
                format!(
                    "Failed to connect to browser on port {cdp_port}.\n\
                     Start with: pechincha daemon start\n\
                     Or launch your browser with: chromium --remote-debugging-port={cdp_port}\n\
                     Error: {e}"
                )
            })?;

            // Spawn handler to process CDP events
            tokio::spawn(async move {
                while let Some(_) = handler.next().await {}
            });

            info!(port = cdp_port, "Connected to browser via CDP");
            Ok(Arc::new(browser))
        })
        .await
        .cloned()
}

/// Search URL for each provider.
pub fn search_url(provider: ProviderId, query: &str) -> String {
    let q = urlencoding::encode(query);
    match provider {
        ProviderId::MercadoLivre => format!("https://lista.mercadolivre.com.br/{}", query.replace(' ', "-")),
        ProviderId::AliExpress => format!("https://pt.aliexpress.com/w/wholesale-{}.html", query.replace(' ', "+")),
        ProviderId::Shopee => format!("https://shopee.com.br/search?keyword={q}"),
        ProviderId::Amazon => format!("https://www.amazon.com.br/s?k={q}"),
        ProviderId::AmazonUS => format!("https://www.amazon.com/s?k={q}"),
        ProviderId::Kabum => format!("https://www.kabum.com.br/busca/{}", query.replace(' ', "-")),
        ProviderId::MagazineLuiza => format!("https://www.magazineluiza.com.br/busca/{q}/"),
        ProviderId::Olx => format!("https://www.olx.com.br/brasil?q={q}"),
    }
}

/// Fetch a single page via CDP — opens a new tab, navigates, waits, extracts HTML, closes tab.
pub async fn fetch_page(cdp_port: u16, url: &str) -> Result<String, ProviderError> {
    let browser = get_browser(cdp_port)
        .await
        .map_err(|e| ProviderError::Browser(e))?;

    let page = browser
        .new_page(url)
        .await
        .map_err(|e| ProviderError::Browser(format!("Failed to open tab for {url}: {e}")))?;

    // Wait for JS to render
    tokio::time::sleep(RENDER_WAIT).await;

    let html = page
        .content()
        .await
        .map_err(|e| ProviderError::Browser(format!("Failed to get content: {e}")))?;

    // Close the tab
    let _ = page.close().await;

    debug!(url = url, html_len = html.len(), "CDP page fetched");
    Ok(html)
}

/// Amazon US detail page data: price, shipping, MSRP, and seller.
pub struct AmazonUsDetails {
    pub product_price: Option<rust_decimal::Decimal>,
    pub shipping_import: Option<rust_decimal::Decimal>,
    pub msrp: Option<rust_decimal::Decimal>,
    pub sold_by: Option<String>,
    pub ships_from: Option<String>,
}

/// Fetch Amazon US product detail page and extract price, shipping, and MSRP.
pub async fn fetch_amazon_us_details(cdp_port: u16, product_url: &str) -> Option<AmazonUsDetails> {
    debug!("Amazon US detail: connecting...");
    let browser = get_browser(cdp_port).await.ok()?;

    debug!("Amazon US detail: opening page...");
    let page = match tokio::time::timeout(
        Duration::from_secs(10),
        browser.new_page(product_url)
    ).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => { warn!("Amazon US detail page open failed: {}", e); return None; }
        Err(_) => { warn!("Amazon US detail page open timed out"); return None; }
    };

    debug!("Amazon US detail: waiting for render...");
    tokio::time::sleep(RENDER_WAIT).await;

    // Click "Details" to expand shipping breakdown (with timeout)
    debug!("Amazon US detail: clicking Details...");
    let _ = tokio::time::timeout(Duration::from_secs(5), page.evaluate(
        r#"(() => {
            const links = document.querySelectorAll('a, span');
            for (const el of links) {
                const text = (el.textContent || '').trim();
                if (text === 'Details' || text === 'Detalhes') {
                    el.click();
                    break;
                }
            }
        })()"#
    )).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Extract product price + shipping/import from the detail page
    debug!("Amazon US detail: extracting price data...");
    let result = match tokio::time::timeout(Duration::from_secs(10), page.evaluate(
        r#"(() => {
            const all = document.body?.innerText || '';

            // Product price — look for the main price display
            const priceEl = document.querySelector('#corePriceDisplay_desktop_feature_div .a-price-whole, #corePrice_desktop .a-price-whole, .a-price-whole');
            let productPrice = null;
            if (priceEl) {
                const whole = priceEl.textContent.replace(/[,\.]/g, '').trim();
                const fractionEl = priceEl.parentElement?.querySelector('.a-price-fraction');
                const fraction = fractionEl ? fractionEl.textContent.trim() : '00';
                productPrice = whole + '.' + fraction;
            }

            // MSRP / List Price extraction — multiple strategies
            let msrp = null;

            // Strategy 1: "List: $XXX.XX" in a-offscreen spans (Amazon's hidden accessible text)
            const offscreenSpans = document.querySelectorAll('.a-offscreen');
            for (const span of offscreenSpans) {
                const t = span.textContent || '';
                const m = t.match(/List:\s*\$(\d+[\.,]\d+)/);
                if (m) {
                    const candidate = parseFloat(m[1].replace(',', ''));
                    // MSRP must be higher than selling price
                    if (!productPrice || candidate > parseFloat(productPrice)) {
                        msrp = m[1];
                        break;
                    }
                }
            }

            // Strategy 2: strikethrough price in core price area
            if (!msrp) {
                const corePrice = document.querySelector('#corePriceDisplay_desktop_feature_div, #corePrice_desktop');
                if (corePrice) {
                    const strikeEl = corePrice.querySelector('span[data-a-strike="true"] .a-offscreen');
                    if (strikeEl) {
                        const m = strikeEl.textContent.match(/\$(\d+[\.,]\d+)/);
                        if (m) msrp = m[1];
                    }
                }
            }

            // Strategy 3: "List Price: $XXX.XX" in page text
            if (!msrp && productPrice) {
                const listMatch = all.match(/List\s*Price:\s*\$(\d+[\.,]\d+)/);
                if (listMatch && parseFloat(listMatch[1].replace(',','')) > parseFloat(productPrice)) {
                    msrp = listMatch[1];
                }
            }

            // Shipping & Import Charges combined
            const combined = all.match(/\$(\d+[\.,]\d+)\s*Shipping\s*&?\s*Import\s*(?:Charges|Fees)/i);

            // Breakdown from Details popup
            const shipping = all.match(/Shipping[^$]*\$(\d+[\.,]\d+)/i);
            const importFee = all.match(/Import\s*(?:Fees?|Charges?|Deposit)[^$]*\$(\d+[\.,]\d+)/i);

            // Seller info from the "Ships from and sold by" section
            let soldBy = null;
            let shipsFrom = null;
            const sfsbEl = document.querySelector('#shipsFromSoldBy_feature_div, #merchant-info');
            if (sfsbEl) {
                const text = sfsbEl.innerText;
                const soldMatch = text.match(/(?:Sold|Vendido)\s+(?:by|por)\s+([^\n]+)/i);
                if (soldMatch) soldBy = soldMatch[1].trim();
                const shipMatch = text.match(/(?:Ships|Enviado|Fulfilled)\s+(?:from|by|de|por)\s+([^\n]+)/i);
                if (shipMatch) shipsFrom = shipMatch[1].trim();
            }
            // Fallback: tabular buybox
            if (!soldBy) {
                const buyboxRows = document.querySelectorAll('#tabular-buybox-container .tabular-buybox-text a');
                for (const a of buyboxRows) {
                    const t = a.textContent.trim();
                    if (t && t.length > 2 && t.length < 60) {
                        if (!soldBy) soldBy = t;
                        else if (!shipsFrom) shipsFrom = t;
                    }
                }
            }

            // Keepa data — if the extension is loaded, try to get the List Price from it
            let keepaMsrp = null;
            const keepaEl = document.querySelector('#keepa');
            if (keepaEl && !msrp) {
                // Keepa shows prices in its chart tooltip or in injected elements
                const keepaText = keepaEl.innerText || '';
                const keepaMatch = keepaText.match(/List\s*Price[:\s]*\$(\d+[\.,]\d+)/i);
                if (keepaMatch) keepaMsrp = keepaMatch[1];
            }

            // Use Keepa MSRP if we didn't find one from Amazon
            if (!msrp && keepaMsrp) msrp = keepaMsrp;

            return JSON.stringify({
                productPrice: productPrice,
                msrp: msrp,
                soldBy: soldBy,
                shipsFrom: shipsFrom,
                combined: combined ? combined[1] : null,
                shipping: shipping ? shipping[1] : null,
                importFee: importFee ? importFee[1] : null,
            });
        })()"#
    )).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => { warn!("Amazon US evaluate failed: {}", e); Err(()) }
        Err(_) => { warn!("Amazon US evaluate timed out"); Err(()) }
    };

    debug!("Amazon US detail: closing tab...");
    let _ = page.close().await;
    debug!("Amazon US detail: done");

    let json_str = match result {
        Ok(eval_result) => {
            let val = eval_result.value().cloned().unwrap_or(serde_json::Value::Null);
            val.as_str().unwrap_or("{}").to_string()
        }
        _ => return None,
    };
    let data: serde_json::Value = serde_json::from_str(&json_str).ok()?;

    let product_price: Option<rust_decimal::Decimal> = data["productPrice"]
        .as_str()
        .and_then(|s| s.replace(',', "").parse().ok());

    let msrp: Option<rust_decimal::Decimal> = data["msrp"]
        .as_str()
        .and_then(|s| s.replace(',', "").parse().ok());

    let shipping_import = if let Some(combined) = data["combined"].as_str() {
        combined.replace(',', "").parse::<rust_decimal::Decimal>().ok()
    } else {
        let shipping: rust_decimal::Decimal = data["shipping"].as_str()
            .and_then(|s| s.replace(',', "").parse().ok())
            .unwrap_or_default();
        let import: rust_decimal::Decimal = data["importFee"].as_str()
            .and_then(|s| s.replace(',', "").parse().ok())
            .unwrap_or_default();
        let total = shipping + import;
        if total > rust_decimal::Decimal::ZERO { Some(total) } else { None }
    };

    let sold_by = data["soldBy"].as_str().map(|s| s.to_string());
    let ships_from = data["shipsFrom"].as_str().map(|s| s.to_string());

    Some(AmazonUsDetails { product_price, shipping_import, msrp, sold_by, ships_from })
}

/// Fetch multiple pages concurrently — opens all tabs at once.
/// Returns results in the same order as the input URLs.
pub async fn fetch_pages(
    cdp_port: u16,
    requests: Vec<(ProviderId, String)>,
) -> Vec<(ProviderId, Result<String, ProviderError>)> {
    let browser = match get_browser(cdp_port).await {
        Ok(b) => b,
        Err(e) => {
            let err = ProviderError::Browser(e);
            return requests
                .into_iter()
                .map(|(id, _)| (id, Err(ProviderError::Browser(format!("{err}")))))
                .collect();
        }
    };

    // Open all tabs concurrently
    let mut handles = Vec::new();

    for (provider_id, url) in requests {
        let browser = browser.clone();
        let handle = tokio::spawn(async move {
            debug!(provider = %provider_id, url = %url, "Opening CDP tab");

            let result = async {
                let page = browser
                    .new_page(&url)
                    .await
                    .map_err(|e| ProviderError::Browser(format!("Tab open failed: {e}")))?;

                tokio::time::sleep(RENDER_WAIT).await;

                // Scroll down to trigger lazy-loading / infinite scroll
                let _ = page.evaluate(
                    "window.scrollTo(0, document.body.scrollHeight)"
                ).await;
                tokio::time::sleep(SCROLL_WAIT).await;

                // Scroll once more for sites with staggered loading
                let _ = page.evaluate(
                    "window.scrollTo(0, document.body.scrollHeight)"
                ).await;
                tokio::time::sleep(SCROLL_WAIT).await;

                // Click "See more" / "Ver mais" / expand buttons to reveal hidden products
                let _ = page.evaluate(
                    r#"(() => {
                        const buttons = document.querySelectorAll(
                            'a.a-expander-header, button[aria-expanded="false"], ' +
                            '[data-action="a-expander-toggle"], ' +
                            'a.s-pagination-next, ' +
                            '.a-section .a-text-bold'
                        );
                        buttons.forEach(b => {
                            const text = (b.textContent || '').toLowerCase();
                            if (text.includes('see more') || text.includes('ver mais') ||
                                text.includes('show more') || text.includes('mais opções')) {
                                b.click();
                            }
                        });
                    })()"#
                ).await;
                tokio::time::sleep(SCROLL_WAIT).await;

                let html = page
                    .content()
                    .await
                    .map_err(|e| ProviderError::Browser(format!("Content failed: {e}")))?;

                let _ = page.close().await;
                Ok(html)
            }
            .await;

            (provider_id, result)
        });
        handles.push(handle);
    }

    // Collect all results
    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                error!(error = %e, "CDP task panicked");
            }
        }
    }

    results
}
