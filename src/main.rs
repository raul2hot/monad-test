//! Monad Arbitrage Bot - Phase 1: Live Price Monitor
//!
//! Monitors live prices from Uniswap V3 and Nad.fun DEX via Alchemy WebSockets

mod config;

use alloy::primitives::{Address, U160, U256};
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::{Filter, TransactionRequest};
use alloy::sol;
use alloy::sol_types::{SolCall, SolEvent};
use dashmap::DashMap;
use eyre::Result;
use futures::StreamExt;
use std::env;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, info, warn, Level};
use tracing_subscriber::EnvFilter;

// Define Uniswap V3 Swap event
sol! {
    #[derive(Debug)]
    event UniswapSwap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );
}

// Uniswap V3 pool slot0 function
sol! {
    #[derive(Debug)]
    function slot0() external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint16 observationIndex,
        uint16 observationCardinality,
        uint16 observationCardinalityNext,
        uint8 feeProtocol,
        bool unlocked
    );
}

// Uniswap V2-style getReserves
sol! {
    #[derive(Debug)]
    function getReserves() external view returns (
        uint112 reserve0,
        uint112 reserve1,
        uint32 blockTimestampLast
    );
}

// Nad.fun LENS getAmountOut function
sol! {
    #[derive(Debug)]
    function getAmountOut(
        address token,
        uint256 amountIn,
        bool isBuy
    ) external view returns (
        address router,
        uint256 amountOut
    );
}

/// Stores the latest prices for each token on each DEX
struct PriceState {
    uniswap_prices: DashMap<String, f64>,
    nadfun_prices: DashMap<String, f64>,
}

impl PriceState {
    fn new() -> Self {
        Self {
            uniswap_prices: DashMap::new(),
            nadfun_prices: DashMap::new(),
        }
    }

    fn update_uniswap(&self, symbol: &str, price: f64) {
        self.uniswap_prices.insert(symbol.to_string(), price);
    }

    fn update_nadfun(&self, symbol: &str, price: f64) {
        self.nadfun_prices.insert(symbol.to_string(), price);
    }

    fn print_prices(&self, symbol: &str, block: u64, source: &str) {
        let uniswap_price = self.uniswap_prices.get(symbol).map(|p| *p);
        let nadfun_price = self.nadfun_prices.get(symbol).map(|p| *p);

        println!("\n[Block {}] {}/WMON (updated by {})", block, symbol, source);

        if let Some(np) = nadfun_price {
            println!("  Nad.fun:   {:.10} MON", np);
        } else {
            println!("  Nad.fun:   -- (waiting for data)");
        }

        if let Some(up) = uniswap_price {
            println!("  Uniswap:   {:.10} MON", up);
        } else {
            println!("  Uniswap:   -- (waiting for data)");
        }

        if let (Some(np), Some(up)) = (nadfun_price, uniswap_price) {
            if up > 0.0 && np > 0.0 {
                let spread = ((np - up) / up) * 100.0;
                let spread_sign = if spread >= 0.0 { "+" } else { "" };
                println!("  Spread:    {}{:.2}%", spread_sign, spread);

                if spread.abs() > 0.5 {
                    if spread > 0.0 {
                        println!("  >>> ARBITRAGE: Buy on Uniswap, Sell on Nad.fun <<<");
                    } else {
                        println!("  >>> ARBITRAGE: Buy on Nad.fun, Sell on Uniswap <<<");
                    }
                }
            }
        }
    }
}

/// Calculate price from Uniswap V3 sqrtPriceX96
/// price = (sqrtPriceX96 / 2^96)^2
fn calculate_price_from_sqrt_price_x96(sqrt_price_x96: U160, token0_is_wmon: bool) -> f64 {
    let sqrt_price = U256::from(sqrt_price_x96);
    let q96: U256 = U256::from(1u128) << 96;

    let sqrt_price_f64 = sqrt_price.to_string().parse::<f64>().unwrap_or(0.0);
    let q96_f64 = q96.to_string().parse::<f64>().unwrap_or(1.0);

    let ratio = sqrt_price_f64 / q96_f64;
    let price = ratio * ratio;

    // We want price in MON per token
    if token0_is_wmon {
        // token0=WMON, token1=TOKEN
        // price = token1/token0 = TOKEN/WMON (tokens per MON)
        // Invert to get MON per token
        if price > 0.0 { 1.0 / price } else { 0.0 }
    } else {
        // token0=TOKEN, token1=WMON
        // price = token1/token0 = WMON/TOKEN (MON per token)
        // Already correct! Don't invert.
        price
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(Level::INFO.into())
                .from_env_lossy(),
        )
        .init();

    // Load environment variables
    dotenvy::dotenv().ok();

    // Get WebSocket URL from environment
    let ws_url = env::var("ALCHEMY_WS_URL")
        .expect("ALCHEMY_WS_URL must be set in .env file");

    println!("=========================================");
    println!("  Monad Arbitrage Monitor - Phase 1");
    println!("=========================================");
    println!("Chain: Monad Mainnet (ID: {})", config::CHAIN_ID);
    println!("Block Time: {}ms", config::BLOCK_TIME_MS);
    println!();

    // Connect to WebSocket
    info!("Connecting to Alchemy WebSocket...");
    let ws = WsConnect::new(&ws_url);
    let provider = ProviderBuilder::new().connect_ws(ws).await?;
    let provider = Arc::new(provider);

    // Verify connection
    let chain_id = provider.get_chain_id().await?;
    if chain_id != config::CHAIN_ID {
        warn!(
            "Chain ID mismatch: expected {}, got {}",
            config::CHAIN_ID,
            chain_id
        );
    } else {
        info!("Connected to Monad Mainnet (Chain ID: {})", chain_id);
    }

    let block_number = provider.get_block_number().await?;
    println!("Current block: {}", block_number);
    println!();

    // Pool addresses
    let chog_uniswap_pool = Address::from_str(config::CHOG_UNISWAP_POOL)?;
    let chog_nadfun_pool = Address::from_str(config::CHOG_NADFUN_POOL)?;
    let wmon = config::wmon_address();
    let chog = Address::from_str(config::CHOG)?;

    // Determine token ordering (lower address is token0)
    let token0_is_wmon = wmon < chog;
    println!("WMON address: {:?}", wmon);
    println!("CHOG address: {:?}", chog);
    println!("Token0 is WMON: {}", token0_is_wmon);
    println!();

    // Initialize price state
    let price_state = Arc::new(PriceState::new());

    // Fetch initial prices
    println!("Fetching initial prices...");

    // Get initial Uniswap price
    let slot0_call = slot0Call {};
    let tx = TransactionRequest::default()
        .to(chog_uniswap_pool)
        .input(slot0_call.abi_encode().into());

    match provider.call(tx).await {
        Ok(result) => {
            if let Ok(decoded) = slot0Call::abi_decode_returns(&result) {
                let price = calculate_price_from_sqrt_price_x96(decoded.sqrtPriceX96, token0_is_wmon);
                println!("Initial CHOG Uniswap price: {:.10} MON", price);
                price_state.update_uniswap("CHOG", price);
            }
        }
        Err(e) => warn!("Could not fetch initial Uniswap price: {}", e),
    }

    // Get initial Nad.fun price via LENS contract
    let lens_address = Address::from_str(config::NADFUN_LENS)?;
    let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 MON

    let lens_call = getAmountOutCall {
        token: chog,
        amountIn: amount_in,
        isBuy: true,
    };
    let tx = TransactionRequest::default()
        .to(lens_address)
        .input(lens_call.abi_encode().into());

    match provider.call(tx).await {
        Ok(result) => {
            if let Ok(decoded) = getAmountOutCall::abi_decode_returns(&result) {
                // tokens_out / 1e18 = tokens per MON
                // price = 1 / tokens_per_mon = MON per token
                let tokens_out = decoded.amountOut.to_string().parse::<f64>().unwrap_or(0.0);
                if tokens_out > 0.0 {
                    let price = 1e18 / tokens_out;
                    println!("Initial CHOG Nad.fun price: {:.10} MON (via LENS)", price);
                    println!("  Router: {:?}", decoded.router);
                    price_state.update_nadfun("CHOG", price);
                }
            }
        }
        Err(e) => warn!("Could not fetch initial Nad.fun price via LENS: {}", e),
    }

    println!();
    println!("Subscribing to swap events...");
    println!("Monitoring pools:");
    println!("  - Uniswap V3 CHOG/WMON: {:?}", chog_uniswap_pool);
    println!("  - Nad.fun CHOG/WMON: {:?}", chog_nadfun_pool);
    println!();
    println!("Waiting for swap events (prices update on each swap)...");
    println!();

    // Subscribe to Uniswap V3 Swap events
    let uniswap_filter = Filter::new()
        .address(chog_uniswap_pool)
        .event_signature(UniswapSwap::SIGNATURE_HASH);

    let uniswap_sub = provider.subscribe_logs(&uniswap_filter).await?;
    let mut uniswap_stream = uniswap_sub.into_stream();

    // Subscribe to Nad.fun Swap events (uses V3 signature!)
    let nadfun_filter = Filter::new()
        .address(chog_nadfun_pool)
        .event_signature(UniswapSwap::SIGNATURE_HASH);  // V3, not V2!

    let nadfun_sub = provider.subscribe_logs(&nadfun_filter).await?;
    let mut nadfun_stream = nadfun_sub.into_stream();

    info!("Subscriptions active");

    // Process events from both streams
    let state1 = price_state.clone();
    let state2 = price_state.clone();

    // Spawn task for Uniswap events
    let uniswap_task = tokio::spawn(async move {
        while let Some(log) = uniswap_stream.next().await {
            if let Ok(decoded) = UniswapSwap::decode_log(&log.inner) {
                let block = log.block_number.unwrap_or(0);
                let price = calculate_price_from_sqrt_price_x96(decoded.data.sqrtPriceX96, token0_is_wmon);

                debug!(
                    "Uniswap CHOG swap: tick={}, sqrtPriceX96={}, price={:.10}",
                    decoded.data.tick, decoded.data.sqrtPriceX96, price
                );

                state1.update_uniswap("CHOG", price);
                state1.print_prices("CHOG", block, "Uniswap");
            }
        }
    });

    // Spawn task for Nad.fun events
    let nadfun_task = tokio::spawn(async move {
        while let Some(log) = nadfun_stream.next().await {
            if let Ok(decoded) = UniswapSwap::decode_log(&log.inner) {
                let block = log.block_number.unwrap_or(0);
                let price = calculate_price_from_sqrt_price_x96(decoded.data.sqrtPriceX96, token0_is_wmon);

                debug!(
                    "Nad.fun CHOG swap: tick={}, sqrtPriceX96={}, price={:.10}",
                    decoded.data.tick, decoded.data.sqrtPriceX96, price
                );

                state2.update_nadfun("CHOG", price);
                state2.print_prices("CHOG", block, "Nad.fun");
            }
        }
    });

    // Wait for all tasks (they run indefinitely)
    tokio::select! {
        _ = uniswap_task => {
            warn!("Uniswap subscription ended");
        }
        _ = nadfun_task => {
            warn!("Nad.fun subscription ended");
        }
    }

    Ok(())
}
