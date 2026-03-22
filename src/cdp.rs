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
        ProviderId::Olx => format!("https://www.olx.com.br/informatica/q/{q}"),
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
