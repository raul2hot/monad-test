//! Monorail API Client

use eyre::Result;
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct QuoteResponse {
    pub output: String,
    pub output_formatted: String,
    pub price: f64,
    pub route: Vec<RouteStep>,
}

#[derive(Debug, Deserialize)]
pub struct RouteStep {
    pub dex: String,
    pub pool: String,
}

pub struct MonorailClient {
    client: Client,
    base_url: String,
    app_id: String,
}

impl MonorailClient {
    pub fn new(app_id: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: crate::config::MONORAIL_API.to_string(),
            app_id: app_id.to_string(),
        }
    }

    /// Get quote for swapping tokens
    /// Returns price as tokenOut per tokenIn
    pub async fn get_quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in: f64,
    ) -> Result<QuoteResponse> {
        let url = format!(
            "{}?source={}&from={}&to={}&amount={}",
            self.base_url, self.app_id, token_in, token_out, amount_in
        );

        tracing::debug!("Monorail API URL: {}", url);

        let response = self.client.get(&url).send().await?;

        let status = response.status();
        let body = response.text().await?;

        tracing::debug!("Monorail API status: {}, body: {}", status, body);

        let quote: QuoteResponse = serde_json::from_str(&body)
            .map_err(|e| eyre::eyre!("Failed to parse response: {}. Body: {}", e, body))?;

        Ok(quote)
    }

    /// Get MON price in USDC
    pub async fn get_mon_price(&self) -> Result<f64> {
        // Use native MON address (0x0...0)
        let quote = self
            .get_quote(
                "0x0000000000000000000000000000000000000000",
                crate::config::USDC,
                1.0, // 1 MON
            )
            .await?;

        Ok(quote.price)
    }
}
