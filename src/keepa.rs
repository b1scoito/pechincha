//! Keepa price intelligence — extracts MSRP, price history, and market data
//! by intercepting Keepa's WebSocket data stream via CDP.
//!
//! Keepa domain IDs: 1=.com (US), 2=.co.uk, 3=.de, 4=.fr, 5=.co.jp,
//!                   6=.ca, 7=.cn, 8=.it, 9=.es, 10=.in, 11=.com.mx, 12=.com.br

use base64::Engine;
use futures::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::connect_async;
use tracing::{debug, info, warn};

/// Keepa CSV indices for price types.
const CSV_AMAZON: usize = 0;
const CSV_NEW_3P: usize = 1;
const CSV_USED: usize = 2;
const CSV_LIST_PRICE: usize = 4;
const CSV_BUY_BOX: usize = 10;
const CSV_BUY_BOX_NEW: usize = 18;

/// Keepa domain IDs.
pub const DOMAIN_US: u8 = 1;
pub const DOMAIN_BR: u8 = 12;

/// Price intelligence extracted from Keepa for a product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeepaInsight {
    pub asin: String,
    pub title: String,
    pub manufacturer: String,
    pub domain: u8,

    /// MSRP / List Price in cents (divide by 100 for dollars)
    pub list_price: Option<i64>,
    /// Current Amazon price in cents
    pub amazon_price: Option<i64>,
    /// Current Buy Box price in cents
    pub buy_box_price: Option<i64>,
    /// Current lowest new price (3rd party) in cents
    pub new_3p_price: Option<i64>,
    /// Current lowest used price in cents
    pub used_price: Option<i64>,

    /// All-time lowest Amazon price in cents
    pub amazon_low: Option<i64>,
    /// All-time lowest new price in cents
    pub new_low: Option<i64>,
    /// All-time lowest used price in cents
    pub used_low: Option<i64>,
}

impl KeepaInsight {
    /// Get the List Price / MSRP as a Decimal in dollars.
    pub fn msrp_usd(&self) -> Option<Decimal> {
        self.list_price.map(|p| Decimal::from(p) / Decimal::from(100))
    }

    /// Get the current Amazon price as a Decimal in dollars.
    pub fn amazon_usd(&self) -> Option<Decimal> {
        self.amazon_price.map(|p| Decimal::from(p) / Decimal::from(100))
    }

    /// Get the all-time low Amazon price as a Decimal in dollars.
    pub fn amazon_low_usd(&self) -> Option<Decimal> {
        self.amazon_low.map(|p| Decimal::from(p) / Decimal::from(100))
    }

    /// Get the Buy Box price as a Decimal in dollars.
    pub fn buy_box_usd(&self) -> Option<Decimal> {
        self.buy_box_price.map(|p| Decimal::from(p) / Decimal::from(100))
    }
}

/// Fetch Keepa price data for an ASIN by intercepting the WebSocket.
/// `domain`: 1 for .com (US), 12 for .com.br
pub async fn fetch_keepa_data(cdp_port: u16, asin: &str, domain: u8) -> Option<KeepaInsight> {
    let url = format!("https://keepa.com/#!product/{domain}-{asin}");
    debug!(asin = asin, domain = domain, "Fetching Keepa data");

    // Connect to browser and open a new tab
    let (browser, mut handler) = chaser_oxide::Browser::connect(
        format!("http://127.0.0.1:{cdp_port}")
    ).await.ok()?;
    tokio::spawn(async move { while let Some(_) = handler.next().await {} });

    // Navigate directly to Keepa — no need for about:blank intermediate
    let page = browser.new_page(&url).await.ok()?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Get the tab's WebSocket URL by finding it in targets
    let client = wreq::Client::builder().build().ok()?;
    let targets: Vec<serde_json::Value> = client
        .get(format!("http://127.0.0.1:{cdp_port}/json/list"))
        .send().await.ok()?
        .json().await.ok()?;

    let page_ws = targets.iter()
        .find(|t| {
            let u = t["url"].as_str().unwrap_or("");
            u.contains("keepa.com") && u.contains(asin) && t["type"].as_str() == Some("page")
        })
        .and_then(|t| t["webSocketDebuggerUrl"].as_str())
        .map(|s| s.to_string())?;

    let (mut ws, _) = connect_async(&page_ws).await.ok()?;

    // Enable network monitoring — with timeout to avoid hanging
    let cmd = serde_json::json!({"id": 1, "method": "Network.enable", "params": {}});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(cmd.to_string().into())).await.ok()?;
    // Read response with timeout
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), ws.next()).await;

    // Page is already navigating — just wait for data

    // Listen for the large WebSocket frame containing product data
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut result = None;

    loop {
        if tokio::time::Instant::now() > deadline {
            warn!("Keepa data timeout for {asin}");
            break;
        }
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
        if let Ok(Some(Ok(msg))) = timeout {
            let resp: serde_json::Value = serde_json::from_str(&msg.to_string()).unwrap_or_default();
            if resp["method"].as_str() != Some("Network.webSocketFrameReceived") { continue; }

            let payload = resp["params"]["response"]["payloadData"].as_str().unwrap_or("");
            if payload.len() < 1000 { continue; }

            // Decode base64 → zstd → JSON
            let decoded = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
            if decoded.len() < 4 || decoded[0] != 0x28 || decoded[1] != 0xB5 { continue; }

            let raw = zstd::decode_all(&decoded[..]).ok()?;
            let json: serde_json::Value = serde_json::from_slice(&raw).ok()?;

            if let Some(products) = json["basicProducts"].as_array() {
                if let Some(p) = products.first() {
                    result = Some(parse_keepa_product(p));
                    break;
                }
            }
        }
    }

    let _ = page.close().await;

    if let Some(ref insight) = result {
        info!(
            asin = %insight.asin,
            msrp = ?insight.list_price.map(|p| p as f64 / 100.0),
            amazon = ?insight.amazon_price.map(|p| p as f64 / 100.0),
            buy_box = ?insight.buy_box_price.map(|p| p as f64 / 100.0),
            low = ?insight.amazon_low.map(|p| p as f64 / 100.0),
            "Keepa data extracted"
        );
    }

    result
}

fn parse_keepa_product(p: &serde_json::Value) -> KeepaInsight {
    let csv = p["csv"].as_array();

    KeepaInsight {
        asin: p["asin"].as_str().unwrap_or("").to_string(),
        title: p["title"].as_str().unwrap_or("").to_string(),
        manufacturer: p["manufacturer"].as_str().unwrap_or("").to_string(),
        domain: p["domainId"].as_u64().unwrap_or(1) as u8,
        list_price: csv_last_price(csv, CSV_LIST_PRICE),
        amazon_price: csv_last_price(csv, CSV_AMAZON),
        buy_box_price: csv_last_price(csv, CSV_BUY_BOX).or(csv_last_price(csv, CSV_BUY_BOX_NEW)),
        new_3p_price: csv_last_price(csv, CSV_NEW_3P),
        used_price: csv_last_price(csv, CSV_USED),
        amazon_low: csv_min_price(csv, CSV_AMAZON),
        new_low: csv_min_price(csv, CSV_NEW_3P),
        used_low: csv_min_price(csv, CSV_USED),
    }
}

/// Get the last valid price from a CSV array.
/// CSV format: [timestamp, price, timestamp, price, ...]
/// Price -1 means out of stock.
fn csv_last_price(csv: Option<&Vec<serde_json::Value>>, index: usize) -> Option<i64> {
    let arr = csv?.get(index)?.as_array()?;
    // Walk backwards through pairs (timestamp, price) to find last valid price
    let mut i = arr.len();
    while i >= 2 {
        i -= 2;
        let price = arr.get(i + 1)?.as_i64()?;
        if price > 0 {
            return Some(price);
        }
    }
    None
}

/// Get the minimum valid price from a CSV array.
fn csv_min_price(csv: Option<&Vec<serde_json::Value>>, index: usize) -> Option<i64> {
    let arr = csv?.get(index)?.as_array()?;
    arr.iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 1) // Only price values (odd indices)
        .filter_map(|(_, v)| v.as_i64())
        .filter(|&p| p > 0)
        .min()
}
