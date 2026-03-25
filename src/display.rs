use colored::Colorize;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, Cell, Color, ContentArrangement, Table};
use rust_decimal::Decimal;

use crate::keepa::KeepaInsight;
use crate::models::{Currency, Product, SearchResults};
use crate::providers::ProviderId;

pub fn print_results(results: &SearchResults, _show_taxes: bool) {
    if results.products.is_empty() {
        println!("{}", "No results found.".yellow());
        if !results.errors.is_empty() {
            println!();
            print_errors(results);
        }
        return;
    }

    // Check if any product has Keepa data or MSRP
    let has_keepa = results.products.iter().any(|p| !p.keepa.is_empty());
    let has_msrp = results.products.iter().any(|p| p.price.original_price.is_some());

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);

    // Header
    let mut headers = vec!["#", "Platform", "Product", "Price", "Ship+Tax", "Total", "★"];
    if has_keepa {
        headers.push("US Price");
        headers.push("Best Int'l");
    }
    if has_msrp {
        headers.push("MSRP");
        headers.push("Savings");
    }
    table.set_header(headers);

    // Find best and worst prices for coloring
    let best_price = results.products.iter().map(|p| p.price.total_cost).min();
    let worst_price = results.products.iter().map(|p| p.price.total_cost).max();

    // Find reference MSRP — prefer Keepa US data, fallback to USD products
    let reference_msrp_usd: Option<Decimal> = results.products.iter()
        .find_map(|p| {
            // First try Keepa US domain MSRP
            p.keepa.iter()
                .find(|k| k.domain == crate::keepa::DOMAIN_US)
                .and_then(|k| k.msrp())
        })
        .or_else(|| {
            results.products.iter()
                .find(|p| p.price.original_price.is_some() && p.price.currency == Currency::USD)
                .and_then(|p| p.price.original_price)
        });

    for (i, product) in results.products.iter().enumerate() {
        let total_color = price_color(product.price.total_cost, best_price, worst_price);

        let platform = if !product.domestic {
            format!("{} 🌎", format_provider(product.provider))
        } else {
            format_provider(product.provider)
        };
        let title = truncate(&product.title, 50);
        let price = format_brl(product.price.price_brl);

        // Combine shipping + tax into one column
        let ship_tax = {
            let ship = product.price.shipping_cost.unwrap_or(Decimal::ZERO);
            let tax = product.price.tax.total_tax;
            let combined = ship + tax;
            if combined > Decimal::ZERO {
                format_brl(combined)
            } else if product.price.tax.taxes_included {
                "Incl.".to_string()
            } else {
                "—".to_string()
            }
        };

        let total = format_brl(product.price.total_cost);
        let rating = product.rating
            .map(|r| {
                if let Some(rc) = product.review_count {
                    format!("{:.1} ({})", r, format_count(rc))
                } else {
                    format!("{:.1}", r)
                }
            })
            .unwrap_or_else(|| "—".to_string());

        let mut row: Vec<Cell> = vec![
            Cell::new(i + 1),
            Cell::new(platform),
            Cell::new(title),
            Cell::new(price),
            Cell::new(ship_tax),
            Cell::new(total).fg(total_color),
            Cell::new(rating),
        ];

        if has_keepa {
            // US Price column — show US Buy Box or Amazon price
            let us_price = product.keepa.iter()
                .find(|k| k.domain == crate::keepa::DOMAIN_US)
                .and_then(|k| k.best_new_price())
                .map(|p| format!("US${:.2}", p))
                .unwrap_or_else(|| "—".to_string());
            row.push(Cell::new(us_price));

            // Best International — cheapest price across all non-BR domains (converted to USD)
            let best_intl = find_cheapest_international(&product.keepa);
            let best_intl_display = match best_intl {
                Some((insight, usd_price)) => {
                    format!("US${:.2} {}", usd_price, insight.domain_tld())
                }
                None => "—".to_string(),
            };
            row.push(Cell::new(best_intl_display));
        }

        if has_msrp {
            // MSRP column — show product's own MSRP or reference MSRP
            let msrp_display = if let Some(msrp) = product.price.original_price {
                // Only show as USD if it actually came from Keepa (has keepa data or is USD product)
                if product.price.currency == Currency::USD || !product.keepa.is_empty() {
                    format!("US${:.2}", msrp)
                } else {
                    format_brl(msrp)
                }
            } else if let Some(ref_msrp) = reference_msrp_usd {
                format!("US${:.2}", ref_msrp).dimmed().to_string()
            } else {
                "—".to_string()
            };
            row.push(Cell::new(msrp_display));

            // Savings column
            let savings_display = if let Some(ref_msrp) = reference_msrp_usd {
                let exchange_rate = results.products.iter()
                    .find(|p| p.price.currency == Currency::USD && p.price.listed_price > Decimal::ZERO)
                    .map(|p| p.price.price_brl / p.price.listed_price)
                    .unwrap_or(Decimal::from(5));

                let msrp_brl = ref_msrp * exchange_rate;

                let reference_total = if !product.domestic {
                    let tax_info = crate::tax::TaxCalculator::calculate(
                        Some(ref_msrp), msrp_brl, false, false, false, exchange_rate,
                    );
                    msrp_brl + tax_info.total_tax
                } else {
                    msrp_brl
                };

                if reference_total > Decimal::ZERO {
                    if product.price.total_cost < reference_total {
                        let pct = ((reference_total - product.price.total_cost) * Decimal::from(100)) / reference_total;
                        format!("-{:.0}%", pct).green().to_string()
                    } else {
                        let pct = ((product.price.total_cost - reference_total) * Decimal::from(100)) / reference_total;
                        format!("+{:.0}%", pct).red().to_string()
                    }
                } else {
                    "—".to_string()
                }
            } else {
                "—".to_string()
            };
            row.push(Cell::new(savings_display));
        }

        table.add_row(row);
    }

    println!("{table}");

    // Keepa international price summary
    if has_keepa {
        print_keepa_summary(results);
    }

    // MSRP reference line with tax breakdown
    if let Some(msrp) = reference_msrp_usd {
        let exchange_rate = results.products.iter()
            .find(|p| p.price.currency == Currency::USD && p.price.listed_price > Decimal::ZERO)
            .map(|p| p.price.price_brl / p.price.listed_price)
            .unwrap_or(Decimal::from(5));
        let msrp_brl = msrp * exchange_rate;
        let tax_info = crate::tax::TaxCalculator::calculate(
            Some(msrp), msrp_brl, false, false, false, exchange_rate,
        );
        let msrp_total = msrp_brl + tax_info.total_tax;
        println!(
            "\n{} US${:.2} = {} + {} tax = {} imported",
            "MSRP:".bold(),
            msrp,
            format_brl(msrp_brl),
            format_brl(tax_info.total_tax),
            format_brl(msrp_total).bold()
        );
    }

    // Print links below
    println!();
    for (i, product) in results.products.iter().enumerate() {
        if !product.url.is_empty() {
            println!(
                "  {} {} {}",
                format!("[{}]", i + 1).dimmed(),
                format_provider(product.provider).dimmed(),
                product.url
            );
        }
    }

    // Summary
    println!(
        "\n{} results from {} providers in {:.1}s",
        results.products.len().to_string().bold(),
        count_unique_providers(&results.products).to_string().bold(),
        results.query_time.as_secs_f64()
    );

    if let Some(best) = results.products.first() {
        println!(
            "{} {} on {} at {}",
            "Best deal:".green().bold(),
            truncate(&best.title, 60),
            best.provider,
            format_brl(best.price.total_cost).green().bold()
        );
    }

    if !results.errors.is_empty() {
        println!();
        print_errors(results);
    }
}

/// Print Keepa international price comparison for products that have it.
fn print_keepa_summary(results: &SearchResults) {
    // Find the first product with Keepa data
    let product = match results.products.iter().find(|p| !p.keepa.is_empty()) {
        Some(p) => p,
        None => return,
    };

    println!("\n{}", "International Amazon prices (via Keepa):".bold());

    // Sort by best new price in USD (normalized for comparison)
    let mut insights: Vec<&KeepaInsight> = product.keepa.iter()
        .filter(|k| k.best_new_price_usd().is_some())
        .collect();
    insights.sort_by(|a, b| {
        a.best_new_price_usd().unwrap_or(Decimal::MAX)
            .cmp(&b.best_new_price_usd().unwrap_or(Decimal::MAX))
    });

    for k in &insights {
        let local_price = k.best_new_price().unwrap();
        let usd_price = k.best_new_price_usd().unwrap();
        let sym = k.currency_symbol();

        let warehouse = k.warehouse_usd()
            .map(|w| format!(" | Warehouse: US${:.2}", w))
            .unwrap_or_default();
        let refurb = k.refurbished_usd()
            .map(|r| format!(" | Refurb: US${:.2}", r))
            .unwrap_or_default();

        let domain_str = format!("Amazon{:<8}", k.domain_tld());
        let is_cheapest = insights.first().map(|f| f.domain) == Some(k.domain);

        // Show local price + USD equivalent for non-USD domains
        let price_str = if k.domain == crate::keepa::DOMAIN_US {
            format!("US${:.2}", local_price)
        } else {
            format!("{}{:.2} (~US${:.2})", sym, local_price, usd_price)
        };

        let line = format!("  {} {}{}{}", domain_str, price_str, warehouse, refurb);

        if is_cheapest {
            println!("{}", line.green());
        } else {
            println!("{}", line);
        }
    }
}

/// Find the cheapest international (non-BR) price from Keepa insights, in USD.
fn find_cheapest_international(keepa: &[KeepaInsight]) -> Option<(&KeepaInsight, Decimal)> {
    keepa.iter()
        .filter(|k| k.domain != crate::keepa::DOMAIN_BR)
        .filter_map(|k| k.best_new_price_usd().map(|p| (k, p)))
        .min_by_key(|(_, p)| *p)
}

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

fn print_errors(results: &SearchResults) {
    for (provider, error) in &results.errors {
        eprintln!(
            "{} {} — {}",
            "⚠".yellow(),
            provider.to_string().yellow(),
            error
        );
    }
}

fn format_provider(id: ProviderId) -> String {
    match id {
        ProviderId::MercadoLivre => "ML".to_string(),
        ProviderId::AliExpress => "Ali".to_string(),
        ProviderId::Shopee => "Shopee".to_string(),
        ProviderId::Amazon => "Amazon".to_string(),
        ProviderId::AmazonUS => "Amz US".to_string(),
        ProviderId::Kabum => "Kabum".to_string(),
        ProviderId::MagazineLuiza => "Magalu".to_string(),
        ProviderId::Olx => "OLX".to_string(),
    }
}

fn format_brl(value: Decimal) -> String {
    format!("R$ {:.2}", value)
}

fn format_count(count: u32) -> String {
    if count >= 1000 {
        format!("{}k", count / 1000)
    } else {
        count.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

fn price_color(
    price: Decimal,
    best: Option<Decimal>,
    worst: Option<Decimal>,
) -> Color {
    match (best, worst) {
        (Some(b), Some(w)) if b != w => {
            let range = w - b;
            let position = price - b;
            let ratio = position / range;
            if ratio <= rust_decimal_macros::dec!(0.33) {
                Color::Green
            } else if ratio <= rust_decimal_macros::dec!(0.66) {
                Color::Yellow
            } else {
                Color::Red
            }
        }
        _ => Color::White,
    }
}

fn count_unique_providers(products: &[Product]) -> usize {
    let mut seen = std::collections::HashSet::new();
    for p in products {
        seen.insert(p.provider);
    }
    seen.len()
}
