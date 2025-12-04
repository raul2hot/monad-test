# Phase 2: Atomic Swap Testing - Implementation Instructions

## For: Claude Code Opus
## Project: Monad Mainnet Arbitrage Bot
## Goal: Execute single test swaps on each DEX to capture real execution data

---

## CRITICAL CONTEXT

**DO NOT MODIFY** the existing price monitoring functionality. This is an **addition** to the codebase.

**Monad-Specific Warning**: On Monad, you pay for GAS LIMIT, not gas used. Set precise gas limits.

---

## 1. ROUTER ADDRESSES (Verified)

```rust
// Add to src/config.rs

// Router Addresses (Monad Mainnet)
pub const UNISWAP_SWAP_ROUTER: Address = address!("fE31F71C1b106EAc32F1A19239c9a9A72ddfb900");
pub const PANCAKE_SMART_ROUTER: Address = address!("21114915Ac6d5A2e156931e20B20b038dEd0Be7C");
pub const LFJ_LB_ROUTER: Address = address!("18556DA13313f3532c54711497A8FedAC273220E");
pub const MONDAY_SWAP_ROUTER: Address = address!("FE951b693A2FE54BE5148614B109E316B567632F");
```

---

## 2. FILE STRUCTURE TO CREATE

```
src/
├── main.rs              # MODIFY: Add CLI subcommands
├── config.rs            # MODIFY: Add router addresses
├── execution/           # NEW DIRECTORY
│   ├── mod.rs           # Module exports
│   ├── swap.rs          # Core swap execution logic
│   ├── report.rs        # Execution report generation
│   └── routers/         # Router-specific implementations
│       ├── mod.rs
│       ├── uniswap_v3.rs
│       ├── pancake_v3.rs
│       ├── lfj.rs
│       └── monday.rs
```

---

## 3. STEP-BY-STEP IMPLEMENTATION

### Step 3.1: Update Cargo.toml

Add/verify these dependencies:

```toml
[dependencies]
# Existing deps should remain...
clap = { version = "4", features = ["derive"] }
# alloy should already have "signers" feature - verify it does
```

### Step 3.2: Update src/config.rs

Add after the existing constants:

```rust
// ============== ROUTER ADDRESSES ==============

pub const UNISWAP_SWAP_ROUTER: Address = alloy::primitives::address!("fE31F71C1b106EAc32F1A19239c9a9A72ddfb900");
pub const PANCAKE_SMART_ROUTER: Address = alloy::primitives::address!("21114915Ac6d5A2e156931e20B20b038dEd0Be7C");
pub const LFJ_LB_ROUTER: Address = alloy::primitives::address!("18556DA13313f3532c54711497A8FedAC273220E");
pub const MONDAY_SWAP_ROUTER: Address = alloy::primitives::address!("FE951b693A2FE54BE5148614B109E316B567632F");

// ============== ROUTER CONFIG ==============

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterType {
    UniswapV3,
    PancakeV3,
    LfjLB,
    MondayTrade,
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub name: &'static str,
    pub address: Address,
    pub router_type: RouterType,
    pub pool_address: Address,  // The specific pool to use
    pub pool_fee: u32,          // Fee tier for V3 pools (in hundredths of bps, e.g., 3000 = 0.3%)
}

pub fn get_routers() -> Vec<RouterConfig> {
    vec![
        RouterConfig {
            name: "Uniswap",
            address: UNISWAP_SWAP_ROUTER,
            router_type: RouterType::UniswapV3,
            pool_address: alloy::primitives::address!("659bd0bc4167ba25c62e05656f78043e7ed4a9da"),
            pool_fee: 3000,  // 0.30%
        },
        RouterConfig {
            name: "PancakeSwap1",
            address: PANCAKE_SMART_ROUTER,
            router_type: RouterType::PancakeV3,
            pool_address: alloy::primitives::address!("63e48B725540A3Db24ACF6682a29f877808C53F2"),
            pool_fee: 500,  // 0.05%
        },
        RouterConfig {
            name: "PancakeSwap2",
            address: PANCAKE_SMART_ROUTER,
            router_type: RouterType::PancakeV3,
            pool_address: alloy::primitives::address!("85717A98d195c9306BBf7c9523Ba71F044Fea0f7"),
            pool_fee: 2500,  // 0.25%
        },
        RouterConfig {
            name: "LFJ",
            address: LFJ_LB_ROUTER,
            router_type: RouterType::LfjLB,
            pool_address: alloy::primitives::address!("5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"),
            pool_fee: 15,  // 0.15% (stored as bps for LFJ)
        },
        RouterConfig {
            name: "MondayTrade",
            address: MONDAY_SWAP_ROUTER,
            router_type: RouterType::MondayTrade,
            pool_address: alloy::primitives::address!("8f889ba499c0a176fb8f233d9d35b1c132eb868c"),
            pool_fee: 3000,  // 0.30%
        },
    ]
}

pub fn get_router_by_name(name: &str) -> Option<RouterConfig> {
    get_routers().into_iter().find(|r| r.name.to_lowercase() == name.to_lowercase())
}
```

### Step 3.3: Create src/execution/mod.rs

```rust
pub mod routers;
pub mod swap;
pub mod report;

pub use swap::{SwapParams, SwapResult, SwapDirection, execute_swap};
pub use report::print_swap_report;
```

### Step 3.4: Create src/execution/routers/mod.rs

```rust
pub mod uniswap_v3;
pub mod pancake_v3;
pub mod lfj;
pub mod monday;

use alloy::primitives::{Address, Bytes, U256};
use eyre::Result;

use crate::config::RouterType;

/// Build swap calldata for the appropriate router
pub fn build_swap_calldata(
    router_type: RouterType,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    pool_fee: u32,
    deadline: u64,
) -> Result<Bytes> {
    match router_type {
        RouterType::UniswapV3 => {
            uniswap_v3::build_exact_input_single(
                token_in, token_out, pool_fee, recipient, amount_in, amount_out_min
            )
        }
        RouterType::PancakeV3 => {
            pancake_v3::build_exact_input_single(
                token_in, token_out, pool_fee, recipient, amount_in, amount_out_min
            )
        }
        RouterType::LfjLB => {
            lfj::build_swap_exact_tokens_for_tokens(
                token_in, token_out, amount_in, amount_out_min, recipient, deadline
            )
        }
        RouterType::MondayTrade => {
            monday::build_exact_input_single(
                token_in, token_out, pool_fee, recipient, amount_in, amount_out_min
            )
        }
    }
}
```

### Step 3.5: Create src/execution/routers/uniswap_v3.rs

```rust
use alloy::primitives::{Address, Bytes, U256, U160};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Uniswap V3 SwapRouter02 interface
sol! {
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee.try_into().unwrap_or(3000),
        recipient,
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,  // No price limit
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
```

### Step 3.6: Create src/execution/routers/pancake_v3.rs

```rust
use alloy::primitives::{Address, Bytes, U256, U160};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// PancakeSwap V3 SmartRouter - same interface as Uniswap V3
sol! {
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee.try_into().unwrap_or(500),
        recipient,
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
```

### Step 3.7: Create src/execution/routers/lfj.rs

```rust
use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// LFJ LBRouter interface
// Note: LFJ uses a path-based routing system
sol! {
    #[derive(Debug)]
    struct Version {
        uint256 v;
    }

    #[derive(Debug)]
    struct Path {
        uint256[] pairBinSteps;
        uint8[] versions;
        address[] tokenPath;
    }

    #[derive(Debug)]
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        Path memory path,
        address to,
        uint256 deadline
    ) external returns (uint256 amountOut);
}

pub fn build_swap_exact_tokens_for_tokens(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: u64,
) -> Result<Bytes> {
    // For LFJ, we need to specify the path
    // binStep for WMON/USDC pool is typically 15 or 20 - this may need adjustment
    // Version 2 = V2.1 (Liquidity Book)
    let path = Path {
        pairBinSteps: vec![U256::from(15)],  // Bin step - may need to query from pool
        versions: vec![2],  // V2.1
        tokenPath: vec![token_in, token_out],
    };

    let calldata = swapExactTokensForTokensCall {
        amountIn: amount_in,
        amountOutMin: amount_out_min,
        path,
        to: recipient,
        deadline: U256::from(deadline),
    }.abi_encode();

    Ok(Bytes::from(calldata))
}
```

### Step 3.8: Create src/execution/routers/monday.rs

```rust
use alloy::primitives::{Address, Bytes, U256, U160};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;

// Monday Trade uses V3-style interface
sol! {
    #[derive(Debug)]
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    #[derive(Debug)]
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

pub fn build_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let params = ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: fee.try_into().unwrap_or(3000),
        recipient,
        amountIn: amount_in,
        amountOutMinimum: amount_out_min,
        sqrtPriceLimitX96: U160::ZERO,
    };

    let calldata = exactInputSingleCall { params }.abi_encode();
    Ok(Bytes::from(calldata))
}
```

### Step 3.9: Create src/execution/swap.rs

```rust
use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::{eyre, Result};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{RouterConfig, RouterType, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS};
use super::routers::build_swap_calldata;

// ERC20 interface for approvals and balance checks
sol! {
    #[derive(Debug)]
    function approve(address spender, uint256 amount) external returns (bool);
    
    #[derive(Debug)]
    function allowance(address owner, address spender) external view returns (uint256);
    
    #[derive(Debug)]
    function balanceOf(address account) external view returns (uint256);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    Buy,   // USDC -> WMON
    Sell,  // WMON -> USDC
}

#[derive(Debug, Clone)]
pub struct SwapParams {
    pub router: RouterConfig,
    pub direction: SwapDirection,
    pub amount_in: f64,          // Human-readable amount
    pub slippage_bps: u32,       // e.g., 100 = 1%
    pub expected_price: f64,     // From price monitor
}

#[derive(Debug, Clone)]
pub struct SwapResult {
    pub dex_name: String,
    pub direction: SwapDirection,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_in_human: f64,
    pub amount_out: U256,
    pub amount_out_human: f64,
    pub expected_price: f64,
    pub executed_price: f64,
    pub price_impact_bps: i32,
    pub gas_used: u64,
    pub gas_price: u128,
    pub gas_cost_wei: U256,
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Convert human amount to U256 with proper decimals
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = 10u64.pow(decimals as u32);
    let wei_amount = (amount * multiplier as f64) as u64;
    U256::from(wei_amount)
}

/// Convert U256 to human-readable with proper decimals
fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}

/// Ensure router has approval to spend tokens
pub async fn ensure_approval<P: Provider>(
    provider: &P,
    signer: &PrivateKeySigner,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    let wallet_address = signer.address();
    
    // Check current allowance
    let allowance_call = allowanceCall {
        owner: wallet_address,
        spender,
    };
    
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(token)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(allowance_call.abi_encode())));
    
    let result = provider.call(tx).await?;
    let current_allowance = U256::from_be_slice(&result);
    
    if current_allowance >= amount {
        println!("  ✓ Sufficient allowance already exists");
        return Ok(());
    }
    
    println!("  → Approving router to spend tokens...");
    
    // Need to approve - use max uint256 for convenience
    let approve_call = approveCall {
        spender,
        amount: U256::MAX,
    };
    
    let wallet = EthereumWallet::from(signer.clone());
    
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(token)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(approve_call.abi_encode())))
        .gas_limit(100_000);  // Approvals are cheap
    
    // Get provider with signer
    let provider_with_signer = provider.clone().with_signer(wallet);
    
    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;
    
    if receipt.status() {
        println!("  ✓ Approval successful: {:?}", receipt.transaction_hash);
        Ok(())
    } else {
        Err(eyre!("Approval transaction failed"))
    }
}

/// Execute a swap on the specified DEX
pub async fn execute_swap<P: Provider + Clone>(
    provider: &P,
    signer: &PrivateKeySigner,
    params: SwapParams,
) -> Result<SwapResult> {
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer.clone());
    
    // Determine token addresses and decimals based on direction
    let (token_in, token_out, decimals_in, decimals_out) = match params.direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS, USDC_DECIMALS, WMON_DECIMALS),
    };
    
    // Convert to wei
    let amount_in = to_wei(params.amount_in, decimals_in);
    
    // Calculate minimum output based on expected price and slippage
    let expected_out = match params.direction {
        SwapDirection::Sell => params.amount_in * params.expected_price,  // WMON * price = USDC
        SwapDirection::Buy => params.amount_in / params.expected_price,   // USDC / price = WMON
    };
    
    let slippage_multiplier = 1.0 - (params.slippage_bps as f64 / 10000.0);
    let min_out = expected_out * slippage_multiplier;
    let amount_out_min = to_wei(min_out, decimals_out);
    
    println!("\n  Swap Details:");
    println!("    Amount In:  {} {}", params.amount_in, if params.direction == SwapDirection::Sell { "WMON" } else { "USDC" });
    println!("    Expected Out: {:.6} {}", expected_out, if params.direction == SwapDirection::Sell { "USDC" } else { "WMON" });
    println!("    Min Out ({:.2}% slip): {:.6}", params.slippage_bps as f64 / 100.0, min_out);
    
    // Ensure approval
    ensure_approval(provider, signer, token_in, params.router.address, amount_in).await?;
    
    // Get deadline (5 minutes from now)
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;
    
    // Build swap calldata
    let calldata = build_swap_calldata(
        params.router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        wallet_address,
        params.router.pool_fee,
        deadline,
    )?;
    
    println!("  → Executing swap on {}...", params.router.name);
    
    // Check balance before
    let balance_before_call = balanceOfCall { account: wallet_address };
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(token_out)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(balance_before_call.abi_encode())));
    let result = provider.call(tx).await?;
    let balance_before = U256::from_be_slice(&result);
    
    // Estimate gas first (CRITICAL for Monad - you pay the limit!)
    let estimate_tx = alloy::rpc::types::TransactionRequest::default()
        .to(params.router.address)
        .from(wallet_address)
        .input(alloy::rpc::types::TransactionInput::new(calldata.clone()));
    
    let gas_estimate = provider.estimate_gas(estimate_tx).await.unwrap_or(250_000);
    let gas_limit = gas_estimate + (gas_estimate / 10);  // Add 10% buffer
    
    println!("    Gas Estimate: {} (using limit: {})", gas_estimate, gas_limit);
    
    // Build and send transaction
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(params.router.address)
        .input(alloy::rpc::types::TransactionInput::new(calldata))
        .gas_limit(gas_limit);
    
    let provider_with_signer = provider.clone().with_signer(wallet);
    
    let start = std::time::Instant::now();
    let send_result = provider_with_signer.send_transaction(tx).await;
    
    match send_result {
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            let elapsed = start.elapsed();
            
            // Check balance after
            let balance_after_call = balanceOfCall { account: wallet_address };
            let tx = alloy::rpc::types::TransactionRequest::default()
                .to(token_out)
                .input(alloy::rpc::types::TransactionInput::new(Bytes::from(balance_after_call.abi_encode())));
            let result = provider.call(tx).await?;
            let balance_after = U256::from_be_slice(&result);
            
            let amount_out = balance_after.saturating_sub(balance_before);
            let amount_out_human = from_wei(amount_out, decimals_out);
            
            // Calculate executed price
            let executed_price = match params.direction {
                SwapDirection::Sell => amount_out_human / params.amount_in,  // USDC / WMON
                SwapDirection::Buy => params.amount_in / amount_out_human,   // USDC / WMON
            };
            
            let price_impact_bps = ((executed_price - params.expected_price) / params.expected_price * 10000.0) as i32;
            
            let gas_used = receipt.gas_used;
            let gas_price = receipt.effective_gas_price;
            let gas_cost_wei = U256::from(gas_used) * U256::from(gas_price);
            
            println!("  ✓ Swap completed in {:?}", elapsed);
            println!("    TX: {:?}", receipt.transaction_hash);
            
            Ok(SwapResult {
                dex_name: params.router.name.to_string(),
                direction: params.direction,
                token_in,
                token_out,
                amount_in,
                amount_in_human: params.amount_in,
                amount_out,
                amount_out_human,
                expected_price: params.expected_price,
                executed_price,
                price_impact_bps,
                gas_used,
                gas_price,
                gas_cost_wei,
                tx_hash: format!("{:?}", receipt.transaction_hash),
                success: receipt.status(),
                error: if receipt.status() { None } else { Some("Transaction reverted".to_string()) },
            })
        }
        Err(e) => {
            Ok(SwapResult {
                dex_name: params.router.name.to_string(),
                direction: params.direction,
                token_in,
                token_out,
                amount_in,
                amount_in_human: params.amount_in,
                amount_out: U256::ZERO,
                amount_out_human: 0.0,
                expected_price: params.expected_price,
                executed_price: 0.0,
                price_impact_bps: 0,
                gas_used: 0,
                gas_price: 0,
                gas_cost_wei: U256::ZERO,
                tx_hash: String::new(),
                success: false,
                error: Some(e.to_string()),
            })
        }
    }
}
```

### Step 3.10: Create src/execution/report.rs

```rust
use chrono::Local;
use crate::execution::{SwapResult, SwapDirection};
use crate::config::{WMON_DECIMALS, USDC_DECIMALS};

pub fn print_swap_report(result: &SwapResult) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  SWAP EXECUTION REPORT | {}", timestamp);
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  DEX: {}", result.dex_name);
    println!("  Direction: {} ({} → {})", 
        if result.direction == SwapDirection::Sell { "SELL" } else { "BUY" },
        if result.direction == SwapDirection::Sell { "WMON" } else { "USDC" },
        if result.direction == SwapDirection::Sell { "USDC" } else { "WMON" }
    );
    println!();
    
    if result.success {
        println!("  INPUT:");
        println!("    Amount In:      {:.6} {}", 
            result.amount_in_human,
            if result.direction == SwapDirection::Sell { "WMON" } else { "USDC" }
        );
        println!("    Expected Price: {:.6} USDC/WMON", result.expected_price);
        
        let expected_out = if result.direction == SwapDirection::Sell {
            result.amount_in_human * result.expected_price
        } else {
            result.amount_in_human / result.expected_price
        };
        println!("    Expected Out:   {:.6} {}", 
            expected_out,
            if result.direction == SwapDirection::Sell { "USDC" } else { "WMON" }
        );
        println!();
        
        println!("  OUTPUT:");
        println!("    Actual Out:     {:.6} {}", 
            result.amount_out_human,
            if result.direction == SwapDirection::Sell { "USDC" } else { "WMON" }
        );
        println!("    Executed Price: {:.6} USDC/WMON", result.executed_price);
        
        let impact_sign = if result.price_impact_bps >= 0 { "+" } else { "" };
        let impact_color = if result.price_impact_bps >= 0 { "32" } else { "31" };  // green/red
        println!("    Price Impact:   \x1b[1;{}m{}{}bps ({}{:.2}%)\x1b[0m", 
            impact_color,
            impact_sign, 
            result.price_impact_bps,
            impact_sign,
            result.price_impact_bps as f64 / 100.0
        );
        println!();
        
        println!("  GAS:");
        println!("    Gas Used:       {}", result.gas_used);
        println!("    Gas Price:      {} gwei", result.gas_price / 1_000_000_000);
        
        let gas_cost_mon = result.gas_cost_wei.to::<u128>() as f64 / 1e18;
        println!("    Gas Cost:       {:.8} MON", gas_cost_mon);
        println!();
        
        println!("  SLIPPAGE ANALYSIS:");
        let slippage_amount = result.amount_out_human - expected_out;
        let slippage_pct = (slippage_amount / expected_out) * 100.0;
        println!("    Deviation:      {:.6} {} ({:+.4}%)", 
            slippage_amount.abs(),
            if result.direction == SwapDirection::Sell { "USDC" } else { "WMON" },
            slippage_pct
        );
        println!();
        
        println!("  TX: {}", result.tx_hash);
    } else {
        println!("  \x1b[1;31mSWAP FAILED\x1b[0m");
        if let Some(ref err) = result.error {
            println!("  Error: {}", err);
        }
    }
    
    println!();
    println!("═══════════════════════════════════════════════════════════════");
}

pub fn print_comparison_report(results: &[SwapResult]) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  MULTI-DEX COMPARISON REPORT | {}", timestamp);
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  {:<15} │ {:>10} │ {:>12} │ {:>10} │ {:>8}",
        "DEX", "Exec Price", "Price Impact", "Gas Used", "Status"
    );
    println!("  {}", "─".repeat(60));
    
    for result in results {
        let status = if result.success { "✓" } else { "✗" };
        let status_color = if result.success { "32" } else { "31" };
        
        if result.success {
            println!("  {:<15} │ {:>10.6} │ {:>+10}bps │ {:>10} │ \x1b[1;{}m{}\x1b[0m",
                result.dex_name,
                result.executed_price,
                result.price_impact_bps,
                result.gas_used,
                status_color,
                status
            );
        } else {
            println!("  {:<15} │ {:>10} │ {:>12} │ {:>10} │ \x1b[1;{}m{}\x1b[0m",
                result.dex_name,
                "N/A",
                "N/A",
                "N/A",
                status_color,
                status
            );
        }
    }
    
    println!();
    
    // Find best result
    let successful: Vec<_> = results.iter().filter(|r| r.success).collect();
    if !successful.is_empty() {
        let best = successful.iter()
            .max_by(|a, b| a.executed_price.partial_cmp(&b.executed_price).unwrap())
            .unwrap();
        
        println!("  Best Execution: {} @ {:.6} USDC/WMON", best.dex_name, best.executed_price);
    }
    
    println!("═══════════════════════════════════════════════════════════════");
}
```

### Step 3.11: Update src/main.rs

Replace the entire file with:

```rust
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

use config::{get_all_pools, get_lfj_pool, get_monday_trade_pool, get_v3_pools, get_router_by_name, POLL_INTERVAL_MS};
use display::{display_prices, init_arb_log};
use execution::{SwapParams, SwapDirection, execute_swap, print_swap_report};
use execution::report::print_comparison_report;
use multicall::fetch_prices_batched;
use pools::{create_lfj_active_id_call, create_lfj_bin_step_call, create_slot0_call, PriceCall, PoolPrice};

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
    
    let result = execute_swap(&provider, &signer, params).await?;
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
        
        match execute_swap(&provider, &signer, params).await {
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
    }
}
```

---

## 4. ENVIRONMENT SETUP

Update `.env` file:

```env
# Existing
MONAD_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/YOUR_KEY

# NEW - Add these
PRIVATE_KEY=your_private_key_without_0x_prefix
```

**IMPORTANT**: The private key should NOT have the `0x` prefix.

---

## 5. BUILD AND TEST COMMANDS

### Build:
```bash
cargo build --release
```

### Run price monitor (default):
```bash
cargo run --release
# or
cargo run --release -- monitor
```

### Test single DEX swap:
```bash
# Sell 1 WMON on Uniswap
cargo run --release -- test-swap --dex uniswap --amount 1.0 --direction sell --slippage 100

# Sell 0.5 WMON on PancakeSwap1 
cargo run --release -- test-swap --dex pancakeswap1 --amount 0.5 --direction sell --slippage 100

# Buy WMON with 0.05 USDC on LFJ
cargo run --release -- test-swap --dex lfj --amount 0.05 --direction buy --slippage 150
```

### Test all DEXes:
```bash
cargo run --release -- test-all --amount 0.5 --direction sell --slippage 150
```

---

## 6. EXPECTED OUTPUT FORMAT

```
══════════════════════════════════════════════════════════════
  SWAP EXECUTION REPORT | 2025-12-04 20:30:45
══════════════════════════════════════════════════════════════

  DEX: PancakeSwap1
  Direction: SELL (WMON → USDC)

  INPUT:
    Amount In:      1.000000 WMON
    Expected Price: 0.029600 USDC/WMON
    Expected Out:   0.029600 USDC

  OUTPUT:
    Actual Out:     0.029450 USDC
    Executed Price: 0.029450 USDC/WMON
    Price Impact:   -51bps (-0.51%)

  GAS:
    Gas Used:       145230
    Gas Price:      1 gwei
    Gas Cost:       0.00014523 MON

  SLIPPAGE ANALYSIS:
    Deviation:      0.000150 USDC (-0.5068%)

  TX: 0x1234...5678

══════════════════════════════════════════════════════════════
```

---

## 7. TROUBLESHOOTING

### Common Issues:

1. **"insufficient allowance"**: The approve transaction may have failed. Check the token approval on block explorer.

2. **"execution reverted"**: 
   - Check slippage is high enough (try 200-300 bps for testing)
   - LFJ may need different binStep value - check pool on explorer

3. **"gas estimation failed"**:
   - The swap may fail simulation. Try with higher slippage.
   - Check you have enough token balance.

4. **PancakeSwap SmartRouter issues**:
   - SmartRouter may use different interface. If fails, try direct pool swap.

5. **LFJ routing issues**:
   - The binStep must match the pool's actual binStep
   - Query pool's getBinStep() to verify

### Debug Mode:
```bash
RUST_LOG=debug cargo run --release -- test-swap --dex uniswap --amount 0.1 --direction sell
```

---

## 8. VERIFICATION STEPS

Before running with real funds:

1. **Verify router addresses** on MonadVision block explorer
2. **Start with smallest amounts** (0.1 WMON or 0.01 USDC)
3. **Check token balances** before each test
4. **Monitor gas usage** - Monad charges full gas limit

---

## 9. KNOWN LIMITATIONS

1. **LFJ binStep**: Hardcoded to 15 - may need adjustment based on actual pool
2. **No MEV protection**: Transactions go directly to public mempool
3. **Single-hop only**: No multi-hop routing for complex paths
4. **Approvals use MAX_UINT**: Consider using exact amounts in production

---

## 10. NEXT STEPS (After Testing)

Once single swaps work on all DEXes:

1. Create atomic arbitrage contract (buy DEX A → sell DEX B in one tx)
2. Add real-time opportunity detection with automatic execution
3. Integrate with MEV protection (aPriori or FastLane)
4. Add triangular arbitrage paths
