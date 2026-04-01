# Pechincha

Brazilian e-commerce price comparison CLI. Searches 10 platforms simultaneously, calculates import taxes, and shows the real total cost of buying domestically vs importing.

## What it does

```
$ pechincha "Dreame L50 Ultra"

  pechincha · Dreame L50 Ultra
  8 results · 4 providers · 45.0s

   1  R$ 4.333,46    DREAME L50 ULTRA ROBO ESFREGAO A VACUO BRANCO, 19.500P...
                     eBay · import · -40% vs MSRP · -56% vs median

   2  R$ 4.398,16    Esfregao a vacuo Dreame L50 ultra robo 19500Pa succao ...
                     eBay · import · -39% vs MSRP · -55% vs median

   3  R$ 9.450,63    DREAME L50 Ultra Robot Vacuum and Mop Black with Auto-...
      US$949 + US$881 ship+tax  Amazon US · import · 4.2* · -32% vs MSRP

   4  R$ 10.417,60   DREAME Aspirador de po L50 Ultra Robot e Mop Black com...
                     Amazon BR · domestic · 4.3* · +44% vs MSRP

   5  R$ 13.288,66   Aspirador Dreame L50 Ultra Robot 19500pa Com Succao
                     ML · import · -3% vs MSRP

  International Amazon Prices
  ASIN B0F3J6BC4P via Keepa

  -> Amazon.com.mx        MX$17418 (~US$853)
     Amazon.ca            CA$1299 (~US$935) -> Warehouse US$712
     Amazon.com           US$1399.99        -> Warehouse US$752

  Import cost at MSRP
  MSRP  US$1399.99  ->  R$ 7.224,78 (USD/BRL 5.16)
  Import tax  R$ 4.334,87 (60%)
  ICMS  R$ 2.367,64 (17% por dentro)
  Total  R$ 13.927,30
```

## Providers

| Platform | Method | Data |
|----------|--------|------|
| Mercado Livre | HTML scraping | Price, shipping, international detection |
| Amazon BR | HTML scraping (wreq TLS impersonation) | Price, rating, ASIN |
| Amazon US | HTML + detail page + Keepa | Price, real shipping/import charges, MSRP |
| Magazine Luiza | `__NEXT_DATA__` JSON | Price, rating, installments, seller |
| Kabum | `__NEXT_DATA__` JSON | Price, installments, manufacturer |
| Shopee | CDP via real browser | Price, seller, sold count |
| AliExpress | CDP via real browser | Price, tax extraction from detail pages |
| OLX | `__NEXT_DATA__` JSON | Price, seller, condition (used/new) |
| Google Shopping | CDP + HTML fallback | Price, store name |
| eBay | CDP + HTML scraping | Price, shipping, condition, import detection |

## How it works

Pechincha uses two strategies depending on the site:

**wreq (TLS fingerprint impersonation)** -- For sites that serve HTML without heavy anti-bot (Mercado Livre, Amazon BR, Kabum, Magazine Luiza, OLX). The `wreq` HTTP client impersonates real browser TLS/JA3/HTTP2 fingerprints.

**CDP (Chrome DevTools Protocol)** -- For sites with aggressive anti-bot (Shopee, AliExpress) and for Amazon US detail pages, eBay, and Google Shopping. Connects to your running Chromium browser via `--remote-debugging-port` and opens tabs in your real browser session.

When a CDP-capable browser is detected on port 9222, **all providers use it** -- even those that work with wreq. This is the recommended setup because your real browser session carries:

- **Login state** -- Logged-in accounts see member prices, coupons, and personalized offers that anonymous scraping misses.
- **Delivery address** -- Shipping costs and availability depend on your location. Set your delivery address on each platform for accurate totals.
- **Cookies and fingerprint** -- Your authentic browser profile bypasses anti-bot measures that block headless clients.

For best results, open each provider's website in the CDP browser, log in, and set your Brazilian delivery address before searching.

### Relevance scoring

Results are filtered using a signal-based scoring engine that distinguishes the actual product from accessories:

- **Title structure** -- Where query tokens appear relative to each other in the title. Products have the query at the start; accessories bury it after "for/compatible with".
- **Price clustering** -- Where the price sits relative to the MSRP (when available) or the detected price distribution. A R$50 screen protector at MSRP R$9,000 scores near zero.
- **String similarity** -- How closely the title matches the query. Products have focused titles; accessories dilute with compatibility lists.

The three signals combine with cluster dampening: when the price signal is low, it gates the title-based signals proportionally, preventing accessories with misleading titles from passing.

## Installation

```
cargo install --path .
```

Requires Rust 1.70+ and a Chromium-based browser for CDP mode.

### CDP setup (recommended)

CDP mode is **strongly recommended**. Without it, Shopee, AliExpress, eBay, and Google Shopping won't work, and other providers will return anonymous prices without member discounts or accurate shipping.

Launch your browser with remote debugging enabled:

```bash
# macOS
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome --remote-debugging-port=9222

# Linux
chromium --remote-debugging-port=9222
```

Pechincha auto-detects the browser on port 9222.

**First-time setup checklist:**
1. Open each provider's website in the CDP browser
2. Log in to your accounts (Amazon, Mercado Livre, Shopee, AliExpress, etc.)
3. Set your delivery address to your Brazilian address on each platform
4. Install the [Keepa browser extension](https://keepa.com/) for Amazon price intelligence

## Usage

### Basic search

```bash
pechincha "RTX 4070"
pechincha "iPhone 15 128gb"
pechincha "Sennheiser HD 600"
```

### Options

```
-n, --limit <N>          Max results per provider [default: 10]
-p, --platforms <LIST>   Filter platforms (comma-separated)
-s, --sort <FIELD>       Sort: total-cost, price, price-desc, rating, relevance
    --min-price <BRL>    Minimum price filter
    --max-price <BRL>    Maximum price filter
    --cdp-port <PORT>    CDP port [default: 9222]
    --no-cache           Skip cache, fetch fresh results
-j, --json               Output as JSON
    --csv                Output as CSV
-v                       Verbose (-v info, -vv debug, -vvv trace)
```

### Platform aliases

```bash
pechincha "Dyson V15" -p ml,amazon,amazon_us    # Only these platforms
pechincha "AirPods Pro" -p ali,shopee,ebay       # International only
```

Available aliases: `ml`, `ali`, `shopee`, `amazon`, `amazon_us`, `kabum`, `magalu`, `olx`, `google`, `ebay`

### Price watch

Monitor products and get notified when prices drop below a threshold:

```bash
pechincha watch add "RTX 4070" --below 3500
pechincha watch list
pechincha watch remove 1
pechincha watch run                # Check all watches (cron-friendly)
```

### Configuration

```bash
pechincha config init    # Create default config
pechincha config show    # Show current config
```

Config file: `~/.config/pechincha/config.toml`

```toml
[general]
default_sort = "total-cost"
results_per_provider = 10
timeout_seconds = 30
cdp_port = 9222

[providers.amazon_us]
enabled = true
```

## Tax calculation

Pechincha calculates Brazilian import taxes for international products:

| Regime | Import Tax | ICMS | Applies to |
|--------|-----------|------|------------|
| Domestic | Included | Included | ML, Amazon BR, Kabum, Magalu, OLX |
| Remessa Conforme < US$50 | 20% | 17% | AliExpress, Shopee |
| Remessa Conforme >= US$50 | 60% (- US$20) | 17% | AliExpress, Shopee |
| International standard | 60% | 17% | eBay, non-RC imports |
| Amazon US | Actual charges | Included | Amazon US detail page extraction |

Exchange rate fetched from BCB (Banco Central do Brasil) PTAX API.

## Keepa integration

When an Amazon product is found, Pechincha intercepts Keepa's WebSocket data stream to extract:

- **MSRP** -- Manufacturer's suggested retail price across international Amazon domains
- **Buy Box price** -- Current featured offer
- **Warehouse / Refurbished** -- Discounted pricing options
- **Price trends** -- Rising, falling, or stable indicators
- **International comparison** -- Prices across Amazon US, CA, MX, UK, DE, JP, etc.

The MSRP anchors the relevance scoring engine, providing the strongest signal for filtering accessories from real products.

Requires the [Keepa browser extension](https://keepa.com/) installed in your CDP browser.

## Architecture

```
src/
├── main.rs          # CLI entry point (clap)
├── lib.rs           # Public API
├── search.rs        # Search orchestrator — CDP-first with wreq fallback
├── scoring.rs       # Signal-based relevance scoring (title, price, similarity)
├── cdp.rs           # CDP tab management, concurrent page fetching
├── keepa.rs         # Keepa WebSocket interception and price extraction
├── tax.rs           # Brazilian import tax calculator
├── currency.rs      # BCB PTAX exchange rate (USD/BRL)
├── display.rs       # Terminal output formatting
├── config.rs        # TOML configuration
├── cache.rs         # Search result caching
├── history.rs       # Price history tracking
├── watch.rs         # Price watch / alert management
├── notify.rs        # Desktop notifications for price drops
├── scraping.rs      # wreq client with TLS fingerprint impersonation
├── models.rs        # Product, PriceInfo, TaxInfo types
├── error.rs         # Error types
└── providers/
    ├── mod.rs              # Provider trait
    ├── mercadolivre.rs     # Mercado Livre
    ├── amazon.rs           # Amazon BR
    ├── amazon_us.rs        # Amazon US + detail pages
    ├── aliexpress.rs       # AliExpress (CDP)
    ├── shopee.rs           # Shopee (CDP)
    ├── magalu.rs           # Magazine Luiza
    ├── kabum.rs            # Kabum
    ├── olx.rs              # OLX
    ├── ebay.rs             # eBay
    └── google_shopping.rs  # Google Shopping
```

## License

GNU General Public License v3.0 -- see [LICENSE](LICENSE) for details.
