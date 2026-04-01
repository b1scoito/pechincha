use colored::Colorize;
use rust_decimal::Decimal;

use crate::history::PriceChange;
use crate::keepa::{KeepaInsight, PriceTrend};
use crate::models::{Currency, Product, SearchResults};
use crate::providers::ProviderId;

// ── Minimal TUI Display ─────────────────────────────────────────────────────

const DIM_LINE: &str = "─";

pub fn print_results(results: &SearchResults, query: &str, price_changes: &[Option<PriceChange>]) {
    if results.products.is_empty() {
        eprintln!("{}", "  No results found.".yellow());
        print_errors(results);
        return;
    }

    let provider_count = count_unique_providers(&results.products);

    // Header
    println!();
    println!(
        "  {} {} {}",
        "pechincha".bold(),
        "·".dimmed(),
        query.italic()
    );
    println!(
        "  {}",
        format!(
            "{} results · {} providers · {:.1}s",
            results.products.len(),
            provider_count,
            results.query_time.as_secs_f64()
        ).dimmed()
    );
    println!();

    // Find best price for highlighting
    let best_price = results.products.iter().map(|p| p.price.total_cost).min();

    // Compute median price for relative comparison
    let median_price = {
        let mut prices: Vec<Decimal> = results.products.iter().map(|p| p.price.total_cost).collect();
        prices.sort();
        if prices.is_empty() {
            None
        } else if prices.len() % 2 == 1 {
            Some(prices[prices.len() / 2])
        } else {
            let mid = prices.len() / 2;
            Some((prices[mid - 1] + prices[mid]) / Decimal::from(2))
        }
    };

    // Find reference MSRP from Keepa (any domain) or product data.
    // Track whether it's domestic (BRL) or international (USD).
    let mut msrp_is_domestic = false;
    let reference_msrp_usd: Option<Decimal> = results.products.iter()
        .find_map(|p| {
            p.keepa.iter()
                .find(|k| k.domain == crate::keepa::DOMAIN_US)
                .and_then(KeepaInsight::msrp)
        })
        .or_else(|| {
            // Fallback: BR MSRP converted to USD
            results.products.iter().find_map(|p| {
                p.keepa.iter()
                    .find(|k| k.domain == crate::keepa::DOMAIN_BR)
                    .and_then(KeepaInsight::msrp)
                    .map(|brl| { msrp_is_domestic = true; brl * k_br_to_usd() })
            })
        })
        .or_else(|| {
            results.products.iter()
                .find(|p| p.price.original_price.is_some() && p.price.currency == Currency::USD)
                .and_then(|p| p.price.original_price)
        });
    // Also get the raw BRL MSRP for domestic display
    let msrp_brl_raw: Option<Decimal> = if msrp_is_domestic {
        results.products.iter().find_map(|p| {
            p.keepa.iter()
                .find(|k| k.domain == crate::keepa::DOMAIN_BR)
                .and_then(KeepaInsight::msrp)
        })
    } else {
        None
    };

    // Exchange rate for MSRP comparison
    let exchange_rate = results.products.iter()
        .find(|p| p.price.currency == Currency::USD && p.price.listed_price > Decimal::ZERO)
        .map_or_else(|| Decimal::from(5), |p| p.price.price_brl / p.price.listed_price);

    // Results list
    for (i, product) in results.products.iter().enumerate() {
        let is_best = best_price == Some(product.price.total_cost) && i == 0;
        let price_change = price_changes.get(i).and_then(|c| c.as_ref());
        print_product_row(i + 1, product, is_best, reference_msrp_usd, exchange_rate, msrp_is_domestic, msrp_brl_raw, median_price, price_change);
    }

    // Keepa international prices
    print_keepa_section(results);

    // MSRP reference
    if let Some(msrp) = reference_msrp_usd {
        if msrp_is_domestic {
            // Domestic MSRP — just show the BRL price, no import calculation
            if let Some(brl) = msrp_brl_raw {
                println!("  {} {}", "MSRP".dimmed(), format_brl(brl).bold());
            }
        } else {
            print_msrp_reference(msrp, exchange_rate);
        }
    }

    // Links — full URLs for clicking
    println!();
    for (i, product) in results.products.iter().enumerate() {
        if !product.url.is_empty() {
            println!(
                "  {} {}",
                format!("{:>2}", i + 1).dimmed(),
                product.url
            );
        }
    }

    // Errors
    print_errors(results);

    println!();
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn print_product_row(
    rank: usize,
    product: &Product,
    is_best: bool,
    msrp_usd: Option<Decimal>,
    exchange_rate: Decimal,
    msrp_is_domestic: bool,
    msrp_brl_raw: Option<Decimal>,
    median_price: Option<Decimal>,
    price_change: Option<&PriceChange>,
) {
    let total = format_brl(product.price.total_cost);

    // Platform tag
    let platform = format_provider(product.provider);
    let origin = if product.domestic { "domestic" } else { "import" };

    // Rating
    let rating_str = product.rating
        .map(|r| {
            product.review_count.map_or_else(
                || format!("{r:.1}"),
                |rc| format!("{r:.1} ({})", format_count(rc)),
            )
        })
        .unwrap_or_default();

    // Savings vs MSRP.
    // MSRP is MSRP — the manufacturer's suggested retail price.
    // Compare the product's BRL price against MSRP converted to BRL.
    // Ship+tax is a separate concern, not mixed into this comparison.
    let savings = if msrp_is_domestic {
        msrp_brl_raw.and_then(|reference| {
            if reference > Decimal::ZERO {
                let pct = ((product.price.price_brl - reference) * Decimal::from(100)) / reference;
                Some(pct)
            } else {
                None
            }
        })
    } else {
        msrp_usd.and_then(|msrp| {
            let msrp_brl = msrp * exchange_rate;
            if msrp_brl > Decimal::ZERO {
                let pct = ((product.price.price_brl - msrp_brl) * Decimal::from(100)) / msrp_brl;
                Some(pct)
            } else {
                None
            }
        })
    };

    // First line: rank, price, title
    let rank_str = format!("{rank:>2}");
    let title = truncate(&product.title, 55);

    if is_best {
        println!(
            "  {}  {}  {}",
            rank_str.green().bold(),
            format!("{total:<13}").green().bold(),
            title.bold()
        );
    } else {
        println!(
            "  {}  {}  {}",
            rank_str.dimmed(),
            format!("{total:<13}").bold(),
            title
        );
    }

    // Second line: metadata (platform, origin, rating, savings)
    let mut meta_parts: Vec<String> = vec![platform, origin.to_string()];

    if !rating_str.is_empty() {
        meta_parts.push(format!("{rating_str}★"));
    }

    if let Some(pct) = savings {
        if pct < Decimal::ZERO {
            meta_parts.push(format!("{pct:.0}% vs MSRP").green().to_string());
        } else if pct > Decimal::ZERO {
            meta_parts.push(format!("+{pct:.0}% vs MSRP").red().to_string());
        }
    }

    // Price history change
    if let Some(change) = price_change {
        if change.pct_change < -3.0 {
            meta_parts.push(
                format!("↓{:.0}% {}d ago", change.pct_change.abs(), change.days_ago)
                    .green().to_string()
            );
        } else if change.pct_change > 3.0 {
            meta_parts.push(
                format!("↑{:.0}% {}d ago", change.pct_change, change.days_ago)
                    .red().to_string()
            );
        }
    }

    // Median price comparison
    if let Some(median) = median_price {
        if median > Decimal::ZERO {
            let pct = ((product.price.total_cost - median) * Decimal::from(100)) / median;
            let threshold = Decimal::from(3);
            if pct < -threshold {
                meta_parts.push(format!("{pct:.0}% vs median").green().to_string());
            } else if pct > threshold {
                meta_parts.push(format!("+{pct:.0}% vs median").red().to_string());
            }
        }
    }

    let meta = meta_parts.join(&format!(" {} ", "·".dimmed()));

    // Price breakdown for imports
    let breakdown = if !product.domestic && product.price.currency == Currency::USD {
        let ship_tax = product.price.shipping_cost.unwrap_or(Decimal::ZERO) + product.price.tax.total_tax;
        if ship_tax > Decimal::ZERO {
            // Convert back to USD for display
            let ship_tax_usd = ship_tax / exchange_rate;
            format!(
                "US${:.0} + US${:.0} ship+tax",
                product.price.listed_price, ship_tax_usd
            )
        } else {
            format!("US${:.0}", product.price.listed_price)
        }
    } else {
        String::new()
    };

    if breakdown.is_empty() {
        println!("      {:<13}  {}", "", meta.dimmed());
    } else {
        println!(
            "      {:<13}  {}",
            breakdown.dimmed(),
            meta.dimmed()
        );
    }

    println!(); // Spacing between results
}

// ── Keepa Section ───────────────────────────────────────────────────────────

fn print_keepa_section(results: &SearchResults) {
    let Some(product) = results.products.iter().find(|p| !p.keepa.is_empty()) else {
        return;
    };

    println!(
        "  {} {}",
        dim_line(50),
        "".dimmed()
    );
    println!();
    println!("  {}", "International Amazon Prices".bold());
    let asin = &product.platform_id;
    println!("  {}", format!("ASIN {asin} via Keepa").dimmed());
    println!();

    let mut insights: Vec<&KeepaInsight> = product.keepa.iter()
        .filter(|k| k.best_new_price_usd().is_some())
        .collect();
    insights.sort_by(|a, b| {
        a.best_new_price_usd().unwrap_or(Decimal::MAX)
            .cmp(&b.best_new_price_usd().unwrap_or(Decimal::MAX))
    });

    let cheapest_domain = insights.first().map(|k| k.domain);

    for k in &insights {
        let local_price = k.best_new_price().unwrap();
        let usd_price = k.best_new_price_usd().unwrap();
        let sym = k.currency_symbol();
        let is_cheapest = cheapest_domain == Some(k.domain);

        // Price display
        let price_str = if k.domain == crate::keepa::DOMAIN_US {
            format!("US${local_price:.2}")
        } else {
            format!("{sym}{local_price:.0} (~US${usd_price:.0})")
        };

        // Extras
        let mut extras = Vec::new();
        if let Some(w) = k.warehouse_usd() {
            extras.push(format!("Warehouse US${w:.0}"));
        }
        if let Some(r) = k.refurbished_usd() {
            extras.push(format!("Refurb US${r:.0}"));
        }
        let extras_str = if extras.is_empty() {
            String::new()
        } else {
            format!("  {}", extras.join(" · ").dimmed())
        };

        // Price trend arrow
        let trend_str = k.trend.map(|t| {
            let arrow = t.arrow();
            match t {
                PriceTrend::Falling => format!(" {}", arrow.green()),
                PriceTrend::Rising => format!(" {}", arrow.red()),
                PriceTrend::Stable => format!(" {}", arrow.dimmed()),
            }
        }).unwrap_or_default();

        let domain_label = format!("Amazon{}", k.domain_tld());

        if is_cheapest {
            println!(
                "  {}  {:<20} {}{}{}",
                "→".green(),
                domain_label,
                price_str.green().bold(),
                trend_str,
                extras_str
            );
        } else {
            println!(
                "    {:<20} {}{}{}",
                domain_label.dimmed(),
                price_str,
                trend_str,
                extras_str
            );
        }
    }

    println!();
}

fn print_msrp_reference(msrp: Decimal, exchange_rate: Decimal) {
    let msrp_brl = msrp * exchange_rate;
    let tax_info = crate::tax::TaxCalculator::calculate(
        Some(msrp), msrp_brl, false, false, false, exchange_rate,
    );
    let msrp_total = msrp_brl + tax_info.total_tax;

    println!("  {}", "Import cost at MSRP".dimmed());
    println!(
        "  {}  US${:.2}  {}  {}",
        "MSRP".dimmed(),
        msrp,
        "→".dimmed(),
        format!("{} (USD/BRL {:.2})", format_brl(msrp_brl), exchange_rate).dimmed()
    );
    if let Some(import) = tax_info.import_tax {
        let pct = if msrp_brl > Decimal::ZERO {
            format!(" ({:.0}%)", (import * Decimal::from(100)) / msrp_brl)
        } else {
            String::new()
        };
        println!(
            "  {}  {}{}",
            "Import tax".dimmed(),
            format_brl(import),
            pct.dimmed()
        );
    }
    if let Some(icms) = tax_info.icms {
        println!(
            "  {}  {} {}",
            "ICMS".dimmed(),
            format_brl(icms),
            "(17% por dentro)".dimmed()
        );
    }
    println!(
        "  {}  {}",
        "Total".dimmed(),
        format_brl(msrp_total).bold()
    );
}

// ── JSON & CSV Output ───────────────────────────────────────────────────────

#[allow(clippy::missing_panics_doc)]
pub fn print_json(results: &SearchResults) {
    let output = serde_json::json!({
        "products": results.products,
        "errors": results.errors.iter().map(|(id, e)| {
            serde_json::json!({
                "provider": id.to_string(),
                "error": e.to_string()
            })
        }).collect::<Vec<_>>(),
        "query_time_ms": results.query_time.as_millis(),
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

pub fn print_csv(results: &SearchResults) {
    println!("rank,platform,title,price_brl,shipping_brl,tax_brl,total_brl,rating,reviews,domestic,url");
    for (i, p) in results.products.iter().enumerate() {
        let shipping = p.price.shipping_cost.unwrap_or(Decimal::ZERO);
        let tax = p.price.tax.total_tax;
        let rating = p.rating.map(|r| format!("{r:.1}")).unwrap_or_default();
        let reviews = p.review_count.map(|r| r.to_string()).unwrap_or_default();
        // Escape commas in title
        let title = p.title.replace('"', "\"\"");
        println!(
            "{},{}.\"{}\",{:.2},{:.2},{:.2},{:.2},{},{},{},{}",
            i + 1,
            p.provider,
            title,
            p.price.price_brl,
            shipping,
            tax,
            p.price.total_cost,
            rating,
            reviews,
            p.domestic,
            p.url
        );
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn print_errors(results: &SearchResults) {
    if results.errors.is_empty() {
        return;
    }
    println!();
    for (provider, error) in &results.errors {
        eprintln!(
            "  {} {} {}",
            "!".yellow(),
            provider.to_string().yellow(),
            format!("{error}").dimmed()
        );
    }
}

fn format_provider(id: ProviderId) -> String {
    match id {
        ProviderId::MercadoLivre => "ML".to_string(),
        ProviderId::AliExpress => "AliExpress".to_string(),
        ProviderId::Shopee => "Shopee".to_string(),
        ProviderId::Amazon => "Amazon BR".to_string(),
        ProviderId::AmazonUS => "Amazon US".to_string(),
        ProviderId::Kabum => "Kabum".to_string(),
        ProviderId::MagazineLuiza => "Magalu".to_string(),
        ProviderId::Olx => "OLX".to_string(),
        ProviderId::GoogleShopping => "Google".to_string(),
        ProviderId::Ebay => "eBay".to_string(),
    }
}

fn format_brl(value: Decimal) -> String {
    // Format with thousands separator
    let whole = value.trunc().abs();
    let frac = ((value - value.trunc()) * Decimal::from(100)).abs().trunc();
    let whole_str = whole.to_string();
    let mut formatted = String::new();
    for (i, ch) in whole_str.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            formatted.push('.');
        }
        formatted.push(ch);
    }
    let formatted: String = formatted.chars().rev().collect();
    format!("R$ {formatted},{frac:02}")
}

fn format_count(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", f64::from(count) / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", f64::from(count) / 1_000.0)
    } else {
        count.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

fn dim_line(width: usize) -> String {
    DIM_LINE.repeat(width).dimmed().to_string()
}

/// Approximate BRL->USD rate for MSRP conversion when only BR data available.
const fn k_br_to_usd() -> Decimal {
    rust_decimal_macros::dec!(0.19) // ~1/5.26
}

fn count_unique_providers(products: &[Product]) -> usize {
    let mut seen = std::collections::HashSet::new();
    for p in products {
        seen.insert(p.provider);
    }
    seen.len()
}
