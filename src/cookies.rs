use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, warn};

use crate::providers::ProviderId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub expires: Option<f64>,
}

/// Get the cookie file path for a provider.
pub fn cookie_path(provider: ProviderId) -> PathBuf {
    let provider_name = match provider {
        ProviderId::MercadoLivre => "mercadolivre",
        ProviderId::AliExpress => "aliexpress",
        ProviderId::Shopee => "shopee",
        ProviderId::Amazon => "amazon_br",
        ProviderId::AmazonUS => "amazon_us",
        ProviderId::Kabum => "kabum",
        ProviderId::MagazineLuiza => "magalu",
        ProviderId::Olx => "olx",
    };

    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pechincha")
        .join("cookies")
        .join(format!("{provider_name}.json"))
}

/// Save cookies to disk for a provider.
pub fn save_cookies(provider: ProviderId, cookies: &[SavedCookie]) -> Result<(), String> {
    let path = cookie_path(provider);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create cookie directory: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cookies)
        .map_err(|e| format!("Failed to serialize cookies: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write cookies: {e}"))?;
    debug!(path = %path.display(), count = cookies.len(), "Saved cookies");
    Ok(())
}

/// Load cookies from disk for a provider. Returns empty vec if no cookies saved.
pub fn load_cookies(provider: ProviderId) -> Vec<SavedCookie> {
    let path = cookie_path(provider);
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str::<Vec<SavedCookie>>(&json) {
            Ok(cookies) => {
                debug!(path = %path.display(), count = cookies.len(), "Loaded cookies");
                cookies
            }
            Err(e) => {
                warn!(error = %e, "Failed to parse cookie file");
                Vec::new()
            }
        },
        Err(e) => {
            warn!(error = %e, "Failed to read cookie file");
            Vec::new()
        }
    }
}

/// Check if a provider has saved cookies.
pub fn has_cookies(provider: ProviderId) -> bool {
    cookie_path(provider).exists()
}

/// Delete saved cookies for a provider.
pub fn delete_cookies(provider: ProviderId) -> Result<(), String> {
    let path = cookie_path(provider);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete cookies: {e}"))?;
    }
    Ok(())
}

/// Extract cookies from the user's real browser (Chrome, Brave, Firefox, Safari, etc.)
/// This is the yt-dlp --cookies-from-browser approach.
pub fn extract_browser_cookies(
    _provider: ProviderId,
    browser_name: &str,
    domain: &str,
) -> Result<Vec<SavedCookie>, String> {
    // rookie filters cookies by domain
    let domains = vec![domain.to_string()];

    let raw_cookies = match browser_name.to_lowercase().as_str() {
        "chrome" => rookie::chrome(Some(domains)),
        "brave" => rookie::brave(Some(domains)),
        "firefox" => rookie::firefox(Some(domains)),
        "safari" => rookie::safari(Some(domains)),
        "edge" => rookie::edge(Some(domains)),
        "chromium" => rookie::chromium(Some(domains)),
        "opera" => rookie::opera(Some(domains)),
        _ => return Err(format!("Unsupported browser: {browser_name}. Use: chrome, brave, firefox, safari, edge, chromium, opera")),
    }
    .map_err(|e| format!("Failed to extract cookies from {browser_name}: {e}"))?;

    let cookies: Vec<SavedCookie> = raw_cookies
        .into_iter()
        .map(|c| SavedCookie {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            secure: c.secure,
            http_only: c.http_only,
            expires: c.expires.map(|e| e as f64),
        })
        .collect();

    Ok(cookies)
}

/// Parse cookies from a curl command string (e.g., from browser DevTools "Copy as cURL").
/// Extracts cookies from `-b '...'`, `--cookie '...'`, or `-H 'cookie: ...'` flags.
pub fn parse_curl_cookies(curl_str: &str) -> Vec<SavedCookie> {
    let mut cookie_str = String::new();

    // Try to find -b 'cookies' or --cookie 'cookies'
    for pattern in &["-b '", "-b \"", "--cookie '", "--cookie \"", "-H 'cookie: ", "-H \"cookie: ", "-H 'Cookie: ", "-H \"Cookie: "] {
        if let Some(start) = curl_str.find(pattern) {
            let value_start = start + pattern.len();
            let quote = if pattern.ends_with('\'') { '\'' } else { '"' };
            if let Some(end) = curl_str[value_start..].find(quote) {
                cookie_str = curl_str[value_start..value_start + end].to_string();
                break;
            }
        }
    }

    if cookie_str.is_empty() {
        return Vec::new();
    }

    // Parse "name1=value1; name2=value2; ..." format
    cookie_str
        .split(';')
        .filter_map(|pair| {
            let pair = pair.trim();
            let eq = pair.find('=')?;
            let name = pair[..eq].trim().to_string();
            let value = pair[eq + 1..].trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(SavedCookie {
                name,
                value,
                domain: String::new(), // Will be set by provider context
                path: "/".to_string(),
                secure: true,
                http_only: false,
                expires: None,
            })
        })
        .collect()
}
