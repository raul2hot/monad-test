use alloy::providers::ProviderBuilder;
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
mod pools;
mod price;
mod wallet;

use config::{get_all_pools, get_lfj_pool, get_monday_trade_pool, get_v3_pools, get_router_by_name, POLL_INTERVAL_MS};
use display::{display_prices, init_arb_log};
use execution::{SwapParams, SwapDirection, execute_swap, print_swap_report};
use execution::report::print_comparison_report;
use multicall::fetch_prices_batched;
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
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    println!("Wallet: {:?}", signer.address());

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

    let result = execute_swap(&provider, &signer, params, &rpc_url).await?;
    print_swap_report(&result);

    Ok(())
}

async fn run_test_all(amount: f64, direction: &str, slippage: u32) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    println!("Wallet: {:?}", signer.address());

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

        match execute_swap(&provider, &signer, params, &rpc_url).await {
            Ok(result) => {
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
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    println!("Wallet: {:?}", signer.address());

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

    let swap_result = execute_swap(&provider, &signer, params, &rpc_url).await?;
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
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    println!("Wallet: {:?}", signer.address());

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

    let swap_result = execute_swap(&provider, &signer, params, &rpc_url).await?;
    print_swap_report(&swap_result);

    // Show updated balances
    println!("\nFinal balances:");
    let balances = get_balances(&provider, signer.address()).await?;
    print_balances(&balances);

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
    }
}
