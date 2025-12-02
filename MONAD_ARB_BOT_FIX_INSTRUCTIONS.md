# Root Cause Analysis: Monad Arbitrage Bot Graph-Quoter Discrepancy

Your arbitrage bot shows **20.70% profit** via Bellman-Ford but **0 bps** from quoter simulation because of a compound failure across LFJ price calculation, liquidity threshold logic, and graph edge weight construction. The primary issue is the liquidity threshold rejection, but even without that, underlying price calculation bugs would cause incorrect opportunity detection.

## The immediate rejection: flawed liquidity comparison

The log shows the opportunity being rejected with `insufficient liquidity: 44323215691692120251 < 100000000000000000000` (44.3e18 < 100e18). This comparison is fundamentally broken because it compares raw token amounts without normalization.

**The bug**: Your `MIN_ACTIVE_LIQUIDITY` threshold of **100e18** assumes all pools use 18-decimal tokens. The LFJ pool `0x5E60BC3F7a7303BC4dfE4dc2220bdC90bc04fE22` returns `reserveX + reserveY` in raw decimals. For WMON (18 decimals), 44.3e18 represents 44.3 WMON—which may be perfectly adequate liquidity depending on trade size.

**Fix for `src/config.rs`**:
```rust
// WRONG: Universal raw threshold
pub const MIN_ACTIVE_LIQUIDITY: U256 = U256::from_limbs([100_000_000_000_000_000_000u64, 0, 0, 0]);

// CORRECT: USD-normalized or token-specific thresholds
pub const MIN_LIQUIDITY_USD: f64 = 10_000.0;  // $10k minimum
// OR token-specific:
pub const MIN_WMON_LIQUIDITY: U256 = U256::from_limbs([50_000_000_000_000_000_000u64, 0, 0, 0]); // 50 WMON
pub const MIN_USDC_LIQUIDITY: U256 = U256::from_limbs([10_000_000_000u64, 0, 0, 0]); // 10,000 USDC (6 decimals)
```

## LFJ price calculation: critical formula errors

LFJ's Liquidity Book uses fundamentally different math than Uniswap V3. Several bugs likely exist in your implementation.

### Token ordering is NOT sorted by address

Unlike Uniswap where `token0 < token1` by address, LFJ's `tokenX` and `tokenY` can be in **any order**. The factory sorts for storage, but the actual pair may have reversed ordering.

**Fix for `src/dex/lfj.rs`**:
```rust
// WRONG: Assuming tokenX is always the lower address
let (token0, token1) = if token_a < token_b { (token_a, token_b) } else { (token_b, token_a) };

// CORRECT: Always query the pair contract
let token_x = pair.getTokenX().call().await?;
let token_y = pair.getTokenY().call().await?;
// Price is ALWAYS tokenY per tokenX (Y/X)
```

### Price direction interpretation

The LFJ price from `getPriceFromId()` represents **tokenY per tokenX**. If you need the price for swapping X→Y, use the price directly. For Y→X, invert it.

```rust
// Price from bin: tokenY per tokenX
let price_y_per_x = calculate_price_from_id(active_id, bin_step);

// For swapping tokenX → tokenY (swapForY = true):
let effective_rate = price_y_per_x * (1.0 - fee);

// For swapping tokenY → tokenX (swapForY = false):
let effective_rate = (1.0 / price_y_per_x) * (1.0 - fee);
```

### Q128.128 fixed-point conversion

LFJ returns prices in 128.128 binary fixed-point. Converting to f64 requires dividing by 2^128:

```rust
// WRONG: Integer division loses all precision
let price_f64 = (price_q128 / U256::from(1u128 << 128)).as_u64() as f64;

// CORRECT: Use proper fixed-point conversion
let price_f64 = price_q128.to_f64_lossy() / (2.0_f64.powi(128));

// OR for more precision:
let (hi, lo) = (price_q128 >> 128, price_q128 & ((U256::ONE << 128) - U256::ONE));
let price_f64 = hi.to_f64_lossy() + lo.to_f64_lossy() / 2.0_f64.powi(128);
```

### Bin price calculation formula

```rust
// Correct formula: price = (1 + binStep/10000)^(activeId - 8388608)
const REAL_ID_SHIFT: i32 = 8388608; // 2^23

fn calculate_price_from_id(active_id: u32, bin_step: u16) -> f64 {
    let base = 1.0 + (bin_step as f64 / 10_000.0);
    let exponent = (active_id as i32) - REAL_ID_SHIFT;
    base.powi(exponent)
}
```

## Graph edge weight construction flaws

The 20.70% "profit" detection is almost certainly a false positive from incorrect edge weight calculation.

### Fee unit confusion between DEXes

Your graph likely mishandles fee representations:

| DEX | 0.3% fee representation | Conversion to decimal |
|-----|------------------------|----------------------|
| **Uniswap V3/V4** | 3000 (hundredths of bps) | `3000 / 1_000_000 = 0.003` |
| **PancakeSwap V3** | 2500 (for 0.25%) | `2500 / 1_000_000 = 0.0025` |
| **LFJ** | Variable (baseFee + variableFee) | Returned from `getSwapOut()` |

**Fix for `src/graph/builder.rs`**:
```rust
// WRONG: Using fee directly as percentage
let fee_decimal = pool.fee as f64; // If fee=3000, this gives 3000% fee!

// CORRECT: Normalize based on DEX type
fn normalize_fee(pool: &Pool) -> f64 {
    match pool.dex_type {
        DexType::UniswapV3 | DexType::UniswapV4 | DexType::PancakeSwapV3 => {
            pool.fee as f64 / 1_000_000.0  // 3000 → 0.003
        }
        DexType::LFJ => {
            // LFJ fees should be fetched from getSwapOut() or calculated
            // baseFee = baseFactor * binStep / 10000
            (pool.base_factor as f64 * pool.bin_step as f64) / 100_000_000.0
        }
    }
}
```

### Edge weight formula must include fees correctly

```rust
// CORRECT edge weight for Bellman-Ford:
// w(A→B) = -ln(price * (1 - fee))

fn calculate_edge_weight(price: f64, fee: f64) -> f64 {
    let effective_rate = price * (1.0 - fee);
    -effective_rate.ln()  // Negative log for arbitrage detection
}

// WRONG approaches that cause false positives:
// -ln(price) - fee          ← Additive fee is wrong
// -ln(price) - ln(1-fee)    ← Mathematical error
// -ln(price / (1 + fee))    ← Fee direction inverted
```

### Bidirectional edge consistency

For a token pair A↔B, you need two edges with correct directional prices:

```rust
// For pool with tokenX=A, tokenY=B, price = B/A (how much B per A)

// Edge A→B (swap A to get B):
let weight_a_to_b = -((price_b_per_a * (1.0 - fee)).ln());

// Edge B→A (swap B to get A):  
let price_a_per_b = 1.0 / price_b_per_a;
let weight_b_to_a = -((price_a_per_b * (1.0 - fee)).ln());

// These are NOT simply negatives of each other due to fees
```

## Uniswap V3/V4 sqrtPriceX96 conversion bugs

If you're also using Uniswap pools in the arbitrage path, verify these calculations:

### Correct sqrtPriceX96 conversion

```rust
// sqrtPriceX96 gives price = token1/token0 (NOT token0/token1)
fn sqrt_price_to_price(sqrt_price_x96: U256) -> f64 {
    let sqrt_p = sqrt_price_x96.to_f64_lossy() / 2.0_f64.powi(96);
    sqrt_p * sqrt_p  // Square it: (sqrtPrice)^2 = price
}

// For human-readable with decimals:
fn sqrt_price_to_human_price(sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
    let raw_price = sqrt_price_to_price(sqrt_price_x96);
    raw_price * 10.0_f64.powi((decimals0 as i32) - (decimals1 as i32))
}
```

### Common mistake: forgetting Q192 scaling

```rust
// WRONG: Only shifting by 96
let price = (sqrt_price_x96 * sqrt_price_x96) >> 96;

// CORRECT: Must shift by 192 after squaring
let price = (sqrt_price_x96 * sqrt_price_x96) >> 192;
// Or equivalently: (sqrtPrice / 2^96)^2
```

## Fix for `src/dex/mod.rs` Pool price methods

Your `Pool` struct needs DEX-aware price calculation:

```rust
impl Pool {
    pub fn get_effective_price(&self, token_in: Address, amount_in: U256) -> Result<f64> {
        match self.dex_type {
            DexType::LFJ => {
                let (token_x, token_y) = (self.token_x, self.token_y);
                let swap_for_y = token_in == token_x;
                
                // Price is tokenY per tokenX
                let base_price = self.calculate_lfj_price()?;
                
                if swap_for_y {
                    Ok(base_price) // Selling X for Y
                } else {
                    Ok(1.0 / base_price) // Selling Y for X
                }
            }
            DexType::UniswapV3 | DexType::UniswapV4 => {
                // sqrtPriceX96 gives token1/token0
                let raw_price = self.calculate_v3_price()?;
                
                if token_in == self.token0 {
                    Ok(raw_price) // Selling token0 for token1
                } else {
                    Ok(1.0 / raw_price) // Selling token1 for token0
                }
            }
            // ... other DEX types
        }
    }
}
```

## Fix for `src/simulation/quote_fetcher.rs`

The quoter should use on-chain calls, not calculated prices:

```rust
pub async fn fetch_lfj_quote(
    pair: Address,
    amount_in: u128,
    swap_for_y: bool,
    provider: &Provider,
) -> Result<QuoteResult> {
    // Use getSwapOut for accurate quote including fees
    let (amount_left, amount_out, fees) = lfj_pair
        .getSwapOut(amount_in, swap_for_y)
        .call()
        .await?;
    
    // Verify full amount can be swapped
    if amount_left > 0 {
        return Err(Error::InsufficientLiquidity(amount_left));
    }
    
    Ok(QuoteResult {
        amount_out,
        fee_amount: fees,
        price_impact: calculate_impact(amount_in, amount_out, swap_for_y),
    })
}
```

## Monad-specific considerations

Monad's **400ms block time** and **deferred execution** create unique challenges:

- **State may be 1-2 blocks behind** during quoter simulation—prices can shift between graph construction and execution
- Use **Multicall3** (`0xcA11bde05977b3631167028862bE2a173976CA11`) to batch all pool state reads atomically
- **High revert rates expected**—Monad's parallel execution with low fees encourages transaction spam similar to Solana's ~41% revert rate
- **Gas charged on limit, not usage**—set gas limits carefully to avoid overpaying on reverts

## Complete diagnosis summary

The **root causes** of your graph-quoter discrepancy are:

1. **Liquidity threshold comparing raw 18-decimal amounts** without token-specific normalization—fix the threshold logic to be USD-normalized or token-aware

2. **LFJ token ordering assumption** (assuming sorted by address)—always query `getTokenX()` and `getTokenY()` from the pair contract

3. **Fee unit confusion**—Uniswap's `3000` means 0.3%, not 3000%; LFJ fees come from getSwapOut or require baseFactor calculation

4. **Graph edge direction errors**—price represents Y/X for LFJ and token1/token0 for Uniswap; swapping in opposite direction requires inversion

5. **Q128.128 or sqrtPriceX96 conversion bugs**—ensure proper fixed-point math with 128 or 192 bit shifts

The 20.70% "profit" is a phantom caused by these compounding errors creating artificially favorable edge weights in your graph, which the quoter correctly identifies as non-profitable when using actual on-chain getSwapOut calls.