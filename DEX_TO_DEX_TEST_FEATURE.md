# Phase 3: DEX-to-DEX Arbitrage Test Feature

## Objective
Add a new CLI command `test-arb` to execute a sequential two-swap arbitrage test between any two DEXes.

---

## The Arbitrage Flow

```
WMON ──(sell on high-price DEX)──► USDC ──(buy on low-price DEX)──► WMON
                                                                      │
                                                          (hopefully more WMON!)
```

**Example from logs:**
```
Buy: 0.02950 (MondayTrade) | Sell: 0.02965 (PancakeSwap1)
```
- Sell WMON on PancakeSwap1 @ 0.02965 USDC/WMON (get more USDC)
- Buy WMON on MondayTrade @ 0.02950 USDC/WMON (get more WMON per USDC)

---

## Implementation Tasks

### 1. Add New CLI Command in `src/main.rs`

Add to the `Commands` enum:

```rust
/// Test DEX-to-DEX arbitrage (sell on one DEX, buy on another)
TestArb {
    /// DEX to sell WMON on (higher price)
    #[arg(long)]
    sell_dex: String,

    /// DEX to buy WMON on (lower price)  
    #[arg(long)]
    buy_dex: String,

    /// Amount of WMON to start with
    #[arg(long, default_value = "1.0")]
    amount: f64,

    /// Slippage tolerance in bps (e.g., 150 = 1.5%)
    #[arg(long, default_value = "150")]
    slippage: u32,
},
```

### 2. Create New Function `run_test_arb()`

```rust
async fn run_test_arb(sell_dex: &str, buy_dex: &str, amount: f64, slippage: u32) -> Result<()> {
    let rpc_url = std::env::var("MONAD_RPC_URL").expect("MONAD_RPC_URL must be set");
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");

    let url: reqwest::Url = rpc_url.parse()?;
    let provider = ProviderBuilder::new().connect_http(url);

    let signer = PrivateKeySigner::from_str(&private_key)?;
    println!("Wallet: {:?}", signer.address());

    // Get routers
    let sell_router = get_router_by_name(sell_dex)
        .ok_or_else(|| eyre::eyre!("Unknown sell DEX: {}", sell_dex))?;
    let buy_router = get_router_by_name(buy_dex)
        .ok_or_else(|| eyre::eyre!("Unknown buy DEX: {}", buy_dex))?;

    // Get current prices
    println!("\nFetching current prices...");
    let prices = get_current_prices(&provider).await?;

    let sell_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == sell_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", sell_dex))?;
    
    let buy_price = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .ok_or_else(|| eyre::eyre!("Could not get price for {}", buy_dex))?;

    println!("  {} price: {:.6} USDC/WMON", sell_dex, sell_price.price);
    println!("  {} price: {:.6} USDC/WMON", buy_dex, buy_price.price);
    
    let spread_bps = ((sell_price.price - buy_price.price) / buy_price.price * 10000.0) as i32;
    println!("  Spread: {} bps ({:.4}%)", spread_bps, spread_bps as f64 / 100.0);

    if spread_bps <= 0 {
        println!("\n⚠️  WARNING: Negative spread! sell_dex should have HIGHER price than buy_dex");
        println!("  Consider swapping: --sell-dex {} --buy-dex {}", buy_dex, sell_dex);
    }

    // Get initial WMON balance
    let balances_before = get_balances(&provider, signer.address()).await?;
    println!("\n  Starting WMON balance: {:.6}", balances_before.wmon_human);

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  DEX-TO-DEX ARBITRAGE TEST");
    println!("══════════════════════════════════════════════════════════════");
    println!("  Route: WMON --({})-> USDC --({})-> WMON", sell_dex, buy_dex);
    println!("  Amount: {} WMON", amount);
    println!("══════════════════════════════════════════════════════════════\n");

    // ═══════════════════════════════════════════════════════════════════
    // STEP 1: Sell WMON for USDC on sell_dex
    // ═══════════════════════════════════════════════════════════════════
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ STEP 1: Sell {} WMON on {} for USDC", amount, sell_dex);
    println!("└─────────────────────────────────────────────────────────────┘");

    let sell_params = SwapParams {
        router: sell_router,
        direction: SwapDirection::Sell,  // WMON -> USDC
        amount_in: amount,
        slippage_bps: slippage,
        expected_price: sell_price.price,
    };

    let sell_result = execute_swap(&provider, &signer, sell_params, &rpc_url).await?;
    print_swap_report(&sell_result);

    if !sell_result.success {
        return Err(eyre::eyre!("Step 1 failed: Sell swap failed"));
    }

    let usdc_received = sell_result.amount_out_human;
    println!("  ✓ Received: {:.6} USDC", usdc_received);

    // Small delay to ensure state is updated
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ═══════════════════════════════════════════════════════════════════
    // STEP 2: Buy WMON with USDC on buy_dex  
    // ═══════════════════════════════════════════════════════════════════
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ STEP 2: Buy WMON with {:.6} USDC on {}", usdc_received, buy_dex);
    println!("└─────────────────────────────────────────────────────────────┘");

    // Refresh prices for step 2 (prices may have moved)
    let prices = get_current_prices(&provider).await?;
    let buy_price_updated = prices.iter()
        .find(|p| p.pool_name.to_lowercase() == buy_dex.to_lowercase())
        .map(|p| p.price)
        .unwrap_or(buy_price.price);

    let buy_params = SwapParams {
        router: buy_router,
        direction: SwapDirection::Buy,  // USDC -> WMON
        amount_in: usdc_received,
        slippage_bps: slippage,
        expected_price: buy_price_updated,
    };

    let buy_result = execute_swap(&provider, &signer, buy_params, &rpc_url).await?;
    print_swap_report(&buy_result);

    if !buy_result.success {
        return Err(eyre::eyre!("Step 2 failed: Buy swap failed"));
    }

    let wmon_received = buy_result.amount_out_human;

    // ═══════════════════════════════════════════════════════════════════
    // FINAL REPORT
    // ═══════════════════════════════════════════════════════════════════
    let balances_after = get_balances(&provider, signer.address()).await?;
    
    let total_gas_cost = sell_result.gas_cost_wei.to::<u128>() as f64 / 1e18 
                       + buy_result.gas_cost_wei.to::<u128>() as f64 / 1e18;
    
    let gross_profit = wmon_received - amount;
    let net_profit = gross_profit; // Gas is paid in MON separately
    let profit_bps = (gross_profit / amount * 10000.0) as i32;

    println!("\n═══════════════════════════════════════════════════════════════");
    println!("  DEX-TO-DEX ARBITRAGE RESULT");
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  Route: WMON --({})-> USDC --({})-> WMON", sell_dex, buy_dex);
    println!();
    println!("  INPUT:");
    println!("    WMON In:         {:>12.6} WMON", amount);
    println!();
    println!("  INTERMEDIATE:");  
    println!("    USDC Received:   {:>12.6} USDC", usdc_received);
    println!();
    println!("  OUTPUT:");
    println!("    WMON Out:        {:>12.6} WMON", wmon_received);
    println!();
    println!("  PROFIT/LOSS:");
    let profit_color = if gross_profit >= 0.0 { "32" } else { "31" };
    println!("    Gross P/L:       \x1b[1;{}m{:>+12.6} WMON ({:+}bps)\x1b[0m", 
        profit_color, gross_profit, profit_bps);
    println!("    Gas Cost:        {:>12.6} MON", total_gas_cost);
    println!();
    println!("  BALANCES:");
    println!("    WMON Before:     {:>12.6}", balances_before.wmon_human);
    println!("    WMON After:      {:>12.6}", balances_after.wmon_human);
    println!("    MON Before:      {:>12.6}", balances_before.mon_human);
    println!("    MON After:       {:>12.6}", balances_after.mon_human);
    println!();
    println!("  TRANSACTIONS:");
    println!("    Sell TX: {}", sell_result.tx_hash);
    println!("    Buy TX:  {}", buy_result.tx_hash);
    println!();

    if gross_profit > 0.0 {
        println!("  ✅ ARBITRAGE PROFITABLE (before gas)");
    } else {
        println!("  ❌ ARBITRAGE UNPROFITABLE");
    }
    
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}
```

### 3. Add Match Arm in `main()`

```rust
Some(Commands::TestArb { sell_dex, buy_dex, amount, slippage }) => {
    run_test_arb(&sell_dex, &buy_dex, amount, slippage).await
}
```

### 4. Add Required Import

At the top of `main.rs`, ensure `Duration` is imported:
```rust
use std::time::Duration;
```

---

## Usage Examples

```bash
# Test arb based on detected opportunity from logs
# "LFJ → PancakeSwap1 | Buy: 0.02982 | Sell: 0.02994"
# This means: buy on LFJ (lower), sell on PancakeSwap1 (higher)
cargo run -- test-arb --sell-dex pancakeswap1 --buy-dex lfj --amount 1.0

# With higher slippage for volatile conditions
cargo run -- test-arb --sell-dex lfj --buy-dex pancakeswap1 --amount 2.0 --slippage 200

# Test smaller amount first
cargo run -- test-arb --sell-dex uniswap --buy-dex pancakeswap1 --amount 0.5
```

---

## Expected Output

```
Wallet: 0xad0e53732ae6ac04edc36f9f81c4bfc9aa344b2b

Fetching current prices...
  PancakeSwap1 price: 0.027194 USDC/WMON
  LFJ price: 0.027216 USDC/WMON
  Spread: 8 bps (0.08%)

══════════════════════════════════════════════════════════════
  DEX-TO-DEX ARBITRAGE TEST
══════════════════════════════════════════════════════════════
  Route: WMON --(lfj)-> USDC --(pancakeswap1)-> WMON
  Amount: 1.0 WMON
══════════════════════════════════════════════════════════════

┌─────────────────────────────────────────────────────────────┐
│ STEP 1: Sell 1.0 WMON on lfj for USDC                       │
└─────────────────────────────────────────────────────────────┘
  [swap execution details...]
  ✓ Received: 0.027216 USDC

┌─────────────────────────────────────────────────────────────┐  
│ STEP 2: Buy WMON with 0.027216 USDC on pancakeswap1         │
└─────────────────────────────────────────────────────────────┘
  [swap execution details...]

═══════════════════════════════════════════════════════════════
  DEX-TO-DEX ARBITRAGE RESULT
═══════════════════════════════════════════════════════════════

  Route: WMON --(lfj)-> USDC --(pancakeswap1)-> WMON

  INPUT:
    WMON In:             1.000000 WMON

  INTERMEDIATE:
    USDC Received:       0.027216 USDC

  OUTPUT:
    WMON Out:            1.000800 WMON

  PROFIT/LOSS:
    Gross P/L:          +0.000800 WMON (+8bps)
    Gas Cost:            0.095000 MON

  ✅ ARBITRAGE PROFITABLE (before gas)
═══════════════════════════════════════════════════════════════
```

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/main.rs` | Add `TestArb` command enum, add `run_test_arb()` function, add match arm |

---

## Testing Checklist

- [ ] `cargo build` succeeds
- [ ] `cargo run -- test-arb --help` shows the new command
- [ ] Test with small amount (0.5 WMON) first
- [ ] Verify both transactions appear on block explorer
- [ ] Compare actual vs expected profit
- [ ] Test with different DEX pairs from the arb logs

---

## Notes

1. **Direction clarity**: The logs show "Buy: X | Sell: Y" where:
   - Buy price = the DEX where you BUY WMON (lower price is better)
   - Sell price = the DEX where you SELL WMON (higher price is better)

2. **Gas costs**: Are paid in MON (native), not WMON. The profit calculation shows gross WMON profit, gas is separate.

3. **Timing**: There's a 500ms delay between swaps to ensure state propagation. In production this would be removed.

4. **Price refresh**: We fetch updated prices before step 2 since prices may have moved during step 1.
