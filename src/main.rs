//! Monad Arbitrage Bot - 0x vs Direct Pool Strategy

mod config;
mod execution;
mod pools;
mod trader;
mod wallet;
mod zrx;

use alloy::network::EthereumWallet;
use alloy::primitives::{B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use clap::Parser;
use eyre::Result;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, Level};

#[derive(Parser, Debug)]
#[command(name = "monad-arb-bot")]
#[command(about = "Monad Arbitrage Bot - 0x vs Direct Pool Strategy")]
struct Args {
    /// Show full wallet balance (native MON, WMON, USDC)
    #[arg(long)]
    balance: bool,

    /// Wrap native MON to WMON (required for trading)
    #[arg(long)]
    wrap: bool,

    /// Unwrap WMON back to native MON
    #[arg(long)]
    unwrap: bool,

    /// Amount of MON to wrap/unwrap
    #[arg(long, default_value = "100.0")]
    wrap_amount: f64,

    /// Run a single test trade (sell WMON via 0x)
    #[arg(long)]
    test_trade: bool,

    /// Run a full arbitrage test (buy on Uniswap + sell via 0x)
    #[arg(long)]
    test_arb: bool,

    /// Amount of WMON to trade in test-trade mode
    #[arg(long, default_value = "10.0")]
    trade_amount: f64,

    /// Amount of USDC to use in test-arb mode
    #[arg(long, default_value = "5.0")]
    usdc_amount: f64,

    /// Uniswap pool fee tier in basis points (500=0.05%, 3000=0.3%, 10000=1%)
    #[arg(long, default_value = "500")]
    pool_fee: u32,

    /// Slippage tolerance in basis points (100 = 1%)
    #[arg(long, default_value = "100")]
    slippage_bps: u32,

    /// Run parallel arbitrage test (both legs simultaneously)
    #[arg(long)]
    test_parallel: bool,

    /// Amount of WMON to sell via 0x in parallel mode (min 30 for profitability)
    #[arg(long, default_value = "50.0")]
    wmon_amount: f64,

    /// Spread threshold (%) to trigger auto-execution. 0 = monitoring only (default)
    #[arg(long, default_value = "1.5")]
    spread_threshold: f64,

    /// Check transaction status by hash
    #[arg(long)]
    check_tx: Option<String>,
}

#[derive(Debug)]
struct ArbOpportunity {
    aggregator_price: f64,
    pool_price: f64,
    pool_name: String,
    spread_pct: f64,
    direction: String,
}

impl ArbOpportunity {
    fn print(&self) {
        println!("\n============ ARBITRAGE DETECTED ============");
        println!("  0x Price:       ${:.6}", self.aggregator_price);
        println!("  {} Price:  ${:.6}", self.pool_name, self.pool_price);
        println!("  Spread:         {:.3}%", self.spread_pct);
        println!("  Direction:      {}", self.direction);
        println!("  Est. Profit:    {:.3}% (before gas)", self.spread_pct - 0.3);
        println!("=============================================\n");
    }
}

fn check_arbitrage(
    aggregator_price: f64,
    pool_price: f64,
    pool_name: &str,
) -> Option<ArbOpportunity> {
    // Validate prices
    if aggregator_price <= 0.0 || pool_price <= 0.0 {
        return None;
    }

    let spread_pct = ((aggregator_price - pool_price) / pool_price) * 100.0;

    // Sanity check
    if spread_pct.abs() > config::MAX_SPREAD_PCT {
        warn!("Unrealistic spread: {:.2}% - ignoring", spread_pct);
        return None;
    }

    // Check minimum spread
    if spread_pct.abs() > config::MIN_SPREAD_PCT {
        let direction = if spread_pct > 0.0 {
            format!("BUY on {} -> SELL via 0x", pool_name)
        } else {
            format!("BUY via 0x -> SELL on {}", pool_name)
        };

        Some(ArbOpportunity {
            aggregator_price,
            pool_price,
            pool_name: pool_name.to_string(),
            spread_pct: spread_pct.abs(),
            direction,
        })
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    dotenvy::dotenv().ok();

    // Setup wallet from private key
    let private_key = env::var("PRIVATE_KEY")
        .map_err(|_| eyre::eyre!("PRIVATE_KEY not set in .env"))?;

    let signer: PrivateKeySigner = private_key.parse()?;
    let eth_wallet = EthereumWallet::from(signer);

    let rpc_url = env::var("MONAD_RPC_URL")
        .unwrap_or_else(|_| "https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string());

    let provider = ProviderBuilder::new()
        .wallet(eth_wallet.clone())
        .connect_http(rpc_url.parse()?);

    let wallet_addr = eth_wallet.default_signer().address();

    println!("==========================================");
    println!("  Monad Arbitrage Bot");
    println!("  Wallet: {:?}", wallet_addr);
    println!("==========================================\n");

    // ========== BALANCE CHECK MODE ==========
    if args.balance {
        println!("WALLET BALANCE CHECK\n");
        let balances =
            wallet::get_full_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;
        balances.print();
        return Ok(());
    }

    // ========== CHECK TRANSACTION STATUS MODE ==========
    if let Some(tx_hash_str) = &args.check_tx {
        println!("TRANSACTION STATUS CHECK\n");

        // Parse the transaction hash
        let tx_hash: B256 = tx_hash_str.parse()
            .map_err(|_| eyre::eyre!("Invalid transaction hash format"))?;

        println!("Tx Hash: {:?}", tx_hash);

        // Try to get the transaction receipt
        match provider.get_transaction_receipt(tx_hash).await? {
            Some(receipt) => {
                let status = if receipt.status() { "SUCCESS ✓" } else { "FAILED ✗" };
                println!("\n========== TRANSACTION RECEIPT ==========");
                println!("  Status:       {}", status);
                println!("  Block:        {:?}", receipt.block_number.unwrap_or(0));
                println!("  Gas Used:     {}", receipt.gas_used);
                println!("  From:         {:?}", receipt.from);
                println!("  To:           {:?}", receipt.to.unwrap_or_default());
                println!("==========================================\n");
            }
            None => {
                // Transaction not yet mined - check if it's pending
                match provider.get_transaction_by_hash(tx_hash).await? {
                    Some(_tx) => {
                        println!("\n  Status: PENDING (not yet mined)");
                        println!("  The transaction is in the mempool waiting for confirmation.\n");
                    }
                    None => {
                        println!("\n  Status: NOT FOUND");
                        println!("  Transaction not found on chain. It may have been dropped.\n");
                    }
                }
            }
        }
        return Ok(());
    }

    // ========== WRAP MON MODE ==========
    if args.wrap {
        println!("WRAP MON -> WMON\n");
        println!("Amount: {} MON", args.wrap_amount);

        // Get initial balance
        let initial =
            wallet::get_full_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;
        println!("\nBefore:");
        initial.print();

        // Convert amount to wei (18 decimals)
        let wrap_amount = U256::from((args.wrap_amount * 1e18) as u128);

        // Check if we have enough native MON
        if initial.native_mon < wrap_amount {
            return Err(eyre::eyre!(
                "Insufficient native MON. Have: {:.6}, Need: {:.6}",
                initial.native_mon.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
                args.wrap_amount
            ));
        }

        // Execute wrap
        let result = execution::wrap_mon(&provider, &eth_wallet, wrap_amount).await?;
        result.print();

        // Get final balance
        let final_balance =
            wallet::get_full_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;
        println!("After:");
        final_balance.print();

        println!("\nWrap complete!");
        return Ok(());
    }

    // ========== UNWRAP WMON MODE ==========
    if args.unwrap {
        println!("UNWRAP WMON -> MON\n");
        println!("Amount: {} WMON", args.wrap_amount);

        // Get initial balance
        let initial =
            wallet::get_full_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;
        println!("\nBefore:");
        initial.print();

        // Convert amount to wei (18 decimals)
        let unwrap_amount = U256::from((args.wrap_amount * 1e18) as u128);

        // Check if we have enough WMON
        if initial.wmon_balance < unwrap_amount {
            return Err(eyre::eyre!(
                "Insufficient WMON. Have: {:.6}, Need: {:.6}",
                initial.wmon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
                args.wrap_amount
            ));
        }

        // Execute unwrap
        let result = execution::unwrap_mon(&provider, &eth_wallet, unwrap_amount).await?;
        result.print();

        // Get final balance
        let final_balance =
            wallet::get_full_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;
        println!("After:");
        final_balance.print();

        println!("\nUnwrap complete!");
        return Ok(());
    }

    // Initialize 0x API client (requires ZRX_API_KEY env var)
    let zrx = zrx::ZrxClient::new()?;
    info!("0x API client initialized");

    // ========== TEST TRADE MODE ==========
    if args.test_trade {
        println!("TEST TRADE MODE");
        println!("Amount: {} MON", args.trade_amount);
        println!(
            "Slippage: {}bps ({}%)",
            args.slippage_bps,
            args.slippage_bps as f64 / 100.0
        );

        // Get initial balance
        let initial =
            wallet::get_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;

        println!("\n Starting Balance:");
        initial.print();

        let params = trader::TradeParams {
            amount_mon: args.trade_amount,
            slippage_bps: args.slippage_bps,
            min_profit_bps: 30,
        };

        // Execute test trade (sell MON via 0x)
        let report = trader::execute_0x_sell(&provider, &eth_wallet, &zrx, &params).await?;
        report.print();

        // Final balance
        let final_balance =
            wallet::get_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;

        println!(" Final Balance:");
        final_balance.print();

        println!(" Test trade complete!");
        return Ok(());
    }

    // ========== TEST ARBITRAGE MODE ==========
    if args.test_arb {
        println!("ARBITRAGE TEST MODE");
        println!("Strategy: BUY on Uniswap -> SELL via 0x");
        println!("USDC Amount: ${}", args.usdc_amount);
        println!("Pool Fee: {}bps", args.pool_fee);
        println!(
            "Slippage: {}bps ({}%)",
            args.slippage_bps,
            args.slippage_bps as f64 / 100.0
        );

        // Get initial balance
        let initial =
            wallet::get_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;

        println!("\n Starting Balance:");
        initial.print();

        // Convert USDC amount to 6 decimals
        let usdc_amount = U256::from((args.usdc_amount * 1_000_000.0) as u128);

        // Execute full arbitrage: BUY on Uniswap -> SELL via 0x
        let report = execution::execute_arbitrage(
            &provider,
            &eth_wallet,
            &zrx,
            usdc_amount,
            args.pool_fee,     // Uniswap pool fee tier
            args.slippage_bps, // 0x slippage
        )
        .await?;

        report.print();

        // Final balance
        let final_balance =
            wallet::get_balances(&provider, wallet_addr, config::WMON, config::USDC).await?;

        println!(" Final Balance:");
        final_balance.print();

        println!(" Arbitrage test complete!");
        return Ok(());
    }

    // ========== PARALLEL ARBITRAGE TEST MODE ==========
    if args.test_parallel {
        println!("PARALLEL ARBITRAGE TEST MODE");
        println!("Strategy: BUY on Uniswap + SELL via 0x (SIMULTANEOUS)");
        println!("WMON Amount (0x SELL): {}", args.wmon_amount);
        println!("Pool Fee: {}bps", args.pool_fee);
        println!("Slippage: {}bps ({}%)", args.slippage_bps, args.slippage_bps as f64 / 100.0);

        // Convert WMON amount to wei
        let wmon_amount = U256::from((args.wmon_amount * 1e18) as u128);

        // Get 0x price quote to determine equivalent USDC amount
        println!("\n Fetching 0x price for {} WMON...", args.wmon_amount);
        let price_quote = zrx.get_price(
            config::WMON,
            config::USDC,
            &wmon_amount.to_string(),
        ).await?;

        // Use the 0x buy_amount (USDC we'd receive) as the USDC input for Uniswap
        // This ensures inventory-neutral execution
        let usdc_from_quote: u128 = price_quote.buy_amount.parse()?;
        let usdc_amount = U256::from(usdc_from_quote);
        let usdc_human = usdc_from_quote as f64 / 1e6;

        println!(" 0x Quote: {} WMON -> ${:.6} USDC", args.wmon_amount, usdc_human);
        println!(" Auto-calculated USDC Amount (Uniswap BUY): ${:.6}", usdc_human);

        // Ensure approvals are done BEFORE parallel execution
        println!("\n Checking approvals...");

        // USDC approval to SwapRouter
        execution::ensure_approval(
            &provider,
            &eth_wallet,
            config::USDC,
            config::UNISWAP_SWAP_ROUTER,
            usdc_amount,
        ).await?;

        // WMON approval to AllowanceHolder
        execution::ensure_approval(
            &provider,
            &eth_wallet,
            config::WMON,
            config::ALLOWANCE_HOLDER,
            wmon_amount,
        ).await?;

        println!(" Approvals confirmed. Executing parallel arbitrage...\n");

        // Execute parallel arbitrage
        let report = execution::execute_parallel_arbitrage(
            &provider,
            &eth_wallet,
            &zrx,
            usdc_amount,
            wmon_amount,
            args.pool_fee,
            args.slippage_bps,
        ).await?;

        report.print();

        println!(" Parallel arbitrage test complete!");
        return Ok(());
    }

    // ========== MONITORING MODE (Default) ==========
    println!("Strategy: 0x API vs Direct Pools");
    println!("Pair: MON/USDC\n");

    // Determine token0 for price calculation
    let wmon = config::WMON.to_lowercase();
    let usdc = config::USDC.to_lowercase();
    let token0_is_mon = wmon < usdc;
    info!("Token0 is MON: {}", token0_is_mon);

    // Discover Uniswap MON/USDC pool (try different fee tiers)
    println!("Discovering Uniswap MON/USDC pool...");
    let mut uniswap_pool: Option<String> = None;
    for fee in [500u32, 3000, 10000] {
        // 0.05%, 0.3%, 1%
        if let Some(pool) = pools::discover_pool(
            &provider,
            config::UNISWAP_FACTORY,
            config::WMON,
            config::USDC,
            fee,
        )
        .await?
        {
            let pool_str = format!("{:?}", pool);
            if pools::has_liquidity(&provider, &pool_str).await? {
                info!("Found Uniswap MON/USDC pool: {} (fee: {})", pool_str, fee);
                uniswap_pool = Some(pool_str);
                break;
            }
        }
    }

    let uniswap_pool = uniswap_pool.ok_or_else(|| {
        eyre::eyre!("No Uniswap MON/USDC pool found with liquidity. Check USDC address.")
    })?;

    // Before loop, if auto-execute enabled
    if args.spread_threshold > 0.0 {
        println!("AUTO-EXECUTE ENABLED");
        println!("Spread Threshold: {}%", args.spread_threshold);

        // Pre-approve USDC to SwapRouter (for Uniswap BUY leg)
        let usdc_approval_amount = U256::from(100_000_000u128); // 100 USDC
        execution::ensure_approval(
            &provider,
            &eth_wallet,
            config::USDC,
            config::UNISWAP_SWAP_ROUTER,
            usdc_approval_amount,
        ).await?;

        // Pre-approve WMON to AllowanceHolder (for 0x SELL leg)
        let wmon_approval_amount = U256::from(100_000_000_000_000_000_000u128); // 100 WMON
        execution::ensure_approval(
            &provider,
            &eth_wallet,
            config::WMON,
            config::ALLOWANCE_HOLDER,
            wmon_approval_amount,
        ).await?;

        println!("Approvals confirmed. Starting monitoring...\n");
    }

    // Main loop - poll every 2 seconds
    let mut poll_interval = interval(Duration::from_secs(2));

    // Execution lock to prevent nonce collisions from concurrent arb attempts
    let execution_in_flight = Arc::new(AtomicBool::new(false));

    println!("\nStarting price monitoring...\n");

    loop {
        poll_interval.tick().await;

        // Get 0x aggregated price
        let zrx_price = match zrx.get_mon_usdc_price().await {
            Ok(p) => p,
            Err(e) => {
                warn!("0x API error: {}", e);
                continue;
            }
        };

        // Get Uniswap direct pool price
        let uniswap_price = match pools::get_pool_price(
            &provider,
            &uniswap_pool, // Use discovered pool
            token0_is_mon,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                warn!("Uniswap pool error: {}", e);
                continue;
            }
        };

        // Calculate spread
        let spread_pct = ((zrx_price - uniswap_price) / uniswap_price) * 100.0;

        // Print current spread on every tick
        println!(
            "[{}] MON/USDC | 0x: ${:.6} | Uniswap: ${:.6} | Spread: {:+.3}%",
            chrono::Local::now().format("%H:%M:%S"),
            zrx_price,
            uniswap_price,
            spread_pct
        );

        // Auto-execute if enabled and spread exceeds threshold
        if args.spread_threshold > 0.0 && spread_pct > args.spread_threshold {
            // Check if an execution is already in flight (prevent nonce collisions)
            if execution_in_flight.load(Ordering::SeqCst) {
                println!("  [SKIPPED] Execution already in flight, waiting for confirmation...");
                continue;
            }

            // Profitability guard: Ensure trade size is sufficient for gas costs
            if args.wmon_amount < config::MIN_WMON_TRADE_AMOUNT {
                println!(
                    "  [SKIPPED] Trade size {:.1} WMON too small. Minimum: {:.1} WMON for profitability",
                    args.wmon_amount,
                    config::MIN_WMON_TRADE_AMOUNT
                );
                continue;
            }

            // For smaller trades, require higher spread
            if args.wmon_amount < config::RECOMMENDED_WMON_AMOUNT
               && spread_pct < config::MIN_SPREAD_FOR_SMALL_TRADE {
                println!(
                    "  [SKIPPED] Spread {:.2}% too low for {:.1} WMON trade. Need {:.1}%+ or increase to {:.0} WMON",
                    spread_pct,
                    args.wmon_amount,
                    config::MIN_SPREAD_FOR_SMALL_TRADE,
                    config::RECOMMENDED_WMON_AMOUNT
                );
                continue;
            }

            // Estimate profitability before executing
            let trade_value_usdc = args.wmon_amount * uniswap_price;  // Approximate value
            let (est_gas_mon, is_profitable) = execution::estimate_trade_profitability(
                spread_pct,
                trade_value_usdc,
                uniswap_price,
            );

            if !is_profitable {
                println!(
                    "  [SKIPPED] Estimated unprofitable. Gas: ~{:.4} MON, Trade value: ${:.2}, Spread: {:.2}%",
                    est_gas_mon,
                    trade_value_usdc,
                    spread_pct
                );
                println!("  Tip: Increase --wmon-amount to {} or wait for higher spread", config::RECOMMENDED_WMON_AMOUNT);
                continue;
            }

            println!("Profitability check PASSED. Est. gas: {:.4} MON", est_gas_mon);

            println!("\n========== SPREAD THRESHOLD TRIGGERED ==========");
            println!("Detected: {:+.3}% > Threshold: {}%", spread_pct, args.spread_threshold);

            // Calculate trade amounts
            // Use fixed WMON amount for sell leg, auto-calculate USDC from 0x quote
            let wmon_amount = U256::from((args.wmon_amount * 1e18) as u128);

            // Get 0x price to determine USDC equivalent
            let price_quote = zrx.get_price(
                config::WMON,
                config::USDC,
                &wmon_amount.to_string(),
            ).await;

            match price_quote {
                Ok(quote) => {
                    let usdc_from_quote: u128 = quote.buy_amount.parse().unwrap_or(0);
                    let usdc_amount = U256::from(usdc_from_quote);

                    println!("Trade Size: {} WMON / ${:.2} USDC", args.wmon_amount, usdc_from_quote as f64 / 1e6);
                    println!("Firing parallel arbitrage (non-blocking)...\n");

                    // Set execution lock BEFORE spawning
                    execution_in_flight.store(true, Ordering::SeqCst);

                    // Clone everything needed for background task
                    let provider_bg = provider.clone();
                    let wallet_bg = eth_wallet.clone();
                    let zrx_bg = zrx.clone();
                    let pool_fee_bg = args.pool_fee;
                    let slippage_bg = args.slippage_bps;
                    let lock = execution_in_flight.clone();

                    // Fire and forget - spawn execution in background
                    tokio::spawn(async move {
                        let start = std::time::Instant::now();
                        match execution::execute_parallel_arbitrage(
                            &provider_bg,
                            &wallet_bg,
                            &zrx_bg,
                            usdc_amount,
                            wmon_amount,
                            pool_fee_bg,
                            slippage_bg,
                        ).await {
                            Ok(report) => {
                                report.print();
                                println!("========== EXECUTION COMPLETE ==========");
                                println!("Total Time: {}ms", start.elapsed().as_millis());
                                println!("USDC P/L: {:+.6}", report.usdc_change);
                                println!("=========================================\n");
                            }
                            Err(e) => {
                                eprintln!("Execution failed: {}", e);
                            }
                        }
                        // Release execution lock when done
                        lock.store(false, Ordering::SeqCst);
                        println!("[READY] Execution complete. Ready for next opportunity.\n");
                    });

                    // Continue immediately - don't wait for execution result
                    println!("Transactions submitted. Continuing to monitor...\n");
                }
                Err(e) => {
                    eprintln!("Failed to get 0x quote: {}", e);
                }
            }
        }

        // Only print detection box if NOT auto-executing
        if args.spread_threshold == 0.0 {
            if let Some(arb) = check_arbitrage(zrx_price, uniswap_price, "Uniswap") {
                arb.print();
            }
        }
    }
}
