# Pechincha

Brazilian e-commerce price comparison CLI. Searches 8 platforms simultaneously, calculates import taxes, and shows the real total cost of buying domestically vs importing.

## What it does

```
$ pechincha "Dyson V15 Detect"

╭───┬───────────┬──────────────────────────────┬────────────┬────────────┬────────────┬─────┬───────────┬─────────╮
│ # ┆ Platform  ┆ Product                      ┆ Price      ┆ Ship+Tax   ┆ Total      ┆ ★   ┆ MSRP      ┆ Savings │
╞═══╪═══════════╪══════════════════════════════╪════════════╪════════════╪════════════╪═════╪═══════════╪═════════╡
│ 1 ┆ Amz US 🌎 ┆ Dyson V15 Detect Plus Cord.. ┆ R$ 2,085   ┆ R$ 2,697   ┆ R$ 4,782   ┆ 4.3 ┆ US$849.99 ┆ -44%    │
│ 2 ┆ ML        ┆ Aspirador Dyson V15 Detect.. ┆ R$ 4,907   ┆ Free       ┆ R$ 4,907   ┆ —   ┆           ┆         │
│ 3 ┆ ML 🌎     ┆ Aspirador Dyson V15 Detect.. ┆ R$ 5,311   ┆ R$ 4,799   ┆ R$ 10,110  ┆ —   ┆           ┆ +17%    │
╰───┴───────────┴──────────────────────────────┴────────────┴────────────┴────────────┴─────┴───────────┴─────────╯

MSRP: US$849.99 = R$ 4,487 + R$ 4,163 tax = R$ 8,651 imported
```

The Dyson V15 costs R$4,782 from Amazon US (including shipping and import duties) vs R$10,110 from Mercado Livre international. That's 53% cheaper.

## Providers

| Platform | Method | Data |
|----------|--------|------|
| Mercado Livre | HTML scraping | Price, shipping, international detection |
| Amazon BR | HTML scraping (wreq TLS impersonation) | Price, rating, ASIN |
| Amazon US | HTML + detail page + Keepa | Price, real shipping/import charges, MSRP, all-time low |
| Magazine Luiza | `__NEXT_DATA__` JSON | Price, rating, installments, seller |
| Kabum | `__NEXT_DATA__` JSON | Price, installments, manufacturer |
| Shopee | CDP via real browser | Price, seller, sold count |
| AliExpress | CDP via real browser | Price, title, images |
| OLX | `__NEXT_DATA__` JSON | Price, seller, condition (used/new) |

## How it works

Pechincha uses two strategies depending on the site:

**wreq (TLS fingerprint impersonation)** — For sites that serve HTML without heavy anti-bot (Mercado Livre, Amazon BR, Kabum, Magazine Luiza, OLX). The `wreq` HTTP client impersonates real browser TLS/JA3/HTTP2 fingerprints to bypass Cloudflare and ShieldSquare.

**CDP (Chrome DevTools Protocol)** — For sites with aggressive anti-bot (Shopee, AliExpress) and for Amazon US detail page extraction. Connects to your running Chromium browser via `--remote-debugging-port` and opens tabs in your real browser session. Your cookies, fingerprint, and login state are all authentic.

When a CDP-capable browser is detected, all providers use it for maximum accuracy — personalized prices, member discounts, accurate shipping to your address.

## Installation

```
cargo install --path .
```

Requires Rust 1.70+ and a Chromium-based browser for CDP mode.

## Usage

### Basic search

```bash
pechincha "RTX 4070"
pechincha "iPhone 15 128gb"
pechincha "Dyson V15 Detect"
```

### Options

```
-n, --limit <N>          Max results per provider [default: 10]
-p, --platforms <LIST>   Filter platforms: ml,ali,shopee,amazon,amazon_us,kabum,magalu,olx
-s, --sort <FIELD>       Sort: total-cost, price, price-desc, rating, relevance
    --min-price <BRL>    Minimum price filter
    --max-price <BRL>    Maximum price filter
    --cdp-port <PORT>    Connect to browser CDP port
-j, --json               Output as JSON
-v                       Verbose (-v info, -vv debug, -vvv trace)
```

### CDP mode (recommended)

Launch your browser with remote debugging enabled:

```bash
chromium --remote-debugging-port=9222
```

Pechincha auto-detects it. All 8 providers will use your real browser session — Shopee and AliExpress require this.

### Managed daemon

```bash
pechincha daemon start              # Opens Chromium with CDP — log into sites
pechincha daemon stop               # Stop and clean up
pechincha daemon start --headless   # Headless mode (after logging in)
pechincha daemon status             # Check if running
```

The daemon uses a separate browser profile at `~/.config/pechincha/browser-profile/`. It does not touch your personal browser data.

### Login / cookie management

```bash
pechincha login shopee                    # Opens browser for manual login
pechincha login ali --from-browser chrome # Extract cookies from Chrome
pechincha login ml --import-curl "..."    # Import from curl command
pechincha logout shopee                   # Clear saved cookies
pechincha providers                       # Show all providers and login status
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
cdp_port = 9222  # auto-connect to browser

[providers.amazon_us]
enabled = true
```

## Tax calculation

Pechincha calculates Brazilian import taxes for international products:

- **Domestic** — Taxes already included in the listed price.
- **Remessa Conforme (< US$50)** — 20% import tax + 17% ICMS.
- **Remessa Conforme (US$50–3000)** — 60% import tax (with US$20 deduction) + 17% ICMS.
- **International (non-RC)** — 60% import tax + 17% ICMS.
- **Amazon US** — Uses Amazon's actual "Shipping & Import Charges to Brazil" from the product detail page instead of estimates.

Exchange rate fetched from BCB (Banco Central do Brasil) PTAX API.

International products are marked with 🌎 in the output.

## Keepa integration

When searching Amazon US products, Pechincha extracts price intelligence from Keepa by intercepting its WebSocket data stream:

- **MSRP / List Price** — The manufacturer's suggested retail price.
- **Current Amazon price** — What Amazon is currently selling it for.
- **Buy Box price** — The featured offer price.
- **All-time low** — The lowest price ever recorded.

The MSRP is used as a reference for the Savings column, adjusted for import taxes — showing whether importing at the current price is actually a good deal vs buying at full MSRP.

Requires the Keepa browser extension installed in your Chromium browser.

## Architecture

```
src/
├── main.rs          # CLI (clap)
├── lib.rs           # Public API
├── search.rs        # Orchestrator — CDP-first with wreq fallback
├── cdp.rs           # CDP tab management, concurrent page fetching
├── keepa.rs         # Keepa WebSocket interception and price extraction
├── tax.rs           # Brazilian import tax calculator
├── currency.rs      # BCB PTAX exchange rate (USD/BRL)
├── display.rs       # Terminal table output
├── config.rs        # TOML configuration
├── cookies.rs       # Cookie persistence and browser extraction
├── browser.rs       # headless_chrome / chaser-oxide integration
├── daemon.rs        # Browser daemon lifecycle management
├── scraping.rs      # wreq client with TLS fingerprint impersonation
├── models.rs        # Product, PriceInfo, TaxInfo types
├── error.rs         # Error types
└── providers/
    ├── mod.rs           # Provider trait
    ├── mercadolivre.rs
    ├── amazon.rs        # Amazon BR
    ├── amazon_us.rs     # Amazon US with detail page + Keepa
    ├── shopee.rs        # CDP-only (anti-bot too aggressive)
    ├── aliexpress.rs    # CDP-only (JS-rendered)
    ├── magalu.rs        # Magazine Luiza
    ├── kabum.rs
    └── olx.rs
```

## Key dependencies

- **wreq** + **wreq-util** — HTTP client with TLS/JA3/HTTP2 browser fingerprint impersonation
- **chaser-oxide** — Stealth Chrome DevTools Protocol client
- **headless_chrome** — Chrome automation for login flows
- **scraper** — HTML parsing with CSS selectors
- **tokio** — Async runtime
- **rust_decimal** — Financial precision arithmetic
- **regex-lite** — Lightweight regex for HTML parsing

## License

MIT
