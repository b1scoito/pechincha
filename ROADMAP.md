# Roadmap

## Planned providers

### Aggregators (high priority)
- **Zoom.com.br** — Brazilian price comparison site. Scraping their results gives prices from dozens of stores in one request.
- **Buscapé** — Another Brazilian price aggregator. Similar approach to Zoom.

### International (import comparison)
- **B&H Photo** (bhphotovideo.com) — Major US electronics/audio retailer. Often cheaper than Amazon US for cameras, monitors, audio gear. Ships internationally.

### Domestic (coverage gaps)
- **Americanas** — One of Brazil's largest e-commerce platforms (B2W group). Massive catalog across all categories.
- **Terabyteshop** — PC hardware specialist. Popular with Brazilian PC builders. Competitive GPU/CPU prices.
- **Pichau** — Another PC hardware specialist. Direct competitor to Terabyteshop and Kabum.

## Planned features

### Search quality
- **Category-aware search** — OLX currently searches all categories. Providers should detect product type and search appropriate categories.
- **Pagination / deep search** — `--deep` flag to fetch page 2+ from each provider for broader results.

### Price intelligence
- **AliExpress Choice/Plus badge detection** — Flag items with faster shipping and better tax handling.

### Import calculation
- **Amazon US "See all options"** — Click through to see all sellers and conditions (New, Used, Renewed) with individual shipping quotes.

### Output
- **Product detail view** — `pechincha detail <url>` to show full info for a single product with Keepa chart data.
- **Interactive mode** — TUI with keyboard navigation, open links in browser.
- **Webhook/notification** — Send results to Telegram/Discord/Slack.

### Performance
- **CDP tab pooling** — Reuse tabs across searches instead of opening/closing.
- **wreq-only fast mode** — Skip CDP for quick searches when Shopee/AliExpress aren't needed.
