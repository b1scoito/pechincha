# Roadmap

## LLM integration

- **Query understanding** — Single LLM call before search to normalize ambiguous queries ("latest dreame vacuum" → "Dreame L50 Ultra"), resolve typos, and disambiguate models ("airpods" → ask "AirPods 4 or AirPods Pro 2?"). Runs once before any scraping starts, so no performance impact on the core engine.
- **Result summary** — Single LLM call after search to generate an actionable recommendation: best deal, best domestic option, whether importing is worth it, and caveats (seller reputation, condition, warranty). Synthesizes price, shipping, tax, and context into one paragraph.

## Planned providers

### Aggregators
- **Zoom.com.br** — Brazilian price comparison site. Scraping their results gives prices from dozens of stores in one request.
- **Buscapé** — Another Brazilian price aggregator. Similar approach to Zoom.

### International
- **B&H Photo** (bhphotovideo.com) — Major US electronics/audio retailer. Often cheaper than Amazon US for cameras, monitors, audio gear. Ships internationally.

### Domestic
- **Americanas** — One of Brazil's largest e-commerce platforms (B2W group). Massive catalog across all categories.
- **Terabyteshop** — PC hardware specialist. Popular with Brazilian PC builders. Competitive GPU/CPU prices.
- **Pichau** — Another PC hardware specialist. Direct competitor to Terabyteshop and Kabum.

## Planned features

### Search quality
- **Pagination / deep search** — `--deep` flag to fetch page 2+ from each provider for broader results.

### Output
- **Product detail view** — `pechincha detail <url>` to show full info for a single product with Keepa chart data.
- **Interactive mode** — TUI with keyboard navigation, open links in browser.
- **Webhook/notification** — Expand price watch to send alerts to Telegram/Discord/Slack.

### Performance
- **CDP tab pooling** — Reuse tabs across searches instead of opening/closing.
