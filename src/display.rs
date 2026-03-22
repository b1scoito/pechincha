use colored::Colorize;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, Cell, Color, ContentArrangement, Table};
use rust_decimal::Decimal;

use crate::models::{Currency, Product, SearchResults};
use crate::providers::ProviderId;

pub fn print_results(results: &SearchResults, show_taxes: bool) {
    if results.products.is_empty() {
        println!("{}", "No results found.".yellow());
        if !results.errors.is_empty() {
            println!();
            print_errors(results);
        }
        return;
    }

    // Check if any product has MSRP data
    let has_msrp = results.products.iter().any(|p| p.price.original_price.is_some());

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);

    // Header
    let mut headers = vec!["#", "Platform", "Product", "Price", "Ship+Tax", "Total", "★"];
    if has_msrp {
        headers.push("MSRP");
        headers.push("Savings");
    }
    table.set_header(headers);

    // Find best and worst prices for coloring
    let best_price = results.products.iter().map(|p| p.price.total_cost).min();
    let worst_price = results.products.iter().map(|p| p.price.total_cost).max();

    // Find reference MSRP (first product that has one — typically from Amazon US)
    let reference_msrp_usd: Option<Decimal> = results.products.iter()
        .find(|p| p.price.original_price.is_some() && p.price.currency == Currency::USD)
        .and_then(|p| p.price.original_price);

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
            .map(|r| format!("{:.1}", r))
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

        if has_msrp {
            // MSRP column
            let msrp_display = if let Some(msrp) = product.price.original_price {
                if product.price.currency == Currency::USD {
                    format!("US${:.2}", msrp)
                } else {
                    format_brl(msrp)
                }
            } else if let Some(ref_msrp) = reference_msrp_usd {
                // Show reference MSRP from Amazon US for comparison
                format!("US${:.2}", ref_msrp).dimmed().to_string()
            } else {
                "—".to_string()
            };
            row.push(Cell::new(msrp_display));

            // Savings column: compare total cost to MSRP + import taxes
            // "What would it cost at MSRP if imported properly?"
            let savings_display = if let Some(ref_msrp) = reference_msrp_usd {
                let exchange_rate = results.products.iter()
                    .find(|p| p.price.currency == Currency::USD && p.price.listed_price > Decimal::ZERO)
                    .map(|p| p.price.price_brl / p.price.listed_price)
                    .unwrap_or(Decimal::from(5));

                let msrp_brl = ref_msrp * exchange_rate;

                // For international products: compare to MSRP + taxes (fair imported price)
                // For domestic products: compare to MSRP in BRL directly (domestic markup)
                let reference_total = if !product.domestic {
                    // Apply import tax calculation to MSRP
                    let tax_info = crate::tax::TaxCalculator::calculate(
                        Some(ref_msrp),
                        msrp_brl,
                        false,  // not domestic
                        false,  // not Remessa Conforme (Amazon US isn't)
                        false,  // taxes not included
                        exchange_rate,
                    );
                    msrp_brl + tax_info.total_tax
                } else {
                    // Domestic: just compare to MSRP converted
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
