use alloy::network::EthereumWallet;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use clap::{Parser, Subcommand};
use eyre::Result;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::interval;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

mod config;
mod display;
mod execution;
mod multicall;
mod nonce;
mod pools;
mod price;
mod wallet;

use config::{
    get_all_pools, get_lfj_pool, get_monday_trade_pool, get_v3_pools, get_router_by_name,
    POLL_INTERVAL_MS, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS,
    UNISWAP_SWAP_ROUTER, PANCAKE_SMART_ROUTER, LFJ_LB_ROUTER, MONDAY_SWAP_ROUTER,
    RouterConfig,
};
use display::{display_prices, init_arb_log};
use execution::{SwapParams, SwapDirection, execute_swap, print_swap_report, build_swap_calldata, wait_for_next_block};
use execution::report::print_comparison_report;
use multicall::fetch_prices_batched;
use nonce::init_nonce;
use pools::{create_lfj_active_id_call, create_lfj_bin_step_call, create_slot0_call, PriceCall, PoolPrice};
use wallet::{get_balances, print_balances, wrap_mon, unwrap_wmon, print_wrap_result};

#[derive(Parser)]
#[command(name = "monad-arb")]
#[command(about = "Monad Mainnet Arbitrage Bot", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run price monitor (default)
    Monitor,

    /// Execute a test swap on a specific DEX
    TestSwap {
        /// DEX name: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade
        #[arg(long)]
        dex: String,

        /// Amount to swap (in WMON for sell, USDC for buy)
        #[arg(long, default_value = "1.0")]
        amount: f64,

        /// Direction: sell (WMON→USDC) or buy (USDC→WMON)
        #[arg(long, default_value = "sell")]
        direction: String,

        /// Slippage tolerance in bps (e.g., 100 = 1%)
        #[arg(long, default_value = "100")]
        slippage: u32,
    },

    /// Test swaps on all DEXes
    TestAll {
        /// Amount to swap per DEX
        #[arg(long, default_value = "0.5")]
        amount: f64,

        /// Direction: sell or buy
        #[arg(long, default_value = "sell")]
        direction: String,

        /// Slippage tolerance in bps
        #[arg(long, default_value = "150")]
        slippage: u32,
    },

    // ============== WALLET COMMANDS ==============

    /// Show wallet balances (MON, WMON, USDC)
    Balance,

    /// Wrap MON to WMON
    Wrap {
        /// Amount of MON to wrap
        #[arg(long)]
        amount: f64,
    },

    /// Unwrap WMON to MON
    Unwrap {
        /// Amount of WMON to unwrap
        #[arg(long)]
        amount: f64,
    },

    /// Swap USDC to MON (buys WMON then unwraps)
    BuyMon {
        /// Amount of USDC to spend
        #[arg(long)]
        amount: f64,

        /// DEX to use: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade
        #[arg(long, default_value = "uniswap")]
        dex: String,

        /// Slippage tolerance in bps (e.g., 100 = 1%)
        #[arg(long, default_value = "100")]
        slippage: u32,

        /// Keep as WMON instead of unwrapping to MON
        #[arg(long, default_value = "false")]
        keep_wrapped: bool,
    },

    /// Swap MON to USDC (wraps MON then sells WMON)
    SellMon {
        /// Amount of MON to sell
        #[arg(long)]
        amount: f64,

        /// DEX to use: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade
        #[arg(long, default_value = "uniswap")]
        dex: String,

        /// Slippage tolerance in bps (e.g., 100 = 1%)
        #[arg(long, default_value = "100")]
        slippage: u32,

        /// Use WMON directly instead of wrapping MON first
        #[arg(long, default_value = "false")]
        use_wmon: bool,
    },

    /// Test DEX-to-DEX arbitrage (sell on one DEX, buy on another)
    TestArb {
        /// DEX to sell WMON on (higher price)
        #[arg(long)]
        sell_dex: String,

        /// DEX to buy WMON on (lower price)
        #[arg(long)]
        buy_dex: String,

        /// Amount of WMON to start with
        #[arg(long, default_value = "1.0")]
        amount: f64,

        /// Slippage tolerance in bps (e.g., 150 = 1.5%)
        #[arg(long, default_value = "150")]
        slippage: u32,
    },

    /// Prepare wallet for arbitrage by approving all routers (one-time setup)
    PrepareArb,
}

async fn get_current_prices<P: alloy::providers::Provider>(provider: &P) -> Result<Vec<PoolPrice>> {
    let mut price_calls: Vec<PriceCall> = Vec::new();

    for pool in get_v3_pools() {
        price_calls.push(create_slot0_call(&pool));
    }

    let lfj_pool = get_lfj_pool();
    price_calls.push(create_lfj_active_id_call(&lfj_pool));
    price_calls.push(create_lfj_bin_step_call(&lfj_pool));

    let monday_pool = get_monday_trade_pool();
    price_calls.push(create_slot0_call(&monday_pool));

    let (prices, _) = fetch_prices_batched(provider, price_calls).await?;
    Ok(prices)
}

async fn run_monitor() -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set in .env file");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let all_pools = get_all_pools();
    info!("Monitoring {} pools", all_pools.len());

    let arb_log_path = init_arb_log();
    eprintln!(
        "\x1b[1;33mARB opportunities are logged to: {}\x1b[0m",
        arb_log_path.canonicalize().unwrap_or(arb_log_path).display()
    );

    let mut price_calls: Vec<PriceCall> = Vec::new();

    for pool in get_v3_pools() {
        price_calls.push(create_slot0_call(&pool));
    }

    let lfj_pool = get_lfj_pool();
    price_calls.push(create_lfj_active_id_call(&lfj_pool));
    price_calls.push(create_lfj_bin_step_call(&lfj_pool));

    let monday_pool = get_monday_trade_pool();
    price_calls.push(create_slot0_call(&monday_pool));

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));

    loop {
        poll_interval.tick().await;

        match fetch_prices_batched(&provider, price_calls.clone()).await {
            Ok((prices, elapsed_ms)) => {
                display_prices(&prices, elapsed_ms);
            }
            Err(e) => {
                error!("Failed to fetch prices: {}", e);
                display::clear_screen();
                println!("\x1b[1;31mError fetching prices: {}\x1b[0m", e);
                println!("\nRetrying in {} ms...", POLL_INTERVAL_MS);
            }
        }
    }
}

async fn run_test_swap(dex: &str, amount: f64, direction: &str, slippage: u32) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;
    println!("Wallet: {:?}", signer_address);

    // Create provider with signer ONCE (optimization: avoid rebuilding per swap)
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Fetch gas price ONCE (optimization: avoid RPC call per swap)
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
    println!("  [TIMING] Gas price: {} gwei", gas_price / 1_000_000_000);

    // Get router config
    let router = get_router_by_name(dex)
        .ok_or_else(|| eyre::eyre!("Unknown DEX: {}. Valid options: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade", dex))?;

    // Get current prices
    println!("Fetching current prices...");
    let t0 = std::time::Instant::now();
    let prices = get_current_prices(&provider).await?;
    println!("  [TIMING] Price fetch: {:?}", t0.elapsed());

    let price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", dex))?;

    println!("Current {} price: {:.6} USDC/WMON", dex, price.price);

    let direction = match direction.to_lowercase().as_str() {
        "sell" => SwapDirection::Sell,
        "buy" => SwapDirection::Buy,
        _ => return Err(eyre::eyre!("Invalid direction. Use 'sell' or 'buy'")),
    };

    let params = SwapParams {
        router,
        direction,
        amount_in: amount,
        slippage_bps: slippage,
        expected_price: price.price,
    };

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  EXECUTING TEST SWAP ON {}", dex.to_uppercase());
    println!("══════════════════════════════════════════════════════════════");

    let t1 = std::time::Instant::now();
    let result = execute_swap(
        &provider,
        &provider_with_signer,
        signer_address,
        params,
        gas_price,
        false,  // Don't skip balance check for test swaps
    ).await?;
    println!("  [TIMING] Swap execution: {:?}", t1.elapsed());
    print_swap_report(&result);

    Ok(())
}

async fn run_test_all(amount: f64, direction: &str, slippage: u32) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;
    println!("Wallet: {:?}", signer_address);

    // Create provider with signer ONCE (optimization: avoid rebuilding per swap)
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Fetch gas price ONCE (optimization: avoid RPC call per swap)
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
    println!("  [TIMING] Gas price: {} gwei", gas_price / 1_000_000_000);

    let prices = get_current_prices(&provider).await?;

    let direction = match direction.to_lowercase().as_str() {
        "sell" => SwapDirection::Sell,
        "buy" => SwapDirection::Buy,
        _ => return Err(eyre::eyre!("Invalid direction")),
    };

    let dexes = vec!["uniswap", "pancakeswap1", "pancakeswap2", "lfj", "mondaytrade"];
    let mut results = Vec::new();

    for dex in dexes {
        println!("\n══════════════════════════════════════════════════════════════");
        println!("  TESTING {}", dex.to_uppercase());
        println!("══════════════════════════════════════════════════════════════");

        let router = match get_router_by_name(dex) {
            Some(r) => r,
            None => {
                println!("  Skipping {} - router not found", dex);
                continue;
            }
        };

        let price = match prices.iter().find(|p| p.pool_name.to_lowercase() == dex.to_lowercase()) {
            Some(p) => p.price,
            None => {
                println!("  Skipping {} - price not available", dex);
                continue;
            }
        };

        let params = SwapParams {
            router,
            direction,
            amount_in: amount,
            slippage_bps: slippage,
            expected_price: price,
        };

        let t0 = std::time::Instant::now();
        match execute_swap(
            &provider,
            &provider_with_signer,
            signer_address,
            params,
            gas_price,
            false,  // Don't skip balance check for test swaps
        ).await {
            Ok(result) => {
                println!("  [TIMING] Swap execution: {:?}", t0.elapsed());
                print_swap_report(&result);
                results.push(result);
            }
            Err(e) => {
                println!("  Error on {}: {}", dex, e);
            }
        }

        // Small delay between swaps
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Print comparison
    print_comparison_report(&results);

    Ok(())
}

// ============== WALLET COMMAND HANDLERS ==============

async fn run_balance() -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;

    println!("Fetching balances...");
    let balances = get_balances(&provider, signer.address()).await?;
    print_balances(&balances);

    Ok(())
}

async fn run_wrap(amount: f64) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    init_nonce(&provider, signer.address()).await?;
    println!("Wallet: {:?}", signer.address());

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  WRAPPING MON TO WMON");
    println!("══════════════════════════════════════════════════════════════");

    let result = wrap_mon(&provider, &signer, amount, &rpc_url).await?;
    print_wrap_result(&result);

    // Show updated balances
    println!("Updated balances:");
    let balances = get_balances(&provider, signer.address()).await?;
    print_balances(&balances);

    Ok(())
}

async fn run_unwrap(amount: f64) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    init_nonce(&provider, signer.address()).await?;
    println!("Wallet: {:?}", signer.address());

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  UNWRAPPING WMON TO MON");
    println!("══════════════════════════════════════════════════════════════");

    let result = unwrap_wmon(&provider, &signer, amount, &rpc_url).await?;
    print_wrap_result(&result);

    // Show updated balances
    println!("Updated balances:");
    let balances = get_balances(&provider, signer.address()).await?;
    print_balances(&balances);

    Ok(())
}

async fn run_buy_mon(amount: f64, dex: &str, slippage: u32, keep_wrapped: bool) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;
    println!("Wallet: {:?}", signer_address);

    // Create provider with signer ONCE (optimization: avoid rebuilding per swap)
    let wallet = EthereumWallet::from(signer.clone());
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Fetch gas price ONCE (optimization: avoid RPC call per swap)
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    // Get router config
    let router = get_router_by_name(dex)
        .ok_or_else(|| eyre::eyre!("Unknown DEX: {}. Valid options: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade", dex))?;

    // Get current prices
    println!("Fetching current prices...");
    let prices = get_current_prices(&provider).await?;

    let price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", dex))?;

    println!("Current {} price: {:.6} USDC/WMON", dex, price.price);

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  BUYING MON WITH USDC (via {})", dex.to_uppercase());
    println!("══════════════════════════════════════════════════════════════");

    // Step 1: Swap USDC -> WMON
    let params = SwapParams {
        router,
        direction: SwapDirection::Buy,  // USDC -> WMON
        amount_in: amount,
        slippage_bps: slippage,
        expected_price: price.price,
    };

    let swap_result = execute_swap(
        &provider,
        &provider_with_signer,
        signer_address,
        params,
        gas_price,
        false,  // Don't skip balance check
    ).await?;
    print_swap_report(&swap_result);

    if !swap_result.success {
        return Err(eyre::eyre!("Swap failed: {:?}", swap_result.error));
    }

    // Step 2: Unwrap WMON -> MON (unless keep_wrapped is true)
    if !keep_wrapped && swap_result.amount_out_human > 0.0 {
        println!("\n  -> Unwrapping received WMON to MON...");
        let unwrap_result = unwrap_wmon(&provider, &signer, swap_result.amount_out_human, &rpc_url).await?;
        print_wrap_result(&unwrap_result);
    } else if keep_wrapped {
        println!("\n  -> Keeping as WMON (--keep-wrapped flag set)");
    }

    // Show updated balances
    println!("\nFinal balances:");
    let balances = get_balances(&provider, signer.address()).await?;
    print_balances(&balances);

    Ok(())
}

async fn run_sell_mon(amount: f64, dex: &str, slippage: u32, use_wmon: bool) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;
    println!("Wallet: {:?}", signer_address);

    // Create provider with signer ONCE (optimization: avoid rebuilding per swap)
    let wallet = EthereumWallet::from(signer.clone());
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Fetch gas price ONCE (optimization: avoid RPC call per swap)
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    // Get router config
    let router = get_router_by_name(dex)
        .ok_or_else(|| eyre::eyre!("Unknown DEX: {}. Valid options: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade", dex))?;

    // Get current prices
    println!("Fetching current prices...");
    let prices = get_current_prices(&provider).await?;

    let price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", dex))?;

    println!("Current {} price: {:.6} USDC/WMON", dex, price.price);

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  SELLING MON FOR USDC (via {})", dex.to_uppercase());
    println!("══════════════════════════════════════════════════════════════");

    // Step 1: Wrap MON -> WMON (unless use_wmon is true)
    let wmon_amount = if !use_wmon {
        println!("\n  -> Wrapping MON to WMON first...");
        let wrap_result = wrap_mon(&provider, &signer, amount, &rpc_url).await?;
        print_wrap_result(&wrap_result);

        if !wrap_result.success {
            return Err(eyre::eyre!("Wrap failed: {:?}", wrap_result.error));
        }
        wrap_result.amount_out
    } else {
        println!("\n  -> Using existing WMON (--use-wmon flag set)");
        amount
    };

    // Step 2: Swap WMON -> USDC
    let params = SwapParams {
        router,
        direction: SwapDirection::Sell,  // WMON -> USDC
        amount_in: wmon_amount,
        slippage_bps: slippage,
        expected_price: price.price,
    };

    let swap_result = execute_swap(
        &provider,
        &provider_with_signer,
        signer_address,
        params,
        gas_price,
        false,  // Don't skip balance check
    ).await?;
    print_swap_report(&swap_result);

    // Show updated balances
    println!("\nFinal balances:");
    let balances = get_balances(&provider, signer_address).await?;
    print_balances(&balances);

    Ok(())
}

/// Helper function to convert human amount to U256 with proper decimals
fn to_wei(amount: f64, decimals: u8) -> alloy::primitives::U256 {
    let multiplier = 10u64.pow(decimals as u32);
    let wei_amount = (amount * multiplier as f64) as u64;
    alloy::primitives::U256::from(wei_amount)
}

/// Query actual USDC balance for a wallet
async fn query_usdc_balance<P: Provider>(provider: &P, wallet_address: alloy::primitives::Address) -> Result<f64> {
    use alloy::sol;
    use alloy::sol_types::SolCall;

    sol! {
        #[derive(Debug)]
        function balanceOf(address account) external view returns (uint256);
    }

    let balance_call = balanceOfCall { account: wallet_address };
    let balance_tx = alloy::rpc::types::TransactionRequest::default()
        .to(USDC_ADDRESS)
        .input(alloy::rpc::types::TransactionInput::new(
            alloy::primitives::Bytes::from(balance_call.abi_encode())
        ));
    let result = provider.call(balance_tx).await?;
    let balance_wei = alloy::primitives::U256::from_be_slice(&result);
    let balance_human = (balance_wei.to::<u128>() as f64) / 1_000_000.0; // USDC has 6 decimals
    Ok(balance_human)
}

/// Helper function to pre-build swap calldata without executing
fn build_swap_calldata_only(
    router: &RouterConfig,
    direction: SwapDirection,
    amount_in: alloy::primitives::U256,
    amount_out_min: alloy::primitives::U256,
    recipient: alloy::primitives::Address,
) -> Result<alloy::primitives::Bytes> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let (token_in, token_out) = match direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS),
    };

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    build_swap_calldata(
        router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        recipient,
        router.pool_fee,
        deadline,
    )
}

async fn run_test_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    let arb_start = std::time::Instant::now();

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;
    println!("Wallet: {:?}", signer_address);

    // ═══════════════════════════════════════════════════════════════════
    // PHASE 4B OPTIMIZATIONS: Create provider_with_signer and fetch gas_price ONCE
    // ═══════════════════════════════════════════════════════════════════
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Fetch gas price ONCE (saves ~100-300ms per swap)
    let t_gas = std::time::Instant::now();
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);
    println!("  [TIMING] Gas price fetch: {:?} ({} gwei)", t_gas.elapsed(), gas_price / 1_000_000_000);

    // Get routers
    let sell_router = get_router_by_name(sell_dex)
        .ok_or_else(|| eyre::eyre!("Unknown sell DEX: {}", sell_dex))?;
    let buy_router = get_router_by_name(buy_dex)
        .ok_or_else(|| eyre::eyre!("Unknown buy DEX: {}", buy_dex))?;

    // Get current prices
    println!("\nFetching current prices...");
    let t_price = std::time::Instant::now();
    let prices = get_current_prices(&provider).await?;
    println!("  [TIMING] Price fetch: {:?}", t_price.elapsed());

    let sell_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == sell_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", sell_dex))?;

    let buy_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", buy_dex))?;

    println!("  {} price: {:.6} USDC/WMON", sell_dex, sell_price.price);
    println!("  {} price: {:.6} USDC/WMON", buy_dex, buy_price.price);

    let spread_bps = ((sell_price.price - buy_price.price) / buy_price.price * 10000.0) as i32;
    println!("  Spread: {} bps ({:.4}%)", spread_bps, spread_bps as f64 / 100.0);

    if spread_bps <= 0 {
        println!("\n⚠️  WARNING: Negative spread! sell_dex should have HIGHER price than buy_dex");
        println!("  Consider swapping: --sell-dex {} --buy-dex {}", buy_dex, sell_dex);
    }

    // ═══════════════════════════════════════════════════════════════════
    // PRE-CALCULATE SWAP 2 PARAMETERS (optimization: ready before swap 1)
    // ═══════════════════════════════════════════════════════════════════
    let expected_usdc = amount * sell_price.price;
    let expected_usdc_wei = to_wei(expected_usdc, USDC_DECIMALS);

    // Pre-calculate expected WMON output from swap 2 (for slippage)
    let expected_wmon_back = expected_usdc / buy_price.price;
    let slippage_multiplier = 1.0 - (slippage as f64 / 10000.0);
    let min_wmon_out = expected_wmon_back * slippage_multiplier;
    let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);

    // Pre-build swap 2 calldata (will be ready when swap 1 completes)
    let _swap2_calldata_prebuilt = build_swap_calldata_only(
        &buy_router,
        SwapDirection::Buy,
        expected_usdc_wei,  // Will use actual USDC if differs significantly
        min_wmon_out_wei,
        signer_address,
    )?;

    println!("\n  Pre-built swap 2 calldata (expected USDC: {:.6})", expected_usdc);

    // Get initial WMON balance
    let balances_before = get_balances(&provider, signer_address).await?;
    println!("  Starting WMON balance: {:.6}", balances_before.wmon_human);

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  DEX-TO-DEX ARBITRAGE TEST");
    println!("══════════════════════════════════════════════════════════════");
    println!("  Route: WMON --({})-> USDC --({})-> WMON", sell_dex, buy_dex);
    println!("  Amount: {} WMON", amount);
    println!("══════════════════════════════════════════════════════════════\n");

    // ═══════════════════════════════════════════════════════════════════
    // STEP 1: Sell WMON for USDC on sell_dex
    // ═══════════════════════════════════════════════════════════════════
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ STEP 1: Sell {} WMON on {} for USDC", amount, sell_dex);
    println!("└─────────────────────────────────────────────────────────────┘");

    let sell_params = SwapParams {
        router: sell_router,
        direction: SwapDirection::Sell,  // WMON -> USDC
        amount_in: amount,
        slippage_bps: slippage,
        expected_price: sell_price.price,
    };

    let t_swap1 = std::time::Instant::now();
    let sell_result = execute_swap(
        &provider,
        &provider_with_signer,
        signer_address,
        sell_params,
        gas_price,
        true,  // Skip balance check for arb (optimization: saves ~400ms)
    ).await?;
    println!("  [TIMING] Swap 1 execution: {:?}", t_swap1.elapsed());
    print_swap_report(&sell_result);

    if !sell_result.success {
        return Err(eyre::eyre!("Step 1 failed: Sell swap failed"));
    }

    let usdc_estimated = sell_result.amount_out_human;
    println!("  ✓ Estimated received: {:.6} USDC", usdc_estimated);

    // ═══════════════════════════════════════════════════════════════════
    // MONAD STATE COMMITMENT + ACTUAL BALANCE QUERY (PARALLEL)
    // Query actual USDC balance while waiting for block confirmation.
    // This ensures we use the real balance for swap 2, not an estimate.
    // Running in parallel adds ZERO latency.
    // ═══════════════════════════════════════════════════════════════════
    let ws_url = std::env::var("MONAD_WS_URL")
        .unwrap_or_else(|_| rpc_url.replace("https://", "wss://").replace("http://", "ws://"));
    println!("  ⏳ Waiting for block + querying actual USDC balance (parallel)...");
    let t_block = std::time::Instant::now();

    // Run block wait and balance query in parallel
    let block_future = wait_for_next_block(&ws_url);
    let balance_future = query_usdc_balance(&provider, signer_address);

    let (block_result, balance_result) = tokio::join!(block_future, balance_future);

    // Handle block wait result
    match block_result {
        Ok(block_num) => {
            println!("  ✓ Block {} confirmed in {:?}", block_num, t_block.elapsed());
        }
        Err(e) => {
            println!("  ⚠ WebSocket failed ({}), falling back to 500ms delay", e);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    // Get actual USDC balance for swap 2
    let usdc_for_swap2 = match balance_result {
        Ok(actual_balance) => {
            if (actual_balance - usdc_estimated).abs() > 0.000001 {
                println!("  ⚠ Estimate vs Actual: {:.6} vs {:.6} (diff: {:+.6})",
                    usdc_estimated, actual_balance, actual_balance - usdc_estimated);
            }
            println!("  ✓ Using actual USDC balance: {:.6}", actual_balance);
            actual_balance
        }
        Err(e) => {
            println!("  ⚠ Balance query failed ({}), using estimate with 0.5% buffer", e);
            usdc_estimated * 0.995  // Safety buffer if query fails
        }
    };

    if usdc_for_swap2 < 0.000001 {
        return Err(eyre::eyre!("No USDC balance available for swap 2. Swap 1 may have failed silently."));
    }

    // ═══════════════════════════════════════════════════════════════════
    // STEP 2: Buy WMON with USDC on buy_dex
    // ═══════════════════════════════════════════════════════════════════
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ STEP 2: Buy WMON with {:.6} USDC on {}", usdc_for_swap2, buy_dex);
    println!("└─────────────────────────────────────────────────────────────┘");

    // Skip price refresh for speed - use original price (optimization: saves ~200ms)
    // For production, you might want to use actual USDC received to recalculate
    let buy_price_updated = buy_price.price;

    let buy_params = SwapParams {
        router: buy_router,
        direction: SwapDirection::Buy,  // USDC -> WMON
        amount_in: usdc_for_swap2,  // Use actual balance, not estimate
        slippage_bps: slippage,
        expected_price: buy_price_updated,
    };

    let t_swap2 = std::time::Instant::now();
    let buy_result = execute_swap(
        &provider,
        &provider_with_signer,
        signer_address,
        buy_params,
        gas_price,
        true,  // Skip balance check for arb (optimization: saves ~400ms)
    ).await?;
    println!("  [TIMING] Swap 2 execution: {:?}", t_swap2.elapsed());
    print_swap_report(&buy_result);

    if !buy_result.success {
        return Err(eyre::eyre!("Step 2 failed: Buy swap failed"));
    }

    let wmon_received = buy_result.amount_out_human;

    // ═══════════════════════════════════════════════════════════════════
    // FINAL REPORT
    // ═══════════════════════════════════════════════════════════════════
    let balances_after = get_balances(&provider, signer_address).await?;

    let total_gas_cost = sell_result.gas_cost_wei.to::<u128>() as f64 / 1e18
                       + buy_result.gas_cost_wei.to::<u128>() as f64 / 1e18;

    let gross_profit = wmon_received - amount;
    let profit_bps = (gross_profit / amount * 10000.0) as i32;

    println!("\n═══════════════════════════════════════════════════════════════");
    println!("  DEX-TO-DEX ARBITRAGE RESULT");
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  Route: WMON --({})-> USDC --({})-> WMON", sell_dex, buy_dex);
    println!();
    println!("  INPUT:");
    println!("    WMON In:         {:>12.6} WMON", amount);
    println!();
    println!("  INTERMEDIATE:");
    println!("    USDC Received:   {:>12.6} USDC", usdc_for_swap2);
    println!();
    println!("  OUTPUT:");
    println!("    WMON Out:        {:>12.6} WMON", wmon_received);
    println!();
    println!("  PROFIT/LOSS:");
    let profit_color = if gross_profit >= 0.0 { "32" } else { "31" };
    println!("    Gross P/L:       \x1b[1;{}m{:>+12.6} WMON ({:+}bps)\x1b[0m",
        profit_color, gross_profit, profit_bps);
    println!("    Gas Cost:        {:>12.6} MON", total_gas_cost);
    println!();
    println!("  BALANCES:");
    println!("    WMON Before:     {:>12.6}", balances_before.wmon_human);
    println!("    WMON After:      {:>12.6}", balances_after.wmon_human);
    println!("    MON Before:      {:>12.6}", balances_before.mon_human);
    println!("    MON After:       {:>12.6}", balances_after.mon_human);
    println!();
    println!("  TRANSACTIONS:");
    println!("    Sell TX: {}", sell_result.tx_hash);
    println!("    Buy TX:  {}", buy_result.tx_hash);
    println!();

    if gross_profit > 0.0 {
        println!("  ✅ ARBITRAGE PROFITABLE (before gas)");
    } else {
        println!("  ❌ ARBITRAGE UNPROFITABLE");
    }

    println!();
    println!("  [TIMING] TOTAL ARB EXECUTION: {:?}", arb_start.elapsed());
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}

async fn run_prepare_arb() -> Result<()> {
    use alloy::network::{EthereumWallet, TransactionBuilder};
    use alloy::primitives::{Bytes, U256};
    use alloy::providers::Provider;
    use alloy::sol;
    use alloy::sol_types::SolCall;

    // Monad mainnet chain ID
    const MONAD_CHAIN_ID: u64 = 143;

    // ERC20 approve interface
    sol! {
        #[derive(Debug)]
        function approve(address spender, uint256 amount) external returns (bool);
    }

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let wallet_address = signer.address();
    init_nonce(&provider, wallet_address).await?;

    // Fetch gas price once
    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    let wallet = EthereumWallet::from(signer.clone());
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    println!("══════════════════════════════════════════════════════════════");
    println!("  PREPARING WALLET FOR ARBITRAGE");
    println!("══════════════════════════════════════════════════════════════");
    println!("Wallet: {:?}", wallet_address);

    // Router addresses and names
    let routers = [
        (UNISWAP_SWAP_ROUTER, "Uniswap SwapRouter"),
        (PANCAKE_SMART_ROUTER, "PancakeSwap SmartRouter"),
        (LFJ_LB_ROUTER, "LFJ LBRouter"),
        (MONDAY_SWAP_ROUTER, "Monday SwapRouter"),
    ];

    // Tokens to approve
    let tokens = [
        (WMON_ADDRESS, "WMON"),
        (USDC_ADDRESS, "USDC"),
    ];

    let mut success_count = 0;
    let total_count = routers.len() * tokens.len();

    for (token, token_name) in &tokens {
        println!("\nApproving routers for {}...", token_name);

        for (router, router_name) in &routers {
            let approve_call = approveCall {
                spender: *router,
                amount: U256::MAX,
            };

            // Build transaction with ALL fields set to prevent filler RPC calls
            let tx = alloy::rpc::types::TransactionRequest::default()
                .to(*token)
                .from(wallet_address)
                .input(alloy::rpc::types::TransactionInput::new(Bytes::from(approve_call.abi_encode())))
                .gas_limit(100_000)
                .nonce(nonce::next_nonce())
                .max_fee_per_gas(gas_price + (gas_price / 10))
                .max_priority_fee_per_gas(gas_price / 10)
                .with_chain_id(MONAD_CHAIN_ID);

            match provider_with_signer.send_transaction(tx).await {
                Ok(pending) => {
                    match pending.get_receipt().await {
                        Ok(receipt) => {
                            if receipt.status() {
                                println!("  ✓ {} approved (tx: {:?})", router_name, receipt.transaction_hash);
                                success_count += 1;
                            } else {
                                println!("  ✗ {} approval reverted", router_name);
                            }
                        }
                        Err(e) => {
                            println!("  ✗ {} failed to get receipt: {}", router_name, e);
                        }
                    }
                }
                Err(e) => {
                    println!("  ✗ {} failed to send tx: {}", router_name, e);
                }
            }
        }
    }

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  PREPARATION COMPLETE - {}/{} approvals successful", success_count, total_count);
    println!("══════════════════════════════════════════════════════════════");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::WARN)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Monitor) | None => {
            run_monitor().await
        }
        Some(Commands::TestSwap { dex, amount, direction, slippage }) => {
            run_test_swap(&dex, amount, &direction, slippage).await
        }
        Some(Commands::TestAll { amount, direction, slippage }) => {
            run_test_all(amount, &direction, slippage).await
        }
        // Wallet commands
        Some(Commands::Balance) => {
            run_balance().await
        }
        Some(Commands::Wrap { amount }) => {
            run_wrap(amount).await
        }
        Some(Commands::Unwrap { amount }) => {
            run_unwrap(amount).await
        }
        Some(Commands::BuyMon { amount, dex, slippage, keep_wrapped }) => {
            run_buy_mon(amount, &dex, slippage, keep_wrapped).await
        }
        Some(Commands::SellMon { amount, dex, slippage, use_wmon }) => {
            run_sell_mon(amount, &dex, slippage, use_wmon).await
        }
        Some(Commands::TestArb { sell_dex, buy_dex, amount, slippage }) => {
            run_test_arb(&sell_dex, &buy_dex, amount, slippage).await
        }
        Some(Commands::PrepareArb) => {
            run_prepare_arb().await
        }
    }
}
