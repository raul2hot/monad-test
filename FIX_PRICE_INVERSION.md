# Fix: Price Calculation Inversion Bug

## Problem
When `token0_is_wmon = false`, the code incorrectly inverts the price.

Uniswap V3 sqrtPriceX96 gives: `price = token1/token0`

- If CHOG is token0, WMON is token1 → price = WMON/CHOG ✅ (already correct!)
- Code inverts it → gets CHOG/WMON ❌ (wrong!)

## Fix in `src/main.rs`

Find `calculate_price_from_sqrt_price_x96`:

```rust
fn calculate_price_from_sqrt_price_x96(sqrt_price_x96: U160, token0_is_wmon: bool) -> f64 {
    let sqrt_price = U256::from(sqrt_price_x96);
    let q96: U256 = U256::from(1u128) << 96;

    let sqrt_price_f64 = sqrt_price.to_string().parse::<f64>().unwrap_or(0.0);
    let q96_f64 = q96.to_string().parse::<f64>().unwrap_or(1.0);

    let ratio = sqrt_price_f64 / q96_f64;
    let price = ratio * ratio;  // price = token1/token0

    // We want price in MON per token
    if token0_is_wmon {
        // price is token1/token0 = token/WMON, need to invert to get WMON/token
        if price > 0.0 { 1.0 / price } else { 0.0 }
    } else {
        // price is token0/token1 = token/WMON, need to invert  <-- WRONG COMMENT & LOGIC
        if price > 0.0 { 1.0 / price } else { 0.0 }
    }
}
```

**Replace with:**

```rust
fn calculate_price_from_sqrt_price_x96(sqrt_price_x96: U160, token0_is_wmon: bool) -> f64 {
    let sqrt_price = U256::from(sqrt_price_x96);
    let q96: U256 = U256::from(1u128) << 96;

    let sqrt_price_f64 = sqrt_price.to_string().parse::<f64>().unwrap_or(0.0);
    let q96_f64 = q96.to_string().parse::<f64>().unwrap_or(1.0);

    let ratio = sqrt_price_f64 / q96_f64;
    let price = ratio * ratio;  // price = token1/token0

    // We want price in MON per token
    if token0_is_wmon {
        // token0=WMON, token1=TOKEN
        // price = token1/token0 = TOKEN/WMON (tokens per MON)
        // Invert to get MON per token
        if price > 0.0 { 1.0 / price } else { 0.0 }
    } else {
        // token0=TOKEN, token1=WMON  
        // price = token1/token0 = WMON/TOKEN (MON per token)
        // Already correct! Don't invert.
        price
    }
}
```

## Expected Result After Fix

```
Initial CHOG Uniswap price: 0.1865 MON   ← Was 5.36 (inverted)
Initial CHOG Nad.fun price: 0.1499 MON   ← Unchanged

Spread: ~24% (reasonable for cross-DEX)
```

## Build & Test
```bash
cargo build --release && cargo run --release
```
