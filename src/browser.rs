use chaser_oxide::{Browser, BrowserConfig, ChaserPage, ChaserProfile};
use chaser_oxide::handler::viewport::Viewport;
use futures::StreamExt;
use headless_chrome::LaunchOptions;
use std::time::Duration;

use crate::cookies::SavedCookie;
use crate::providers::ProviderId;

/// Login page URL for each provider.
fn login_url(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::MercadoLivre => "https://www.mercadolivre.com.br/gz/home",
        ProviderId::AliExpress => "https://login.aliexpress.com/",
        ProviderId::Shopee => "https://shopee.com.br/buyer/login?next=https%3A%2F%2Fshopee.com.br%2F",
        ProviderId::Amazon => "https://www.amazon.com.br/ap/signin?openid.pape.max_auth_age=0&openid.return_to=https%3A%2F%2Fwww.amazon.com.br%2F%3Fref_%3Dnav_ya_signin&openid.identity=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0%2Fidentifier_select&openid.assoc_handle=brflex&openid.mode=checkid_setup&openid.claimed_id=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0%2Fidentifier_select&openid.ns=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0",
        ProviderId::AmazonUS => "https://www.amazon.com/ap/signin?openid.pape.max_auth_age=0&openid.return_to=https%3A%2F%2Fwww.amazon.com%2F%3Fref_%3Dnav_ya_signin&openid.identity=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0%2Fidentifier_select&openid.assoc_handle=usflex&openid.mode=checkid_setup&openid.claimed_id=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0%2Fidentifier_select&openid.ns=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0",
        ProviderId::Kabum => "https://www.kabum.com.br/login",
        ProviderId::MagazineLuiza => "https://sacola.magazineluiza.com.br/v2/login",
        ProviderId::Olx => "https://auth2.olx.com.br/login",
    }
}

/// Open a visible browser window for the user to log in, then capture and save cookies.
pub fn login_interactive(provider: ProviderId) -> Result<Vec<SavedCookie>, String> {
    println!("Opening browser for {} login...", provider);
    println!();
    println!("  1. A Chrome window will open to the login page");
    println!("  2. Log in manually (complete captcha, 2FA, etc.)");
    println!("  3. Once you're logged in, type 'done' and press ENTER here");
    println!();

    let browser = headless_chrome::Browser::new(
        LaunchOptions::default_builder()
            .headless(false)
            .sandbox(false)
            .window_size(Some((1280, 900)))
            .idle_browser_timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| format!("Launch options error: {e}"))?,
    )
    .map_err(|e| format!("Failed to launch browser: {e}"))?;

    let tab = browser
        .new_tab()
        .map_err(|e| format!("Failed to create tab: {e}"))?;

    let login = login_url(provider);
    tab.navigate_to(login)
        .map_err(|e| format!("Failed to navigate: {e}"))?;
    let _ = tab.wait_until_navigated();

    println!("Browser opened. Log in now.");
    println!("Type 'done' when finished:");

    loop {
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("stdin error: {e}"))?;
        let trimmed = input.trim().to_lowercase();
        if trimmed == "done" || trimmed == "d" {
            break;
        }
        let _ = tab.get_url(); // keep browser alive
        if trimmed.is_empty() {
            println!("(Type 'done' when login is complete)");
        }
    }

    println!("Capturing cookies...");
    std::thread::sleep(Duration::from_secs(1));

    let domain_filter = crate::scraping::provider_domain(provider)
        .trim_start_matches("www.");

    let chrome_cookies = tab
        .get_cookies()
        .map_err(|e| format!("Failed to get cookies: {e}"))?;

    let saved: Vec<SavedCookie> = chrome_cookies
        .into_iter()
        .filter(|c| c.domain.contains(domain_filter))
        .map(|c| SavedCookie {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            secure: c.secure,
            http_only: c.http_only,
            expires: if c.expires > 0.0 { Some(c.expires) } else { None },
        })
        .collect();

    if saved.is_empty() {
        return Err("No cookies captured.".into());
    }

    crate::cookies::save_cookies(provider, &saved)?;
    println!("Saved {} cookies for {}.", saved.len(), provider);
    Ok(saved)
}

/// Fetch a page using chaser-oxide stealth browser.
/// This bypasses Shopee/AliExpress anti-bot detection at the CDP protocol level.
pub async fn fetch_stealth(url: &str, provider: ProviderId) -> Result<String, String> {
    let config = BrowserConfig::builder()
        .no_sandbox()
        .viewport(Viewport {
            width: 1920,
            height: 1080,
            device_scale_factor: None,
            emulating_mobile: false,
            is_landscape: false,
            has_touch: false,
        })
        .build()
        .map_err(|e| format!("BrowserConfig error: {e}"))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("Failed to launch stealth browser: {e}"))?;

    // Must spawn the handler to process CDP events
    let handler_task = tokio::spawn(async move {
        while let Some(_) = handler.next().await {}
    });

    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| format!("Failed to create page: {e}"))?;

    let chaser = ChaserPage::new(page);

    // Apply stealth fingerprint profile BEFORE navigation
    let profile = ChaserProfile::macos_arm().build();
    chaser
        .apply_profile(&profile)
        .await
        .map_err(|e| format!("Failed to apply stealth profile: {e}"))?;

    // Load cookies if available
    let cookies = crate::cookies::load_cookies(provider);
    if !cookies.is_empty() {
        let domain = crate::scraping::provider_domain(provider);
        // Set cookies via CDP - chaser-oxide uses chromiumoxide under the hood
        for c in &cookies {
            let cookie_domain = if c.domain.is_empty() {
                domain.to_string()
            } else {
                c.domain.clone()
            };
            let js = format!(
                "document.cookie = '{}={}; domain={}; path=/; secure'",
                c.name, c.value, cookie_domain
            );
            // We'll set cookies after navigating to the domain first
            let _ = chaser.goto(&format!("https://{domain}")).await;
            let _ = chaser.evaluate(&js).await;
        }
    }

    // Navigate to the target URL
    chaser
        .goto(url)
        .await
        .map_err(|e| format!("Navigation error: {e}"))?;

    // Wait for JS to render content
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get rendered HTML
    let html = chaser
        .content()
        .await
        .map_err(|e| format!("Failed to get content: {e}"))?;

    // Clean up
    handler_task.abort();

    Ok(html)
}

/// Fetch a page by connecting to your REAL browser via Chrome DevTools Protocol.
///
/// Launch your browser with: chromium --remote-debugging-port=9222
/// This uses your real browser session — cookies, fingerprint, everything is authentic.
/// Shopee/AliExpress trust it because it IS your real browser.
pub async fn fetch_via_cdp(url: &str, cdp_port: u16) -> Result<String, String> {
    let cdp_url = format!("http://127.0.0.1:{cdp_port}");

    let (browser, mut handler) = Browser::connect(&cdp_url)
        .await
        .map_err(|e| format!(
            "Failed to connect to browser at port {cdp_port}. \
             Launch your browser with: chromium --remote-debugging-port={cdp_port}\n\
             Error: {e}"
        ))?;

    let handler_task = tokio::spawn(async move {
        while let Some(_) = handler.next().await {}
    });

    // Open a NEW tab (don't interfere with existing ones)
    let page = browser
        .new_page(url)
        .await
        .map_err(|e| format!("Failed to open new tab: {e}"))?;

    // Wait for the page to load and JS to render products
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Extract the fully rendered HTML
    let html = page
        .content()
        .await
        .map_err(|e| format!("Failed to get page content: {e}"))?;

    // Close the tab we opened (don't close the browser!)
    let _ = page.close().await;

    handler_task.abort();

    Ok(html)
}

/// Legacy headless_chrome fetch (for sites that don't need stealth)
pub fn fetch_with_browser(url: &str, provider: ProviderId) -> Result<String, String> {
    let browser = headless_chrome::Browser::new(
        LaunchOptions::default_builder()
            .headless(true)
            .sandbox(false)
            .window_size(Some((1920, 1080)))
            .idle_browser_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Launch error: {e}"))?,
    )
    .map_err(|e| format!("Failed to launch browser: {e}"))?;

    let tab = browser
        .new_tab()
        .map_err(|e| format!("Tab error: {e}"))?;

    // Load saved cookies
    let cookies = crate::cookies::load_cookies(provider);
    if !cookies.is_empty() {
        let provider_url = format!("https://{}", crate::scraping::provider_domain(provider));
        let cookie_params: Vec<headless_chrome::protocol::cdp::Network::CookieParam> = cookies
            .iter()
            .map(|c| headless_chrome::protocol::cdp::Network::CookieParam {
                name: c.name.clone(),
                value: c.value.clone(),
                url: if c.domain.is_empty() { Some(provider_url.clone()) } else { None },
                domain: if c.domain.is_empty() { None } else { Some(c.domain.clone()) },
                path: Some(c.path.clone()),
                secure: Some(c.secure),
                http_only: Some(c.http_only),
                same_site: None,
                expires: c.expires,
                priority: None,
                same_party: None,
                source_scheme: None,
                source_port: None,
                partition_key: None,
            })
            .collect();

        tab.set_cookies(cookie_params)
            .map_err(|e| format!("Cookie error: {e}"))?;
    }

    tab.navigate_to(url)
        .map_err(|e| format!("Navigation error: {e}"))?;
    tab.wait_until_navigated()
        .map_err(|e| format!("Navigation timeout: {e}"))?;

    std::thread::sleep(Duration::from_secs(3));

    tab.get_content()
        .map_err(|e| format!("Content error: {e}"))
}
