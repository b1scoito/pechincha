pub mod aliexpress;
pub mod amazon;
pub mod amazon_us;
pub mod kabum;
pub mod magalu;
pub mod mercadolivre;
pub mod olx;
pub mod shopee;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::ProviderError;
use crate::models::{Product, SearchQuery};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    MercadoLivre,
    AliExpress,
    Shopee,
    Amazon,
    AmazonUS,
    Kabum,
    MagazineLuiza,
    Olx,
}

impl ProviderId {
    pub fn all() -> &'static [ProviderId] {
        &[
            Self::MercadoLivre,
            Self::AliExpress,
            Self::Shopee,
            Self::Amazon,
            Self::AmazonUS,
            Self::Kabum,
            Self::MagazineLuiza,
            Self::Olx,
        ]
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MercadoLivre => write!(f, "Mercado Livre"),
            Self::AliExpress => write!(f, "AliExpress"),
            Self::Shopee => write!(f, "Shopee"),
            Self::Amazon => write!(f, "Amazon BR"),
            Self::AmazonUS => write!(f, "Amazon US"),
            Self::Kabum => write!(f, "Kabum"),
            Self::MagazineLuiza => write!(f, "Magazine Luiza"),
            Self::Olx => write!(f, "OLX"),
        }
    }
}

impl std::str::FromStr for ProviderId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ml" | "mercadolivre" | "mercado_livre" => Ok(Self::MercadoLivre),
            "ali" | "aliexpress" => Ok(Self::AliExpress),
            "shopee" => Ok(Self::Shopee),
            "amazon" | "amz" | "amazon_br" => Ok(Self::Amazon),
            "amazon_us" | "amz_us" | "amazonus" => Ok(Self::AmazonUS),
            "kabum" => Ok(Self::Kabum),
            "magalu" | "magazineluiza" | "magazine_luiza" => Ok(Self::MagazineLuiza),
            "olx" => Ok(Self::Olx),
            _ => Err(format!("unknown provider: {s}")),
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn id(&self) -> ProviderId;
    fn is_available(&self) -> bool;

    /// wreq mode: provider handles its own HTTP request.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<Product>, ProviderError>;

    /// CDP mode: parse pre-fetched HTML from the real browser.
    /// Default implementation calls search() as fallback.
    fn parse_html(&self, _html: &str, _max_results: usize) -> Result<Vec<Product>, ProviderError> {
        // Default: providers that haven't implemented parse_html yet
        Err(ProviderError::Parse("parse_html not implemented for this provider".into()))
    }
}
