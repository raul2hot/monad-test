# Fast Arbitrage Integration Instructions

## Goal
Reduce `test-arb` execution from ~4s to <1.5s by adding optimized `fast-arb` command.

---

## Files to Add

### 1. Create `src/execution/fast_arb.rs`
Copy the complete `fast_arb.rs` file from the outputs folder. This contains:
- `execute_fast_arb()` - Main optimized arb function
- `FastArbResult` - Result struct
- `print_fast_arb_result()` - Display function
- `build_fast_swap_tx()` - Pre-build swap calldata

### 2. Update `src/execution/mod.rs`
```rust
pub mod routers;
pub mod swap;
pub mod report;
pub mod fast_arb;  // ADD THIS

pub use swap::{SwapParams, SwapResult, SwapDirection, execute_swap, wait_for_next_block};
pub use report::print_swap_report;
pub use routers::build_swap_calldata;
pub use fast_arb::{execute_fast_arb, FastArbResult, print_fast_arb_result};  // ADD THIS
```

---

## Changes to `src/main.rs`

### 1. Add Import
```rust
use execution::{execute_fast_arb, print_fast_arb_result};
```

### 2. Add Command to `Commands` enum
```rust
/// Fast DEX-to-DEX arbitrage (optimized <1.5s execution)
FastArb {
    #[arg(long)]
    sell_dex: String,
    #[arg(long)]
    buy_dex: String,
    #[arg(long, default_value = "1.0")]
    amount: f64,
    #[arg(long, default_value = "200")]
    slippage: u32,
},
```

### 3. Add Match Arm
```rust
Some(Commands::FastArb { sell_dex, buy_dex, amount, slippage }) => {
    run_fast_arb(&sell_dex, &buy_dex, amount, slippage).await
}
```

### 4. Add Handler Function
```rust
async fn run_fast_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    let total_start = std::time::Instant::now();
    
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url.clone());

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let signer_address = signer.address();
    
    // PARALLEL INIT: gas + nonce + prices
    let (gas_result, nonce_result, prices_result) = tokio::join!(
        provider.get_gas_price(),
        init_nonce(&provider, signer_address),
        get_current_prices(&provider)
    );
    
    let gas_price = gas_result.unwrap_or(100_000_000_000);
    nonce_result?;
    let prices = prices_result?;
    
    println!("  [TIMING] Parallel init: {:?}", total_start.elapsed());
    
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
    println!("  FAST ARB | {} -> {}", sell_dex, buy_dex);
    println!("══════════════════════════════════════════════════════════════");
    
    let result = execute_fast_arb(
        &provider_with_signer,
        signer_address,
        &sell_router,
        &buy_router,
        amount,
        sell_price,
        buy_price,
        slippage,
        gas_price,
    ).await?;
    
    print_fast_arb_result(&result, sell_dex, buy_dex);
    println!("  [TIMING] TOTAL: {:?}", total_start.elapsed());
    
    Ok(())
}
```

---

## Key Optimizations Applied

| Optimization | Time Saved |
|--------------|------------|
| Parallel init (gas + nonce + prices) | ~300ms |
| Skip `wait_for_next_block` | ~1000ms |
| Send both TXs back-to-back | ~800ms |
| Parallel receipt wait | ~400ms |
| Skip approval/balance checks | ~400ms |
| 50ms polling (vs 100ms) | ~200ms |

---

## Usage

```bash
# Ensure approvals exist first
cargo run -- prepare-arb

# Run fast arbitrage
cargo run -- fast-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 1.0 --slippage 200
```

---

## Important Notes

1. **Run `prepare-arb` first** - Fast arb skips approval checks
2. **Uses estimated amounts** - Swap 2 uses 99% of expected USDC from swap 1
3. **Higher slippage recommended** - Default 200bps (2%) for safety
4. **Both TXs sent immediately** - If swap 1 fails, swap 2 also fails (by design)
