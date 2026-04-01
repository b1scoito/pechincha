use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PechinchaConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_sort")]
    pub default_sort: String,
    #[serde(default = "default_results_per_provider")]
    pub results_per_provider: usize,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// Chrome `DevTools` Protocol port for connecting to your real browser.
    /// Launch browser with: `chromium --remote-debugging-port=9222`
    /// Used by Shopee and `AliExpress` which require a real browser session.
    #[serde(default)]
    pub cdp_port: Option<u16>,
    /// Cache TTL in minutes (0 to disable). Default: 30.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_minutes: u64,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            default_sort: default_sort(),
            results_per_provider: default_results_per_provider(),
            timeout_seconds: default_timeout(),
            cdp_port: None,
            cache_ttl_minutes: default_cache_ttl(),
        }
    }
}

fn default_sort() -> String {
    "total-cost".to_string()
}
const fn default_results_per_provider() -> usize {
    5
}
const fn default_timeout() -> u64 {
    30
}
const fn default_cache_ttl() -> u64 {
    30
}
/// Google Shopping disabled by default — redundant with ML/Amazon/Magalu/Kabum
/// and shows misleading prices (import prices displayed as BRL without tax).
const fn default_google_shopping() -> ProviderConfig {
    ProviderConfig { enabled: false }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub mercadolivre: ProviderConfig,
    #[serde(default)]
    pub aliexpress: AliExpressConfig,
    #[serde(default)]
    pub shopee: ShopeeConfig,
    #[serde(default)]
    pub amazon: AmazonConfig,
    #[serde(default)]
    pub amazon_us: AmazonConfig,
    #[serde(default)]
    pub kabum: ProviderConfig,
    #[serde(default)]
    pub magalu: ProviderConfig,
    #[serde(default)]
    pub olx: ProviderConfig,
    #[serde(default = "default_google_shopping")]
    pub google_shopping: ProviderConfig,
    #[serde(default)]
    pub ebay: ProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

const fn default_true() -> bool {
    true
}

/// Default: `enabled=false` (requires Affiliate API or remote browser CDP)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AliExpressConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default)]
    pub tracking_id: Option<String>,
}

/// Default: `enabled=false` (requires Affiliate API or remote browser CDP)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShopeeConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmazonConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub access_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub partner_tag: Option<String>,
}

impl Default for AmazonConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            access_key: None,
            secret_key: None,
            partner_tag: None,
        }
    }
}

impl PechinchaConfig {
    #[allow(clippy::missing_errors_doc)]
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let config_path = path
            .map_or_else(default_config_path, PathBuf::from);

        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)
                .map_err(|e| format!("Failed to read config: {e}"))?;
            toml::from_str(&contents)
                .map_err(|e| format!("Failed to parse config: {e}"))
        } else {
            Ok(Self::default())
        }
    }

    /// Generate a template config file.
    #[must_use]
    pub fn template() -> String {
        let default = Self::default();
        toml::to_string_pretty(&default).unwrap_or_default()
    }

    /// Save config to the default path, creating directories if needed.
    #[allow(clippy::missing_errors_doc)]
    pub fn save(&self, path: Option<&Path>) -> Result<(), String> {
        let config_path = path
            .map_or_else(default_config_path, PathBuf::from);

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {e}"))?;
        }

        let contents = toml::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;

        std::fs::write(&config_path, contents)
            .map_err(|e| format!("Failed to write config: {e}"))?;

        Ok(())
    }
}

#[must_use]
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pechincha")
        .join("config.toml")
}
