use colored::Colorize;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, Cell, Color, ContentArrangement, Table};
use rust_decimal::Decimal;

use crate::models::{Product, SearchResults};
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

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);

    // Header
    let mut headers = vec!["#", "Platform", "Product", "Price", "Ship"];
    if show_taxes {
        headers.push("Tax");
    }
    headers.extend_from_slice(&["Total", "★"]);
    table.set_header(headers);

    // Find best and worst prices for coloring
    let best_price = results
        .products
        .iter()
        .map(|p| p.price.total_cost)
        .min();
    let worst_price = results
        .products
        .iter()
        .map(|p| p.price.total_cost)
        .max();

    for (i, product) in results.products.iter().enumerate() {
        let total_color = price_color(product.price.total_cost, best_price, worst_price);

        let platform = format_provider(product.provider);
        let title = truncate(&product.title, 55);
        let price = format_brl(product.price.price_brl);
        let shipping = match product.price.shipping_cost {
            Some(c) if c == Decimal::ZERO => "Free".to_string(),
            Some(c) => format_brl(c),
            None => "—".to_string(),
        };
        let total = format_brl(product.price.total_cost);
        let rating = product
            .rating
            .map(|r| format!("{:.1}", r))
            .unwrap_or_else(|| "—".to_string());

        let mut row: Vec<Cell> = vec![
            Cell::new(i + 1),
            Cell::new(platform),
            Cell::new(title),
            Cell::new(price),
            Cell::new(shipping),
        ];

        if show_taxes {
            let tax = if product.price.tax.total_tax > Decimal::ZERO {
                format!(
                    "{} ({})",
                    format_brl(product.price.tax.total_tax),
                    product.price.tax.tax_regime
                )
            } else if product.price.tax.taxes_included {
                "Incl.".to_string()
            } else {
                "—".to_string()
            };
            row.push(Cell::new(tax));
        }

        row.push(Cell::new(total).fg(total_color));
        row.push(Cell::new(rating));

        table.add_row(row);
    }

    println!("{table}");

    // Print links below the table
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
        count_unique_providers(&results.products)
            .to_string()
            .bold(),
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
