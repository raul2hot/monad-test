# Monad Arbitrage Bot - Execution Layer Implementation Guide

## TL;DR - Quick Start

```
SINGLE-SHOT BUILD - All steps:
1. Add ALLOWANCE_HOLDER constant to src/config.rs
2. Create src/wallet.rs - balance & approval functions
3. Update src/zrx.rs - add get_quote() and get_quote_with_exclusions()
4. Create src/execution.rs - 0x swap execution + full arb execution
5. Create src/trader.rs - trade orchestration with reporting
6. Update src/main.rs - add --test-trade and --test-arb flags
7. Update Cargo.toml - add clap dependency

TEST COMMANDS:
  cargo run -- --test-trade --trade-amount 10   # Single 0x sell test
  cargo run -- --test-arb --usdc-amount 5       # Full arb cycle test

SUCCESS = Trades execute, balances change, tx confirmed on-chain
```

---

## Overview

You are implementing the **execution layer** for an existing Monad arbitrage bot. The detection layer is already working and identifies price discrepancies between 0x aggregator and Uniswap V3 pools. Your task is to add the ability to **execute real trades** when arbitrage opportunities are detected.

**Goal**: Successfully execute trades on Monad mainnet with detailed logging of balance changes, slippage, and transaction results.

---

## Current State

### What Exists
- **Detection system**: Compares 0x API prices vs Uniswap V3 pool prices
- **0x price fetching**: Uses `/swap/allowance-holder/price` endpoint
- **Pool price querying**: Reads slot0 from Uniswap V3 pools
- **Arbitrage detection logic**: Identifies spreads > 0.5%

### What You Must Implement
- Token approval management (one-time setup)
- 0x quote fetching (for actual execution)
- Transaction building and submission
- Uniswap V3 swap execution
- Balance tracking and trade reporting
- Single-trade test mode

---

## Architecture Decision: EOA-Based Execution

**DO NOT** implement smart contract-based atomic arbitrage yet.
**DO** implement EOA (Externally Owned Account) based execution.

Rationale: We're testing trade execution mechanics with small amounts. EOA execution is simpler, faster to implement, and sufficient for Phase 2 validation.

---

## Key Constants & Addresses

All addresses below are **verified** for Monad mainnet (chain 143):

```rust
// Chain (already in config.rs)
pub const CHAIN_ID: u64 = 143;
pub const BLOCK_TIME_MS: u64 = 400;

// Tokens (already in config.rs - VERIFIED)
pub const WMON: &str = "0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A";
pub const USDC: &str = "0x754704Bc059F8C67012fEd69BC8A327a5aafb603";

// Uniswap V3 Factory (already in config.rs - VERIFIED, used for pool discovery)
pub const UNISWAP_FACTORY: &str = "0x204FAca1764B154221e35c0d20aBb3c525710498";

// NEW - Uniswap V3 SwapRouter02 (VERIFIED from MonadVision)
pub const UNISWAP_SWAP_ROUTER: &str = "0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900";

// NEW - 0x AllowanceHolder (VERIFIED from 0x docs - same across all chains)
pub const ALLOWANCE_HOLDER: &str = "0x0000000000001fF3684f28c67538d4D072C22734";

// 0x API (already working in zrx.rs)
pub const ZRX_API_BASE: &str = "https://api.0x.org";
pub const ZRX_QUOTE_ENDPOINT: &str = "/swap/allowance-holder/quote";
```

## Execution Strategy (SRS-Compliant)

**BUY on Uniswap → SELL via 0x**

When the detection shows `0x price > Uniswap price`:
1. **BUY MON on Uniswap** using SwapRouter02 (`exactInputSingle`)
2. **SELL MON via 0x** using AllowanceHolder pattern

This matches your detection output:
```
Direction: BUY on Uniswap → SELL via 0x
```

---

## Implementation Tasks

### Task 1: Create `src/wallet.rs` - Wallet Management

```rust
//! Wallet and balance management

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::rpc::types::TransactionRequest;
use eyre::Result;
use std::str::FromStr;

sol! {
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function decimals() external view returns (uint8);
}

pub struct WalletInfo {
    pub address: Address,
    pub mon_balance: U256,
    pub usdc_balance: U256,
}

impl WalletInfo {
    pub fn print(&self) {
        let mon_human = self.mon_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let usdc_human = self.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        println!("Wallet: {:?}", self.address);
        println!("  MON Balance:  {:.6} MON", mon_human);
        println!("  USDC Balance: {:.6} USDC", usdc_human);
    }
}

pub async fn get_balances<P: Provider>(
    provider: &P,
    wallet: Address,
    wmon: &str,
    usdc: &str,
) -> Result<WalletInfo> {
    let wmon_addr = Address::from_str(wmon)?;
    let usdc_addr = Address::from_str(usdc)?;
    
    // Get WMON balance
    let call = balanceOfCall { account: wallet };
    let tx = TransactionRequest::default()
        .to(wmon_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let mon_balance = balanceOfCall::abi_decode_returns(&result)?;
    
    // Get USDC balance
    let call = balanceOfCall { account: wallet };
    let tx = TransactionRequest::default()
        .to(usdc_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let usdc_balance = balanceOfCall::abi_decode_returns(&result)?;
    
    Ok(WalletInfo {
        address: wallet,
        mon_balance: mon_balance._0,
        usdc_balance: usdc_balance._0,
    })
}

pub async fn check_allowance<P: Provider>(
    provider: &P,
    token: &str,
    owner: Address,
    spender: &str,
) -> Result<U256> {
    let token_addr = Address::from_str(token)?;
    let spender_addr = Address::from_str(spender)?;
    
    let call = allowanceCall { owner, spender: spender_addr };
    let tx = TransactionRequest::default()
        .to(token_addr)
        .input(call.abi_encode().into());
    let result = provider.call(tx).await?;
    let allowance = allowanceCall::abi_decode_returns(&result)?;
    
    Ok(allowance._0)
}
```

---

### Task 2: Create `src/execution.rs` - Trade Execution

This is the core execution module. Implement these functions:

#### 2.1 Token Approval Function

```rust
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256, Bytes};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use std::str::FromStr;

sol! {
    function approve(address spender, uint256 amount) external returns (bool);
}

/// Approve token spending to AllowanceHolder (one-time per token)
pub async fn ensure_approval<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    token: &str,
    spender: &str,  // AllowanceHolder address
    required_amount: U256,
) -> Result<Option<alloy::primitives::TxHash>> {
    let token_addr = Address::from_str(token)?;
    let spender_addr = Address::from_str(spender)?;
    let owner = wallet.default_signer().address();
    
    // Check current allowance
    let current = crate::wallet::check_allowance(provider, token, owner, spender).await?;
    
    if current >= required_amount {
        tracing::info!("Allowance sufficient: {} >= {}", current, required_amount);
        return Ok(None);
    }
    
    tracing::info!("Approving {} to spend tokens...", spender);
    
    // Approve max amount (U256::MAX)
    let call = approveCall { 
        spender: spender_addr, 
        amount: U256::MAX 
    };
    
    let tx = TransactionRequest::default()
        .to(token_addr)
        .input(call.abi_encode().into())
        .from(owner);
    
    let tx_hash = provider
        .send_transaction(tx)
        .await?
        .watch()
        .await?;
    
    tracing::info!("Approval tx confirmed: {:?}", tx_hash);
    Ok(Some(tx_hash))
}
```

#### 2.2 0x Quote Fetching (Upgrade from Price to Quote)

Modify `src/zrx.rs` - Add these structs and function:

```rust
// Add to existing zrx.rs

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteTransaction {
    pub to: String,
    pub data: String,
    pub gas: String,
    pub gas_price: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteIssues {
    pub allowance: Option<AllowanceIssue>,
}

#[derive(Debug, Deserialize)]
pub struct AllowanceIssue {
    pub spender: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteResponse {
    pub buy_amount: String,
    pub min_buy_amount: String,
    pub sell_amount: String,
    pub buy_token: String,
    pub sell_token: String,
    pub liquidity_available: bool,
    pub transaction: QuoteTransaction,
    pub issues: QuoteIssues,
    #[serde(default)]
    pub route: Option<RouteInfo>,
}

impl ZrxClient {
    /// Get executable quote for selling tokens
    /// Returns transaction data ready for submission
    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        taker: &str,  // Your wallet address
        slippage_bps: u32,  // e.g., 100 = 1%
    ) -> Result<QuoteResponse> {
        let url = format!(
            "{}/swap/allowance-holder/quote?chainId={}&sellToken={}&buyToken={}&sellAmount={}&taker={}&slippageBps={}",
            crate::config::ZRX_API_BASE,
            crate::config::CHAIN_ID,
            sell_token,
            buy_token,
            sell_amount,
            taker,
            slippage_bps
        );

        tracing::debug!("0x Quote URL: {}", url);

        let response = self.client
            .get(&url)
            .header("0x-api-key", &self.api_key)
            .header("0x-version", "v2")
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(eyre::eyre!("0x API error: {} - {}", status, body));
        }

        let quote: QuoteResponse = serde_json::from_str(&body)
            .map_err(|e| eyre::eyre!("Failed to parse 0x quote: {}. Body: {}", e, body))?;

        if !quote.liquidity_available {
            return Err(eyre::eyre!("No liquidity available for this pair"));
        }

        // Log routing info
        if let Some(route) = &quote.route {
            let sources: Vec<&str> = route.fills.iter()
                .map(|f| f.source.as_str())
                .collect();
            tracing::info!("0x quote routing through: {:?}", sources);
        }

        Ok(quote)
    }

    /// Get quote with optional source exclusions
    /// Use excludedSources to force different routing for arbitrage
    pub async fn get_quote_with_exclusions(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        taker: &str,
        slippage_bps: u32,
        excluded_sources: Option<&str>,  // e.g., "Uniswap_V3,Sushiswap"
    ) -> Result<QuoteResponse> {
        let mut url = format!(
            "{}/swap/allowance-holder/quote?chainId={}&sellToken={}&buyToken={}&sellAmount={}&taker={}&slippageBps={}",
            crate::config::ZRX_API_BASE,
            crate::config::CHAIN_ID,
            sell_token,
            buy_token,
            sell_amount,
            taker,
            slippage_bps
        );

        if let Some(excluded) = excluded_sources {
            url.push_str(&format!("&excludedSources={}", excluded));
        }

        tracing::debug!("0x Quote URL: {}", url);

        let response = self.client
            .get(&url)
            .header("0x-api-key", &self.api_key)
            .header("0x-version", "v2")
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(eyre::eyre!("0x API error: {} - {}", status, body));
        }

        let quote: QuoteResponse = serde_json::from_str(&body)
            .map_err(|e| eyre::eyre!("Failed to parse 0x quote: {}. Body: {}", e, body))?;

        if !quote.liquidity_available {
            return Err(eyre::eyre!("No liquidity available for this pair"));
        }

        // Log routing info
        if let Some(route) = &quote.route {
            let sources: Vec<&str> = route.fills.iter()
                .map(|f| f.source.as_str())
                .collect();
            tracing::info!("0x quote routing through: {:?}", sources);
        }

        Ok(quote)
    }
}
```

#### 2.3 Execute 0x Swap

```rust
// In src/execution.rs

use alloy::primitives::Bytes;

/// Execute a swap via 0x using the quote response
pub async fn execute_0x_swap<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    quote: &crate::zrx::QuoteResponse,
) -> Result<ExecutionResult> {
    let from = wallet.default_signer().address();
    
    // Parse transaction fields from quote
    let to = Address::from_str(&quote.transaction.to)?;
    let data: Bytes = quote.transaction.data.parse()?;
    let gas_limit: u64 = quote.transaction.gas.parse()?;
    let gas_price: u128 = quote.transaction.gas_price.parse()?;
    let value: U256 = quote.transaction.value.parse()?;
    
    // CRITICAL: Monad charges full gas_limit, not gas_used
    // Add minimal buffer, don't over-estimate
    let adjusted_gas_limit = gas_limit + 5_000;
    
    // Add 10% priority bump for faster inclusion
    let adjusted_gas_price = gas_price * 110 / 100;
    
    tracing::info!("Executing 0x swap:");
    tracing::info!("  To: {:?}", to);
    tracing::info!("  Gas Limit: {} (buffered from {})", adjusted_gas_limit, gas_limit);
    tracing::info!("  Gas Price: {} (10% bump)", adjusted_gas_price);
    tracing::info!("  Value: {}", value);
    
    let tx = TransactionRequest::default()
        .to(to)
        .input(data)
        .value(value)
        .gas_limit(adjusted_gas_limit)
        .gas_price(adjusted_gas_price)
        .from(from);
    
    let start_time = std::time::Instant::now();
    
    let pending = provider.send_transaction(tx).await?;
    let tx_hash = pending.tx_hash().clone();
    
    tracing::info!("Transaction submitted: {:?}", tx_hash);
    
    // Wait for confirmation
    let receipt = pending.get_receipt().await?;
    
    let elapsed = start_time.elapsed();
    
    let success = receipt.status();
    let gas_used = receipt.gas_used;
    
    Ok(ExecutionResult {
        tx_hash,
        success,
        gas_used,
        gas_limit: adjusted_gas_limit,
        execution_time_ms: elapsed.as_millis() as u64,
    })
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub tx_hash: alloy::primitives::TxHash,
    pub success: bool,
    pub gas_used: u128,
    pub gas_limit: u64,
    pub execution_time_ms: u64,
}

impl ExecutionResult {
    pub fn print(&self) {
        println!("\n========== EXECUTION RESULT ==========");
        println!("  Tx Hash:       {:?}", self.tx_hash);
        println!("  Status:        {}", if self.success { "✅ SUCCESS" } else { "❌ FAILED" });
        println!("  Gas Used:      {}", self.gas_used);
        println!("  Gas Limit:     {}", self.gas_limit);
        println!("  Exec Time:     {}ms", self.execution_time_ms);
        println!("=======================================\n");
    }
}
```

#### 2.4 Execute Uniswap V3 Swap (BUY Leg)

```rust
// In src/execution.rs

use alloy::sol;
use alloy::primitives::U160;

sol! {
    // Uniswap V3 SwapRouter02 exactInputSingle
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    
    function exactInputSingle(ExactInputSingleParams calldata params) 
        external payable returns (uint256 amountOut);
}

/// Execute BUY on Uniswap V3 (USDC → MON)
pub async fn execute_uniswap_buy<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    usdc_amount: U256,
    min_mon_out: U256,
    pool_fee: u32,  // e.g., 500 for 0.05%, 3000 for 0.3%
) -> Result<ExecutionResult> {
    let from = wallet.default_signer().address();
    let router = Address::from_str(crate::config::UNISWAP_SWAP_ROUTER)?;
    let usdc = Address::from_str(crate::config::USDC)?;
    let wmon = Address::from_str(crate::config::WMON)?;
    
    let params = ExactInputSingleParams {
        tokenIn: usdc,
        tokenOut: wmon,
        fee: pool_fee.try_into()?,
        recipient: from,
        amountIn: usdc_amount,
        amountOutMinimum: min_mon_out,
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };
    
    let call = exactInputSingleCall { params };
    
    // CRITICAL: Monad charges full gas_limit
    let tx = TransactionRequest::default()
        .to(router)
        .input(call.abi_encode().into())
        .from(from)
        .gas_limit(200_000);  // Typical Uniswap V3 swap
    
    let start_time = std::time::Instant::now();
    
    let pending = provider.send_transaction(tx).await?;
    let tx_hash = pending.tx_hash().clone();
    
    tracing::info!("Uniswap BUY tx submitted: {:?}", tx_hash);
    
    let receipt = pending.get_receipt().await?;
    let elapsed = start_time.elapsed();
    
    Ok(ExecutionResult {
        tx_hash,
        success: receipt.status(),
        gas_used: receipt.gas_used,
        gas_limit: 200_000,
        execution_time_ms: elapsed.as_millis() as u64,
    })
}
```

#### 2.5 Full Arbitrage: BUY on Uniswap → SELL via 0x

```rust
// In src/execution.rs

/// Execute full arbitrage: BUY on Uniswap, SELL via 0x
/// This is the SRS-compliant implementation
pub async fn execute_arbitrage<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    zrx: &crate::zrx::ZrxClient,
    usdc_amount: U256,       // Starting USDC amount
    pool_fee: u32,           // Uniswap pool fee tier (500, 3000, 10000)
    slippage_bps: u32,       // Slippage for 0x sell leg
) -> Result<ArbTradeReport> {
    let wallet_addr = wallet.default_signer().address();
    
    // Get balances before
    let balances_before = crate::wallet::get_balances(
        provider, wallet_addr, crate::config::WMON, crate::config::USDC
    ).await?;
    
    println!("\n🔄 Executing Arbitrage: BUY on Uniswap → SELL via 0x");
    balances_before.print();
    
    // ========== LEG 1: BUY MON on Uniswap ==========
    println!("\n📈 LEG 1: Buying MON on Uniswap...");
    
    // Ensure USDC approval to SwapRouter02
    ensure_approval(
        provider, wallet, 
        crate::config::USDC, 
        crate::config::UNISWAP_SWAP_ROUTER, 
        usdc_amount
    ).await?;
    
    // Calculate minimum MON out (with slippage protection)
    // For now, use 0 and rely on the trade report to show actual slippage
    let min_mon_out = U256::ZERO;  // TODO: Calculate from pool price
    
    let buy_result = execute_uniswap_buy(
        provider, wallet, usdc_amount, min_mon_out, pool_fee
    ).await?;
    
    if !buy_result.success {
        return Err(eyre::eyre!("Uniswap BUY leg failed"));
    }
    buy_result.print();
    
    // Get MON balance after buy
    let mid_balances = crate::wallet::get_balances(
        provider, wallet_addr, crate::config::WMON, crate::config::USDC
    ).await?;
    let mon_received = mid_balances.mon_balance.saturating_sub(balances_before.mon_balance);
    
    let mon_human = mon_received.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
    println!("  MON received: {:.6}", mon_human);
    
    // ========== LEG 2: SELL MON via 0x ==========
    println!("\n📉 LEG 2: Selling MON via 0x...");
    
    // Ensure WMON approval to AllowanceHolder
    ensure_approval(
        provider, wallet,
        crate::config::WMON,
        crate::config::ALLOWANCE_HOLDER,
        mon_received
    ).await?;
    
    // Get 0x quote for selling MON
    let sell_quote = zrx.get_quote(
        crate::config::WMON,
        crate::config::USDC,
        &mon_received.to_string(),
        &format!("{:?}", wallet_addr),
        slippage_bps,
    ).await?;
    
    let usdc_expected: f64 = sell_quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;
    println!("  Expected USDC: {:.6}", usdc_expected);
    
    // Execute 0x sell
    let sell_result = execute_0x_swap(provider, wallet, &sell_quote).await?;
    sell_result.print();
    
    // ========== Calculate Results ==========
    let balances_after = crate::wallet::get_balances(
        provider, wallet_addr, crate::config::WMON, crate::config::USDC
    ).await?;
    
    let usdc_before = balances_before.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
    let usdc_after = balances_after.usdc_balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
    let profit_loss = usdc_after - usdc_before;
    
    Ok(ArbTradeReport {
        buy_result,
        sell_result,
        usdc_before: balances_before.usdc_balance,
        usdc_after: balances_after.usdc_balance,
        mon_traded: mon_received,
        profit_loss_usdc: profit_loss,
    })
}

#[derive(Debug)]
pub struct ArbTradeReport {
    pub buy_result: ExecutionResult,
    pub sell_result: ExecutionResult,
    pub usdc_before: U256,
    pub usdc_after: U256,
    pub mon_traded: U256,
    pub profit_loss_usdc: f64,
}

impl ArbTradeReport {
    pub fn print(&self) {
        let usdc_before = self.usdc_before.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        let usdc_after = self.usdc_after.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        let mon_traded = self.mon_traded.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        
        println!("\n╔═══════════════════════════════════════════════════╗");
        println!("║     ARBITRAGE EXECUTION REPORT                    ║");
        println!("║     BUY on Uniswap → SELL via 0x                  ║");
        println!("╠═══════════════════════════════════════════════════╣");
        println!("║ BUY LEG (Uniswap)                                 ║");
        println!("║   Tx:     {:?}", self.buy_result.tx_hash);
        println!("║   Status: {}", if self.buy_result.success { "✅ SUCCESS" } else { "❌ FAILED" });
        println!("║   Gas:    {} / {}", self.buy_result.gas_used, self.buy_result.gas_limit);
        println!("║   Time:   {}ms", self.buy_result.execution_time_ms);
        println!("╠═══════════════════════════════════════════════════╣");
        println!("║ SELL LEG (0x)                                     ║");
        println!("║   Tx:     {:?}", self.sell_result.tx_hash);
        println!("║   Status: {}", if self.sell_result.success { "✅ SUCCESS" } else { "❌ FAILED" });
        println!("║   Gas:    {} / {}", self.sell_result.gas_used, self.sell_result.gas_limit);
        println!("║   Time:   {}ms", self.sell_result.execution_time_ms);
        println!("╠═══════════════════════════════════════════════════╣");
        println!("║ SUMMARY                                           ║");
        println!("║   MON Traded:    {:.6}", mon_traded);
        println!("║   USDC Before:   {:.6}", usdc_before);
        println!("║   USDC After:    {:.6}", usdc_after);
        println!("║   Profit/Loss:   {:.6} USDC", self.profit_loss_usdc);
        println!("╚═══════════════════════════════════════════════════╝");
    }
}
```
```

---

### Task 3: Create `src/trader.rs` - Trade Orchestration

This module orchestrates the full arbitrage trade:

```rust
//! Trade orchestration - combines detection with execution

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use eyre::Result;
use std::str::FromStr;

use crate::{config, execution, wallet, zrx};

#[derive(Debug)]
pub struct TradeParams {
    pub amount_mon: f64,        // Amount of MON to trade (e.g., 10.0)
    pub slippage_bps: u32,      // Slippage in basis points (e.g., 100 = 1%)
    pub min_profit_bps: u32,    // Minimum profit to proceed (e.g., 30 = 0.3%)
}

#[derive(Debug)]
pub struct TradeReport {
    pub direction: String,
    pub amount_in: String,
    pub expected_out: String,
    pub actual_out: String,
    pub slippage_realized: f64,
    pub mon_balance_before: U256,
    pub mon_balance_after: U256,
    pub usdc_balance_before: U256,
    pub usdc_balance_after: U256,
    pub profit_loss: f64,
    pub execution_result: execution::ExecutionResult,
}

impl TradeReport {
    pub fn print(&self) {
        let mon_before = self.mon_balance_before.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let mon_after = self.mon_balance_after.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
        let usdc_before = self.usdc_balance_before.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        let usdc_after = self.usdc_balance_after.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
        
        println!("\n╔═══════════════════════════════════════════════════╗");
        println!("║            TRADE EXECUTION REPORT                 ║");
        println!("╠═══════════════════════════════════════════════════╣");
        println!("║ Direction:        {:<30} ║", self.direction);
        println!("║ Amount In:        {:<30} ║", self.amount_in);
        println!("║ Expected Out:     {:<30} ║", self.expected_out);
        println!("║ Actual Out:       {:<30} ║", self.actual_out);
        println!("║ Slippage:         {:<28.4}% ║", self.slippage_realized);
        println!("╠═══════════════════════════════════════════════════╣");
        println!("║ BALANCE CHANGES                                   ║");
        println!("║ MON Before:       {:<28.6} ║", mon_before);
        println!("║ MON After:        {:<28.6} ║", mon_after);
        println!("║ USDC Before:      {:<28.6} ║", usdc_before);
        println!("║ USDC After:       {:<28.6} ║", usdc_after);
        println!("╠═══════════════════════════════════════════════════╣");
        println!("║ P/L:              {:<28.6} ║", self.profit_loss);
        println!("║ Tx Hash:          {:?} ║", self.execution_result.tx_hash);
        println!("║ Status:           {:<30} ║", 
            if self.execution_result.success { "✅ SUCCESS" } else { "❌ FAILED" });
        println!("║ Execution Time:   {:<26}ms ║", self.execution_result.execution_time_ms);
        println!("╚═══════════════════════════════════════════════════╝\n");
    }
}

/// Execute a SELL via 0x (MON → USDC)
/// This is the simplest test - just sell MON for USDC through 0x
pub async fn execute_0x_sell<P: Provider>(
    provider: &P,
    wallet: &EthereumWallet,
    zrx: &zrx::ZrxClient,
    params: &TradeParams,
) -> Result<TradeReport> {
    let wallet_addr = wallet.default_signer().address();
    
    // Get balances before
    let balances_before = wallet::get_balances(
        provider, 
        wallet_addr, 
        config::WMON, 
        config::USDC
    ).await?;
    
    println!("\n🔄 Starting 0x SELL trade...");
    println!("Selling {} MON for USDC via 0x", params.amount_mon);
    balances_before.print();
    
    // Convert MON amount to wei (18 decimals)
    let sell_amount = (params.amount_mon * 1e18) as u128;
    let sell_amount_str = sell_amount.to_string();
    
    // Ensure approval to AllowanceHolder
    execution::ensure_approval(
        provider,
        wallet,
        config::WMON,
        config::ALLOWANCE_HOLDER,
        U256::from(sell_amount),
    ).await?;
    
    // Get quote from 0x
    println!("Fetching 0x quote...");
    let quote = zrx.get_quote(
        config::WMON,
        config::USDC,
        &sell_amount_str,
        &format!("{:?}", wallet_addr),
        params.slippage_bps,
    ).await?;
    
    let expected_usdc = quote.buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;
    let min_usdc = quote.min_buy_amount.parse::<f64>().unwrap_or(0.0) / 1e6;
    
    println!("Quote received:");
    println!("  Expected USDC: {:.6}", expected_usdc);
    println!("  Min USDC:      {:.6}", min_usdc);
    
    // Execute the swap
    println!("Executing swap...");
    let exec_result = execution::execute_0x_swap(provider, wallet, &quote).await?;
    exec_result.print();
    
    // Get balances after
    let balances_after = wallet::get_balances(
        provider, 
        wallet_addr, 
        config::WMON, 
        config::USDC
    ).await?;
    
    // Calculate actual output
    let usdc_received = balances_after.usdc_balance
        .checked_sub(balances_before.usdc_balance)
        .unwrap_or(U256::ZERO);
    let actual_usdc = usdc_received.to_string().parse::<f64>().unwrap_or(0.0) / 1e6;
    
    // Calculate slippage
    let slippage = if expected_usdc > 0.0 {
        ((expected_usdc - actual_usdc) / expected_usdc) * 100.0
    } else {
        0.0
    };
    
    Ok(TradeReport {
        direction: "SELL MON → USDC via 0x".to_string(),
        amount_in: format!("{} MON", params.amount_mon),
        expected_out: format!("{:.6} USDC", expected_usdc),
        actual_out: format!("{:.6} USDC", actual_usdc),
        slippage_realized: slippage,
        mon_balance_before: balances_before.mon_balance,
        mon_balance_after: balances_after.mon_balance,
        usdc_balance_before: balances_before.usdc_balance,
        usdc_balance_after: balances_after.usdc_balance,
        profit_loss: actual_usdc - (params.amount_mon * 0.029),  // Rough estimate
        execution_result: exec_result,
    })
}
```

---

### Task 4: Update `src/main.rs` - Add Test Mode

Add a command-line flag for single trade test mode:

```rust
// Add to main.rs

mod wallet;
mod execution;
mod trader;

use clap::Parser;
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;

#[derive(Parser, Debug)]
#[command(name = "monad-arb-bot")]
#[command(about = "Monad Arbitrage Bot")]
struct Args {
    /// Run a single test trade (sell MON via 0x)
    #[arg(long)]
    test_trade: bool,
    
    /// Run a full arbitrage test (buy + sell cycle)
    #[arg(long)]
    test_arb: bool,
    
    /// Amount of MON to trade in test-trade mode
    #[arg(long, default_value = "10.0")]
    trade_amount: f64,
    
    /// Amount of USDC to use in test-arb mode
    #[arg(long, default_value = "5.0")]
    usdc_amount: f64,
    
    /// Slippage tolerance in basis points (100 = 1%)
    #[arg(long, default_value = "100")]
    slippage_bps: u32,
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
    let wallet = EthereumWallet::from(signer);
    
    let rpc_url = env::var("MONAD_RPC_URL")
        .unwrap_or_else(|_| "https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string());
    
    let provider = ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect_http(rpc_url.parse()?);
    
    let wallet_addr = wallet.default_signer().address();
    
    println!("==========================================");
    println!("  Monad Arbitrage Bot");
    println!("  Wallet: {:?}", wallet_addr);
    println!("==========================================\n");
    
    // Initialize 0x client
    let zrx = zrx::ZrxClient::new()?;
    
    if args.test_trade {
        // SINGLE TRADE TEST MODE
        println!("🧪 TEST TRADE MODE");
        println!("Amount: {} MON", args.trade_amount);
        println!("Slippage: {}bps ({}%)", args.slippage_bps, args.slippage_bps as f64 / 100.0);
        
        // Get initial balance
        let initial = wallet::get_balances(
            &provider, 
            wallet_addr, 
            config::WMON, 
            config::USDC
        ).await?;
        
        println!("\n📊 Starting Balance:");
        initial.print();
        
        let params = trader::TradeParams {
            amount_mon: args.trade_amount,
            slippage_bps: args.slippage_bps,
            min_profit_bps: 30,
        };
        
        // Execute test trade (sell MON via 0x)
        let report = trader::execute_0x_sell(&provider, &wallet, &zrx, &params).await?;
        report.print();
        
        // Final balance
        let final_balance = wallet::get_balances(
            &provider, 
            wallet_addr, 
            config::WMON, 
            config::USDC
        ).await?;
        
        println!("📊 Final Balance:");
        final_balance.print();
        
        println!("✅ Test trade complete!");
        return Ok(());
    }
    
    if args.test_arb {
        // FULL ARBITRAGE TEST MODE: BUY on Uniswap → SELL via 0x
        println!("🧪 ARBITRAGE TEST MODE");
        println!("Strategy: BUY on Uniswap → SELL via 0x");
        println!("USDC Amount: ${}", args.usdc_amount);
        println!("Pool Fee: {}bps", args.pool_fee);
        println!("Slippage: {}bps ({}%)", args.slippage_bps, args.slippage_bps as f64 / 100.0);
        
        // Get initial balance
        let initial = wallet::get_balances(
            &provider, 
            wallet_addr, 
            config::WMON, 
            config::USDC
        ).await?;
        
        println!("\n📊 Starting Balance:");
        initial.print();
        
        // Convert USDC amount to 6 decimals
        let usdc_amount = U256::from((args.usdc_amount * 1_000_000.0) as u128);
        
        // Execute full arbitrage: BUY on Uniswap → SELL via 0x
        let report = execution::execute_arbitrage(
            &provider,
            &wallet,
            &zrx,
            usdc_amount,
            args.pool_fee,       // Uniswap pool fee tier
            args.slippage_bps,   // 0x slippage
        ).await?;
        
        report.print();
        
        // Final balance
        let final_balance = wallet::get_balances(
            &provider, 
            wallet_addr, 
            config::WMON, 
            config::USDC
        ).await?;
        
        println!("📊 Final Balance:");
        final_balance.print();
        
        println!("✅ Arbitrage test complete!");
        return Ok(());
    }
    
    // ... existing monitoring loop code ...
}
```

---

### Task 5: Add Config Constants

Update `src/config.rs`:

```rust
// Add these to config.rs

// Uniswap V3 SwapRouter02 - VERIFIED from MonadVision
pub const UNISWAP_SWAP_ROUTER: &str = "0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900";

// 0x AllowanceHolder - VERIFIED (same address on all EVM chains)
pub const ALLOWANCE_HOLDER: &str = "0x0000000000001fF3684f28c67538d4D072C22734";

// 0x API Quote endpoint
pub const ZRX_QUOTE_ENDPOINT: &str = "/swap/allowance-holder/quote";

// Execution settings
pub const DEFAULT_SLIPPAGE_BPS: u32 = 100;  // 1%
pub const DEFAULT_POOL_FEE: u32 = 500;      // 0.05% fee tier (most common for stables)
pub const GAS_BUFFER: u64 = 5_000;
pub const GAS_PRICE_BUMP_PCT: u64 = 110;    // 10% bump
```

---

### Task 6: Update `Cargo.toml`

Add required dependencies:

```toml
[dependencies]
# Existing deps...
clap = { version = "4", features = ["derive"] }
```

---

## Environment Variables Required

Create/update `.env`:

```env
# RPC endpoint
MONAD_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY

# Wallet private key (NO 0x prefix)
PRIVATE_KEY=your_private_key_here

# 0x API key
ZRX_API_KEY=your_0x_api_key_here
```

---

## Testing Commands

### Step 1: Verify Setup
```bash
cargo run -- --test-trade --trade-amount 0
# Should print balances without executing
```

### Step 2: Small Test Trade (Single Sell)
```bash
cargo run -- --test-trade --trade-amount 10 --slippage-bps 100
```

### Step 3: Full Arbitrage Test
```bash
cargo run -- --test-arb --usdc-amount 5 --slippage-bps 100
```

Expected output for test-trade:
```
==========================================
  Monad Arbitrage Bot
  Wallet: 0x...
==========================================

🧪 TEST TRADE MODE
Amount: 10 MON
Slippage: 100bps (1.0%)

📊 Starting Balance:
Wallet: 0x...
  MON Balance:  1100.000000 MON
  USDC Balance: 0.000000 USDC

🔄 Starting 0x SELL trade...
Selling 10 MON for USDC via 0x
...
Fetching 0x quote...
Quote received:
  Expected USDC: 0.291340
  Min USDC:      0.288426

Executing swap...

========== EXECUTION RESULT ==========
  Tx Hash:       0x...
  Status:        ✅ SUCCESS
  Gas Used:      150000
  Gas Limit:     155000
  Exec Time:     450ms
=======================================

╔═══════════════════════════════════════════════════╗
║            TRADE EXECUTION REPORT                 ║
╠═══════════════════════════════════════════════════╣
║ Direction:        SELL MON → USDC via 0x         ║
║ Amount In:        10 MON                          ║
║ Expected Out:     0.291340 USDC                   ║
║ Actual Out:       0.290500 USDC                   ║
║ Slippage:         0.2883%                         ║
...
╚═══════════════════════════════════════════════════╝

📊 Final Balance:
Wallet: 0x...
  MON Balance:  1090.000000 MON
  USDC Balance: 0.290500 USDC

✅ Test trade complete!
```

---

## Critical Implementation Notes

### 1. NEVER Approve to Settler Contract
```rust
// ❌ WRONG - Don't do this
approve(quote.transaction.to, amount)  // This is the Settler!

// ✅ CORRECT
approve(ALLOWANCE_HOLDER, amount)  // Always use AllowanceHolder
```

### 2. Monad Gas Model
```rust
// ❌ WRONG - Over-estimated gas wastes money
.gas_limit(500_000)  // You pay for all 500k!

// ✅ CORRECT - Tight limit + small buffer
.gas_limit(quote_gas + 5_000)
```

### 3. Transaction Value Field
The `value` field in 0x quotes is usually "0" for token swaps. Only non-zero when selling native MON (not WMON).

### 4. Nonce Management
For single trades, let the provider handle nonces. For rapid sequential trades, implement local nonce tracking.

---

## Success Criteria

The implementation is successful when:

### For `--test-trade` (Single Sell):
1. ✅ `cargo run -- --test-trade --trade-amount 10` executes without errors
2. ✅ Transaction is confirmed on Monad
3. ✅ MON balance decreases by ~10
4. ✅ USDC balance increases proportionally
5. ✅ Trade report shows realistic slippage (<2%)
6. ✅ Execution time is <1 second

### For `--test-arb` (Full Arbitrage Cycle):
1. ✅ `cargo run -- --test-arb --usdc-amount 5` executes without errors
2. ✅ TWO transactions confirmed on Monad (buy + sell)
3. ✅ USDC balance changes (profit or loss is fine for testing)
4. ✅ Both legs show different routing sources
5. ✅ Total execution time is <2 seconds

---

## File Structure After Implementation

```
src/
├── config.rs       # Constants (updated)
├── main.rs         # Entry point with test mode (updated)
├── pools.rs        # Pool queries (existing)
├── zrx.rs          # 0x API client (updated with quote)
├── wallet.rs       # NEW: Balance & approval management
├── execution.rs    # NEW: Trade execution
└── trader.rs       # NEW: Trade orchestration
```

---

## Next Steps After Successful Tests

Once both test modes work:

1. Integrate execution into the main monitoring loop
2. Add profit threshold checking before executing
3. Add retry logic with gas bumps for failed transactions
4. Implement local nonce management for speed
5. Optimize for speed (<200ms detection-to-execution)
6. Add Telegram/Discord alerts for executed trades
