# Roadmap

## Planned providers

### Aggregators (high priority)
- **Google Shopping** — Aggregates hundreds of Brazilian sellers. One provider that multiplies coverage by 10x. Likely accessible via CDP or SerpAPI.
- **Zoom.com.br** — Brazilian price comparison site. Scraping their results gives prices from dozens of stores in one request.
- **Buscapé** — Another Brazilian price aggregator. Similar approach to Zoom.

### International (import comparison)
- **B&H Photo** (bhphotovideo.com) — Major US electronics/audio retailer. Often cheaper than Amazon US for cameras, monitors, audio gear. Ships internationally.
- **eBay** — International marketplace. Used/refurbished deals. Multiple sellers per product.

### Domestic (coverage gaps)
- **Americanas** — One of Brazil's largest e-commerce platforms (B2W group). Massive catalog across all categories.
- **Casas Bahia / Ponto** — Via group. Strong in electronics and home appliances.
- **Submarino** — B2W group, tech-focused. Shares catalog with Americanas.
- **Terabyteshop** — PC hardware specialist. Popular with Brazilian PC builders. Competitive GPU/CPU prices.
- **Pichau** — Another PC hardware specialist. Direct competitor to Terabyteshop and Kabum.

## Planned features

### Search quality
- **Product deduplication** — Same product on multiple platforms should be grouped, not listed separately. Use title similarity (strsim) and ASIN/EAN matching.
- **Category-aware search** — OLX currently searches all categories. Providers should detect product type and search appropriate categories.
- **Pagination / deep search** — `--deep` flag to fetch page 2+ from each provider for broader results.

### Price intelligence
- **Keepa full integration** — Extract all CSV price types (Amazon, Buy Box, New, Used, List Price) and show price trend (rising/falling/stable).
- **Keepa for Amazon BR** — Fetch domain=12 data for Brazilian MSRP comparison.
- **Price history tracking** — Save search results to local SQLite database. Show "price dropped since last search" indicators.
- **Price alerts** — `pechincha watch "RTX 4070" --below 5000` to get notified when price drops.

### Import calculation
- **Amazon US "See all options"** — Click through to see all sellers and conditions (New, Used, Renewed) with individual shipping quotes.
- **Per-product shipping estimate** — For sites that don't show shipping upfront, estimate based on product weight/category.
- **Remessa Conforme detection** — Automatically detect which platforms/sellers participate in the Remessa Conforme program.
- **Customs duty calculator** — NCM code lookup for accurate tariff rates instead of blanket 60%.

### Output
- **CSV export** — `--csv` flag for spreadsheet import.
- **Product detail view** — `pechincha detail <url>` to show full info for a single product with Keepa chart data.
- **Interactive mode** — TUI with keyboard navigation, open links in browser.
- **Webhook/notification** — Send results to Telegram/Discord/Slack.

### Performance
- **CDP tab pooling** — Reuse tabs across searches instead of opening/closing.
- **Response caching** — Cache search results for N minutes to avoid re-fetching on repeated queries.
- **Parallel detail pages** — Fetch Amazon US detail pages and Keepa data concurrently with each other (currently sequential).
- **wreq-only fast mode** — Skip CDP for quick searches when Shopee/AliExpress aren't needed.

### Infrastructure
- **Affiliate link support** — Optionally generate affiliate links for revenue.
- **API mode** — `pechincha serve` to run as an HTTP API for integration with other tools.
- **Browser extension** — Show pechincha comparison when visiting any product page.
