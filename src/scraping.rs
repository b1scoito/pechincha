use wreq::header::{HeaderValue, COOKIE};
use wreq_util::Emulation;

use crate::cookies;
use crate::providers::ProviderId;

const EMULATION_PROFILES: &[fn() -> Emulation] = &[
    || Emulation::Chrome131,
    || Emulation::Chrome127,
    || Emulation::Chrome126,
    || Emulation::Edge127,
    || Emulation::Safari18,
];

pub fn random_user_agent() -> &'static str {
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
}

fn random_emulation() -> Emulation {
    let idx = rand::random::<usize>() % EMULATION_PROFILES.len();
    EMULATION_PROFILES[idx]()
}

/// Build a wreq client that impersonates a real browser at the TLS/JA3/HTTP2 level.
pub fn build_impersonating_client(timeout_secs: u64) -> wreq::Client {
    wreq::Client::builder()
        .emulation(random_emulation())
        .cookie_store(true)
        .redirect(wreq::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .expect("failed to build impersonating HTTP client")
}

/// Build a wreq client with saved cookies pre-loaded for a specific provider.
/// Cookies are injected as default headers since wreq doesn't expose cookie store manipulation.
pub fn build_client_with_cookies(provider: ProviderId, timeout_secs: u64) -> wreq::Client {
    let saved = cookies::load_cookies(provider);

    let mut builder = wreq::Client::builder()
        .emulation(random_emulation())
        .cookie_store(true)
        .redirect(wreq::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(timeout_secs));

    // If we have saved cookies, add them as a default Cookie header
    if !saved.is_empty() {
        let cookie_str: String = saved
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");

        let mut headers = wreq::header::HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&cookie_str) {
            headers.insert(COOKIE, val);
        }
        builder = builder.default_headers(headers);

        tracing::debug!(
            provider = %provider,
            cookies = saved.len(),
            "Loaded saved cookies into client"
        );
    }

    builder.build().expect("failed to build HTTP client")
}

pub fn provider_domain(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::MercadoLivre => "www.mercadolivre.com.br",
        ProviderId::AliExpress => "pt.aliexpress.com",
        ProviderId::Shopee => "shopee.com.br",
        ProviderId::Amazon => "www.amazon.com.br",
        ProviderId::AmazonUS => "www.amazon.com",
        ProviderId::Kabum => "www.kabum.com.br",
        ProviderId::MagazineLuiza => "www.magazineluiza.com.br",
        ProviderId::Olx => "www.olx.com.br",
    }
}
