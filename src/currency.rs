use chrono::Utc;
use wreq::Client;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

const FALLBACK_RATE: Decimal = dec!(5.50);
const CACHE_TTL_SECS: i64 = 3600; // 1 hour

#[derive(Clone)]
pub struct ExchangeRateService {
    client: Client,
    cache: Arc<RwLock<Option<CachedRate>>>,
}

struct CachedRate {
    rate: Decimal,
    fetched_at: chrono::DateTime<Utc>,
}

impl ExchangeRateService {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the current USD/BRL exchange rate.
    /// Returns cached value if fresh, otherwise fetches from BCB PTAX API.
    pub async fn get_usd_brl(&self) -> Decimal {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(ref cached) = *cache {
                let age = Utc::now() - cached.fetched_at;
                if age.num_seconds() < CACHE_TTL_SECS {
                    return cached.rate;
                }
            }
        }

        // Fetch fresh rate
        match self.fetch_ptax_rate().await {
            Ok(rate) => {
                let mut cache = self.cache.write().await;
                *cache = Some(CachedRate {
                    rate,
                    fetched_at: Utc::now(),
                });
                rate
            }
            Err(e) => {
                warn!(error = %e, "Failed to fetch exchange rate, using fallback {}", FALLBACK_RATE);
                FALLBACK_RATE
            }
        }
    }

    async fn fetch_ptax_rate(&self) -> Result<Decimal, Box<dyn std::error::Error + Send + Sync>> {
        let today = Utc::now().date_naive();
        // BCB API may not have today's rate yet, try last 3 business days
        for days_back in 0..=4 {
            let date = today - chrono::Duration::days(days_back);
            let formatted = date.format("%m-%d-%Y");

            let url = format!(
                "https://olinda.bcb.gov.br/olinda/servico/PTAX/versao/v1/odata/CotacaoDolarDia(dataCotacao=@dataCotacao)?@dataCotacao='{formatted}'&$format=json&$top=1&$orderby=dataHoraCotacao%20desc"
            );

            debug!(url = %url, "Fetching PTAX exchange rate");

            let resp = self.client.get(&url).send().await?;
            if !resp.status().is_success() {
                continue;
            }

            let body: serde_json::Value = resp.json().await?;
            let values = body["value"].as_array();

            if let Some(values) = values {
                if let Some(first) = values.first() {
                    if let Some(rate) = first["cotacaoVenda"].as_f64() {
                        let rate = Decimal::try_from(rate)?;
                        debug!(rate = %rate, date = %date, "Got PTAX rate");
                        return Ok(rate);
                    }
                }
            }
        }

        Err("No PTAX rate available for recent dates".into())
    }
}
