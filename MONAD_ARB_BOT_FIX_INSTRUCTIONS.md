# Monad Arbitrage Bot Simulation - Root Cause Analysis & Fix Instructions

**Document Version:** 1.0  
**Date:** December 2, 2025  
**Target Audience:** Claude Code Opus Agent  
**Priority:** CRITICAL

---

## Executive Summary

The Monad arbitrage MVP is experiencing a **massive discrepancy** between theoretical cycle detection and actual simulation results:

| Phase | Result |
|-------|--------|
| Bellman-Ford Detection | 20-27% profit cycles |
| Actual Simulation | **-99.99% losses** (output ~10^-6 of input) |

This document identifies **5 critical root causes** and provides detailed fix instructions.

---

## Root Cause Analysis

### 🔴 ROOT CAUSE #1: LFJ Liquidity Book Fundamental Misunderstanding

**The Problem:**  
LFJ (formerly Trader Joe V2) uses the **Liquidity Book AMM**, which is fundamentally different from Uniswap V2/V3.

| Feature | Uniswap V2/V3 | LFJ Liquidity Book |
|---------|---------------|-------------------|
| Price Model | Constant Product (x*y=k) | **Constant Sum within Bins** |
| Liquidity | Continuous curve | **Discrete price bins** |
| Slippage | Always present | **Zero within active bin** |
| Quote Method | Calculate from reserves | **Call `getSwapOut()` on Router** |

**Impact:**  
The graph edge weights are calculated using constant-product formulas, but LFJ pools don't follow this model. This creates the massive disparity.

**Evidence from Logs:**
```
Gross output 142420662024044 is < 50% of input 100000000000000000000
```
Input: 100 WMON (100 * 10^18)  
Output: ~0.00014 WMON (142420662024044)  
This is a **99.9999%+ loss** - impossible with proper quoting.

---

### 🔴 ROOT CAUSE #2: Graph Edge Weight Calculation is Wrong

**The Problem:**  
The Bellman-Ford algorithm uses edge weights that don't represent actual swap outputs.

**Current (Broken) Approach:**
```rust
// WRONG: Using theoretical price ratios
edge_weight = -log(price_ratio)  // Where price_ratio is from reserves
```

**What's Happening:**
1. Pool reserves are queried
2. A theoretical exchange rate is calculated (possibly using x*y=k math)
3. This rate is logged and used as graph edge weight
4. Bellman-Ford finds "profitable" negative cycles
5. But the theoretical rates DON'T MATCH actual quote outputs

**Required Fix:**
```rust
// CORRECT: Use actual quote output for edge weight
let actual_output = call_router_get_swap_out(pool, amount_in, swap_for_y);
let actual_rate = actual_output / amount_in;
edge_weight = -log(actual_rate)
```

---

### 🔴 ROOT CAUSE #3: LFJ Quote Function Usage is Incorrect

**The Problem:**  
LFJ requires specific quote functions that must be called correctly.

**LFJ Router Interface:**
```solidity
interface ILBRouter {
    /// @notice Get the swap output amount for a given input
    /// @param LBPair The LBPair contract address
    /// @param amountIn The input amount (uint128, NOT uint256!)
    /// @param swapForY True if swapping token X -> token Y
    /// @return amountInLeft Amount of input not swapped (overflow)
    /// @return amountOut The output amount
    /// @return fee The fee amount
    function getSwapOut(
        ILBPair LBPair,
        uint128 amountIn,
        bool swapForY
    ) external view returns (
        uint128 amountInLeft,
        uint128 amountOut,
        uint128 fee
    );
}
```

**Critical Parameters:**

1. **`amountIn` must be `uint128`** - If your code passes `uint256`, it may overflow or truncate incorrectly.

2. **`swapForY` determination:**
```rust
// You MUST determine the correct direction
let token_x = lb_pair.getTokenX();
let token_y = lb_pair.getTokenY();

// If swapping FROM token_x TO token_y: swapForY = true
// If swapping FROM token_y TO token_x: swapForY = false
let swap_for_y = (token_in == token_x);
```

3. **Handle `amountInLeft`:**
```rust
// If amountInLeft > 0, the pool couldn't absorb all input
// This means insufficient liquidity or bad price
if amount_in_left > 0 {
    // The trade partially failed - reject this opportunity
}
```

---

### 🔴 ROOT CAUSE #4: Decimal Handling Errors

**The Problem:**  
Different tokens have different decimals, and conversions must be handled carefully.

| Token | Decimals | 1 Token in Raw |
|-------|----------|----------------|
| WMON | 18 | 1000000000000000000 |
| USDC | 6 | 1000000 |
| USDT | 6 | 1000000 |
| WETH | 18 | 1000000000000000000 |

**Common Error Pattern:**
```rust
// WRONG: Comparing raw amounts without decimal adjustment
let profit = output_wmon - input_wmon;  // Both in 18 decimals - OK

// WRONG: Comparing across different decimals
let profit = output_usdc - input_wmon;  // 6 vs 18 decimals - BROKEN!
```

**Fix Approach:**
```rust
// Convert to common base for comparison (e.g., 18 decimals)
fn normalize_amount(amount: U256, token_decimals: u8) -> U256 {
    if token_decimals < 18 {
        amount * U256::from(10).pow(18 - token_decimals)
    } else if token_decimals > 18 {
        amount / U256::from(10).pow(token_decimals - 18)
    } else {
        amount
    }
}
```

**In Multi-Hop Swaps:**
```rust
// WMON (18) -> USDC (6) -> WMON (18)
let step1_output = quote_wmon_to_usdc(100e18);  // Returns ~2800 USDC (2800000000 raw)
let step2_output = quote_usdc_to_wmon(step1_output);  // Returns WMON in 18 decimals

// The simulation MUST use the raw output from step1 as input to step2
// Do NOT convert or normalize between steps!
```

---

### 🔴 ROOT CAUSE #5: `swapForY` Direction Bug (Most Likely Culprit)

**The Problem:**  
Getting `swapForY` wrong inverts the swap direction, causing massive losses.

**What `swapForY` Means:**
- `swapForY = true`: You're giving Token X, receiving Token Y
- `swapForY = false`: You're giving Token Y, receiving Token X

**LFJ Pair Token Ordering:**
```solidity
// In LBPair, tokens are ordered by address
// tokenX < tokenY (tokenX has the lower address)
```

**Example Bug:**
```
Pool: WMON/USDC
tokenX = 0x3bd359C1... (WMON - lower address)
tokenY = 0x754704Bc... (USDC - higher address)

Intended swap: WMON -> USDC
Correct: swapForY = true (giving X, want Y)
Bug: swapForY = false (giving Y, want X) <- This returns garbage!
```

**Detection Method:**
```rust
// Add debug logging
println!("Pool: {:?}", pool_address);
println!("tokenX: {:?}", token_x);
println!("tokenY: {:?}", token_y);
println!("token_in: {:?}", token_in);
println!("swapForY: {:?}", swap_for_y);
println!("Expected: giving {} for {}", token_in_symbol, token_out_symbol);
```

---

## Detailed Fix Instructions

### FIX #1: Implement Proper LFJ Quoting

**Step 1.1: Add LFJ Router Interface**

```rust
// In your contracts/interfaces module
abigen!(
    ILBRouter,
    r#"[
        function getSwapOut(address LBPair, uint128 amountIn, bool swapForY) external view returns (uint128 amountInLeft, uint128 amountOut, uint128 fee)
        function getSwapIn(address LBPair, uint128 amountOut, bool swapForY) external view returns (uint128 amountIn, uint128 amountOutLeft, uint128 fee)
    ]"#
);

abigen!(
    ILBPair,
    r#"[
        function getTokenX() external view returns (address)
        function getTokenY() external view returns (address)
        function getActiveId() external view returns (uint24)
        function getBin(uint24 id) external view returns (uint128 binReserveX, uint128 binReserveY)
    ]"#
);
```

**Step 1.2: Create LFJ Quote Function**

```rust
pub async fn quote_lfj_swap(
    provider: &Provider<Http>,
    router_address: Address,
    pair_address: Address,
    token_in: Address,
    amount_in: U256,
) -> Result<(U256, U256, bool), Error> {
    let pair = ILBPair::new(pair_address, provider.clone());
    let router = ILBRouter::new(router_address, provider.clone());
    
    // Get token ordering
    let token_x = pair.get_token_x().call().await?;
    let token_y = pair.get_token_y().call().await?;
    
    // Determine swap direction
    let swap_for_y = token_in == token_x;
    
    // Validate amount fits in uint128
    let amount_in_128: u128 = amount_in.as_u128();  // Will panic if > u128::MAX
    
    // Get quote
    let (amount_in_left, amount_out, fee) = router
        .get_swap_out(pair_address, amount_in_128, swap_for_y)
        .call()
        .await?;
    
    // Check if swap is valid
    if amount_in_left > 0 {
        return Err(Error::InsufficientLiquidity {
            pool: pair_address,
            amount_in: amount_in,
            amount_in_left: U256::from(amount_in_left),
        });
    }
    
    Ok((U256::from(amount_out), U256::from(fee), swap_for_y))
}
```

**Step 1.3: LFJ Contract Addresses on Monad Mainnet**

You need to discover or verify these addresses:

```rust
// These need to be verified on-chain or from LFJ documentation
pub const LFJ_ROUTER_V2_2: Address = address!("..."); // Check monadvision.com
pub const LFJ_FACTORY_V2_2: Address = address!("...");
pub const LFJ_QUOTER: Address = address!("...");

// If unknown, query the factory to find pairs
pub async fn get_lfj_pairs(
    factory: &ILBFactory,
    token_a: Address,
    token_b: Address,
) -> Vec<LBPairInfo> {
    factory.get_all_lb_pairs(token_a, token_b).call().await.unwrap_or_default()
}
```

---

### FIX #2: Rebuild Graph Edge Weights Using Real Quotes

**Step 2.1: New Edge Weight Calculator**

```rust
pub struct EdgeWeight {
    pub log_rate: f64,          // -log(output/input) for Bellman-Ford
    pub actual_output: U256,     // Real quote output
    pub fee: U256,               // DEX fee
    pub swap_for_y: bool,        // Direction flag
}

pub async fn calculate_edge_weight(
    provider: &Provider<Http>,
    pool: &Pool,
    token_in: Address,
    simulation_amount: U256,
) -> Result<EdgeWeight, Error> {
    let (output, fee, swap_for_y) = match pool.dex_type {
        DexType::LFJ => quote_lfj_swap(
            provider, 
            LFJ_ROUTER_V2_2, 
            pool.address, 
            token_in, 
            simulation_amount
        ).await?,
        DexType::UniswapV3 | DexType::PancakeSwapV3 => quote_v3_swap(
            provider,
            pool.address,
            token_in,
            simulation_amount
        ).await?,
        // ... other DEX types
    };
    
    // Calculate actual exchange rate
    let input_f64 = simulation_amount.as_u128() as f64;
    let output_f64 = output.as_u128() as f64;
    
    // Validate output is reasonable (at least 10% of input value)
    if output_f64 < input_f64 * 0.1 {
        return Err(Error::BadQuote {
            pool: pool.address,
            input: simulation_amount,
            output: output,
        });
    }
    
    let rate = output_f64 / input_f64;
    let log_rate = -rate.ln();  // Negative log for Bellman-Ford
    
    Ok(EdgeWeight {
        log_rate,
        actual_output: output,
        fee,
        swap_for_y,
    })
}
```

**Step 2.2: Update Graph Construction**

```rust
pub async fn build_arbitrage_graph(
    provider: &Provider<Http>,
    pools: &[Pool],
    tokens: &[Address],
    simulation_amount: U256,
) -> Result<ArbitrageGraph, Error> {
    let mut graph = ArbitrageGraph::new();
    
    for pool in pools {
        // Add edge for token0 -> token1
        match calculate_edge_weight(provider, pool, pool.token0, simulation_amount).await {
            Ok(weight) => {
                graph.add_edge(pool.token0, pool.token1, weight);
            }
            Err(e) => {
                warn!("Skipping edge {} -> {}: {:?}", pool.token0, pool.token1, e);
            }
        }
        
        // Add edge for token1 -> token0
        match calculate_edge_weight(provider, pool, pool.token1, simulation_amount).await {
            Ok(weight) => {
                graph.add_edge(pool.token1, pool.token0, weight);
            }
            Err(e) => {
                warn!("Skipping edge {} -> {}: {:?}", pool.token1, pool.token0, e);
            }
        }
    }
    
    Ok(graph)
}
```

---

### FIX #3: Fix Multi-Hop Simulation

**Step 3.1: Correct Multi-Hop Quote Chain**

```rust
pub async fn simulate_arbitrage_cycle(
    provider: &Provider<Http>,
    cycle: &[CycleStep],
    initial_amount: U256,
) -> Result<SimulationResult, Error> {
    let mut current_amount = initial_amount;
    let mut total_fees = U256::zero();
    let mut step_results = Vec::new();
    
    for (i, step) in cycle.iter().enumerate() {
        // CRITICAL: Use actual output from previous step as input
        let (output, fee, _) = match step.dex_type {
            DexType::LFJ => quote_lfj_swap(
                provider,
                LFJ_ROUTER_V2_2,
                step.pool_address,
                step.token_in,
                current_amount,  // Use current_amount, NOT simulation_amount
            ).await?,
            // ... other DEX types
        };
        
        step_results.push(StepResult {
            pool: step.pool_address,
            token_in: step.token_in,
            token_out: step.token_out,
            amount_in: current_amount,
            amount_out: output,
            fee: fee,
        });
        
        total_fees += fee;
        current_amount = output;  // Chain the output to next input
    }
    
    // Calculate profit (assuming cycle returns to starting token)
    let gross_profit_bps = calculate_profit_bps(initial_amount, current_amount);
    
    // Deduct flash loan fee (9 bps for Neverland)
    let flash_loan_fee_bps = 9i64;
    let net_profit_bps = gross_profit_bps - flash_loan_fee_bps;
    
    Ok(SimulationResult {
        initial_amount,
        final_amount: current_amount,
        gross_profit_bps,
        net_profit_bps,
        total_fees,
        steps: step_results,
        is_profitable: net_profit_bps > 0,
    })
}

fn calculate_profit_bps(input: U256, output: U256) -> i64 {
    let input_f64 = input.as_u128() as f64;
    let output_f64 = output.as_u128() as f64;
    ((output_f64 - input_f64) / input_f64 * 10000.0) as i64
}
```

---

### FIX #4: Add Comprehensive Logging

**Step 4.1: Debug Logger for Each Step**

```rust
fn log_simulation_step(step: &StepResult, step_index: usize) {
    let rate = step.amount_out.as_u128() as f64 / step.amount_in.as_u128() as f64;
    let change_bps = ((rate - 1.0) * 10000.0) as i64;
    
    info!(
        "Step {}: {} -> {} via pool {}",
        step_index,
        format_token(step.token_in),
        format_token(step.token_out),
        step.pool
    );
    info!(
        "  Input:  {} ({})",
        step.amount_in,
        format_human_amount(step.amount_in, get_decimals(step.token_in))
    );
    info!(
        "  Output: {} ({})",
        step.amount_out,
        format_human_amount(step.amount_out, get_decimals(step.token_out))
    );
    info!("  Rate: {:.6} ({:+} bps)", rate, change_bps);
    info!("  Fee: {}", step.fee);
}
```

---

### FIX #5: Validate Quotes Before Using

**Step 5.1: Sanity Checks**

```rust
pub fn validate_quote(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out: U256,
) -> Result<(), Error> {
    let in_decimals = get_decimals(token_in);
    let out_decimals = get_decimals(token_out);
    
    // Normalize both to 18 decimals for comparison
    let normalized_in = normalize_to_18(amount_in, in_decimals);
    let normalized_out = normalize_to_18(amount_out, out_decimals);
    
    // Check output is at least 1% of input (accounting for price)
    // For stables, expect near 1:1
    // For WMON/USDC, get current price and validate
    
    let ratio = normalized_out.as_u128() as f64 / normalized_in.as_u128() as f64;
    
    // Reject if output is less than 10% of input value
    // (This catches catastrophic quote failures)
    if ratio < 0.1 {
        return Err(Error::QuoteTooLow {
            expected_min_ratio: 0.1,
            actual_ratio: ratio,
            token_in,
            token_out,
            amount_in,
            amount_out,
        });
    }
    
    // Reject if output is more than 100x input value
    // (This catches reversed direction bugs)
    if ratio > 100.0 {
        return Err(Error::QuoteTooHigh {
            expected_max_ratio: 100.0,
            actual_ratio: ratio,
            token_in,
            token_out,
            amount_in,
            amount_out,
        });
    }
    
    Ok(())
}
```

---

## Testing Strategy

### Test 1: Single Pool Quote Validation

```rust
#[tokio::test]
async fn test_lfj_quote_wmon_usdc() {
    let provider = get_provider();
    
    // Find WMON/USDC pool on LFJ
    let pool = find_lfj_pool(WMON, USDC).await;
    
    // Quote 100 WMON -> USDC
    let (output, fee, swap_for_y) = quote_lfj_swap(
        &provider,
        LFJ_ROUTER_V2_2,
        pool.address,
        WMON,
        parse_units("100", 18).unwrap().into(),
    ).await.unwrap();
    
    // WMON is ~$0.028, so 100 WMON ≈ $2.80
    // Output should be ~2.8 USDC (2800000 raw with 6 decimals)
    let output_usdc = output.as_u128() as f64 / 1e6;
    assert!(output_usdc > 2.0 && output_usdc < 4.0, 
        "Expected ~2.8 USDC, got {}", output_usdc);
}
```

### Test 2: Round-Trip (No Profit Expected)

```rust
#[tokio::test]
async fn test_round_trip_no_profit() {
    // WMON -> USDC -> WMON on same DEX should result in ~0.6% loss (2 x 0.3% fee)
    let initial = parse_units("100", 18).unwrap().into();
    
    let result = simulate_arbitrage_cycle(
        &provider,
        &[
            CycleStep { token_in: WMON, token_out: USDC, pool: lfj_wmon_usdc },
            CycleStep { token_in: USDC, token_out: WMON, pool: lfj_wmon_usdc },
        ],
        initial,
    ).await.unwrap();
    
    // Should lose about 0.6% to fees
    assert!(result.gross_profit_bps < 0, "Expected loss, got profit");
    assert!(result.gross_profit_bps > -100, "Loss too high: {} bps", result.gross_profit_bps);
}
```

### Test 3: Cross-DEX Arbitrage Opportunity

```rust
#[tokio::test]
async fn test_cross_dex_opportunity() {
    // If there's a price discrepancy between LFJ and PancakeSwap
    // This test validates the simulation catches real opportunities
    
    let result = simulate_arbitrage_cycle(
        &provider,
        &[
            CycleStep { token_in: WMON, token_out: USDC, pool: lfj_wmon_usdc },
            CycleStep { token_in: USDC, token_out: WMON, pool: pancake_wmon_usdc },
        ],
        parse_units("100", 18).unwrap().into(),
    ).await.unwrap();
    
    // Log the result for manual inspection
    println!("Cross-DEX result: {:?}", result);
    
    // The test is informational - we're checking simulation works, not that
    // there IS an opportunity
}
```

---

## Contract Addresses Reference

### Tokens (Verified)

```rust
pub const WMON: Address = address!("3bd359C1119dA7Da1D913D1C4D2B7c461115433A");
pub const USDC: Address = address!("754704Bc059F8C67012fEd69BC8A327a5aafb603");
pub const USDT: Address = address!("e7cd86e13AC4309349F30B3435a9d337750fC82D");
pub const WETH: Address = address!("EE8c0E9f1BFFb4Eb878d8f15f368A02a35481242");
pub const WBTC: Address = address!("0555E30da8f98308EdB960aa94C0Db47230d2B9c");
```

### DEX Contracts (Need Verification)

```rust
// These need to be verified from monadvision.com or protocol docs
// LFJ
pub const LFJ_FACTORY: Address = address!("TODO");
pub const LFJ_ROUTER: Address = address!("TODO");
pub const LFJ_QUOTER: Address = address!("TODO");

// PancakeSwap V3
pub const PANCAKE_V3_FACTORY: Address = address!("TODO");
pub const PANCAKE_V3_QUOTER: Address = address!("TODO");

// Uniswap V3
pub const UNI_V3_FACTORY: Address = address!("TODO");
pub const UNI_V3_QUOTER: Address = address!("TODO");
```

---

## Checklist for Implementation

- [ ] **RC1**: Implement correct LFJ `getSwapOut` quote function
- [ ] **RC2**: Rebuild graph edges using real quotes (not theoretical rates)
- [ ] **RC3**: Fix `swapForY` direction calculation
- [ ] **RC4**: Add decimal-aware amount handling
- [ ] **RC5**: Chain multi-hop amounts correctly (output -> next input)
- [ ] **Test**: Verify single pool quotes return sane values
- [ ] **Test**: Verify round-trip results in expected fee loss
- [ ] **Deploy**: Run with verbose logging to validate fixes
- [ ] **Verify**: Contract addresses for LFJ on Monad mainnet

---

## Summary of Changes Required

1. **Remove theoretical price calculations** - Don't calculate rates from reserves
2. **Use router quote functions** - Call `getSwapOut()` for actual quotes
3. **Fix swap direction** - Correctly determine `swapForY` from token ordering
4. **Chain amounts correctly** - Use actual output as next input
5. **Validate quotes** - Reject quotes that indicate failures
6. **Add logging** - Debug each step to find remaining issues

---

*This document should be used as the primary reference for fixing the Monad arbitrage bot simulation. The root causes are listed in order of likelihood, with RC#3 (swapForY direction bug) and RC#2 (graph edge calculation) being the most probable culprits.*
