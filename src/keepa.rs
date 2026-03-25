//! Keepa price intelligence — extracts MSRP, price history, and market data
//! by intercepting Keepa's WebSocket data stream via CDP.
//!
//! Keepa domain IDs: 1=.com (US), 2=.co.uk, 3=.de, 4=.fr, 5=.co.jp,
//!                   6=.ca, 7=.cn, 8=.it, 9=.es, 10=.in, 11=.com.mx, 12=.com.br
//!
//! CSV types come in two formats:
//!   - Pairs:    [timestamp, price, timestamp, price, ...]
//!   - Triplets: [timestamp, price, shipping, timestamp, price, shipping, ...]
//!     (used by _SHIPPING types like BUY_BOX_SHIPPING, NEW_FBM_SHIPPING, etc.)

use base64::Engine;
use futures::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::connect_async;
use tracing::{debug, info, warn};

// ── CSV indices (pair format: [timestamp, price, ...]) ──────────────────────

const CSV_AMAZON: usize = 0;
const CSV_NEW: usize = 1;
const CSV_USED: usize = 2;
const CSV_SALES_RANK: usize = 3;
const CSV_LIST_PRICE: usize = 4;
const CSV_REFURBISHED: usize = 6;
const CSV_LIGHTNING_DEAL: usize = 8;
const CSV_WAREHOUSE: usize = 9;
const CSV_NEW_FBA: usize = 10;
const CSV_COUNT_NEW: usize = 11;
const CSV_COUNT_USED: usize = 12;
const CSV_RATING: usize = 16;
const CSV_COUNT_REVIEWS: usize = 17;

// ── CSV indices (triplet format: [timestamp, price, shipping, ...]) ─────────

const CSV_NEW_FBM_SHIPPING: usize = 7;
const CSV_BUY_BOX_SHIPPING: usize = 18;
#[allow(dead_code)]
const CSV_USED_LIKE_NEW_SHIPPING: usize = 19;

// ── Keepa domain IDs ────────────────────────────────────────────────────────

pub const DOMAIN_US: u8 = 1;
pub const DOMAIN_UK: u8 = 2;
pub const DOMAIN_DE: u8 = 3;
pub const DOMAIN_FR: u8 = 4;
pub const DOMAIN_JP: u8 = 5;
pub const DOMAIN_CA: u8 = 6;
pub const DOMAIN_IT: u8 = 8;
pub const DOMAIN_ES: u8 = 9;
pub const DOMAIN_IN: u8 = 10;
pub const DOMAIN_MX: u8 = 11;
pub const DOMAIN_BR: u8 = 12;

/// Price intelligence extracted from Keepa for a product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeepaInsight {
    pub asin: String,
    pub title: String,
    pub manufacturer: String,
    pub domain: u8,

    // ── Product metadata ────────────────────────────────────────────────
    pub parent_asin: Option<String>,
    pub ean_list: Vec<String>,
    pub rating: Option<f32>,
    pub review_count: Option<u32>,

    // ── Current prices (cents — divide by 100 for dollars/currency) ─────
    /// MSRP / List Price
    pub list_price: Option<i64>,
    /// Amazon direct price
    pub amazon_price: Option<i64>,
    /// Buy Box price (price + shipping combined)
    pub buy_box_price: Option<i64>,
    /// Buy Box shipping component (from triplet)
    pub buy_box_shipping: Option<i64>,
    /// Lowest new price (3rd party)
    pub new_3p_price: Option<i64>,
    /// Lowest FBA new price
    pub fba_price: Option<i64>,
    /// Lowest FBM new price (with shipping)
    pub fbm_price: Option<i64>,
    /// FBM shipping component (from triplet)
    pub fbm_shipping: Option<i64>,
    /// Lowest used price
    pub used_price: Option<i64>,
    /// Amazon Warehouse deals price
    pub warehouse_price: Option<i64>,
    /// Refurbished price
    pub refurbished_price: Option<i64>,
    /// Lightning deal price (if active)
    pub lightning_deal: Option<i64>,

    // ── Offer counts ────────────────────────────────────────────────────
    pub new_offer_count: Option<u32>,
    pub used_offer_count: Option<u32>,

    // ── All-time lows (cents) ───────────────────────────────────────────
    pub amazon_low: Option<i64>,
    pub new_low: Option<i64>,
    pub used_low: Option<i64>,
    pub warehouse_low: Option<i64>,

    // ── Sales rank ──────────────────────────────────────────────────────
    pub sales_rank: Option<i64>,
}

impl KeepaInsight {
    fn cents_to_decimal(cents: i64) -> Decimal {
        Decimal::from(cents) / Decimal::from(100)
    }

    pub fn msrp(&self) -> Option<Decimal> {
        self.list_price.map(Self::cents_to_decimal)
    }

    pub fn amazon(&self) -> Option<Decimal> {
        self.amazon_price.map(Self::cents_to_decimal)
    }

    pub fn amazon_low_price(&self) -> Option<Decimal> {
        self.amazon_low.map(Self::cents_to_decimal)
    }

    pub fn buy_box(&self) -> Option<Decimal> {
        self.buy_box_price.map(Self::cents_to_decimal)
    }

    /// Buy Box total = price + shipping (already combined in the triplet's getLast logic)
    pub fn buy_box_total(&self) -> Option<Decimal> {
        let price = self.buy_box_price?;
        let shipping = self.buy_box_shipping.unwrap_or(0).max(0);
        Some(Self::cents_to_decimal(price + shipping))
    }

    pub fn warehouse(&self) -> Option<Decimal> {
        self.warehouse_price.map(Self::cents_to_decimal)
    }

    pub fn refurbished(&self) -> Option<Decimal> {
        self.refurbished_price.map(Self::cents_to_decimal)
    }

    pub fn fba(&self) -> Option<Decimal> {
        self.fba_price.map(Self::cents_to_decimal)
    }

    /// Best available new price: buy box > amazon > fba > new_3p
    pub fn best_new_price(&self) -> Option<Decimal> {
        self.buy_box_price
            .or(self.amazon_price)
            .or(self.fba_price)
            .or(self.new_3p_price)
            .map(Self::cents_to_decimal)
    }

    /// Domain TLD for display
    pub fn domain_tld(&self) -> &'static str {
        match self.domain {
            1 => ".com",
            2 => ".co.uk",
            3 => ".de",
            4 => ".fr",
            5 => ".co.jp",
            6 => ".ca",
            8 => ".it",
            9 => ".es",
            10 => ".in",
            11 => ".com.mx",
            12 => ".com.br",
            _ => ".com",
        }
    }

    /// Approximate exchange rate from this domain's currency to USD.
    /// Used to normalize international prices for comparison.
    fn currency_to_usd(&self) -> Decimal {
        // Approximate rates — good enough for comparison ranking.
        // US, CA, MX use dollars/pesos; EU uses EUR; UK uses GBP; JP uses JPY; IN uses INR; BR uses BRL
        match self.domain {
            1 => Decimal::ONE,                                          // USD
            2 => rust_decimal_macros::dec!(1.27),                       // GBP → USD
            3 | 4 | 8 | 9 => rust_decimal_macros::dec!(1.08),          // EUR → USD
            5 => rust_decimal_macros::dec!(0.0067),                     // JPY → USD
            6 => rust_decimal_macros::dec!(0.72),                       // CAD → USD
            10 => rust_decimal_macros::dec!(0.012),                     // INR → USD
            11 => rust_decimal_macros::dec!(0.049),                     // MXN → USD
            12 => rust_decimal_macros::dec!(0.19),                      // BRL → USD
            _ => Decimal::ONE,
        }
    }

    /// Best new price converted to USD for cross-domain comparison.
    pub fn best_new_price_usd(&self) -> Option<Decimal> {
        self.best_new_price().map(|p| p * self.currency_to_usd())
    }

    /// Warehouse price converted to USD.
    pub fn warehouse_usd(&self) -> Option<Decimal> {
        self.warehouse().map(|p| p * self.currency_to_usd())
    }

    /// Refurbished price converted to USD.
    pub fn refurbished_usd(&self) -> Option<Decimal> {
        self.refurbished().map(|p| p * self.currency_to_usd())
    }

    /// Currency symbol for this domain.
    pub fn currency_symbol(&self) -> &'static str {
        match self.domain {
            1 => "US$",
            2 => "£",
            3 | 4 | 8 | 9 => "€",
            5 => "¥",
            6 => "CA$",
            10 => "₹",
            11 => "MX$",
            12 => "R$",
            _ => "$",
        }
    }
}

/// Fetch Keepa price data for an ASIN by intercepting the WebSocket.
/// `domain`: 1 for .com (US), 12 for .com.br
pub async fn fetch_keepa_data(cdp_port: u16, asin: &str, domain: u8) -> Option<KeepaInsight> {
    let results = fetch_keepa_ws(cdp_port, asin, domain, false).await;
    let result = results.into_iter().find(|k| k.domain == domain);

    if let Some(ref k) = result {
        info!(
            asin = %k.asin,
            domain = %k.domain_tld(),
            msrp = ?k.msrp(),
            amazon = ?k.amazon(),
            buy_box = ?k.buy_box(),
            warehouse = ?k.warehouse(),
            low = ?k.amazon_low_price(),
            rating = ?k.rating,
            reviews = ?k.review_count,
            "Keepa data extracted"
        );
    }

    result
}

/// Fetch Keepa price data for an ASIN across ALL Amazon locales.
/// Opens the Keepa page for the given domain, then clicks "Compare international
/// Amazon prices" to trigger fetches for all other locales (US, CA, MX, UK, DE, etc.).
/// Returns a Vec of KeepaInsight, one per domain that has data.
/// `domain`: the home domain of this ASIN (1=US if from Amazon.com, 12=BR if from Amazon.com.br)
pub async fn fetch_keepa_comparison(cdp_port: u16, asin: &str, domain: u8) -> Vec<KeepaInsight> {
    let results = fetch_keepa_ws(cdp_port, asin, domain, true).await;

    info!(
        asin = asin,
        domains = results.len(),
        locales = ?results.iter().map(|k| k.domain_tld()).collect::<Vec<_>>(),
        "Keepa comparison data"
    );

    results
}

/// Core WebSocket interception logic shared by single-domain and comparison fetches.
/// When `compare` is true, clicks the "Compare" button and collects products from
/// multiple domains. Otherwise, returns after the first product is received.
async fn fetch_keepa_ws(
    cdp_port: u16,
    asin: &str,
    domain: u8,
    compare: bool,
) -> Vec<KeepaInsight> {
    let url = format!("https://keepa.com/#!product/{domain}-{asin}");
    debug!(asin = asin, domain = domain, compare = compare, "Fetching Keepa data");

    let inner = async {
        // Connect to browser and open a new tab
        let (browser, mut handler) = chaser_oxide::Browser::connect(
            format!("http://127.0.0.1:{cdp_port}")
        ).await.ok()?;
        tokio::spawn(async move { while let Some(_) = handler.next().await {} });

        // Close any stale Keepa tabs first to avoid cross-contamination
        let client = wreq::Client::builder().build().ok()?;
        let targets: Vec<serde_json::Value> = client
            .get(format!("http://127.0.0.1:{cdp_port}/json/list"))
            .send().await.ok()?
            .json().await.ok()?;
        for t in &targets {
            let u = t["url"].as_str().unwrap_or("");
            if u.contains("keepa.com") && t["type"].as_str() == Some("page") {
                if let Some(id) = t["id"].as_str() {
                    let _ = client.get(format!("http://127.0.0.1:{cdp_port}/json/close/{id}"))
                        .send().await;
                    debug!(url = u, "Closed stale Keepa tab");
                }
            }
        }

        let page = browser.new_page(&url).await.ok()?;

        // Find the Keepa tab's WebSocket URL — retry a few times since URL updates async
        let mut page_ws = None;
        for attempt in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(if attempt == 0 { 500 } else { 1000 })).await;

            let targets: Vec<serde_json::Value> = client
                .get(format!("http://127.0.0.1:{cdp_port}/json/list"))
                .send().await.ok()?
                .json().await.ok()?;

            page_ws = targets.iter()
                .find(|t| {
                    let u = t["url"].as_str().unwrap_or("");
                    u.contains("keepa.com") && u.contains(asin) && t["type"].as_str() == Some("page")
                })
                .and_then(|t| t["webSocketDebuggerUrl"].as_str())
                .map(|s| s.to_string());

            if page_ws.is_some() {
                debug!(attempt = attempt, "Found Keepa tab");
                break;
            }

            if attempt == 4 {
                // Last attempt: try matching just keepa.com without ASIN
                page_ws = targets.iter()
                    .find(|t| {
                        let u = t["url"].as_str().unwrap_or("");
                        u.contains("keepa.com") && t["type"].as_str() == Some("page")
                    })
                    .and_then(|t| t["webSocketDebuggerUrl"].as_str())
                    .map(|s| s.to_string());

                if page_ws.is_some() {
                    debug!("Found Keepa tab (without ASIN match)");
                }
            }
        }

        let page_ws = page_ws?;

        let (mut ws, _) = connect_async(&page_ws).await.ok()?;

        // Enable network monitoring
        let cmd = serde_json::json!({"id": 1, "method": "Network.enable", "params": {}});
        ws.send(tokio_tungstenite::tungstenite::Message::Text(cmd.to_string().into())).await.ok()?;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), ws.next()).await;

        // Collect products from WebSocket frames
        let mut results: Vec<KeepaInsight> = Vec::new();
        let mut seen_domains = std::collections::HashSet::new();
        let mut got_initial = false;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(if compare { 20 } else { 15 });

        loop {
            if tokio::time::Instant::now() > deadline {
                warn!("Keepa data timeout for {asin} (got {} domains)", results.len());
                break;
            }

            let timeout_dur = if got_initial && compare {
                // After clicking compare, wait for multi-domain responses.
                // Responses arrive in bursts — 5s silence means no more coming.
                std::time::Duration::from_secs(5)
            } else {
                std::time::Duration::from_secs(2)
            };

            let timeout = tokio::time::timeout(timeout_dur, ws.next()).await;
            match timeout {
                Ok(Some(Ok(msg))) => {
                    let resp: serde_json::Value = serde_json::from_str(&msg.to_string()).unwrap_or_default();
                    if resp["method"].as_str() != Some("Network.webSocketFrameReceived") { continue; }

                    let payload = resp["params"]["response"]["payloadData"].as_str().unwrap_or("");
                    if payload.len() < 1000 { continue; }

                    // Decode base64 → zstd → JSON
                    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(payload) else { continue };
                    if decoded.len() < 4 || decoded[0] != 0x28 || decoded[1] != 0xB5 { continue; }
                    let Ok(raw) = zstd::decode_all(&decoded[..]) else { continue };
                    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&raw) else { continue };

                    if let Some(products) = json["basicProducts"].as_array() {
                        for p in products {
                            let insight = parse_keepa_product(p);
                            if !seen_domains.contains(&insight.domain) {
                                debug!(
                                    asin = %insight.asin,
                                    domain = %insight.domain_tld(),
                                    buy_box = ?insight.buy_box(),
                                    "Keepa product received"
                                );
                                seen_domains.insert(insight.domain);
                                results.push(insight);
                            }
                        }

                        if !got_initial && !results.is_empty() {
                            got_initial = true;

                            if !compare {
                                // Single-domain mode: we're done
                                break;
                            }

                            // Wait for page to fully render before clicking Compare
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                            // Click "Compare international Amazon prices" via CDP.
                            // Uses Keepa's internal comparePricesOverlay() function directly
                            // instead of DOM clicks, which is more reliable.
                            debug!("Triggering Keepa international price comparison");

                            // Step 1: Set locale preferences using Keepa's own storage system,
                            // then call comparePricesOverlay to open the comparison panel.
                            let compare_js = r#"
                                (function() {
                                    // Remove ALL overlays that might interfere
                                    ['overlayShadowTop3', 'overlayShadow', 'overlayMain'].forEach(function(id) {
                                        var el = document.getElementById(id);
                                        if (el) el.style.display = 'none';
                                    });

                                    // Set locale preferences via Keepa's storage object (not localStorage)
                                    try {
                                        if (typeof storage !== 'undefined') {
                                            var locales = {};
                                            locales[0] = false; // don't limit to same region
                                            for (var i = 1; i <= 12; i++) {
                                                if (i === 7) continue; // skip .cn
                                                locales[i] = true;
                                            }
                                            storage.casinLocales = JSON.stringify(locales);
                                            if (typeof settings !== 'undefined' && settings.send) settings.send();
                                        }
                                    } catch(e) {}

                                    // Call the function directly
                                    if (typeof comparePricesOverlay === 'function') {
                                        try {
                                            var hash = window.location.hash;
                                            var parts = hash.split('-');
                                            var asin = parts.length > 1 ? parts[1] : '';
                                            var domain = parts.length > 0 ? parseInt(parts[0].replace('#!product/', '')) : 1;
                                            comparePricesOverlay(asin, domain);
                                            return 'called comparePricesOverlay';
                                        } catch(e) {
                                            return 'error: ' + e.message;
                                        }
                                    }
                                    return 'not found';
                                })()
                            "#;
                            let eval_cmd = serde_json::json!({
                                "id": 2,
                                "method": "Runtime.evaluate",
                                "params": { "expression": compare_js }
                            });
                            let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(
                                eval_cmd.to_string().into()
                            )).await;

                            // Step 2: After overlay opens, click any unchecked locale checkboxes
                            // to ensure all domains are fetched.
                            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                            let enable_locales_js = r#"
                                (function() {
                                    // Uncheck "Only same region" if checked
                                    var limited = document.getElementById('casinLimitedRadio');
                                    if (limited && limited.checked) limited.click();
                                    // Enable all locale checkboxes
                                    var enabled = 0;
                                    for (var i = 1; i <= 12; i++) {
                                        if (i === 7) continue;
                                        var cb = document.getElementById('casinLocaleRadio' + i);
                                        if (cb && !cb.checked) { cb.click(); enabled++; }
                                    }
                                    return 'enabled ' + enabled + ' locales';
                                })()
                            "#;
                            let eval_cmd2 = serde_json::json!({
                                "id": 3,
                                "method": "Runtime.evaluate",
                                "params": { "expression": enable_locales_js }
                            });
                            let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(
                                eval_cmd2.to_string().into()
                            )).await;
                        }
                    }
                }
                Ok(Some(Err(_))) | Ok(None) => break, // WS closed
                Err(_) => {
                    // Timeout on ws.next() — no new data in timeout_dur
                    if !compare && got_initial {
                        break;
                    }
                    // In compare mode: break if we have 2+ domains (got comparison data)
                    // or if we only have the initial domain after waiting
                    if compare && results.len() >= 2 {
                        info!("Keepa comparison done: {} domains collected", results.len());
                        break;
                    }
                    // Still waiting for initial or comparison data — continue
                }
            }
        }

        let _ = page.close().await;
        Some(results)
    };

    inner.await.unwrap_or_default()
}

fn parse_keepa_product(p: &serde_json::Value) -> KeepaInsight {
    let csv = p["csv"].as_array();

    // Buy box uses triplet format: [timestamp, price, shipping, ...]
    let (bb_price, bb_shipping) = csv_last_price_shipping(csv, CSV_BUY_BOX_SHIPPING);
    let (fbm_price, fbm_shipping) = csv_last_price_shipping(csv, CSV_NEW_FBM_SHIPPING);

    KeepaInsight {
        asin: p["asin"].as_str().unwrap_or("").to_string(),
        title: p["title"].as_str().unwrap_or("").to_string(),
        manufacturer: p["manufacturer"].as_str().unwrap_or("").to_string(),
        domain: p["domainId"].as_u64().unwrap_or(1) as u8,

        // Metadata
        parent_asin: p["parentAsin"].as_str().map(|s| s.to_string()),
        ean_list: p["eanList"].as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        rating: csv_last_value(csv, CSV_RATING).map(|r| r as f32 / 10.0),
        review_count: csv_last_value(csv, CSV_COUNT_REVIEWS).map(|r| r as u32),

        // Current prices
        list_price: csv_last_price(csv, CSV_LIST_PRICE),
        amazon_price: csv_last_price(csv, CSV_AMAZON),
        buy_box_price: bb_price,
        buy_box_shipping: bb_shipping,
        new_3p_price: csv_last_price(csv, CSV_NEW),
        fba_price: csv_last_price(csv, CSV_NEW_FBA),
        fbm_price,
        fbm_shipping,
        used_price: csv_last_price(csv, CSV_USED),
        warehouse_price: csv_last_price(csv, CSV_WAREHOUSE),
        refurbished_price: csv_last_price(csv, CSV_REFURBISHED),
        lightning_deal: csv_last_price(csv, CSV_LIGHTNING_DEAL),

        // Offer counts
        new_offer_count: csv_last_value(csv, CSV_COUNT_NEW).map(|v| v as u32),
        used_offer_count: csv_last_value(csv, CSV_COUNT_USED).map(|v| v as u32),

        // All-time lows
        amazon_low: csv_min_price(csv, CSV_AMAZON),
        new_low: csv_min_price(csv, CSV_NEW),
        used_low: csv_min_price(csv, CSV_USED),
        warehouse_low: csv_min_price(csv, CSV_WAREHOUSE),

        // Sales rank
        sales_rank: csv_last_value(csv, CSV_SALES_RANK),
    }
}

// ── CSV pair format helpers ─────────────────────────────────────────────────

/// Get the last valid price from a pair-format CSV array.
/// Format: [timestamp, price, timestamp, price, ...]
/// Price -1 means out of stock, -2 means no data.
fn csv_last_price(csv: Option<&Vec<serde_json::Value>>, index: usize) -> Option<i64> {
    let arr = csv?.get(index)?.as_array()?;
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

/// Get the last value from a pair-format CSV (for counts, ratings, rank).
/// Accepts any non-negative value (0 is valid for counts).
fn csv_last_value(csv: Option<&Vec<serde_json::Value>>, index: usize) -> Option<i64> {
    let arr = csv?.get(index)?.as_array()?;
    let mut i = arr.len();
    while i >= 2 {
        i -= 2;
        let val = arr.get(i + 1)?.as_i64()?;
        if val >= 0 {
            return Some(val);
        }
    }
    None
}

/// Get the minimum valid price from a pair-format CSV array.
fn csv_min_price(csv: Option<&Vec<serde_json::Value>>, index: usize) -> Option<i64> {
    let arr = csv?.get(index)?.as_array()?;
    arr.iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 1) // Only price values (odd indices)
        .filter_map(|(_, v)| v.as_i64())
        .filter(|&p| p > 0)
        .min()
}

// ── CSV triplet format helpers (_SHIPPING types) ────────────────────────────

/// Get the last valid price and shipping from a triplet-format CSV array.
/// Format: [timestamp, price, shipping, timestamp, price, shipping, ...]
/// Returns (price, shipping) where shipping may be 0 (free) or negative (unknown).
fn csv_last_price_shipping(
    csv: Option<&Vec<serde_json::Value>>,
    index: usize,
) -> (Option<i64>, Option<i64>) {
    let arr = match csv.and_then(|c| c.get(index)).and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return (None, None),
    };

    // Triplets: walk backwards in steps of 3
    let len = arr.len();
    if len < 3 || len % 3 != 0 {
        // Fallback: might be pair format or empty
        return (None, None);
    }

    let mut i = len;
    while i >= 3 {
        i -= 3;
        let price = arr.get(i + 1).and_then(|v| v.as_i64()).unwrap_or(-1);
        let shipping = arr.get(i + 2).and_then(|v| v.as_i64()).unwrap_or(-1);
        if price > 0 {
            return (Some(price), Some(shipping.max(0)));
        }
    }

    (None, None)
}
