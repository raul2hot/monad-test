# Nad.fun Integration Fix Instructions

## Problem Summary

The monitor runs but shows:
```
Could not fetch initial Nad.fun price: execution reverted, data: "0x"
```

**Root Cause:** Nad.fun pools don't implement standard Uniswap V2 `getReserves()`. They use a custom LENS contract for price queries.

**Pool Status:** ✅ Confirmed active - $273K TVL, $641K 24h volume, ~4K daily txns

---

## Fix 1: Add LENS Contract for Price Queries

### Step 1: Update `src/config.rs`

Add this constant:
```rust
// Nad.fun LENS contract (for price queries)
pub const NADFUN_LENS: &str = "0x7e78A8DE94f21804F7a17F4E8BF9EC2c872187ea";
```

### Step 2: Update `src/main.rs`

#### 2a. Add LENS ABI definition (after existing sol! macros):

```rust
// Nad.fun LENS getAmountOut function
sol! {
    #[derive(Debug)]
    function getAmountOut(
        address token,
        uint256 amountIn,
        bool isBuy
    ) external view returns (
        address router,
        uint256 amountOut
    );
}
```

#### 2b. Replace the Nad.fun initial price fetch section:

Find this code block:
```rust
// Get initial Nad.fun price
let reserves_call = getReservesCall {};
let tx = TransactionRequest::default()
    .to(chog_nadfun_pool)
    .input(reserves_call.abi_encode().into());

match provider.call(tx).await {
    Ok(result) => {
        if let Ok(decoded) = getReservesCall::abi_decode_returns(&result) {
            let price = calculate_price_from_reserves(
                U256::from(decoded.reserve0),
                U256::from(decoded.reserve1),
                token0_is_wmon,
            );
            println!("Initial CHOG Nad.fun price: {:.10} MON", price);
            price_state.update_nadfun("CHOG", price);
        }
    }
    Err(e) => warn!("Could not fetch initial Nad.fun price: {}", e),
}
```

Replace with:
```rust
// Get initial Nad.fun price via LENS contract
let lens_address = Address::from_str(config::NADFUN_LENS)?;
let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 MON

let lens_call = getAmountOutCall {
    token: chog,
    amountIn: amount_in,
    isBuy: true,
};
let tx = TransactionRequest::default()
    .to(lens_address)
    .input(lens_call.abi_encode().into());

match provider.call(tx).await {
    Ok(result) => {
        if let Ok(decoded) = getAmountOutCall::abi_decode_returns(&result) {
            // tokens_out / 1e18 = tokens per MON
            // price = 1 / tokens_per_mon = MON per token
            let tokens_out = decoded.amountOut.to_string().parse::<f64>().unwrap_or(0.0);
            if tokens_out > 0.0 {
                let price = 1e18 / tokens_out;
                println!("Initial CHOG Nad.fun price: {:.10} MON (via LENS)", price);
                println!("  Router: {:?}", decoded.router);
                price_state.update_nadfun("CHOG", price);
            }
        }
    }
    Err(e) => warn!("Could not fetch initial Nad.fun price via LENS: {}", e),
}
```

---

## Fix 2: Add Debug Logging for Events (Optional)

If swap events aren't being received, add this to identify what events the pool actually emits.

#### Add after existing filter subscriptions:

```rust
// Debug: Subscribe to ALL events from Nad.fun pool
let debug_filter = Filter::new().address(chog_nadfun_pool);
let debug_sub = provider.subscribe_logs(&debug_filter).await?;
let mut debug_stream = debug_sub.into_stream();

let debug_task = tokio::spawn(async move {
    println!("\n=== DEBUG: Listening for ALL Nad.fun pool events ===\n");
    while let Some(log) = debug_stream.next().await {
        let block = log.block_number.unwrap_or(0);
        println!("\n[DEBUG] Block {} - Topics: {:?}", block, log.topics());
        
        if let Some(topic0) = log.topics().first() {
            let t = format!("{:?}", topic0);
            if t.contains("d78ad95f") { println!("  >> Uniswap V2 Swap"); }
            else if t.contains("c42079f9") { println!("  >> Uniswap V3 Swap"); }
            else if t.contains("1c411e9a") { println!("  >> Sync event"); }
            else { println!("  >> Unknown/Custom event"); }
        }
    }
});
```

#### Update tokio::select!:
```rust
tokio::select! {
    _ = uniswap_task => { warn!("Uniswap subscription ended"); }
    _ = nadfun_task => { warn!("Nad.fun subscription ended"); }
    _ = debug_task => { warn!("Debug subscription ended"); }
}
```

---

## Quick Terminal Commands

```bash
# Add LENS constant to config.rs
sed -i '/^pub const WMON/a pub const NADFUN_LENS: \&str = "0x7e78A8DE94f21804F7a17F4E8BF9EC2c872187ea";' src/config.rs

# Verify change
grep NADFUN_LENS src/config.rs

# Build and run
cargo build --release && cargo run --release
```

---

## Contract Reference

| Contract | Address | Use |
|----------|---------|-----|
| LENS | `0x7e78A8DE94f21804F7a17F4E8BF9EC2c872187ea` | Price queries |
| DEX_ROUTER | `0x0B79d71AE99528D1dB24A4148b5f4F865cc2b137` | Swap execution |
| DEX_FACTORY | `0x6B5F564339DbAD6b780249827f2198a841FEB7F3` | Pool creation |
| CHOG Pool | `0x116e7d070f1888b81e1e0324f56d6746b2d7d8f1` | CHOG/WMON trading |

---

## Expected Output After Fix

```
Fetching initial prices...
Initial CHOG Uniswap price: 5.3651053134 MON
Initial CHOG Nad.fun price: 0.1279000000 MON (via LENS)
  Router: 0x0B79d71AE99528D1dB24A4148b5f4F865cc2b137
```

---

## If Swap Events Still Don't Work

Nad.fun might use custom event signatures. Run with debug logging enabled and share the topic0 values you see. We can then update the event filter accordingly.

Alternative: Poll LENS every N seconds instead of relying on events:
```rust
loop {
    // Query LENS for current price
    let price = get_nadfun_price_via_lens(&provider, chog, lens_address).await?;
    price_state.update_nadfun("CHOG", price);
    tokio::time::sleep(Duration::from_millis(500)).await;
}
```
