use chrono::Local;
use crate::execution::{SwapResult, SwapDirection};

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
