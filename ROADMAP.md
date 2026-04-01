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

### Shipping
- **AliExpress domestic shipping** — AliExpress has warehouses in Brazil (Remessa Conforme). Products shipped domestically have no import tax and faster delivery. Detect and flag these listings separately from international ones.
- **Shopee domestic shipping** — Shopee also operates local warehouses and Brazilian sellers. Distinguish domestic Shopee listings (no import tax) from cross-border ones.

### Price intelligence
- **Installment display** — Magalu already extracts installment data (e.g., "12x R$750 sem juros"). Show installment options alongside total cost for all providers that support it. Critical for the Brazilian market where parcelamento drives purchase decisions.
- **Price history command** — `pechincha history "query"` to view tracked price changes over time. Data is already collected by the history module; needs a display layer.
- **Coupon / cashback detection** — Detect active coupons shown in search results (eBay shows codes like "EXTRA 10% OFF WITH CODE ..."). In CDP mode, Cuponomia and Meliuz browser extensions may inject cashback badges into pages — detect and display the cashback percentage alongside the price.

### Output
- **Product detail view** — `pechincha detail <url>` to show full info for a single product with Keepa chart data.
- **Webhook/notification** — Expand price watch to send alerts to Telegram/Discord/Slack.

### Performance
- **CDP tab pooling** — Reuse tabs across searches instead of opening/closing.
