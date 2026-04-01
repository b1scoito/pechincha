use wreq_util::Emulation;

use crate::providers::ProviderId;

const EMULATION_PROFILES: &[fn() -> Emulation] = &[
    || Emulation::Chrome131,
    || Emulation::Chrome127,
    || Emulation::Chrome126,
    || Emulation::Edge127,
    || Emulation::Safari18,
];

#[must_use]
pub const fn random_user_agent() -> &'static str {
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
}

fn random_emulation() -> Emulation {
    let idx = rand::random::<usize>() % EMULATION_PROFILES.len();
    EMULATION_PROFILES[idx]()
}

/// Build a wreq client that impersonates a real browser at the TLS/JA3/HTTP2 level.
///
/// # Panics
///
/// Panics if the HTTP client cannot be built.
#[must_use]
pub fn build_impersonating_client(timeout_secs: u64) -> wreq::Client {
    wreq::Client::builder()
        .emulation(random_emulation())
        .cookie_store(true)
        .redirect(wreq::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .expect("failed to build impersonating HTTP client")
}

#[must_use]
pub const fn provider_domain(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::MercadoLivre => "www.mercadolivre.com.br",
        ProviderId::AliExpress => "pt.aliexpress.com",
        ProviderId::Shopee => "shopee.com.br",
        ProviderId::Amazon => "www.amazon.com.br",
        ProviderId::AmazonUS => "www.amazon.com",
        ProviderId::Kabum => "www.kabum.com.br",
        ProviderId::MagazineLuiza => "www.magazineluiza.com.br",
        ProviderId::Olx => "www.olx.com.br",
        ProviderId::GoogleShopping => "www.google.com.br",
        ProviderId::Ebay => "www.ebay.com",
    }
}
