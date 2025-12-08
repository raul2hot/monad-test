use alloy::network::EthereumWallet;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use clap::{Parser, Subcommand};
use eyre::Result;
use reqwest::Client;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::interval;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

// Global HTTP client for connection reuse (Issue 7)
static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

#[allow(dead_code)]
fn get_http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .expect("Failed to create HTTP client")
    })
}

mod config;
mod display;
mod execution;
mod health;
mod mev_validation;
mod multicall;
mod node_config;
mod nonce;
mod pools;
mod price;
mod spread_display;
mod spread_filter;
mod spread_logger;
mod spread_tracker;
mod stats;
mod wallet;

use config::{
    get_all_pools, get_lfj_pool, get_monday_trade_pool, get_v3_pools, get_router_by_name,
    POLL_INTERVAL_MS, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS,
    UNISWAP_SWAP_ROUTER, PANCAKE_SMART_ROUTER, LFJ_LB_ROUTER, MONDAY_SWAP_ROUTER,
    RouterConfig, ATOMIC_ARB_CONTRACT,
};
use health::verify_node_ready;
use node_config::NodeConfig;
use display::{display_prices, init_arb_log, calculate_spreads};
use stats::{
    StatsLogger, ArbExecutionRecord, PreExecutionSnapshot, PostExecutionSnapshot,
    print_pre_execution, print_post_execution,
};
use execution::{SwapParams, SwapDirection, execute_swap, print_swap_report, build_swap_calldata, wait_for_next_block, execute_fast_arb, print_fast_arb_result, execute_atomic_arb, print_atomic_arb_result, query_contract_balances};
use execution::report::print_comparison_report;
use spread_filter::{SpreadFilterConfig, FilterResult};
use spread_tracker::SpreadTracker;
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

    /// Fast DEX-to-DEX arbitrage (optimized <1.5s execution)
    FastArb {
        #[arg(long)]
        sell_dex: String,
        #[arg(long)]
        buy_dex: String,
        #[arg(long, default_value = "1.0")]
        amount: f64,
        #[arg(long, default_value = "200")]
        slippage: u32,
    },

    /// Atomic arbitrage via smart contract (single TX, MEV-resistant)
    AtomicArb {
        #[arg(long)]
        sell_dex: String,
        #[arg(long)]
        buy_dex: String,
        #[arg(long, default_value = "1.0")]
        amount: f64,
        #[arg(long, default_value = "150")]
        slippage: u32,
        #[arg(long, default_value = "0")]
        min_profit_bps: i32,
        /// Force execution even if unprofitable (for testing)
        #[arg(long, default_value = "false")]
        force: bool,
        /// TURBO mode: skip balance queries and gas estimation for <300ms execution
        #[arg(long, default_value = "false")]
        turbo: bool,
    },

    /// Automated arbitrage: monitors prices and executes when opportunity found
    AutoArb {
        /// Minimum net spread in bps to trigger execution (e.g., -50 for testing, 10 for production)
        #[arg(long, default_value = "-100", allow_hyphen_values = true)]
        min_spread_bps: i32,

        /// Amount of WMON per arb execution
        #[arg(long, default_value = "0.1")]
        amount: f64,

        /// Slippage tolerance in bps
        #[arg(long, default_value = "200")]
        slippage: u32,

        /// Maximum executions (0 = unlimited)
        #[arg(long, default_value = "1")]
        max_executions: u32,

        /// Cooldown between executions in seconds
        #[arg(long, default_value = "10")]
        cooldown_secs: u64,

        /// Dry run mode (detect but don't execute)
        #[arg(long, default_value = "false")]
        dry_run: bool,

        /// Force execution even if unprofitable (for testing)
        #[arg(long, default_value = "false")]
        force: bool,

        /// Enable spread velocity tracking (saves last N spreads before trigger)
        #[arg(long, default_value = "false")]
        track_velocity: bool,

        /// Number of spread snapshots to retain in ring buffer
        #[arg(long, default_value = "10")]
        history_size: usize,

        /// Minimum spread velocity (bps/sec) to trigger - 0 disables velocity filter
        #[arg(long, default_value = "0")]
        min_velocity: i32,

        /// Maximum spread velocity (bps/sec) - skip if exceeded (bot signature)
        #[arg(long, default_value = "100")]
        max_velocity: i32,

        /// Minimum final spread (bps) required for margin
        #[arg(long, default_value = "9")]
        min_final_spread: i32,

        /// Maximum baseline spread (bps) - skip if already elevated
        #[arg(long, default_value = "2")]
        max_baseline: i32,
    },

    /// Production arbitrage bot with safety checks
    ProdArb {
        /// Minimum net spread in bps (must be positive for production)
        #[arg(long, default_value = "20")]
        min_spread_bps: i32,

        /// Amount of WMON per arb
        #[arg(long, default_value = "1.0")]
        amount: f64,

        /// Slippage tolerance in bps
        #[arg(long, default_value = "100")]
        slippage: u32,

        /// Max daily loss in WMON (stops bot if exceeded)
        #[arg(long, default_value = "0.5")]
        max_daily_loss: f64,

        /// Max consecutive failures before pause
        #[arg(long, default_value = "3")]
        max_failures: u32,
    },

    /// Fund the atomic arb contract with WMON
    FundContract {
        #[arg(long)]
        amount: f64,
    },

    /// Withdraw WMON from atomic arb contract
    WithdrawContract {
        #[arg(long, default_value = "0")]
        amount: f64,  // 0 = withdraw all
    },

    /// Check atomic arb contract balances
    ContractBalance,

    /// Test transaction revert to measure gas costs
    TestRevert {
        /// DEX to use: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade
        #[arg(long, default_value = "uniswap")]
        dex: String,

        /// Gas limit to use for the transaction
        #[arg(long, default_value = "300000")]
        gas_limit: u64,

        /// Revert method: "zero" (0 amount), "huge" (impossibly large amount), "minout" (impossible min output)
        #[arg(long, default_value = "minout")]
        method: String,
    },

    /// MEV validation - observe block timing and spread persistence (Phase 1)
    MevValidate {
        /// Duration to run validation in seconds
        #[arg(long, default_value = "300")]
        duration: u64,

        /// Minimum spread in bps to consider "actionable" (default: 10)
        #[arg(long, default_value = "10")]
        min_spread: i32,

        /// Output mode: "dashboard" (default), "log", "quiet"
        #[arg(long, default_value = "dashboard")]
        output: String,
    },

    /// Live spread dashboard with detailed visualization
    Dashboard {
        /// Minimum spread to display (bps)
        #[arg(long, default_value = "5")]
        min_spread: i32,

        /// History depth per pair
        #[arg(long, default_value = "30")]
        history: usize,

        /// Refresh rate in milliseconds
        #[arg(long, default_value = "100")]
        refresh_ms: u64,

        /// Enable sound alerts for HOT+ spreads
        #[arg(long, default_value = "false")]
        sound: bool,
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
    use std::io::{stdout, Write};

    // Load node configuration (auto-detects local vs remote)
    let node_config = NodeConfig::from_env();
    node_config.log_config();

    let url: reqwest::Url = node_config.rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    // Verify node health before starting
    verify_node_ready(&provider).await?;

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

    // Initialize spread display with 5bps threshold, 20 history
    let mut spread_display = spread_display::SpreadDisplay::new(5, 20);

    // Check if running in interactive mode
    let interactive = spread_display::is_interactive();

    if interactive {
        // Enter alternate screen buffer for clean display
        spread_display::enter_alternate_screen();
    }

    // Use node-aware polling interval (100ms for local, 1000ms for remote)
    let poll_interval_ms = node_config.poll_interval.as_millis() as u64;
    let mut poll_interval = interval(Duration::from_millis(poll_interval_ms));

    println!("Starting price monitor with {}ms polling interval...\n",
        poll_interval_ms);

    loop {
        poll_interval.tick().await;

        match fetch_prices_batched(&provider, price_calls.clone()).await {
            Ok((prices, _elapsed_ms)) => {
                let spreads = calculate_spreads(&prices);
                spread_display.update(&spreads);

                if interactive {
                    // Move cursor to top and render enhanced display
                    spread_display::cursor_home();
                    print!("{}", spread_display::render_full_dashboard(&spread_display, &prices, None));
                    stdout().flush().ok();
                } else {
                    // Non-interactive: single line update
                    print!("\r{}", spread_display.render_oneline());
                    stdout().flush().ok();
                }
            }
            Err(e) => {
                error!("Failed to fetch prices: {}", e);
                if interactive {
                    spread_display::cursor_home();
                }
                println!("\x1b[1;31mError fetching prices: {}\x1b[0m", e);
                println!("\nRetrying in {} ms...", poll_interval_ms);
            }
        }
    }

    // Note: This code is unreachable in the current infinite loop
    // In production, add Ctrl+C handler to properly exit
    #[allow(unreachable_code)]
    {
        if interactive {
            spread_display::exit_alternate_screen();
        }
        Ok(())
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

/// Test transaction revert to measure exact gas consumption on Monad
async fn run_test_revert(dex: &str, gas_limit: u64, method: &str) -> Result<()> {
    use alloy::network::TransactionBuilder;
    use alloy::primitives::U256;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::time::{timeout, Duration};

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║          REVERT GAS TEST - MONAD GAS ANALYSIS                ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
    println!("⚠️  WARNING: This WILL send a transaction that reverts!");
    println!("   You will pay: gas_limit × gas_price = {} gas units", gas_limit);
    println!();

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;
    println!("Wallet: {:?}", signer_address);

    // Create provider with signer
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Get gas price
    let gas_price = provider.get_gas_price().await.unwrap_or(50_000_000_000); // 50 gwei default
    println!("Gas price: {} gwei", gas_price / 1_000_000_000);

    // Get router config
    let router = get_router_by_name(dex)
        .ok_or_else(|| eyre::eyre!("Unknown DEX: {}. Valid options: uniswap, pancakeswap1, pancakeswap2, lfj, mondaytrade", dex))?;

    println!("DEX: {} ({})", router.name, format!("{:?}", router.router_type));
    println!("Revert method: {}", method);

    // Build calldata designed to revert based on method
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    // Use a small real amount (0.001 WMON) to pass initial checks, but set impossible min_out
    let (amount_in, amount_out_min) = match method {
        "zero" => {
            println!("→ Using 0 amount (should fail input validation)");
            (U256::ZERO, U256::ZERO)
        }
        "huge" => {
            println!("→ Using impossibly large amount (should fail balance check)");
            (U256::from(1_000_000_000_000_000_000_000_000_000_u128), U256::ZERO) // 1 billion WMON
        }
        "minout" | _ => {
            println!("→ Using tiny amount with impossible min_out (should fail slippage)");
            // 0.001 WMON = 1e15 wei, but expect 1 trillion USDC out (impossible)
            (U256::from(1_000_000_000_000_000_u64), U256::MAX / U256::from(2))
        }
    };

    println!();
    println!("Transaction parameters:");
    println!("  amount_in:     {} wei", amount_in);
    println!("  amount_out_min: {} (guaranteed to fail)", if amount_out_min > U256::from(1_000_000_000_000_000_000_u128) { "MAX/2".to_string() } else { format!("{}", amount_out_min) });
    println!("  gas_limit:     {}", gas_limit);

    // Build swap calldata (sell WMON -> USDC)
    let calldata = build_swap_calldata(
        router.router_type,
        WMON_ADDRESS,
        USDC_ADDRESS,
        amount_in,
        amount_out_min,
        signer_address,
        router.pool_fee,
        deadline,
    )?;

    // Build transaction with explicit gas limit (no estimation)
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(router.address)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(calldata))
        .gas_limit(gas_limit)
        .nonce(nonce::next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(143); // Monad mainnet

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  SENDING TRANSACTION (expecting revert)...");
    println!("═══════════════════════════════════════════════════════════════");

    let start = std::time::Instant::now();

    // Send transaction
    let send_result = timeout(Duration::from_secs(30), provider_with_signer.send_transaction(tx)).await;

    match send_result {
        Ok(Ok(pending)) => {
            let tx_hash = *pending.tx_hash();
            println!("  TX Hash: {:?}", tx_hash);
            println!("  Waiting for receipt...");

            // Wait for receipt with polling
            let receipt_result = timeout(Duration::from_secs(60), async {
                loop {
                    if let Ok(Some(receipt)) = provider.get_transaction_receipt(tx_hash).await {
                        return Ok::<_, eyre::Report>(receipt);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }).await;

            match receipt_result {
                Ok(Ok(receipt)) => {
                    let elapsed = start.elapsed();
                    let status = receipt.status();
                    let gas_used = receipt.gas_used;
                    let effective_gas_price = receipt.effective_gas_price;

                    // Calculate costs
                    let gas_cost_actual = U256::from(gas_used) * U256::from(effective_gas_price);
                    let gas_cost_limit = U256::from(gas_limit) * U256::from(effective_gas_price);

                    println!();
                    println!("╔══════════════════════════════════════════════════════════════╗");
                    println!("║                    REVERT GAS ANALYSIS                       ║");
                    println!("╚══════════════════════════════════════════════════════════════╝");
                    println!();
                    println!("  Status:           {} {}",
                        if status { "✓ SUCCESS" } else { "✗ REVERTED" },
                        if !status { "(as expected)" } else { "(unexpected!)" }
                    );
                    println!("  TX Hash:          {:?}", receipt.transaction_hash);
                    println!("  Block:            {:?}", receipt.block_number);
                    println!("  Time:             {:?}", elapsed);
                    println!();
                    println!("  ┌─────────────────────────────────────────────────────────┐");
                    println!("  │                    GAS METRICS                          │");
                    println!("  ├─────────────────────────────────────────────────────────┤");
                    println!("  │  Gas Used:        {:>15}                       │", gas_used);
                    println!("  │  Gas Limit:       {:>15}                       │", gas_limit);
                    println!("  │  Efficiency:      {:>14.1}%                       │", (gas_used as f64 / gas_limit as f64) * 100.0);
                    println!("  │  Gas Price:       {:>12} gwei                    │", effective_gas_price / 1_000_000_000);
                    println!("  └─────────────────────────────────────────────────────────┘");
                    println!();
                    println!("  ┌─────────────────────────────────────────────────────────┐");
                    println!("  │                    COST ANALYSIS                        │");
                    println!("  ├─────────────────────────────────────────────────────────┤");

                    let actual_cost_mon = gas_cost_actual.to::<u128>() as f64 / 1e18;
                    let limit_cost_mon = gas_cost_limit.to::<u128>() as f64 / 1e18;

                    println!("  │  Actual (gas_used × price):                            │");
                    println!("  │    {} wei = {:.8} MON         │", gas_cost_actual, actual_cost_mon);
                    println!("  │                                                         │");
                    println!("  │  MONAD CHARGED (gas_limit × price):                     │");
                    println!("  │    {} wei = {:.8} MON         │", gas_cost_limit, limit_cost_mon);
                    println!("  │                                                         │");
                    println!("  │  Overpayment: {:.8} MON ({:.1}%)                   │",
                        limit_cost_mon - actual_cost_mon,
                        ((gas_limit as f64 - gas_used as f64) / gas_limit as f64) * 100.0
                    );
                    println!("  └─────────────────────────────────────────────────────────┘");
                    println!();

                    if !status {
                        println!("  💡 KEY INSIGHT: On Monad, reverted transactions cost:");
                        println!("     gas_limit × gas_price = {:.8} MON", limit_cost_mon);
                        println!();
                        println!("     The 'gas_used' ({}) shows how far execution got", gas_used);
                        println!("     before the revert, but you paid for the full gas_limit.");
                    }
                }
                Ok(Err(e)) => {
                    println!("  ✗ Error getting receipt: {:?}", e);
                }
                Err(_) => {
                    println!("  ✗ Timeout waiting for receipt (60s)");
                }
            }
        }
        Ok(Err(e)) => {
            // Transaction was rejected (e.g., simulation failed)
            println!();
            println!("  ✗ Transaction rejected by node:");
            println!("    {:?}", e);
            println!();
            println!("  💡 This means the transaction was rejected BEFORE being sent.");
            println!("     No gas was consumed. The node's simulation caught the revert.");
            println!();
            println!("     This is actually useful info: eth_estimateGas / eth_call can");
            println!("     detect reverts before you spend any gas!");
        }
        Err(_) => {
            println!("  ✗ Timeout sending transaction (30s)");
        }
    }

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
    let multiplier = alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(decimals));
    let amount_scaled = (amount * 1e18) as u128;
    alloy::primitives::U256::from(amount_scaled) * multiplier / alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(18u8))
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

    // Get initial balances (WMON and USDC)
    let balances_before = get_balances(&provider, signer_address).await?;
    let usdc_before = balances_before.usdc_human;
    println!("  Starting WMON balance: {:.6}", balances_before.wmon_human);
    println!("  Starting USDC balance: {:.6}", usdc_before);

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
        false,  // CHANGED: Must get actual USDC received for swap 2
    ).await?;
    println!("  [TIMING] Swap 1 execution: {:?}", t_swap1.elapsed());
    print_swap_report(&sell_result);

    if !sell_result.success {
        return Err(eyre::eyre!("Step 1 failed: Sell swap failed"));
    }

    let usdc_estimated = sell_result.amount_out_human;
    println!("  ✓ Estimated received: {:.6} USDC", usdc_estimated);

    // MONAD STATE COMMITMENT - Wait for block then retry balance query
    let ws_url = std::env::var("MONAD_WS_URL")
        .unwrap_or_else(|_| rpc_url.replace("https://", "wss://").replace("http://", "ws://"));
    println!("  ⏳ Waiting for Monad state commitment...");
    let t_block = std::time::Instant::now();

    match wait_for_next_block(&ws_url).await {
        Ok(block_num) => {
            println!("  ✓ Block {} confirmed in {:?}", block_num, t_block.elapsed());
        }
        Err(e) => {
            println!("  ⚠ WebSocket failed ({}), using 1s delay", e);
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
    }

    // Retry balance query up to 3 times with 200ms gaps
    let mut usdc_for_swap2 = 0.0;
    for attempt in 1..=3 {
        match query_usdc_balance(&provider, signer_address).await {
            Ok(actual_balance) => {
                let usdc_received = actual_balance - usdc_before;
                if usdc_received > 0.0001 {
                    let usdc_received = (usdc_received * 1_000_000.0).floor() / 1_000_000.0;
                    let usdc_safe = usdc_received * 0.998;
                    println!("  ✓ USDC received: {:.6} (using {:.6})", usdc_received, usdc_safe);
                    usdc_for_swap2 = usdc_safe;
                    break;
                }
            }
            Err(_) => {}
        }
        if attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    if usdc_for_swap2 < 0.000001 {
        usdc_for_swap2 = usdc_estimated * 0.99;
        println!("  ⚠ Using estimated USDC: {:.6}", usdc_for_swap2);
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

async fn run_fast_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    let total_start = std::time::Instant::now();

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();

    // PARALLEL INIT: gas + nonce + prices
    let (gas_result, nonce_result, prices_result) = tokio::join!(
        provider.get_gas_price(),
        init_nonce(&provider, signer_address),
        get_current_prices(&provider)
    );

    let gas_price = gas_result.unwrap_or(100_000_000_000);
    nonce_result?;
    let prices = prices_result?;

    println!("  [TIMING] Parallel init: {:?}", total_start.elapsed());

    // Create provider with signer
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Get routers
    let sell_router = get_router_by_name(sell_dex)
        .ok_or_else(|| eyre::eyre!("Unknown sell DEX: {}", sell_dex))?;
    let buy_router = get_router_by_name(buy_dex)
        .ok_or_else(|| eyre::eyre!("Unknown buy DEX: {}", buy_dex))?;

    // Get prices
    let sell_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == sell_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", sell_dex))?.price;
    let buy_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", buy_dex))?.price;

    // Pre-validate profitability before execution
    let spread_bps = ((sell_price - buy_price) / buy_price * 10000.0) as i32;
    let total_fee_bps = (sell_router.pool_fee / 100 + buy_router.pool_fee / 100) as i32;
    let net_spread_bps = spread_bps - total_fee_bps;

    println!("  Spread: {} bps | Fees: {} bps | Net: {} bps",
             spread_bps, total_fee_bps, net_spread_bps);

    if net_spread_bps < 0 {
        println!("\n  ⚠️  Warning: Negative net spread. Arb may be unprofitable.");
    }

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  FAST ARB | {} -> {}", sell_dex, buy_dex);
    println!("══════════════════════════════════════════════════════════════");

    let result = execute_fast_arb(
        &provider_with_signer,
        signer_address,
        &sell_router,
        &buy_router,
        amount,
        sell_price,
        buy_price,
        slippage,
        gas_price,
    ).await?;

    print_fast_arb_result(&result, sell_dex, buy_dex);
    println!("  [TIMING] TOTAL: {:?}", total_start.elapsed());

    Ok(())
}

async fn run_atomic_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32, min_profit_bps: i32, force: bool) -> Result<()> {
    let total_start = std::time::Instant::now();

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();

    // Parallel init
    let (gas_result, nonce_result, prices_result) = tokio::join!(
        provider.get_gas_price(),
        init_nonce(&provider, signer_address),
        get_current_prices(&provider)
    );

    let gas_price = gas_result.unwrap_or(100_000_000_000);
    nonce_result?;
    let prices = prices_result?;

    println!("  [TIMING] Init: {:?}", total_start.elapsed());

    // Create provider with signer
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Get routers
    let sell_router = get_router_by_name(sell_dex)
        .ok_or_else(|| eyre::eyre!("Unknown sell DEX: {}", sell_dex))?;
    let buy_router = get_router_by_name(buy_dex)
        .ok_or_else(|| eyre::eyre!("Unknown buy DEX: {}", buy_dex))?;

    // Get prices
    let sell_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == sell_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", sell_dex))?.price;
    let buy_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", buy_dex))?.price;

    println!("\n==============================================================");
    println!("  ATOMIC ARB | {} -> {} (single TX)", sell_dex, buy_dex);
    println!("==============================================================");

    let result = execute_atomic_arb(
        &provider_with_signer,
        signer_address,
        &sell_router,
        &buy_router,
        amount,
        sell_price,
        buy_price,
        slippage,
        min_profit_bps,
        gas_price,
        force,
    ).await?;

    print_atomic_arb_result(&result);
    println!("  [TIMING] TOTAL: {:?}", total_start.elapsed());

    Ok(())
}

/// TURBO MODE: Execute atomic arb with MAXIMUM speed (fire-and-forget)
/// Target: <50ms total execution
/// - Uses hardcoded gas price (no RPC)
/// - Parallel nonce + price fetch only
/// - No receipt waiting
async fn run_atomic_arb_turbo(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    let total_start = std::time::Instant::now();

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();

    // TURBO: Hardcoded gas price - skip RPC call
    let gas_price: u128 = 52_000_000_000; // 52 gwei - slightly above base

    // PARALLEL: Only nonce + prices (gas price is hardcoded)
    let (nonce_result, prices_result) = tokio::join!(
        init_nonce(&provider, signer_address),
        get_current_prices(&provider)
    );
    nonce_result?;
    let prices = prices_result?;

    let init_time = total_start.elapsed();

    // Create provider with signer
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Get routers (pure computation)
    let sell_router = get_router_by_name(sell_dex)
        .ok_or_else(|| eyre::eyre!("Unknown sell DEX: {}", sell_dex))?;
    let buy_router = get_router_by_name(buy_dex)
        .ok_or_else(|| eyre::eyre!("Unknown buy DEX: {}", buy_dex))?;

    // Get prices (pure lookup)
    let sell_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == sell_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", sell_dex))?.price;
    let buy_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("No price for {}", buy_dex))?.price;

    // Execute TURBO (fire-and-forget)
    let exec_start = std::time::Instant::now();
    let result = execution::atomic_arb::execute_atomic_arb_turbo(
        &provider_with_signer,
        signer_address,
        &sell_router,
        &buy_router,
        amount,
        sell_price,
        buy_price,
        slippage,
        gas_price,
    ).await?;

    let total_time = total_start.elapsed();

    // Minimal output for speed
    println!("TURBO | {} -> {} | TX: {} | Init: {:?} | Exec: {:?} | TOTAL: {:?}",
        sell_dex, buy_dex,
        &result.tx_hash[0..18],
        init_time,
        exec_start.elapsed(),
        total_time
    );

    Ok(())
}

/// Automated arbitrage: monitors and executes when spread opportunity detected
async fn run_auto_arb(
    min_spread_bps: i32,
    amount: f64,
    slippage: u32,
    max_executions: u32,
    cooldown_secs: u64,
    dry_run: bool,
    force: bool,
    track_velocity: bool,
    history_size: usize,
    min_velocity: i32,
    max_velocity: i32,
    min_final_spread: i32,
    max_baseline: i32,
) -> Result<()> {
    use chrono::Local;

    // Load node configuration (auto-detects local vs remote)
    let node_config = NodeConfig::from_env();
    node_config.log_config();

    let url: reqwest::Url = node_config.rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    // Verify node health before starting
    verify_node_ready(&provider).await?;

    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");
    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();

    // Initialize nonce
    init_nonce(&provider, signer_address).await?;

    // Create provider with signer (reused for all executions)
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Initialize stats logger
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let stats_file = format!("arb_stats_{}.jsonl", timestamp);
    let mut stats_logger = StatsLogger::new(&stats_file);

    // Initialize spread tracker for velocity analysis
    let mut spread_tracker = if track_velocity {
        Some(SpreadTracker::new(history_size))
    } else {
        None
    };

    // Get polling interval from node config (50ms local, 1000ms remote)
    let poll_interval_ms = node_config.poll_interval.as_millis() as u64;

    println!("═══════════════════════════════════════════════════════════════");
    println!("  AUTO-ARB BOT STARTED");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Wallet:          {:?}", signer_address);
    println!("  Min Spread:      {} bps", min_spread_bps);
    println!("  Amount per arb:  {} WMON", amount);
    println!("  Slippage:        {} bps", slippage);
    println!("  Max executions:  {}", if max_executions == 0 { "unlimited".to_string() } else { max_executions.to_string() });
    println!("  Cooldown:        {} seconds", cooldown_secs);
    println!("  Poll interval:   {} ms {}", poll_interval_ms, if node_config.is_local { "(local node optimized)" } else { "" });
    println!("  Receipt poll:    {} ms", node_config.receipt_poll_interval.as_millis());
    println!("  Dry run:         {}", dry_run);
    println!("  Stats file:      {}", stats_file);
    if track_velocity {
        println!("  Velocity track:  enabled (history: {})", history_size);
        println!("  Filter config:");
        println!("    min_velocity:     {} bps/sec", min_velocity);
        println!("    max_velocity:     {} bps/sec", max_velocity);
        println!("    min_final_spread: {} bps", min_final_spread);
        println!("    max_baseline:     {} bps", max_baseline);
    }
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Show initial contract balances
    let (initial_wmon, initial_usdc) = query_contract_balances(&provider).await?;
    println!("  Contract Balances:");
    println!("    WMON: {:>18.6}", initial_wmon);
    println!("    USDC: {:>18.6}", initial_usdc);
    println!();

    let mut execution_count = 0u32;
    let mut last_execution = std::time::Instant::now() - std::time::Duration::from_secs(cooldown_secs);
    let mut poll_interval = tokio::time::interval(Duration::from_millis(poll_interval_ms));

    // Initialize enhanced spread display for better visualization
    let mut arb_spread_display = spread_display::SpreadDisplay::new(min_spread_bps, history_size);
    let mut cumulative_pnl: f64 = 0.0;

    loop {
        poll_interval.tick().await;

        // Check if we've hit max executions
        if max_executions > 0 && execution_count >= max_executions {
            println!("\n  Reached max executions ({}). Stopping.", max_executions);
            break;
        }

        // Fetch current prices
        let prices = match get_current_prices(&provider).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  Price fetch error: {}", e);
                continue;
            }
        };

        // Calculate spreads
        let spreads = calculate_spreads(&prices);

        // Find best opportunity (first one is best due to sorting)
        let best_spread = spreads.first();

        if let Some(spread) = best_spread {
            // Record spread for velocity tracking (no extra latency - uses existing data)
            if let Some(ref mut tracker) = spread_tracker {
                tracker.record(
                    &spread.buy_pool,
                    &spread.sell_pool,
                    spread.buy_price,
                    spread.sell_price,
                    (spread.gross_spread_pct * 100.0) as i32,
                    (spread.net_spread_pct * 100.0) as i32,
                );
            }

            // Update enhanced spread display
            arb_spread_display.update(&spreads);

            // Display current best opportunity with enhanced visualization
            let net_bps = (spread.net_spread_pct * 100.0) as i32;
            let level = spread_display::SpreadLevel::from_bps(net_bps);
            let pair_key = format!("{}→{}", spread.buy_pool, spread.sell_pool);
            let trend = arb_spread_display.pair_histories
                .get(&pair_key)
                .map(|h| h.trend())
                .unwrap_or(spread_display::Trend::Stable);

            let now = Local::now().format("%H:%M:%S");
            print!("\r\x1b[2K");
            print!("[{}] ", now);
            print!("{}{:<20}\x1b[0m ", level.color_code(),
                format!("{}→{}", spread.buy_pool, spread.sell_pool));
            print!("{:>+6}bps ", net_bps);
            print!("{}{}\x1b[0m ", trend.color(), trend.arrow());
            print!("{}{:>6}\x1b[0m ", level.color_code(), level.label());

            // Show sparkline if we have history
            if let Some(hist) = arb_spread_display.pair_histories.get(&pair_key) {
                print!("[{}] ", hist.sparkline());
            }

            // Show P&L if tracking
            print!("P&L: {:>+.4} WMON ", cumulative_pnl);

            std::io::Write::flush(&mut std::io::stdout()).ok();

            let net_spread_bps = (spread.net_spread_pct * 100.0) as i32;

            // Check if spread meets threshold and cooldown has passed
            let cooldown_elapsed = last_execution.elapsed().as_secs() >= cooldown_secs;

            if net_spread_bps >= min_spread_bps && cooldown_elapsed {
                println!();  // New line after the \r print
                println!("\n  OPPORTUNITY DETECTED! Net spread: {} bps (threshold: {} bps)",
                    net_spread_bps, min_spread_bps);

                // Analyze spread velocity before execution
                let velocity_analysis = spread_tracker.as_ref().and_then(|t| t.analyze());

                if let Some(ref analysis) = velocity_analysis {
                    println!("\n  SPREAD VELOCITY ANALYSIS:");
                    println!("    History: {}", spread_tracker.as_ref().unwrap().format_history());
                    println!("    Velocity: {:.2} bps/sec", analysis.velocity_bps_per_sec);
                    println!("    Acceleration: {:.2} bps/sec^2", analysis.acceleration);
                    println!("    Pattern: {}", if analysis.is_spike { "SPIKE" } else { "GRADUAL" });
                    println!("    Window: {} ms", analysis.window_duration_ms);
                    println!("    Range: {} to {} bps", analysis.min_spread_in_window, analysis.max_spread_in_window);
                }

                // Apply smart spread filter if velocity tracking enabled
                if track_velocity {
                    if let Some(ref analysis) = velocity_analysis {
                        let filter = SpreadFilterConfig {
                            min_velocity: min_velocity as f64,
                            max_velocity: max_velocity as f64,
                            min_final_spread,
                            max_baseline,
                        };

                        match filter.evaluate(analysis) {
                            FilterResult::Execute => {
                                println!("    FILTER: PASS - executing arb");
                            }
                            FilterResult::Skip { reason } => {
                                println!("    FILTER: SKIP - {}", reason);
                                continue;
                            }
                        }
                    }
                }

                // Get routers for the opportunity
                let sell_router = match get_router_by_name(&spread.sell_pool) {
                    Some(r) => r,
                    None => {
                        eprintln!("  Router not found for {}", spread.sell_pool);
                        continue;
                    }
                };
                let buy_router = match get_router_by_name(&spread.buy_pool) {
                    Some(r) => r,
                    None => {
                        eprintln!("  Router not found for {}", spread.buy_pool);
                        continue;
                    }
                };

                // Get current contract balances (pre-execution)
                let (contract_wmon_before, contract_usdc_before) = query_contract_balances(&provider).await?;

                // Check if contract has enough WMON
                if contract_wmon_before < amount {
                    println!("  Insufficient contract WMON. Have: {:.6}, Need: {:.6}",
                        contract_wmon_before, amount);
                    continue;
                }

                // Calculate expected amounts
                let expected_usdc = amount * spread.sell_price;
                let expected_wmon_back = expected_usdc / spread.buy_price;

                // Create pre-execution snapshot (using contract balances)
                let pre_snapshot = PreExecutionSnapshot {
                    timestamp: Local::now().to_rfc3339(),
                    wmon_balance: contract_wmon_before,
                    usdc_balance: contract_usdc_before,
                    mon_balance: 0.0, // Contract doesn't hold native MON
                    sell_dex: spread.sell_pool.clone(),
                    sell_price: spread.sell_price,
                    buy_dex: spread.buy_pool.clone(),
                    buy_price: spread.buy_price,
                    gross_spread_bps: (spread.gross_spread_pct * 100.0) as i32,
                    net_spread_bps,
                    amount_wmon: amount,
                    expected_usdc,
                    expected_wmon_back,
                    slippage_bps: slippage,
                    // Velocity tracking data (optional)
                    spread_history: velocity_analysis.as_ref().map(|a| a.snapshots.clone()),
                    velocity_bps_per_sec: velocity_analysis.as_ref().map(|a| a.velocity_bps_per_sec),
                    acceleration: velocity_analysis.as_ref().map(|a| a.acceleration),
                    is_spike_pattern: velocity_analysis.as_ref().map(|a| a.is_spike),
                };

                print_pre_execution(&pre_snapshot);

                if dry_run {
                    println!("\n  [DRY RUN] Would execute arb but dry_run=true. Skipping.");

                    // Log dry run
                    let record = ArbExecutionRecord {
                        id: stats_logger.next_id(),
                        pre: pre_snapshot,
                        post: None,
                        success: false,
                        error: Some("Dry run - execution skipped".to_string()),
                    };
                    stats_logger.log_execution(&record);

                    last_execution = std::time::Instant::now();
                    execution_count += 1;
                    continue;
                }

                // Fetch gas price
                let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

                // Fix 6: Re-check prices before execution to avoid stale spread
                println!("  Re-checking prices before execution...");
                let fresh_prices = match get_current_prices(&provider).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("  Price recheck failed: {}. Skipping execution.", e);
                        continue;
                    }
                };
                let fresh_spreads = calculate_spreads(&fresh_prices);

                // Find the same pair in fresh spreads
                let fresh_spread = fresh_spreads.iter().find(|s| {
                    s.sell_pool == spread.sell_pool && s.buy_pool == spread.buy_pool
                });

                if let Some(fs) = fresh_spread {
                    let fresh_spread_bps = (fs.net_spread_pct * 100.0) as i32;
                    if fresh_spread_bps < min_spread_bps {
                        println!("  Spread evaporated! Was {} bps, now {} bps. Skipping.",
                            net_spread_bps, fresh_spread_bps);
                        continue;
                    }
                    println!("  Fresh spread: {} bps (still above threshold)", fresh_spread_bps);
                } else {
                    println!("  WARNING: Could not find matching spread in fresh prices. Proceeding with caution.");
                }

                // Execute arb - use atomic if contract is deployed, otherwise fast_arb
                println!("\n  EXECUTING ARB...");
                let exec_start = std::time::Instant::now();

                // Use atomic arb if contract is deployed, otherwise fall back to fast_arb
                let use_atomic = ATOMIC_ARB_CONTRACT != alloy::primitives::Address::ZERO;

                let arb_result: Result<execution::FastArbResult, eyre::Report> = if use_atomic {
                    println!("  Using ATOMIC execution (single TX)...");
                    match execute_atomic_arb(
                        &provider_with_signer,
                        signer_address,
                        &sell_router,
                        &buy_router,
                        amount,
                        spread.sell_price,
                        spread.buy_price,
                        slippage,
                        0, // min_profit_bps = 0 (any profit)
                        gas_price,
                        force, // force execution even if unprofitable
                    ).await {
                        Ok(result) => {
                            print_atomic_arb_result(&result);
                            // Convert AtomicArbResult to FastArbResult for stats compatibility
                            Ok(execution::FastArbResult {
                                success: result.success,
                                swap1_success: result.success,
                                swap1_tx_hash: result.tx_hash.clone(),
                                swap1_gas_used: result.gas_used,
                                swap1_gas_estimated: result.gas_limit,
                                swap2_success: result.success,
                                swap2_tx_hash: String::new(), // Atomic has single TX
                                swap2_gas_used: 0,
                                swap2_gas_estimated: 0,
                                wmon_in: result.wmon_in,
                                usdc_intermediate: 0.0,
                                wmon_out: result.wmon_in + result.profit_wmon,
                                usdc_before: 0.0,
                                usdc_after_swap1: 0.0,
                                wmon_before: result.wmon_in,
                                wmon_after_swap2: result.wmon_in + result.profit_wmon,
                                actual_usdc_received: 0.0,
                                actual_wmon_received: result.profit_wmon,
                                swap1_slippage_bps: 0,
                                swap2_slippage_bps: 0,
                                wmon_out_actual: Some(result.wmon_in + result.profit_wmon),
                                estimation_error_bps: None,
                                gross_profit_wmon: result.profit_wmon,
                                profit_bps: result.profit_bps,
                                total_gas_cost_wei: alloy::primitives::U256::ZERO,
                                total_gas_cost_mon: result.gas_cost_mon,
                                total_gas_used: result.gas_used,
                                total_gas_estimated: result.gas_limit,
                                total_time_ms: result.execution_time_ms,
                                swap1_time_ms: result.execution_time_ms,
                                swap2_time_ms: 0,
                                execution_time_ms: result.execution_time_ms,
                                error: result.error,
                            })
                        }
                        Err(e) => Err(e)
                    }
                } else {
                    println!("  Using FAST execution (2 TXs) - deploy atomic contract for better results!");
                    execute_fast_arb(
                        &provider_with_signer,
                        signer_address,
                        &sell_router,
                        &buy_router,
                        amount,
                        spread.sell_price,
                        spread.buy_price,
                        slippage,
                        gas_price,
                    ).await
                };

                let exec_time = exec_start.elapsed().as_millis();

                // Get post-execution contract balances
                let (contract_wmon_after, contract_usdc_after) = query_contract_balances(&provider).await?;

                // Create post-execution snapshot (using contract balances)
                let post_snapshot = match &arb_result {
                    Ok(result) => {
                        let wmon_delta = contract_wmon_after - contract_wmon_before;
                        let usdc_delta = contract_usdc_after - contract_usdc_before;
                        let mon_delta = 0.0; // Contract doesn't hold native MON
                        let net_profit_bps = if amount > 0.0 {
                            (wmon_delta / amount * 10000.0) as i32
                        } else {
                            0
                        };

                        PostExecutionSnapshot {
                            timestamp: Local::now().to_rfc3339(),
                            wmon_balance: contract_wmon_after,
                            usdc_balance: contract_usdc_after,
                            mon_balance: 0.0, // Contract doesn't hold native MON
                            swap1_success: result.swap1_success,
                            swap1_tx_hash: result.swap1_tx_hash.clone(),
                            swap1_gas_used: result.swap1_gas_used,
                            swap1_gas_estimated: result.swap1_gas_estimated,
                            swap2_success: result.swap2_success,
                            swap2_tx_hash: result.swap2_tx_hash.clone(),
                            swap2_gas_used: result.swap2_gas_used,
                            swap2_gas_estimated: result.swap2_gas_estimated,
                            actual_usdc_received: result.usdc_intermediate,
                            actual_wmon_back: result.wmon_out,
                            wmon_delta,
                            usdc_delta,
                            mon_delta,
                            total_gas_cost_mon: result.total_gas_cost_mon,
                            net_profit_wmon: wmon_delta,
                            net_profit_bps,
                            total_execution_ms: exec_time,
                        }
                    }
                    Err(_e) => {
                        // Failed execution - still record balances
                        PostExecutionSnapshot {
                            timestamp: Local::now().to_rfc3339(),
                            wmon_balance: contract_wmon_after,
                            usdc_balance: contract_usdc_after,
                            mon_balance: 0.0, // Contract doesn't hold native MON
                            swap1_success: false,
                            swap1_tx_hash: String::new(),
                            swap1_gas_used: 0,
                            swap1_gas_estimated: 0,
                            swap2_success: false,
                            swap2_tx_hash: String::new(),
                            swap2_gas_used: 0,
                            swap2_gas_estimated: 0,
                            actual_usdc_received: 0.0,
                            actual_wmon_back: 0.0,
                            wmon_delta: contract_wmon_after - contract_wmon_before,
                            usdc_delta: contract_usdc_after - contract_usdc_before,
                            mon_delta: 0.0, // Contract doesn't hold native MON
                            total_gas_cost_mon: 0.0,
                            net_profit_wmon: 0.0,
                            net_profit_bps: 0,
                            total_execution_ms: exec_time,
                        }
                    }
                };

                print_post_execution(&pre_snapshot, &post_snapshot);

                // Log execution record
                let record = ArbExecutionRecord {
                    id: stats_logger.next_id(),
                    pre: pre_snapshot,
                    post: Some(post_snapshot),
                    success: arb_result.as_ref().map(|r| r.success).unwrap_or(false),
                    error: arb_result.as_ref().err().map(|e| e.to_string()),
                };
                stats_logger.log_execution(&record);

                // Print result summary
                if let Ok(result) = &arb_result {
                    print_fast_arb_result(result, &spread.sell_pool, &spread.buy_pool);
                } else if let Err(e) = &arb_result {
                    println!("\n  ARB EXECUTION FAILED: {}", e);
                }

                last_execution = std::time::Instant::now();
                execution_count += 1;

                println!("\n  Executions: {} / {}",
                    execution_count,
                    if max_executions == 0 { "unlimited".to_string() } else { max_executions.to_string() }
                );
                println!("  Cooldown: {} seconds before next execution...\n", cooldown_secs);
            }
        }
    }

    // Final summary
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("  AUTO-ARB SESSION COMPLETE");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Total executions: {}", execution_count);
    println!("  Stats saved to:   {}", stats_file);

    let (final_wmon, final_usdc) = query_contract_balances(&provider).await?;
    println!("\n  Final Contract Balances:");
    println!("    WMON: {:>18.6} (Delta {:>+.6})", final_wmon, final_wmon - initial_wmon);
    println!("    USDC: {:>18.6} (Delta {:>+.6})", final_usdc, final_usdc - initial_usdc);
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}

/// Production arbitrage bot with safety checks
async fn run_prod_arb(
    min_spread_bps: i32,
    amount: f64,
    slippage: u32,
    max_daily_loss: f64,
    max_failures: u32,
) -> Result<()> {
    use chrono::Local;

    // Safety check: enforce positive spread for production
    if min_spread_bps <= 0 {
        return Err(eyre::eyre!(
            "Production mode requires positive min_spread_bps. Got: {}. Use --min-spread-bps with a positive value.",
            min_spread_bps
        ));
    }

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();

    // Initialize nonce
    init_nonce(&provider, signer_address).await?;

    // Create provider with signer (reused for all executions)
    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    // Initialize stats logger
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let stats_file = format!("prod_arb_stats_{}.jsonl", timestamp);
    let mut stats_logger = StatsLogger::new(&stats_file);

    println!("═══════════════════════════════════════════════════════════════");
    println!("  PRODUCTION ARB BOT STARTED");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Wallet:          {:?}", signer_address);
    println!("  Min Spread:      {} bps (ENFORCED POSITIVE)", min_spread_bps);
    println!("  Amount per arb:  {} WMON", amount);
    println!("  Slippage:        {} bps", slippage);
    println!("  Max daily loss:  {} WMON", max_daily_loss);
    println!("  Max failures:    {}", max_failures);
    println!("  Stats file:      {}", stats_file);
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Show initial contract balances
    let (initial_wmon, initial_usdc) = query_contract_balances(&provider).await?;
    println!("  Contract Balances:");
    println!("    WMON: {:>18.6}", initial_wmon);
    println!("    USDC: {:>18.6}", initial_usdc);
    println!();

    let mut execution_count = 0u32;
    let mut successful_arbs = 0u32;
    let mut consecutive_failures = 0u32;
    let mut cumulative_pnl: f64 = 0.0;
    let mut poll_interval = tokio::time::interval(Duration::from_millis(POLL_INTERVAL_MS));
    let cooldown_secs: u64 = 10; // Fixed cooldown for production
    let mut last_execution = std::time::Instant::now() - std::time::Duration::from_secs(cooldown_secs);

    loop {
        poll_interval.tick().await;

        // Safety check: stop if cumulative loss exceeds threshold
        if cumulative_pnl < -max_daily_loss {
            println!("\n  MAX DAILY LOSS EXCEEDED ({:.6} WMON). Stopping.", cumulative_pnl);
            break;
        }

        // Safety check: pause if too many consecutive failures
        if consecutive_failures >= max_failures {
            println!("\n  {} consecutive failures. Pausing for 60 seconds...", consecutive_failures);
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            consecutive_failures = 0;
            continue;
        }

        // Fetch current prices
        let prices = match get_current_prices(&provider).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  Price fetch error: {}", e);
                continue;
            }
        };

        // Calculate spreads
        let spreads = calculate_spreads(&prices);

        // Find best opportunity (first one is best due to sorting)
        let best_spread = spreads.first();

        if let Some(spread) = best_spread {
            // Display current best opportunity
            let now = Local::now().format("%H:%M:%S");
            print!("\r[{}] Best: {} -> {} | Net: {:+.2}% | P&L: {:+.6} WMON    ",
                now,
                spread.buy_pool,
                spread.sell_pool,
                spread.net_spread_pct,
                cumulative_pnl
            );
            std::io::Write::flush(&mut std::io::stdout()).ok();

            let net_spread_bps = (spread.net_spread_pct * 100.0) as i32;

            // Check if spread meets threshold and cooldown has passed
            let cooldown_elapsed = last_execution.elapsed().as_secs() >= cooldown_secs;

            if net_spread_bps >= min_spread_bps && cooldown_elapsed {
                println!();
                println!("\n  PROFITABLE OPPORTUNITY! Net spread: {} bps (threshold: {} bps)",
                    net_spread_bps, min_spread_bps);

                // Get routers for the opportunity
                let sell_router = match get_router_by_name(&spread.sell_pool) {
                    Some(r) => r,
                    None => {
                        eprintln!("  Router not found for {}", spread.sell_pool);
                        continue;
                    }
                };
                let buy_router = match get_router_by_name(&spread.buy_pool) {
                    Some(r) => r,
                    None => {
                        eprintln!("  Router not found for {}", spread.buy_pool);
                        continue;
                    }
                };

                // Get current contract balances (pre-execution)
                let (contract_wmon_before, contract_usdc_before) = query_contract_balances(&provider).await?;

                // Check if contract has enough WMON
                if contract_wmon_before < amount {
                    println!("  Insufficient contract WMON. Have: {:.6}, Need: {:.6}",
                        contract_wmon_before, amount);
                    continue;
                }

                // Calculate expected amounts
                let expected_usdc = amount * spread.sell_price;
                let expected_wmon_back = expected_usdc / spread.buy_price;

                // Create pre-execution snapshot (using contract balances)
                let pre_snapshot = PreExecutionSnapshot {
                    timestamp: Local::now().to_rfc3339(),
                    wmon_balance: contract_wmon_before,
                    usdc_balance: contract_usdc_before,
                    mon_balance: 0.0, // Contract doesn't hold native MON
                    sell_dex: spread.sell_pool.clone(),
                    sell_price: spread.sell_price,
                    buy_dex: spread.buy_pool.clone(),
                    buy_price: spread.buy_price,
                    gross_spread_bps: (spread.gross_spread_pct * 100.0) as i32,
                    net_spread_bps,
                    amount_wmon: amount,
                    expected_usdc,
                    expected_wmon_back,
                    slippage_bps: slippage,
                    // Velocity tracking not enabled for ProdArb
                    spread_history: None,
                    velocity_bps_per_sec: None,
                    acceleration: None,
                    is_spike_pattern: None,
                };

                print_pre_execution(&pre_snapshot);

                // Fetch gas price
                let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

                // Execute fast arb
                println!("\n  EXECUTING PRODUCTION ARB...");
                let exec_start = std::time::Instant::now();

                let arb_result = execute_fast_arb(
                    &provider_with_signer,
                    signer_address,
                    &sell_router,
                    &buy_router,
                    amount,
                    spread.sell_price,
                    spread.buy_price,
                    slippage,
                    gas_price,
                ).await;

                let exec_time = exec_start.elapsed().as_millis();

                // Get post-execution contract balances
                let (contract_wmon_after, contract_usdc_after) = query_contract_balances(&provider).await?;

                let wmon_delta = contract_wmon_after - contract_wmon_before;

                // Create post-execution snapshot (using contract balances)
                let post_snapshot = match &arb_result {
                    Ok(result) => {
                        let usdc_delta = contract_usdc_after - contract_usdc_before;
                        let mon_delta = 0.0; // Contract doesn't hold native MON
                        let net_profit_bps = if amount > 0.0 {
                            (wmon_delta / amount * 10000.0) as i32
                        } else {
                            0
                        };

                        PostExecutionSnapshot {
                            timestamp: Local::now().to_rfc3339(),
                            wmon_balance: contract_wmon_after,
                            usdc_balance: contract_usdc_after,
                            mon_balance: 0.0, // Contract doesn't hold native MON
                            swap1_success: result.swap1_success,
                            swap1_tx_hash: result.swap1_tx_hash.clone(),
                            swap1_gas_used: result.swap1_gas_used,
                            swap1_gas_estimated: result.swap1_gas_estimated,
                            swap2_success: result.swap2_success,
                            swap2_tx_hash: result.swap2_tx_hash.clone(),
                            swap2_gas_used: result.swap2_gas_used,
                            swap2_gas_estimated: result.swap2_gas_estimated,
                            actual_usdc_received: result.usdc_intermediate,
                            actual_wmon_back: result.wmon_out,
                            wmon_delta,
                            usdc_delta,
                            mon_delta,
                            total_gas_cost_mon: result.total_gas_cost_mon,
                            net_profit_wmon: wmon_delta,
                            net_profit_bps,
                            total_execution_ms: exec_time,
                        }
                    }
                    Err(_e) => {
                        PostExecutionSnapshot {
                            timestamp: Local::now().to_rfc3339(),
                            wmon_balance: contract_wmon_after,
                            usdc_balance: contract_usdc_after,
                            mon_balance: 0.0, // Contract doesn't hold native MON
                            swap1_success: false,
                            swap1_tx_hash: String::new(),
                            swap1_gas_used: 0,
                            swap1_gas_estimated: 0,
                            swap2_success: false,
                            swap2_tx_hash: String::new(),
                            swap2_gas_used: 0,
                            swap2_gas_estimated: 0,
                            actual_usdc_received: 0.0,
                            actual_wmon_back: 0.0,
                            wmon_delta,
                            usdc_delta: contract_usdc_after - contract_usdc_before,
                            mon_delta: 0.0, // Contract doesn't hold native MON
                            total_gas_cost_mon: 0.0,
                            net_profit_wmon: 0.0,
                            net_profit_bps: 0,
                            total_execution_ms: exec_time,
                        }
                    }
                };

                print_post_execution(&pre_snapshot, &post_snapshot);

                // Update cumulative P&L
                cumulative_pnl += wmon_delta;

                // Log execution record
                let record = ArbExecutionRecord {
                    id: stats_logger.next_id(),
                    pre: pre_snapshot,
                    post: Some(post_snapshot),
                    success: arb_result.as_ref().map(|r| r.success).unwrap_or(false),
                    error: arb_result.as_ref().err().map(|e| e.to_string()),
                };
                stats_logger.log_execution(&record);

                // Update counters
                if let Ok(result) = &arb_result {
                    if result.success && wmon_delta > 0.0 {
                        successful_arbs += 1;
                        consecutive_failures = 0;
                        print_fast_arb_result(result, &spread.sell_pool, &spread.buy_pool);
                    } else {
                        consecutive_failures += 1;
                    }
                } else if let Err(e) = &arb_result {
                    consecutive_failures += 1;
                    println!("\n  ARB EXECUTION FAILED: {}", e);
                }

                last_execution = std::time::Instant::now();
                execution_count += 1;

                let win_rate = if execution_count > 0 {
                    (successful_arbs as f64 / execution_count as f64) * 100.0
                } else {
                    0.0
                };

                println!("\n  PRODUCTION STATS:");
                println!("    Executions:    {}", execution_count);
                println!("    Successful:    {} ({:.1}% win rate)", successful_arbs, win_rate);
                println!("    Cumulative P&L: {:+.6} WMON", cumulative_pnl);
                println!("    Failures:      {} consecutive", consecutive_failures);
                println!("  Cooldown: {} seconds...\n", cooldown_secs);
            }
        }
    }

    // Final summary
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("  PRODUCTION ARB SESSION COMPLETE");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Total executions:  {}", execution_count);
    println!("  Successful arbs:   {}", successful_arbs);
    println!("  Win rate:          {:.1}%", if execution_count > 0 {
        (successful_arbs as f64 / execution_count as f64) * 100.0
    } else { 0.0 });
    println!("  Cumulative P&L:    {:+.6} WMON", cumulative_pnl);
    println!("  Stats saved to:    {}", stats_file);

    let (final_wmon, final_usdc) = query_contract_balances(&provider).await?;
    println!("\n  Final Contract Balances:");
    println!("    WMON: {:>18.6} (Delta {:>+.6})", final_wmon, final_wmon - initial_wmon);
    println!("    USDC: {:>18.6} (Delta {:>+.6})", final_usdc, final_usdc - initial_usdc);
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}

async fn run_fund_contract(amount: f64) -> Result<()> {
    use alloy::sol;
    use alloy::sol_types::SolCall;
    use alloy::network::TransactionBuilder;

    sol! {
        function transfer(address to, uint256 amount) external returns (bool);
    }

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;

    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    let amount_wei = to_wei(amount, WMON_DECIMALS);

    let transfer_call = transferCall {
        to: ATOMIC_ARB_CONTRACT,
        amount: amount_wei,
    };

    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(WMON_ADDRESS)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(
            alloy::primitives::Bytes::from(transfer_call.abi_encode())
        ))
        .gas_limit(100_000)
        .nonce(nonce::next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(143);

    println!("Funding contract with {} WMON...", amount);

    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;

    if receipt.status() {
        println!("  Funded contract with {} WMON", amount);
        println!("  TX: {:?}", receipt.transaction_hash);
    } else {
        println!("  Transfer failed");
    }

    Ok(())
}

async fn run_withdraw_contract(amount: f64) -> Result<()> {
    use alloy::sol;
    use alloy::sol_types::SolCall;
    use alloy::network::TransactionBuilder;

    sol! {
        function withdrawToken(address token, uint256 amount) external;
        function withdrawAllToken(address token) external;
    }

    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    init_nonce(&provider, signer_address).await?;

    let wallet = EthereumWallet::from(signer);
    let provider_with_signer = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(url);

    let gas_price = provider.get_gas_price().await.unwrap_or(100_000_000_000);

    let calldata = if amount == 0.0 {
        println!("Withdrawing ALL WMON from contract...");
        withdrawAllTokenCall { token: WMON_ADDRESS }.abi_encode()
    } else {
        println!("Withdrawing {} WMON from contract...", amount);
        let amount_wei = to_wei(amount, WMON_DECIMALS);
        withdrawTokenCall { token: WMON_ADDRESS, amount: amount_wei }.abi_encode()
    };

    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(ATOMIC_ARB_CONTRACT)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(
            alloy::primitives::Bytes::from(calldata)
        ))
        .gas_limit(100_000)
        .nonce(nonce::next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(143);

    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;

    if receipt.status() {
        println!("  Withdrawal successful");
        println!("  TX: {:?}", receipt.transaction_hash);
    } else {
        println!("  Withdrawal failed");
    }

    Ok(())
}

async fn run_contract_balance() -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let (wmon, usdc) = query_contract_balances(&provider).await?;

    println!("\n==============================================================");
    println!("  ATOMIC ARB CONTRACT BALANCES");
    println!("==============================================================");
    println!("  Contract: {:?}", ATOMIC_ARB_CONTRACT);
    println!("  WMON: {:>18.6}", wmon);
    println!("  USDC: {:>18.6}", usdc);
    println!("==============================================================");

    Ok(())
}

async fn run_mev_validate(duration: u64, min_spread_bps: i32, output_mode: &str) -> Result<()> {
    let node_config = NodeConfig::from_env();
    node_config.log_config();

    match output_mode {
        "dashboard" => {
            mev_validation::run_mev_validation_dashboard(
                &node_config.rpc_url,
                &node_config.ws_url,
                duration,
                min_spread_bps
            ).await
        }
        "log" => {
            mev_validation::run_mev_validation_log(
                &node_config.rpc_url,
                &node_config.ws_url,
                duration,
                min_spread_bps
            ).await
        }
        "quiet" => {
            mev_validation::run_mev_validation_quiet(
                &node_config.rpc_url,
                &node_config.ws_url,
                duration,
                min_spread_bps
            ).await
        }
        _ => {
            eprintln!("Unknown output mode: {}. Using 'dashboard'", output_mode);
            mev_validation::run_mev_validation_dashboard(
                &node_config.rpc_url,
                &node_config.ws_url,
                duration,
                min_spread_bps
            ).await
        }
    }
}

/// Live spread dashboard with detailed visualization
async fn run_dashboard(min_spread: i32, history: usize, refresh_ms: u64, sound: bool) -> Result<()> {
    use std::io::{stdout, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let node_config = NodeConfig::from_env();
    node_config.log_config();

    let url: reqwest::Url = node_config.rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    // Verify node health before starting
    verify_node_ready(&provider).await?;

    // Build price calls
    let mut price_calls: Vec<PriceCall> = Vec::new();

    for pool in get_v3_pools() {
        price_calls.push(create_slot0_call(&pool));
    }

    let lfj_pool = get_lfj_pool();
    price_calls.push(create_lfj_active_id_call(&lfj_pool));
    price_calls.push(create_lfj_bin_step_call(&lfj_pool));

    let monday_pool = get_monday_trade_pool();
    price_calls.push(create_slot0_call(&monday_pool));

    // Setup display
    let mut display = spread_display::SpreadDisplay::new(min_spread, history);
    display.alert_sound = sound;

    // Enter alternate screen
    spread_display::enter_alternate_screen();

    // Install Ctrl+C handler to restore terminal
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    let mut poll_interval = interval(Duration::from_millis(refresh_ms));

    while running.load(Ordering::SeqCst) {
        poll_interval.tick().await;

        let block_num = provider.get_block_number().await.ok();

        match fetch_prices_batched(&provider, price_calls.clone()).await {
            Ok((prices, _)) => {
                let spreads = calculate_spreads(&prices);
                display.update(&spreads);

                // Render dashboard
                spread_display::cursor_home();
                print!("{}", spread_display::render_full_dashboard(&display, &prices, block_num));
                stdout().flush().ok();

                // Sound alert for HOT+ spreads
                if display.alert_sound {
                    if let Some(best) = spreads.first() {
                        let bps = (best.net_spread_pct * 100.0) as i32;
                        if bps >= 15 {
                            print!("\x07"); // Terminal bell
                        }
                    }
                }
            }
            Err(e) => {
                spread_display::cursor_home();
                println!("\x1b[1;31mError fetching prices: {}\x1b[0m", e);
            }
        }
    }

    // Restore terminal
    spread_display::exit_alternate_screen();

    println!("\nDashboard stopped.");

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
        Some(Commands::FastArb { sell_dex, buy_dex, amount, slippage }) => {
            run_fast_arb(&sell_dex, &buy_dex, amount, slippage).await
        }
        Some(Commands::AtomicArb { sell_dex, buy_dex, amount, slippage, min_profit_bps, force, turbo }) => {
            if turbo {
                run_atomic_arb_turbo(&sell_dex, &buy_dex, amount, slippage).await
            } else {
                run_atomic_arb(&sell_dex, &buy_dex, amount, slippage, min_profit_bps, force).await
            }
        }
        Some(Commands::AutoArb {
            min_spread_bps,
            amount,
            slippage,
            max_executions,
            cooldown_secs,
            dry_run,
            force,
            track_velocity,
            history_size,
            min_velocity,
            max_velocity,
            min_final_spread,
            max_baseline,
        }) => {
            run_auto_arb(min_spread_bps, amount, slippage, max_executions, cooldown_secs, dry_run, force, track_velocity, history_size, min_velocity, max_velocity, min_final_spread, max_baseline).await
        }
        Some(Commands::ProdArb {
            min_spread_bps,
            amount,
            slippage,
            max_daily_loss,
            max_failures,
        }) => {
            run_prod_arb(min_spread_bps, amount, slippage, max_daily_loss, max_failures).await
        }
        Some(Commands::FundContract { amount }) => {
            run_fund_contract(amount).await
        }
        Some(Commands::WithdrawContract { amount }) => {
            run_withdraw_contract(amount).await
        }
        Some(Commands::ContractBalance) => {
            run_contract_balance().await
        }
        Some(Commands::TestRevert { dex, gas_limit, method }) => {
            run_test_revert(&dex, gas_limit, &method).await
        }
        Some(Commands::MevValidate { duration, min_spread, output }) => {
            run_mev_validate(duration, min_spread, &output).await
        }
        Some(Commands::Dashboard { min_spread, history, refresh_ms, sound }) => {
            run_dashboard(min_spread, history, refresh_ms, sound).await
        }
    }
}
