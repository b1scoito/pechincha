use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::providers::ProviderId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub provider: ProviderId,
    pub platform_id: String,
    pub title: String,
    pub normalized_title: Option<String>,
    pub url: String,
    pub image_url: Option<String>,
    pub price: PriceInfo,
    pub seller: Option<SellerInfo>,
    pub condition: ProductCondition,
    pub rating: Option<f32>,
    pub review_count: Option<u32>,
    pub sold_count: Option<u32>,
    pub domestic: bool,
    pub fetched_at: DateTime<Utc>,
    /// Keepa price intelligence for this product across Amazon locales.
    /// Index 0 is the product's own domain, rest are international prices.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub keepa: Vec<crate::keepa::KeepaInsight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceInfo {
    pub listed_price: Decimal,
    pub currency: Currency,
    pub price_brl: Decimal,
    pub shipping_cost: Option<Decimal>,
    pub tax: TaxInfo,
    pub total_cost: Decimal,
    pub original_price: Option<Decimal>,
    pub installments: Option<InstallmentInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxInfo {
    pub remessa_conforme: bool,
    pub taxes_included: bool,
    pub import_tax: Option<Decimal>,
    pub icms: Option<Decimal>,
    pub total_tax: Decimal,
    pub tax_regime: TaxRegime,
}

impl Default for TaxInfo {
    fn default() -> Self {
        Self {
            remessa_conforme: false,
            taxes_included: false,
            import_tax: None,
            icms: None,
            total_tax: Decimal::ZERO,
            tax_regime: TaxRegime::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxRegime {
    Domestic,
    RemessaConformeUnder50,
    RemessaConformeOver50,
    InternationalStandard,
    Unknown,
}

impl fmt::Display for TaxRegime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domestic => write!(f, "Domestic"),
            Self::RemessaConformeUnder50 => write!(f, "RC <$50"),
            Self::RemessaConformeOver50 => write!(f, "RC >$50"),
            Self::InternationalStandard => write!(f, "International"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallmentInfo {
    pub count: u8,
    pub amount_per: Decimal,
    pub interest_free: bool,
}

impl fmt::Display for InstallmentInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let suffix = if self.interest_free {
            " sem juros"
        } else {
            ""
        };
        write!(f, "{}x R${}{}", self.count, self.amount_per, suffix)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SellerInfo {
    pub name: String,
    pub reputation: Option<f32>,
    pub official_store: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductCondition {
    New,
    Used,
    Refurbished,
    Unknown,
}

impl fmt::Display for ProductCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New => write!(f, "New"),
            Self::Used => write!(f, "Used"),
            Self::Refurbished => write!(f, "Refurbished"),
            Self::Unknown => write!(f, ""),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Currency {
    BRL,
    USD,
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BRL => write!(f, "R$"),
            Self::USD => write!(f, "US$"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    PriceAsc,
    PriceDesc,
    Rating,
    Relevance,
    #[default]
    TotalCost,
}

impl std::str::FromStr for SortOrder {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "price" | "price-asc" => Ok(Self::PriceAsc),
            "price-desc" => Ok(Self::PriceDesc),
            "rating" => Ok(Self::Rating),
            "relevance" => Ok(Self::Relevance),
            "total-cost" | "total_cost" => Ok(Self::TotalCost),
            _ => Err(format!("unknown sort order: {s}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub query: String,
    pub max_results: usize,
    pub min_price: Option<Decimal>,
    pub max_price: Option<Decimal>,
    pub condition: Option<ProductCondition>,
    pub sort: SortOrder,
    pub platforms: Vec<ProviderId>,
}

impl SearchQuery {
    pub fn simple(query: &str) -> Self {
        Self {
            query: query.to_string(),
            max_results: 5,
            min_price: None,
            max_price: None,
            condition: None,
            sort: SortOrder::TotalCost,
            platforms: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct SearchResults {
    pub products: Vec<Product>,
    pub errors: Vec<(ProviderId, crate::error::ProviderError)>,
    pub query_time: std::time::Duration,
}
