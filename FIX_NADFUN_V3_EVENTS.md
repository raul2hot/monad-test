# Fix: Nad.fun Uses Uniswap V3 Swap Events

## Problem
Nad.fun pool emits **Uniswap V3** Swap events, but code subscribes to **V2** signature.

## Evidence
```
[DEBUG] Block 39403835 - Topics: [0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67...]
  >> Uniswap V3 Swap
```

## Fix in `src/main.rs`

### Change 1: Remove V2-style NadfunSwap subscription

Find and **DELETE** this block:
```rust
// Subscribe to Nad.fun Swap events
let nadfun_filter = Filter::new()
    .address(chog_nadfun_pool)
    .event_signature(NadfunSwap::SIGNATURE_HASH);

let nadfun_sub = provider.subscribe_logs(&nadfun_filter).await?;
let mut nadfun_stream = nadfun_sub.into_stream();
```

### Change 2: Subscribe Nad.fun to V3 Swap events (same as Uniswap)

Replace with:
```rust
// Subscribe to Nad.fun Swap events (uses V3 signature!)
let nadfun_filter = Filter::new()
    .address(chog_nadfun_pool)
    .event_signature(UniswapSwap::SIGNATURE_HASH);  // V3, not V2!

let nadfun_sub = provider.subscribe_logs(&nadfun_filter).await?;
let mut nadfun_stream = nadfun_sub.into_stream();
```

### Change 3: Update nadfun_task to decode V3 events

Find this task:
```rust
let nadfun_task = tokio::spawn(async move {
    while let Some(log) = nadfun_stream.next().await {
        if let Ok(decoded) = NadfunSwap::decode_log(&log.inner) {
            // ... V2 decoding logic
        }
    }
});
```

Replace with:
```rust
let nadfun_task = tokio::spawn(async move {
    while let Some(log) = nadfun_stream.next().await {
        if let Ok(decoded) = UniswapSwap::decode_log(&log.inner) {
            let block = log.block_number.unwrap_or(0);
            let price = calculate_price_from_sqrt_price_x96(decoded.data.sqrtPriceX96, token0_is_wmon);

            debug!(
                "Nad.fun CHOG swap: tick={}, sqrtPriceX96={}, price={:.10}",
                decoded.data.tick, decoded.data.sqrtPriceX96, price
            );

            state2.update_nadfun("CHOG", price);
            state2.print_prices("CHOG", block, "Nad.fun");
        }
    }
});
```

### Change 4: Remove unused NadfunSwap definition (optional cleanup)

Can delete this sol! block since it's no longer used:
```rust
// Nad.fun DEX uses Uniswap V2-style Swap events  <-- THIS COMMENT WAS WRONG
sol! {
    #[derive(Debug)]
    event NadfunSwap(
        address indexed sender,
        uint256 amount0In,
        uint256 amount1In,
        uint256 amount0Out,
        uint256 amount1Out,
        address indexed to
    );
}
```

Also delete `calculate_price_from_swap()` function if no longer needed.

### Change 5: Remove debug task (optional)

Once working, remove the debug subscription code.

---

## Summary

| Item | Before | After |
|------|--------|-------|
| Event signature | V2 (`NadfunSwap`) | V3 (`UniswapSwap`) |
| Decode function | `NadfunSwap::decode_log` | `UniswapSwap::decode_log` |
| Price calculation | `calculate_price_from_swap()` | `calculate_price_from_sqrt_price_x96()` |

## Expected Result

Both Uniswap and Nad.fun will now show live prices:
```
[Block 39403835] CHOG/WMON (updated by Nad.fun)
  Nad.fun:   0.1499818822 MON
  Uniswap:   5.3651053134 MON
  Spread:    -97.20%
  >>> ARBITRAGE: Buy on Nad.fun, Sell on Uniswap <<<
```

## Build & Run
```bash
cargo build --release && cargo run --release
```
