use scraper::{Html, Selector};

#[test]
fn test_ml_scraping_selectors() {
    let html = std::fs::read_to_string("tests/fixtures/ml_search_page.html")
        .expect("fixture missing");
    let document = Html::parse_document(&html);

    let card_sel = Selector::parse("li.ui-search-layout__item").unwrap();
    let cards: Vec<_> = document.select(&card_sel).collect();
    assert!(cards.len() > 0, "No cards found");
    println!("Found {} cards", cards.len());

    // Working selectors
    let title_sel = Selector::parse("a.poly-component__title").unwrap();
    let price_sel = Selector::parse(".poly-price__current .andes-money-amount__fraction").unwrap();
    let img_sel = Selector::parse("img.poly-component__picture").unwrap();
    let shipping_sel = Selector::parse(".poly-component__shipping").unwrap();

    for (i, card) in cards.iter().take(3).enumerate() {
        let title = card.select(&title_sel).next()
            .map(|el| el.text().collect::<String>()).unwrap_or_default();
        let href = card.select(&title_sel).next()
            .and_then(|el| el.value().attr("href")).unwrap_or("");
        let price = card.select(&price_sel).next()
            .map(|el| el.text().collect::<String>()).unwrap_or_default();
        let img = card.select(&img_sel).next()
            .and_then(|el| el.value().attr("src").or(el.value().attr("data-src"))).unwrap_or("");
        let shipping = card.select(&shipping_sel).next()
            .map(|el| el.text().collect::<String>()).unwrap_or_default();

        println!("\n--- Product {} ---", i+1);
        println!("Title: {}", title);
        println!("Price: {}", price);
        println!("Link: {}", &href[..href.len().min(80)]);
        println!("Img: {}", &img[..img.len().min(80)]);
        println!("Shipping: {}", shipping.trim());
    }
}
