//! 0x Swap API Client for Monad

use eyre::Result;
use reqwest::Client;
use serde::Deserialize;
use std::env;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceResponse {
    pub buy_amount: String,
    pub sell_amount: String,
    pub buy_token: String,
    pub sell_token: String,
    pub liquidity_available: bool,
    pub gas: String,
    pub gas_price: String,
    #[serde(default)]
    pub route: Option<RouteInfo>,
}

#[derive(Debug, Deserialize)]
pub struct RouteInfo {
    pub fills: Vec<Fill>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fill {
    pub source: String,
    pub proportion_bps: String,
}

pub struct ZrxClient {
    client: Client,
    api_key: String,
}

impl ZrxClient {
    pub fn new() -> Result<Self> {
        let api_key = env::var("ZRX_API_KEY")
            .map_err(|_| eyre::eyre!("ZRX_API_KEY not set. Get free key at https://dashboard.0x.org"))?;

        Ok(Self {
            client: Client::new(),
            api_key,
        })
    }

    /// Get price for selling tokens
    pub async fn get_price(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
    ) -> Result<PriceResponse> {
        let url = format!(
            "{}{}?chainId={}&sellToken={}&buyToken={}&sellAmount={}",
            crate::config::ZRX_API_BASE,
            crate::config::ZRX_PRICE_ENDPOINT,
            crate::config::CHAIN_ID,
            sell_token,
            buy_token,
            sell_amount
        );

        tracing::debug!("0x API URL: {}", url);

        let response = self.client
            .get(&url)
            .header("0x-api-key", &self.api_key)
            .header("0x-version", "v2")
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        tracing::debug!("0x API status: {}, body length: {}", status, body.len());

        if !status.is_success() {
            return Err(eyre::eyre!("0x API error: {} - {}", status, body));
        }

        let price: PriceResponse = serde_json::from_str(&body)
            .map_err(|e| eyre::eyre!("Failed to parse 0x response: {}. Body: {}", e, body))?;

        if !price.liquidity_available {
            return Err(eyre::eyre!("No liquidity available for this pair"));
        }

        Ok(price)
    }

    /// Get MON price in USDC
    /// Returns price as USDC per 1 MON
    pub async fn get_mon_usdc_price(&self) -> Result<f64> {
        // Sell 1 WMON (18 decimals)
        let sell_amount = "1000000000000000000"; // 1e18

        let response = self.get_price(
            crate::config::WMON,
            crate::config::USDC,
            sell_amount,
        ).await?;

        // buyAmount is USDC (6 decimals)
        // Example: buyAmount="29132" means $0.029132
        let buy_amount: f64 = response.buy_amount.parse().unwrap_or(0.0);
        let usdc_price = buy_amount / 1_000_000.0; // Convert from 6 decimals

        // Log which DEXes 0x is routing through
        if let Some(route) = &response.route {
            let sources: Vec<&str> = route.fills.iter()
                .map(|f| f.source.as_str())
                .collect();
            tracing::info!("0x routing through: {:?}", sources);
        }

        Ok(usdc_price)
    }
}
