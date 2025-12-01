# Arbitrage Opportunity Simulation & Verification Module

## Implementation Instructions for Claude Code

**Project:** Monad Mainnet Arbitrage Bot MVP  
**Module:** Simulation & Verification  
**Priority:** HIGH - Must complete before any live execution  
**Date:** December 1, 2025

---

## 1. Problem Statement

The current MVP detects arbitrage opportunities but **cannot verify if they are real**. The logs show:

```
--- Opportunity #1 [CROSS-DEX] ---
   Path: WMON -> gMON -> WMON
   Profit: 0.3621% (36 bps)
   Avg Fee: 3.00 bps   ← SUSPICIOUS: Most DEXs charge 30+ bps
```

**Critical Issues to Solve:**
1. Fee calculation appears incorrect (3 bps vs expected 30+ bps)
2. No slippage accounting for trade size vs liquidity
3. No simulation of actual swap execution
4. No validation that "profitable on paper" = "profitable on-chain"

---

## 2. Requirements

### 2.1 Core Simulation Features

| Feature | Description | Priority |
|---------|-------------|----------|
| `eth_call` Simulation | Simulate full swap path without broadcasting | P0 |
| Fee Verification | Query actual fee tiers from pool contracts | P0 |
| Liquidity Depth Check | Get pool liquidity to calculate max viable trade | P0 |
| Slippage Calculation | Estimate price impact for given trade size | P1 |
| Profit Validation | Compare simulated output vs input + all costs | P0 |
| Same-Block Quotes | Ensure all prices from same block number | P0 |

### 2.2 Module Structure

Create a new module: `src/simulation/`

```
src/simulation/
├── mod.rs              # Module exports
├── simulator.rs        # Main simulation orchestrator
├── fee_validator.rs    # DEX fee verification
├── liquidity.rs        # Liquidity depth analysis
├── quote_fetcher.rs    # Atomic multi-pool quotes
└── profit_calculator.rs # Net profit after all costs
```

---

## 3. Technical Specifications

### 3.1 Monad RPC Capabilities

Monad supports standard Ethereum RPC with some differences:

| Method | Supported | Notes |
|--------|-----------|-------|
| `eth_call` | ✅ Yes | Primary simulation method |
| `debug_traceCall` | ✅ Yes | Requires Alchemy Pro/paid tier |
| `eth_estimateGas` | ✅ Yes | For gas cost estimation |
| `eth_createAccessList` | ✅ Yes | Optimize gas simulation |

**Monad-Specific RPC Differences:**
- `debug_traceCall` requires explicit trace options parameter (even if empty `{}`)
- Default tracer is `callTracer` (not struct logs)
- `eth_getLogs` has max 1000 block range, recommend 1-10 blocks
- No reorgs occur (single-slot finality)

### 3.2 DEX Interfaces

#### 3.2.1 Uniswap V3 / PancakeSwap V3

```solidity
// Pool interface for quotes
interface IUniswapV3Pool {
    function slot0() external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint16 observationIndex,
        uint16 observationCardinality,
        uint16 observationCardinalityNext,
        uint8 feeProtocol,
        bool unlocked
    );
    
    function liquidity() external view returns (uint128);
    
    function fee() external view returns (uint24); // In hundredths of bps (e.g., 3000 = 0.30%)
    
    function token0() external view returns (address);
    function token1() external view returns (address);
}

// Quoter for simulating swaps (READ-ONLY, no state changes)
interface IQuoterV2 {
    struct QuoteExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint24 fee;
        uint160 sqrtPriceLimitX96;
    }
    
    function quoteExactInputSingle(QuoteExactInputSingleParams memory params)
        external
        returns (
            uint256 amountOut,
            uint160 sqrtPriceX96After,
            uint32 initializedTicksCrossed,
            uint256 gasEstimate
        );
}
```

**Fee Tier Mapping (Uniswap V3 style):**
- 100 = 0.01% (1 bp)
- 500 = 0.05% (5 bps)
- 3000 = 0.30% (30 bps)
- 10000 = 1.00% (100 bps)

#### 3.2.2 LFJ (Liquidity Book / Trader Joe V2)

LFJ uses a **bin-based** AMM model. Key interfaces:

```solidity
interface ILBPair {
    function getTokenX() external view returns (address);
    function getTokenY() external view returns (address);
    
    function getActiveId() external view returns (uint24);
    
    function getBin(uint24 id) external view returns (
        uint128 binReserveX,
        uint128 binReserveY
    );
    
    function getStaticFeeParameters() external view returns (
        uint16 baseFactor,
        uint16 filterPeriod,
        uint16 decayPeriod,
        uint16 reductionFactor,
        uint24 variableFeeControl,
        uint16 protocolShare,
        uint24 maxVolatilityAccumulator
    );
    
    // Get swap output
    function getSwapOut(uint128 amountIn, bool swapForY) 
        external view returns (
            uint128 amountInLeft,
            uint128 amountOut,
            uint128 fee
        );
        
    // Get swap input needed for desired output
    function getSwapIn(uint128 amountOut, bool swapForY)
        external view returns (
            uint128 amountIn,
            uint128 amountOutLeft,
            uint128 fee
        );
}

interface ILBRouter {
    function getSwapOut(
        ILBPair lbPair,
        uint128 amountIn,
        bool swapForY
    ) external view returns (
        uint128 amountInLeft,
        uint128 amountOut,
        uint128 fee
    );
}
```

**LFJ Fee Calculation:**
- Base fee + variable fee based on volatility
- Fee returned directly in `getSwapOut` response
- Typically 0.20% - 0.50% effective fee

#### 3.2.3 Uniswap V4

Uniswap V4 uses a singleton `PoolManager` pattern:

```solidity
interface IPoolManager {
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }
    
    function getSlot0(PoolId id) external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint16 protocolFee,
        uint24 lpFee
    );
    
    function getLiquidity(PoolId id) external view returns (uint128);
}
```

**V4 Fee Notes:**
- `fee` can be dynamic (8388608 = dynamic fee flag)
- Check `lpFee` from slot0 for actual current fee
- Hooks can modify fees per-swap

---

## 4. Implementation Guide

### 4.1 Fee Validator Module

**File:** `src/simulation/fee_validator.rs`

```rust
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;

pub struct PoolFeeInfo {
    pub pool_address: Address,
    pub dex_type: DexType,
    pub fee_bps: u32,           // Fee in basis points (e.g., 30 = 0.30%)
    pub is_dynamic: bool,       // True if fee can change per-swap
}

pub enum DexType {
    UniswapV3,
    PancakeSwapV3,
    UniswapV4,
    LFJ,
}

impl FeeValidator {
    /// Query the actual fee tier from a Uniswap V3 style pool
    pub async fn get_v3_pool_fee(&self, pool: Address) -> Result<u32> {
        // Call pool.fee() - returns fee in hundredths of bps
        // 3000 = 0.30% = 30 bps
        let fee: u32 = self.provider
            .call(/* encode fee() call */)
            .await?;
        
        Ok(fee / 100) // Convert to bps
    }
    
    /// Query fee from LFJ pool (includes dynamic component)
    pub async fn get_lfj_pool_fee(
        &self, 
        pool: Address, 
        amount_in: u128,
        swap_for_y: bool
    ) -> Result<u32> {
        // Call getSwapOut to get actual fee for this trade
        let (_, amount_out, fee) = self.call_get_swap_out(pool, amount_in, swap_for_y).await?;
        
        // Calculate effective fee in bps
        let effective_fee_bps = (fee as u64 * 10000) / (amount_in as u64);
        Ok(effective_fee_bps as u32)
    }
}
```

**CRITICAL:** The current MVP shows "Avg Fee: 3.00 bps" which is almost certainly wrong. Uniswap V3 returns fees as hundredths of bps (e.g., `3000` means 30 bps, not 3 bps). **Verify the fee parsing logic.**

### 4.2 Quote Fetcher Module

**File:** `src/simulation/quote_fetcher.rs`

```rust
pub struct AtomicQuote {
    pub block_number: u64,
    pub timestamp: u64,
    pub quotes: Vec<PoolQuote>,
}

pub struct PoolQuote {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
    pub fee_paid: U256,
    pub price: f64,            // token_out per token_in
    pub liquidity: U256,       // Current pool liquidity
}

impl QuoteFetcher {
    /// Fetch quotes from multiple pools atomically (same block)
    pub async fn get_atomic_quotes(
        &self,
        pools: &[PoolInfo],
        amount_in: U256,
        block: Option<BlockId>,
    ) -> Result<AtomicQuote> {
        let block = block.unwrap_or(BlockId::latest());
        let block_number = self.provider.get_block_number().await?;
        
        // Use multicall or sequential eth_call with same block
        let mut quotes = Vec::new();
        
        for pool in pools {
            let quote = match pool.dex_type {
                DexType::UniswapV3 | DexType::PancakeSwapV3 => {
                    self.quote_v3_pool(pool, amount_in, block).await?
                }
                DexType::LFJ => {
                    self.quote_lfj_pool(pool, amount_in, block).await?
                }
                DexType::UniswapV4 => {
                    self.quote_v4_pool(pool, amount_in, block).await?
                }
            };
            quotes.push(quote);
        }
        
        Ok(AtomicQuote {
            block_number,
            timestamp: /* get from block */,
            quotes,
        })
    }
    
    async fn quote_v3_pool(
        &self,
        pool: &PoolInfo,
        amount_in: U256,
        block: BlockId,
    ) -> Result<PoolQuote> {
        // Option 1: Use QuoterV2 if deployed
        // Option 2: Direct calculation from slot0 + liquidity
        
        // Get slot0 for current price
        let slot0 = self.call_slot0(pool.address, block).await?;
        let liquidity = self.call_liquidity(pool.address, block).await?;
        let fee = self.call_fee(pool.address, block).await?;
        
        // Calculate expected output using V3 math
        let amount_out = calculate_v3_swap_output(
            amount_in,
            slot0.sqrt_price_x96,
            liquidity,
            fee,
            pool.token0 == pool.token_in, // zeroForOne
        )?;
        
        Ok(PoolQuote {
            pool: pool.address,
            token_in: pool.token_in,
            token_out: pool.token_out,
            amount_in,
            amount_out,
            fee_paid: calculate_fee(amount_in, fee),
            price: amount_out.to::<f64>() / amount_in.to::<f64>(),
            liquidity: U256::from(liquidity),
        })
    }
}
```

### 4.3 Simulator Module

**File:** `src/simulation/simulator.rs`

```rust
pub struct SimulationResult {
    pub path: Vec<Address>,           // Token path
    pub pools: Vec<Address>,          // Pools used
    pub input_amount: U256,
    pub output_amount: U256,
    pub gross_profit_bps: i32,        // Before fees
    pub net_profit_bps: i32,          // After ALL fees
    pub total_fees_bps: u32,          // Sum of all fees
    pub flash_loan_fee_bps: u32,      // 9 bps for Neverland
    pub gas_cost_wei: U256,
    pub is_profitable: bool,
    pub confidence: SimulationConfidence,
    pub block_number: u64,
}

pub enum SimulationConfidence {
    High,      // eth_call simulation passed
    Medium,    // Quote-based calculation only
    Low,       // Stale data or estimation
}

impl Simulator {
    /// Full simulation of an arbitrage path
    pub async fn simulate_arb(
        &self,
        opportunity: &ArbOpportunity,
        input_amount: U256,
    ) -> Result<SimulationResult> {
        // 1. Get atomic quotes for all pools in path
        let quotes = self.quote_fetcher
            .get_atomic_quotes(&opportunity.pools, input_amount, None)
            .await?;
        
        // 2. Calculate output through entire path
        let mut current_amount = input_amount;
        let mut total_fees_bps = 0u32;
        
        for quote in &quotes.quotes {
            current_amount = quote.amount_out;
            total_fees_bps += self.fee_validator
                .get_pool_fee_bps(&quote.pool, quote.dex_type)
                .await?;
        }
        
        // 3. Add flash loan fee (Neverland = 9 bps)
        let flash_loan_fee_bps = 9u32;
        let flash_loan_cost = input_amount * U256::from(flash_loan_fee_bps) / U256::from(10000);
        
        // 4. Estimate gas cost
        let gas_estimate = self.estimate_gas(&opportunity.pools).await?;
        let gas_price = self.provider.get_gas_price().await?;
        let gas_cost_wei = gas_estimate * gas_price;
        
        // 5. Calculate net profit
        let gross_output = current_amount;
        let net_output = gross_output
            .saturating_sub(flash_loan_cost)
            .saturating_sub(gas_cost_wei);
        
        let gross_profit_bps = calculate_profit_bps(input_amount, gross_output);
        let net_profit_bps = calculate_profit_bps(input_amount, net_output);
        
        // 6. Run eth_call simulation for confidence
        let confidence = self.run_eth_call_simulation(opportunity, input_amount).await?;
        
        Ok(SimulationResult {
            path: opportunity.path.clone(),
            pools: opportunity.pools.iter().map(|p| p.address).collect(),
            input_amount,
            output_amount: net_output,
            gross_profit_bps,
            net_profit_bps,
            total_fees_bps: total_fees_bps + flash_loan_fee_bps,
            flash_loan_fee_bps,
            gas_cost_wei,
            is_profitable: net_profit_bps > 0,
            confidence,
            block_number: quotes.block_number,
        })
    }
    
    /// Simulate using eth_call (no state change, no gas spent)
    async fn run_eth_call_simulation(
        &self,
        opportunity: &ArbOpportunity,
        amount: U256,
    ) -> Result<SimulationConfidence> {
        // Encode the full swap sequence
        let calldata = self.encode_arb_execution(opportunity, amount)?;
        
        // Use eth_call to simulate
        let result = self.provider
            .call(&TransactionRequest::new()
                .to(self.executor_contract)
                .data(calldata)
                .from(self.bot_address))
            .block(BlockId::latest())
            .await;
        
        match result {
            Ok(output) => {
                // Decode output, verify profit
                let simulated_profit = decode_profit(output)?;
                if simulated_profit > U256::ZERO {
                    Ok(SimulationConfidence::High)
                } else {
                    Ok(SimulationConfidence::Low)
                }
            }
            Err(e) => {
                // Simulation reverted - would fail on-chain
                tracing::warn!("Simulation reverted: {:?}", e);
                Ok(SimulationConfidence::Low)
            }
        }
    }
}

fn calculate_profit_bps(input: U256, output: U256) -> i32 {
    if output >= input {
        let profit = output - input;
        ((profit * U256::from(10000)) / input).to::<i32>()
    } else {
        let loss = input - output;
        -(((loss * U256::from(10000)) / input).to::<i32>())
    }
}
```

### 4.4 Liquidity Depth Module

**File:** `src/simulation/liquidity.rs`

```rust
pub struct LiquidityInfo {
    pub pool: Address,
    pub total_liquidity: U256,
    pub max_trade_1pct_slippage: U256,  // Max trade for <1% price impact
    pub max_trade_05pct_slippage: U256, // Max trade for <0.5% price impact
}

impl LiquidityAnalyzer {
    pub async fn get_v3_liquidity(&self, pool: Address) -> Result<LiquidityInfo> {
        let liquidity: u128 = self.provider
            .call(/* encode liquidity() */)
            .await?;
        
        let slot0 = self.get_slot0(pool).await?;
        
        // Estimate max trade sizes for acceptable slippage
        // Rule of thumb: trade < 1% of liquidity for <0.5% slippage
        let max_trade_05pct = U256::from(liquidity) / U256::from(100);
        let max_trade_1pct = U256::from(liquidity) / U256::from(50);
        
        Ok(LiquidityInfo {
            pool,
            total_liquidity: U256::from(liquidity),
            max_trade_1pct_slippage: max_trade_1pct,
            max_trade_05pct_slippage: max_trade_05pct,
        })
    }
    
    pub async fn get_lfj_liquidity(&self, pool: Address) -> Result<LiquidityInfo> {
        // LFJ bins - sum liquidity in active bin and nearby bins
        let active_id = self.get_active_id(pool).await?;
        
        let mut total_reserve_x = U256::ZERO;
        let mut total_reserve_y = U256::ZERO;
        
        // Check active bin and 5 bins on each side
        for offset in -5i32..=5 {
            let bin_id = (active_id as i32 + offset) as u32;
            let (reserve_x, reserve_y) = self.get_bin(pool, bin_id as u24).await?;
            total_reserve_x += U256::from(reserve_x);
            total_reserve_y += U256::from(reserve_y);
        }
        
        let total_liquidity = total_reserve_x + total_reserve_y;
        
        Ok(LiquidityInfo {
            pool,
            total_liquidity,
            max_trade_1pct_slippage: total_liquidity / U256::from(50),
            max_trade_05pct_slippage: total_liquidity / U256::from(100),
        })
    }
}
```

---

## 5. Integration with Existing Code

### 5.1 Update Opportunity Detection

Modify the existing opportunity detection to call simulation:

```rust
// In your main arb detection loop
for opportunity in detected_opportunities {
    // NEW: Simulate before logging as opportunity
    let simulation = simulator
        .simulate_arb(&opportunity, test_amount)
        .await?;
    
    if simulation.is_profitable && simulation.confidence == SimulationConfidence::High {
        info!(
            "VERIFIED Opportunity: {} -> {} -> {}",
            opportunity.path[0],
            opportunity.path[1], 
            opportunity.path[2]
        );
        info!(
            "  Net Profit: {} bps (after {} bps fees)",
            simulation.net_profit_bps,
            simulation.total_fees_bps
        );
        info!(
            "  Confidence: {:?}, Block: {}",
            simulation.confidence,
            simulation.block_number
        );
    } else {
        debug!(
            "REJECTED: {} -> {} -> {} (net: {} bps, confidence: {:?})",
            opportunity.path[0],
            opportunity.path[1],
            opportunity.path[2],
            simulation.net_profit_bps,
            simulation.confidence
        );
    }
}
```

### 5.2 Updated Logging Format

Replace current logging with verified data:

```
========================================
 VERIFIED ARBITRAGE OPPORTUNITIES: 1
========================================

--- Opportunity #1 [VERIFIED] ---
   Path: WMON -> gMON -> WMON
   Block: 39219091
   Simulation: PASSED (eth_call)
   
   Pool 1: 0xD8aD... (PancakeSwap V3)
     - Fee Tier: 30 bps (0.30%)    ← ACTUAL fee from contract
     - Liquidity: $45,230
     - Quote: 1 WMON -> 0.9892 gMON
   
   Pool 2: 0xDf61... (LFJ)
     - Effective Fee: 25 bps
     - Liquidity: $23,100
     - Quote: 0.9892 gMON -> 1.0028 WMON
   
   Gross Profit: 28 bps
   DEX Fees: -55 bps
   Flash Loan: -9 bps
   Gas Cost: ~0 MON (~$0.00)
   ─────────────────────────
   NET PROFIT: -36 bps    ← NOT PROFITABLE
   Status: REJECTED

========================================
 REJECTED OPPORTUNITIES: 3
========================================
```

---

## 6. Contract Addresses Reference

### Monad Mainnet Addresses

```rust
// tokens.rs
pub mod tokens {
    pub const WMON: Address = address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
    pub const USDC: Address = address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
    pub const USDT: Address = address!("e7cd86e13AC4309349F30B3435a9d337750fC82D");
    pub const WETH: Address = address!("EE8c0E9f1BFFb4Eb878d8f15f368A02a35481242");
    pub const WBTC: Address = address!("0555E30da8f98308EdB960aa94C0Db47230d2B9c");
    pub const SMON: Address = address!("A3227C5969757783154C60bF0bC1944180ed81B9");
    pub const GMON: Address = address!("8498312A6B3CbD158bf0c93AbdCF29E6e4F55081");
}

// flash_loans.rs
pub mod neverland {
    pub const POOL: Address = address!("80F00661b13CC5F6ccd3885bE7b4C9c67545D585");
    pub const POOL_ADDRESSES_PROVIDER: Address = address!("49D75170F55C964dfdd6726c74fdEDEe75553A0f");
    pub const FLASH_LOAN_FEE_BPS: u32 = 9; // 0.09%
}

// dex_routers.rs (TBD - need to discover on mainnet)
pub mod uniswap_v3 {
    pub const QUOTER_V2: Address = /* TBD */;
    pub const SWAP_ROUTER: Address = /* TBD */;
}

pub mod lfj {
    pub const ROUTER: Address = /* TBD */;
    pub const FACTORY: Address = /* TBD */;
}
```

---

## 7. Testing Checklist

### Unit Tests

- [ ] `fee_validator`: Correctly parses V3 fee tiers (3000 -> 30 bps)
- [ ] `fee_validator`: Gets LFJ dynamic fees from getSwapOut
- [ ] `quote_fetcher`: Returns quotes from same block
- [ ] `liquidity`: Calculates max trade sizes correctly
- [ ] `simulator`: Includes all fee components in net profit
- [ ] `simulator`: eth_call simulation catches reverts

### Integration Tests

- [ ] Simulate known-unprofitable path, verify rejection
- [ ] Simulate with dust amount (0.001 WMON), verify no errors
- [ ] Compare simulation output to actual swap (tiny test trade)
- [ ] Verify flash loan fee (9 bps) is included

### Mainnet Verification

```bash
# Test with real RPC
MONAD_RPC_URL=https://monad-mainnet.g.alchemy.com/v2/<key> \
cargo test --features mainnet-test
```

---

## 8. Known Issues & Mitigations

### Issue 1: Fee Parsing Bug

**Symptom:** "Avg Fee: 3.00 bps" in logs  
**Likely Cause:** Uniswap V3 `fee()` returns hundredths of bps (3000 = 30 bps)  
**Fix:** Divide by 100 when converting to bps

### Issue 2: Probabilistic Execution

**Symptom:** Simulation passes but on-chain execution fails  
**Cause:** Monad's deferred execution - state may change between simulation and execution  
**Mitigation:**
- Always use atomic execution (revert on loss)
- Add profit margin buffer (e.g., require 10+ bps net profit)
- Track failure rates and adjust strategy

### Issue 3: Stale Quotes

**Symptom:** Profitable simulation, unprofitable execution  
**Cause:** Using quotes from different blocks  
**Fix:** Ensure all quotes use `BlockId::Number(specific_block)`

---

## 9. Success Criteria

The simulation module is complete when:

1. ✅ All pool fees are fetched from on-chain (not hardcoded)
2. ✅ Quotes are atomic (same block for all pools in path)
3. ✅ Net profit includes: DEX fees + flash loan fee + gas
4. ✅ eth_call simulation validates execution path
5. ✅ Liquidity depth prevents oversized trades
6. ✅ Logging shows verified vs rejected opportunities
7. ✅ Zero false positives in 1-hour test run

---

## 10. Dependencies

Add to `Cargo.toml`:

```toml
[dependencies]
alloy = { version = "0.5", features = ["full"] }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
eyre = "0.6"

# For V3 math (optional, can implement manually)
uniswap-v3-math = "0.4"
```

---

## 11. Execution Order

1. **First:** Fix fee parsing bug (likely dividing by 100 issue)
2. **Second:** Implement `quote_fetcher` with atomic block queries
3. **Third:** Implement `simulator` with full cost calculation
4. **Fourth:** Implement `liquidity` module
5. **Fifth:** Integration - wire into main detection loop
6. **Sixth:** Testing with dust amounts on mainnet

---

*Document prepared for Claude Code implementation. All code examples are illustrative - adapt to existing project structure and coding conventions.*
