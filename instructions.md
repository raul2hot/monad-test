# ATOMIC ARBITRAGE IMPLEMENTATION - CLAUDE CODE INSTRUCTIONS

## OBJECTIVE
Replace the current 2-transaction arbitrage execution in `src/execution/fast_arb.rs` with a single atomic transaction that calls a deployed smart contract. The `auto-arb` command must continue to work exactly as before, but execute via the atomic contract.

---

## PHASE 1: CREATE SOLIDITY CONTRACT

### Task 1.1: Create contract directory structure
```
mkdir -p contracts/src
mkdir -p contracts/script
mkdir -p contracts/lib
```

### Task 1.2: Create `contracts/foundry.toml`
```toml
[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
optimizer = true
optimizer_runs = 1000000
via_ir = false

[rpc_endpoints]
monad = "${MONAD_RPC_URL}"

[etherscan]
monad = { key = "${MONAD_EXPLORER_API_KEY}", chain = 143, url = "https://api.monadexplorer.com/api" }
```

### Task 1.3: Create `contracts/src/MonadAtomicArb.sol`
```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "./interfaces/IERC20.sol";

/// @title MonadAtomicArb
/// @notice Atomic arbitrage contract for Monad mainnet
/// @dev Executes two swaps in single TX, reverts if unprofitable
contract MonadAtomicArb {
    address public immutable owner;
    
    // Token addresses (Monad mainnet)
    address public constant WMON = 0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A;
    address public constant USDC = 0x754704Bc059F8C67012fEd69BC8A327a5aafb603;
    
    // Router addresses (Monad mainnet)
    address public constant UNISWAP_ROUTER = 0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900;
    address public constant PANCAKE_ROUTER = 0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C;
    address public constant MONDAY_ROUTER = 0xFE951b693A2FE54BE5148614B109E316B567632F;
    address public constant LFJ_ROUTER = 0x18556DA13313f3532c54711497A8FedAC273220E;
    
    // Router enum matching Rust RouterType
    enum Router { Uniswap, PancakeSwap, MondayTrade, LFJ }
    
    error OnlyOwner();
    error SwapFailed(uint8 swapIndex);
    error Unprofitable(uint256 wmonBefore, uint256 wmonAfter);
    error InvalidRouter();
    
    event ArbExecuted(
        uint8 indexed sellRouter,
        uint8 indexed buyRouter,
        uint256 wmonIn,
        uint256 wmonOut,
        int256 profit
    );
    
    constructor() {
        owner = msg.sender;
    }
    
    modifier onlyOwner() {
        if (msg.sender != owner) revert OnlyOwner();
        _;
    }
    
    /// @notice Setup max approvals for all routers (call once after deployment)
    function setupApprovals() external onlyOwner {
        // Approve WMON to all routers
        IERC20(WMON).approve(UNISWAP_ROUTER, type(uint256).max);
        IERC20(WMON).approve(PANCAKE_ROUTER, type(uint256).max);
        IERC20(WMON).approve(MONDAY_ROUTER, type(uint256).max);
        IERC20(WMON).approve(LFJ_ROUTER, type(uint256).max);
        
        // Approve USDC to all routers
        IERC20(USDC).approve(UNISWAP_ROUTER, type(uint256).max);
        IERC20(USDC).approve(PANCAKE_ROUTER, type(uint256).max);
        IERC20(USDC).approve(MONDAY_ROUTER, type(uint256).max);
        IERC20(USDC).approve(LFJ_ROUTER, type(uint256).max);
    }
    
    /// @notice Execute atomic arbitrage: WMON -> USDC -> WMON
    /// @param sellRouter Router to sell WMON for USDC (higher price)
    /// @param sellRouterData Pre-encoded calldata for sell swap
    /// @param buyRouter Router to buy WMON with USDC (lower price)  
    /// @param buyRouterData Pre-encoded calldata for buy swap
    /// @param minProfit Minimum WMON profit required (reverts if not met)
    /// @return profit The WMON profit achieved
    function executeArb(
        Router sellRouter,
        bytes calldata sellRouterData,
        Router buyRouter,
        bytes calldata buyRouterData,
        uint256 minProfit
    ) external onlyOwner returns (int256 profit) {
        uint256 wmonBefore = IERC20(WMON).balanceOf(address(this));
        
        // Swap 1: WMON -> USDC on sellRouter
        address sellAddr = _getRouterAddress(sellRouter);
        (bool success1,) = sellAddr.call(sellRouterData);
        if (!success1) revert SwapFailed(1);
        
        // Swap 2: USDC -> WMON on buyRouter  
        address buyAddr = _getRouterAddress(buyRouter);
        (bool success2,) = buyAddr.call(buyRouterData);
        if (!success2) revert SwapFailed(2);
        
        uint256 wmonAfter = IERC20(WMON).balanceOf(address(this));
        
        // Calculate profit (can be negative)
        profit = int256(wmonAfter) - int256(wmonBefore);
        
        // Revert if below minimum profit threshold
        if (wmonAfter < wmonBefore + minProfit) {
            revert Unprofitable(wmonBefore, wmonAfter);
        }
        
        emit ArbExecuted(
            uint8(sellRouter),
            uint8(buyRouter),
            wmonBefore,
            wmonAfter,
            profit
        );
    }
    
    /// @notice Withdraw tokens (emergency or profit collection)
    function withdrawToken(address token, uint256 amount) external onlyOwner {
        IERC20(token).transfer(owner, amount);
    }
    
    /// @notice Withdraw all of a token
    function withdrawAllToken(address token) external onlyOwner {
        uint256 balance = IERC20(token).balanceOf(address(this));
        IERC20(token).transfer(owner, balance);
    }
    
    /// @notice Get router address from enum
    function _getRouterAddress(Router router) internal pure returns (address) {
        if (router == Router.Uniswap) return UNISWAP_ROUTER;
        if (router == Router.PancakeSwap) return PANCAKE_ROUTER;
        if (router == Router.MondayTrade) return MONDAY_ROUTER;
        if (router == Router.LFJ) return LFJ_ROUTER;
        revert InvalidRouter();
    }
    
    /// @notice Check current balances
    function getBalances() external view returns (uint256 wmon, uint256 usdc) {
        wmon = IERC20(WMON).balanceOf(address(this));
        usdc = IERC20(USDC).balanceOf(address(this));
    }
    
    receive() external payable {}
}
```

### Task 1.4: Create `contracts/src/interfaces/IERC20.sol`
```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
}
```

### Task 1.5: Create `contracts/script/Deploy.s.sol`
```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import "../src/MonadAtomicArb.sol";

contract DeployScript is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        
        vm.startBroadcast(deployerPrivateKey);
        
        MonadAtomicArb arb = new MonadAtomicArb();
        console.log("MonadAtomicArb deployed at:", address(arb));
        
        // Setup approvals
        arb.setupApprovals();
        console.log("Approvals configured");
        
        vm.stopBroadcast();
    }
}
```

---

## PHASE 2: MODIFY RUST CODEBASE

### Task 2.1: Add contract address to `src/config.rs`

Add after line 11 (after `MULTICALL3_ADDRESS`):
```rust
// Atomic Arbitrage Contract (deployed by user - UPDATE THIS AFTER DEPLOYMENT)
pub const ATOMIC_ARB_CONTRACT: Address = alloy::primitives::address!("0000000000000000000000000000000000000000");
```

Add a comment above it:
```rust
// TODO: Update this address after deploying MonadAtomicArb contract
```

### Task 2.2: Create new file `src/execution/atomic_arb.rs`

```rust
//! Atomic DEX-to-DEX Arbitrage Module
//!
//! Executes arbitrage via smart contract in a SINGLE transaction.
//! Benefits over fast_arb.rs:
//! - No MEV front-running between swaps
//! - Atomic: reverts if unprofitable
//! - Single gas payment
//! - ~500-800ms total execution vs ~2600ms

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use chrono::Local;
use eyre::{eyre, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::timeout;

use crate::config::{
    RouterConfig, RouterType, WMON_ADDRESS, USDC_ADDRESS, WMON_DECIMALS, USDC_DECIMALS,
    ATOMIC_ARB_CONTRACT,
};
use crate::nonce::next_nonce;
use super::routers::build_swap_calldata;
use super::SwapDirection;

// Monad mainnet chain ID
const MONAD_CHAIN_ID: u64 = 143;

// Gas buffer for atomic arb (tighter than 2-TX version)
const GAS_BUFFER_PERCENT: u64 = 12;

// Router enum matching Solidity contract
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum ContractRouter {
    Uniswap = 0,
    PancakeSwap = 1,
    MondayTrade = 2,
    LFJ = 3,
}

impl From<RouterType> for ContractRouter {
    fn from(rt: RouterType) -> Self {
        match rt {
            RouterType::UniswapV3 => ContractRouter::Uniswap,
            RouterType::PancakeV3 => ContractRouter::PancakeSwap,
            RouterType::MondayTrade => ContractRouter::MondayTrade,
            RouterType::LfjLB => ContractRouter::LFJ,
        }
    }
}

// Contract interface
sol! {
    #[derive(Debug)]
    function executeArb(
        uint8 sellRouter,
        bytes calldata sellRouterData,
        uint8 buyRouter,
        bytes calldata buyRouterData,
        uint256 minProfit
    ) external returns (int256 profit);

    #[derive(Debug)]
    function getBalances() external view returns (uint256 wmon, uint256 usdc);

    // Custom errors for decoding
    error SwapFailed(uint8 swapIndex);
    error Unprofitable(uint256 wmonBefore, uint256 wmonAfter);
}

/// Result of atomic arbitrage execution
#[derive(Debug, Clone)]
pub struct AtomicArbResult {
    pub tx_hash: String,
    pub success: bool,
    pub profit_wmon: f64,
    pub profit_bps: i32,
    pub gas_used: u64,
    pub gas_limit: u64,
    pub gas_cost_mon: f64,
    pub execution_time_ms: u128,
    pub sell_dex: String,
    pub buy_dex: String,
    pub wmon_in: f64,
    pub error: Option<String>,
}

/// Convert human amount to U256 with proper decimals
fn to_wei(amount: f64, decimals: u8) -> U256 {
    let multiplier = U256::from(10u64).pow(U256::from(decimals));
    let amount_scaled = (amount * 1e18) as u128;
    U256::from(amount_scaled) * multiplier / U256::from(10u64).pow(U256::from(18u8))
}

/// Convert U256 to human-readable with proper decimals
fn from_wei(amount: U256, decimals: u8) -> f64 {
    let divisor = 10u64.pow(decimals as u32) as f64;
    let amount_u128: u128 = amount.try_into().unwrap_or(0);
    amount_u128 as f64 / divisor
}

/// Query contract WMON/USDC balances
pub async fn query_contract_balances<P: Provider>(provider: &P) -> Result<(f64, f64)> {
    let call = getBalancesCall {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(ATOMIC_ARB_CONTRACT)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(call.abi_encode())));
    
    let result = provider.call(tx).await?;
    let decoded = getBalancesCall::abi_decode_returns(&result)?;
    
    Ok((from_wei(decoded.wmon, WMON_DECIMALS), from_wei(decoded.usdc, USDC_DECIMALS)))
}

/// Build swap calldata for the contract to forward to router
fn build_router_calldata(
    router: &RouterConfig,
    direction: SwapDirection,
    amount_in: U256,
    amount_out_min: U256,
) -> Result<Bytes> {
    let (token_in, token_out) = match direction {
        SwapDirection::Sell => (WMON_ADDRESS, USDC_ADDRESS),
        SwapDirection::Buy => (USDC_ADDRESS, WMON_ADDRESS),
    };

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    // IMPORTANT: recipient is the CONTRACT address, not the wallet
    build_swap_calldata(
        router.router_type,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
        ATOMIC_ARB_CONTRACT,  // Contract receives tokens, not wallet
        router.pool_fee,
        deadline,
    )
}

/// Execute atomic arbitrage via smart contract
///
/// # Arguments
/// * `provider_with_signer` - Provider with wallet for sending transactions
/// * `sell_router` - Router to sell WMON on (higher price)
/// * `buy_router` - Router to buy WMON on (lower price)
/// * `amount` - Amount of WMON to arb
/// * `sell_price` - Expected price on sell DEX
/// * `buy_price` - Expected price on buy DEX
/// * `slippage_bps` - Slippage tolerance in basis points
/// * `min_profit_bps` - Minimum profit required (0 = any profit)
/// * `gas_price` - Pre-fetched gas price
pub async fn execute_atomic_arb<P: Provider>(
    provider_with_signer: &P,
    signer_address: Address,
    sell_router: &RouterConfig,
    buy_router: &RouterConfig,
    amount: f64,
    sell_price: f64,
    buy_price: f64,
    slippage_bps: u32,
    min_profit_bps: i32,
    gas_price: u128,
) -> Result<AtomicArbResult> {
    let start = std::time::Instant::now();
    
    // Validate contract address is set
    if ATOMIC_ARB_CONTRACT == Address::ZERO {
        return Err(eyre!("ATOMIC_ARB_CONTRACT not set in config.rs. Deploy contract first!"));
    }

    // Calculate amounts
    let wmon_in_wei = to_wei(amount, WMON_DECIMALS);
    let expected_usdc = amount * sell_price;
    let slippage_mult = 1.0 - (slippage_bps as f64 / 10000.0);
    let min_usdc_out = expected_usdc * slippage_mult;
    let min_usdc_out_wei = to_wei(min_usdc_out, USDC_DECIMALS);

    // For swap 2, use conservative USDC estimate
    let usdc_for_swap2 = min_usdc_out * 0.999; // Tiny buffer for dust
    let usdc_for_swap2_wei = to_wei(usdc_for_swap2, USDC_DECIMALS);
    let expected_wmon_back = usdc_for_swap2 / buy_price;
    let min_wmon_out = expected_wmon_back * slippage_mult;
    let min_wmon_out_wei = to_wei(min_wmon_out, WMON_DECIMALS);

    // Calculate minimum profit for contract
    let min_profit_wmon = if min_profit_bps > 0 {
        amount * (min_profit_bps as f64 / 10000.0)
    } else {
        0.0
    };
    let min_profit_wei = to_wei(min_profit_wmon, WMON_DECIMALS);

    println!("  Building atomic arb calldata...");
    println!("    WMON in: {:.6}", amount);
    println!("    Expected USDC: {:.6}", expected_usdc);
    println!("    Min USDC out: {:.6}", min_usdc_out);
    println!("    USDC for swap2: {:.6}", usdc_for_swap2);
    println!("    Expected WMON back: {:.6}", expected_wmon_back);
    println!("    Min profit: {:.6} WMON ({} bps)", min_profit_wmon, min_profit_bps);

    // Build calldata for both swaps
    let sell_calldata = build_router_calldata(
        sell_router,
        SwapDirection::Sell,
        wmon_in_wei,
        min_usdc_out_wei,
    )?;

    let buy_calldata = build_router_calldata(
        buy_router,
        SwapDirection::Buy,
        usdc_for_swap2_wei,
        min_wmon_out_wei,
    )?;

    // Build executeArb call
    let execute_call = executeArbCall {
        sellRouter: ContractRouter::from(sell_router.router_type) as u8,
        sellRouterData: sell_calldata,
        buyRouter: ContractRouter::from(buy_router.router_type) as u8,
        buyRouterData: buy_calldata,
        minProfit: min_profit_wei,
    };

    let calldata = Bytes::from(execute_call.abi_encode());

    // Estimate gas
    println!("  Estimating gas...");
    let estimate_tx = alloy::rpc::types::TransactionRequest::default()
        .to(ATOMIC_ARB_CONTRACT)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(calldata.clone()));

    let gas_estimate = match provider_with_signer.estimate_gas(estimate_tx).await {
        Ok(est) => {
            let with_buffer = est * (100 + GAS_BUFFER_PERCENT) / 100;
            println!("    Estimated: {} + {}% = {}", est, GAS_BUFFER_PERCENT, with_buffer);
            with_buffer
        }
        Err(e) => {
            // If estimation fails, the arb would likely fail
            return Ok(AtomicArbResult {
                tx_hash: String::new(),
                success: false,
                profit_wmon: 0.0,
                profit_bps: 0,
                gas_used: 0,
                gas_limit: 0,
                gas_cost_mon: 0.0,
                execution_time_ms: start.elapsed().as_millis(),
                sell_dex: sell_router.name.to_string(),
                buy_dex: buy_router.name.to_string(),
                wmon_in: amount,
                error: Some(format!("Gas estimation failed (arb likely unprofitable): {}", e)),
            });
        }
    };

    // Build and send transaction
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(ATOMIC_ARB_CONTRACT)
        .from(signer_address)
        .input(alloy::rpc::types::TransactionInput::new(calldata))
        .gas_limit(gas_estimate)
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(MONAD_CHAIN_ID);

    println!("  Sending atomic arb transaction...");
    let send_start = std::time::Instant::now();

    let pending = match timeout(
        Duration::from_secs(10),
        provider_with_signer.send_transaction(tx)
    ).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            return Ok(AtomicArbResult {
                tx_hash: String::new(),
                success: false,
                profit_wmon: 0.0,
                profit_bps: 0,
                gas_used: 0,
                gas_limit: gas_estimate,
                gas_cost_mon: 0.0,
                execution_time_ms: start.elapsed().as_millis(),
                sell_dex: sell_router.name.to_string(),
                buy_dex: buy_router.name.to_string(),
                wmon_in: amount,
                error: Some(format!("Send failed: {}", e)),
            });
        }
        Err(_) => {
            return Ok(AtomicArbResult {
                tx_hash: String::new(),
                success: false,
                profit_wmon: 0.0,
                profit_bps: 0,
                gas_used: 0,
                gas_limit: gas_estimate,
                gas_cost_mon: 0.0,
                execution_time_ms: start.elapsed().as_millis(),
                sell_dex: sell_router.name.to_string(),
                buy_dex: buy_router.name.to_string(),
                wmon_in: amount,
                error: Some("Send timeout".to_string()),
            });
        }
    };

    let tx_hash = *pending.tx_hash();
    println!("    TX sent: {:?}", tx_hash);

    // Wait for receipt (fast polling for Monad)
    println!("  Waiting for confirmation...");
    let receipt = match timeout(
        Duration::from_secs(15),
        wait_for_receipt_fast(provider_with_signer, tx_hash)
    ).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Ok(AtomicArbResult {
                tx_hash: format!("{:?}", tx_hash),
                success: false,
                profit_wmon: 0.0,
                profit_bps: 0,
                gas_used: 0,
                gas_limit: gas_estimate,
                gas_cost_mon: 0.0,
                execution_time_ms: start.elapsed().as_millis(),
                sell_dex: sell_router.name.to_string(),
                buy_dex: buy_router.name.to_string(),
                wmon_in: amount,
                error: Some(format!("Receipt error: {}", e)),
            });
        }
        Err(_) => {
            return Ok(AtomicArbResult {
                tx_hash: format!("{:?}", tx_hash),
                success: false,
                profit_wmon: 0.0,
                profit_bps: 0,
                gas_used: 0,
                gas_limit: gas_estimate,
                gas_cost_mon: 0.0,
                execution_time_ms: start.elapsed().as_millis(),
                sell_dex: sell_router.name.to_string(),
                buy_dex: buy_router.name.to_string(),
                wmon_in: amount,
                error: Some("Confirmation timeout".to_string()),
            });
        }
    };

    let gas_cost_wei = U256::from(gas_estimate) * U256::from(receipt.effective_gas_price);
    let gas_cost_mon = gas_cost_wei.to::<u128>() as f64 / 1e18;
    let exec_time = start.elapsed().as_millis();

    if receipt.status() {
        // Parse profit from logs/return value
        // For now, query contract balance change or estimate
        let estimated_profit = expected_wmon_back - amount;
        let profit_bps = if amount > 0.0 {
            (estimated_profit / amount * 10000.0) as i32
        } else {
            0
        };

        println!("  ✓ Atomic arb SUCCESS in {}ms", exec_time);

        Ok(AtomicArbResult {
            tx_hash: format!("{:?}", tx_hash),
            success: true,
            profit_wmon: estimated_profit,
            profit_bps,
            gas_used: receipt.gas_used,
            gas_limit: gas_estimate,
            gas_cost_mon,
            execution_time_ms: exec_time,
            sell_dex: sell_router.name.to_string(),
            buy_dex: buy_router.name.to_string(),
            wmon_in: amount,
            error: None,
        })
    } else {
        println!("  ✗ Atomic arb REVERTED (likely unprofitable)");
        
        Ok(AtomicArbResult {
            tx_hash: format!("{:?}", tx_hash),
            success: false,
            profit_wmon: 0.0,
            profit_bps: 0,
            gas_used: receipt.gas_used,
            gas_limit: gas_estimate,
            gas_cost_mon,
            execution_time_ms: exec_time,
            sell_dex: sell_router.name.to_string(),
            buy_dex: buy_router.name.to_string(),
            wmon_in: amount,
            error: Some("Transaction reverted (unprofitable or swap failed)".to_string()),
        })
    }
}

/// Fast receipt polling for Monad
async fn wait_for_receipt_fast<P: Provider>(
    provider: &P,
    tx_hash: alloy::primitives::TxHash,
) -> Result<alloy::rpc::types::TransactionReceipt> {
    use tokio::time::interval;
    
    let mut poll = interval(Duration::from_millis(20));
    
    for _ in 0..750 { // 15 seconds max
        poll.tick().await;
        if let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? {
            return Ok(receipt);
        }
    }
    
    Err(eyre!("Receipt timeout"))
}

/// Print atomic arb result
pub fn print_atomic_arb_result(result: &AtomicArbResult) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  ATOMIC ARB RESULT | {}", timestamp);
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  Route: WMON --({})-> USDC --({})-> WMON", result.sell_dex, result.buy_dex);
    println!("  Mode: ATOMIC (single transaction)");
    println!();
    println!("  Status: {}", if result.success { "SUCCESS ✓" } else { "FAILED ✗" });
    println!("  TX: {}", result.tx_hash);
    println!();
    println!("  WMON In:    {:>12.6}", result.wmon_in);
    
    let profit_color = if result.profit_wmon >= 0.0 { "32" } else { "31" };
    println!("  Profit:     \x1b[1;{}m{:>+12.6} WMON ({:+} bps)\x1b[0m",
        profit_color, result.profit_wmon, result.profit_bps);
    println!();
    println!("  Gas Used:   {:>12} / {} limit", result.gas_used, result.gas_limit);
    println!("  Gas Cost:   {:>12.6} MON", result.gas_cost_mon);
    println!("  Time:       {:>12} ms", result.execution_time_ms);

    if let Some(ref err) = result.error {
        println!();
        println!("  Error: {}", err);
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
}
```

### Task 2.3: Update `src/execution/mod.rs`

Add after line 4:
```rust
pub mod atomic_arb;
```

Add to exports at end of file:
```rust
pub use atomic_arb::{execute_atomic_arb, AtomicArbResult, print_atomic_arb_result, query_contract_balances};
```

### Task 2.4: Add `atomic-arb` command to `src/main.rs`

Add to the `Commands` enum (after `FastArb`):
```rust
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
    },
```

Add the handler function:
```rust
async fn run_atomic_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32, min_profit_bps: i32) -> Result<()> {
    use crate::execution::{execute_atomic_arb, print_atomic_arb_result};
    
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

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  ATOMIC ARB | {} -> {} (single TX)", sell_dex, buy_dex);
    println!("══════════════════════════════════════════════════════════════");

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
    ).await?;

    print_atomic_arb_result(&result);
    println!("  [TIMING] TOTAL: {:?}", total_start.elapsed());

    Ok(())
}
```

Add to match in main():
```rust
        Some(Commands::AtomicArb { sell_dex, buy_dex, amount, slippage, min_profit_bps }) => {
            run_atomic_arb(&sell_dex, &buy_dex, amount, slippage, min_profit_bps).await
        }
```

### Task 2.5: Modify `run_auto_arb` in `src/main.rs` to use atomic execution

Find the section in `run_auto_arb` where `execute_fast_arb` is called (around line 950-970) and replace it with atomic execution:

Replace:
```rust
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
```

With:
```rust
                // Use atomic arb if contract is deployed, otherwise fall back to fast_arb
                let use_atomic = crate::config::ATOMIC_ARB_CONTRACT != Address::ZERO;
                
                let (arb_success, arb_profit, arb_tx1, arb_tx2, arb_error) = if use_atomic {
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
                    ).await {
                        Ok(result) => {
                            print_atomic_arb_result(&result);
                            (result.success, result.profit_wmon, result.tx_hash.clone(), String::new(), result.error)
                        }
                        Err(e) => (false, 0.0, String::new(), String::new(), Some(e.to_string()))
                    }
                } else {
                    println!("  Using FAST execution (2 TXs) - deploy atomic contract for better results!");
                    match execute_fast_arb(
                        &provider_with_signer,
                        signer_address,
                        &sell_router,
                        &buy_router,
                        amount,
                        spread.sell_price,
                        spread.buy_price,
                        slippage,
                        gas_price,
                    ).await {
                        Ok(result) => {
                            print_fast_arb_result(&result, &spread.sell_pool, &spread.buy_pool);
                            (result.success, result.gross_profit_wmon, result.swap1_tx_hash.clone(), result.swap2_tx_hash.clone(), result.error)
                        }
                        Err(e) => (false, 0.0, String::new(), String::new(), Some(e.to_string()))
                    }
                };
```

**NOTE**: This requires adapting the subsequent code that uses `arb_result`. The key change is that atomic arb returns a single result with one TX hash, while fast_arb returns two.

### Task 2.6: Add necessary imports to `src/main.rs`

Add near the top imports:
```rust
use execution::{execute_atomic_arb, print_atomic_arb_result, AtomicArbResult};
```

---

## PHASE 3: ADD FUND/WITHDRAW COMMANDS

### Task 3.1: Add `fund-contract` command to CLI

Add to Commands enum:
```rust
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
```

### Task 3.2: Implement fund/withdraw handlers

```rust
async fn run_fund_contract(amount: f64) -> Result<()> {
    use alloy::sol;
    use alloy::sol_types::SolCall;
    
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
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(143);

    println!("Funding contract with {} WMON...", amount);
    
    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;
    
    if receipt.status() {
        println!("✓ Funded contract with {} WMON", amount);
        println!("  TX: {:?}", receipt.transaction_hash);
    } else {
        println!("✗ Transfer failed");
    }
    
    Ok(())
}

async fn run_withdraw_contract(amount: f64) -> Result<()> {
    use alloy::sol;
    use alloy::sol_types::SolCall;
    
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
        .nonce(next_nonce())
        .max_fee_per_gas(gas_price + (gas_price / 10))
        .max_priority_fee_per_gas(gas_price / 10)
        .with_chain_id(143);

    let pending = provider_with_signer.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;
    
    if receipt.status() {
        println!("✓ Withdrawal successful");
        println!("  TX: {:?}", receipt.transaction_hash);
    } else {
        println!("✗ Withdrawal failed");
    }
    
    Ok(())
}

async fn run_contract_balance() -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let (wmon, usdc) = query_contract_balances(&provider).await?;
    
    println!("\n══════════════════════════════════════════════════════════════");
    println!("  ATOMIC ARB CONTRACT BALANCES");
    println!("══════════════════════════════════════════════════════════════");
    println!("  Contract: {:?}", ATOMIC_ARB_CONTRACT);
    println!("  WMON: {:>18.6}", wmon);
    println!("  USDC: {:>18.6}", usdc);
    println!("══════════════════════════════════════════════════════════════");
    
    Ok(())
}
```

Add to main() match:
```rust
        Some(Commands::FundContract { amount }) => {
            run_fund_contract(amount).await
        }
        Some(Commands::WithdrawContract { amount }) => {
            run_withdraw_contract(amount).await
        }
        Some(Commands::ContractBalance) => {
            run_contract_balance().await
        }
```

---

## VERIFICATION CHECKLIST

After implementation, verify:

1. [ ] `contracts/` directory exists with all Solidity files
2. [ ] `src/config.rs` has `ATOMIC_ARB_CONTRACT` constant
3. [ ] `src/execution/atomic_arb.rs` exists and compiles
4. [ ] `src/execution/mod.rs` exports atomic_arb module
5. [ ] `src/main.rs` has AtomicArb, FundContract, WithdrawContract, ContractBalance commands
6. [ ] `cargo build` succeeds
7. [ ] Commands work: `cargo run -- atomic-arb --help`

---

## NOTES FOR CLAUDE CODE

1. Do NOT modify the existing `fast_arb.rs` - keep it as fallback
2. The atomic contract recipient for swaps must be the CONTRACT address, not the wallet
3. Router enum values must match exactly: Uniswap=0, PancakeSwap=1, MondayTrade=2, LFJ=3
4. The contract address constant will be 0x0...0 until user deploys and updates it
5. All existing functionality must continue to work
6. Add appropriate error messages when contract is not deployed
