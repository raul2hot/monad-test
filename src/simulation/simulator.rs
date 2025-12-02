//! Arbitrage Simulator Module
//!
//! Main orchestrator for simulating arbitrage opportunities. Combines:
//! - Fee validation
//! - Atomic quotes
//! - Liquidity analysis
//! - Profit calculation
//! - eth_call simulation

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use eyre::{eyre, Result};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Maximum reasonable gas for a 2-3 hop swap (1M gas)
/// When eth_estimateGas reverts, it returns absurdly high values
/// A typical 2-hop swap uses ~300-500k gas
const MAX_REASONABLE_GAS: u64 = 1_000_000;

use super::fee_validator::FeeValidator;
use super::liquidity::{LiquidityAnalyzer, LiquidityInfo};
use super::profit_calculator::{FlashLoanProvider, ProfitBreakdown, ProfitCalculator};
use super::quote_fetcher::{AtomicQuote, PoolInfo, QuoteFetcher};
use crate::config::thresholds;
use crate::config::tokens;
use crate::dex::Dex;
use crate::graph::ArbitrageCycle;

/// Confidence level for simulation results
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulationConfidence {
    /// eth_call simulation passed - high confidence
    High,
    /// Quote-based calculation only - medium confidence
    Medium,
    /// Stale data or estimation - low confidence
    Low,
    /// Simulation failed or reverted
    Failed,
}

impl std::fmt::Display for SimulationConfidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SimulationConfidence::High => write!(f, "HIGH"),
            SimulationConfidence::Medium => write!(f, "MEDIUM"),
            SimulationConfidence::Low => write!(f, "LOW"),
            SimulationConfidence::Failed => write!(f, "FAILED"),
        }
    }
}

/// Complete simulation result for an arbitrage opportunity
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Token path
    pub path: Vec<Address>,
    /// Pool addresses used
    pub pools: Vec<Address>,
    /// DEXes involved
    pub dexes: Vec<Dex>,
    /// Input amount used for simulation
    pub input_amount: U256,
    /// Expected output amount after all swaps
    pub output_amount: U256,
    /// Gross profit in basis points (before fees)
    pub gross_profit_bps: i32,
    /// Net profit in basis points (after ALL fees)
    pub net_profit_bps: i32,
    /// Total DEX fees in basis points
    pub total_dex_fees_bps: u32,
    /// Flash loan fee in basis points
    pub flash_loan_fee_bps: u32,
    /// Estimated gas cost in wei
    pub gas_cost_wei: U256,
    /// Estimated gas units
    pub gas_units: u64,
    /// Is the opportunity profitable after all costs?
    pub is_profitable: bool,
    /// Is profit above minimum threshold?
    pub above_threshold: bool,
    /// Simulation confidence level
    pub confidence: SimulationConfidence,
    /// Block number at which simulation was performed
    pub block_number: u64,
    /// Detailed profit breakdown
    pub profit_breakdown: ProfitBreakdown,
    /// Liquidity info for each pool
    pub liquidity_info: Vec<LiquidityInfo>,
    /// Individual pool quotes
    pub quotes: AtomicQuote,
    /// Rejection reason if not profitable
    pub rejection_reason: Option<String>,
}

/// Main simulator for arbitrage opportunities
pub struct Simulator<P> {
    provider: Arc<P>,
    fee_validator: FeeValidator<P>,
    quote_fetcher: QuoteFetcher<P>,
    liquidity_analyzer: LiquidityAnalyzer<P>,
    profit_calculator: ProfitCalculator,
    /// Minimum liquidity required in a pool (in wei)
    min_liquidity: U256,
}

impl<P> Simulator<P>
where
    P: Provider + Clone + 'static,
{
    pub fn new(provider: P) -> Self {
        let provider = Arc::new(provider);

        Self {
            fee_validator: FeeValidator::new((*provider).clone()),
            quote_fetcher: QuoteFetcher::new((*provider).clone()),
            liquidity_analyzer: LiquidityAnalyzer::new((*provider).clone()),
            profit_calculator: ProfitCalculator::new(
                FlashLoanProvider::Neverland,
                thresholds::MIN_PROFIT_BPS,  // Use centralized threshold
                5,   // 0.05% safety margin
            ),
            // Use MIN_ACTIVE_LIQUIDITY since simulation checks active liquidity near current price
            // This matches what liquidity.rs measures (bins [-10, +10] for LFJ, current tick for V3)
            min_liquidity: U256::from(thresholds::MIN_ACTIVE_LIQUIDITY),
            provider,
        }
    }

    /// Create simulator with custom settings
    pub fn with_settings(
        provider: P,
        flash_loan_provider: FlashLoanProvider,
        min_profit_bps: u32,
        safety_margin_bps: u32,
        min_liquidity: U256,
    ) -> Self {
        let provider = Arc::new(provider);

        Self {
            fee_validator: FeeValidator::new((*provider).clone()),
            quote_fetcher: QuoteFetcher::new((*provider).clone()),
            liquidity_analyzer: LiquidityAnalyzer::new((*provider).clone()),
            profit_calculator: ProfitCalculator::new(
                flash_loan_provider,
                min_profit_bps,
                safety_margin_bps,
            ),
            min_liquidity,
            provider,
        }
    }

    /// Simulate a full arbitrage cycle
    pub async fn simulate_cycle(
        &self,
        cycle: &ArbitrageCycle,
        input_amount: U256,
    ) -> Result<SimulationResult> {
        debug!(
            "Simulating cycle: {} with {} input",
            cycle.token_path(),
            input_amount
        );

        // 1. Build pool info list with correct swap directions
        let pool_infos = self.build_pool_infos(cycle)?;

        // 2. Check liquidity for all pools (for diagnostics/logging only)
        // CRITICAL FIX: Do NOT reject based on liquidity threshold alone!
        // The liquidity value from V3 pools is the L parameter, not actual reserves.
        // The only reliable way to validate swap feasibility is via the quoter.
        let liquidity_info = self.check_liquidity(&cycle.pools, &cycle.dexes).await?;

        // Log low liquidity pools but DON'T reject - let the quoter decide
        for (i, liq) in liquidity_info.iter().enumerate() {
            if liq.total_liquidity < self.min_liquidity {
                debug!(
                    "Pool {} has low liquidity: {} < threshold {} (will verify via quoter)",
                    cycle.pools[i],
                    liq.total_liquidity,
                    self.min_liquidity
                );
            }
        }

        // 3. Get atomic quotes for the path (this is the REAL validation)
        //
        // DIAGNOSTIC LOGGING: Log the expected path and direction for each hop
        // This helps debug issues where swapForY direction might be wrong
        debug!("=== QUOTE PATH DETAILS ===");
        for (i, pool_info) in pool_infos.iter().enumerate() {
            let (token_in, token_out) = if pool_info.zero_for_one {
                (pool_info.token0, pool_info.token1)
            } else {
                (pool_info.token1, pool_info.token0)
            };
            debug!(
                "  Hop {}: {} -> {} via {} pool {} (zero_for_one={})",
                i + 1,
                tokens::symbol(token_in),
                tokens::symbol(token_out),
                pool_info.dex,
                pool_info.address,
                pool_info.zero_for_one
            );
        }

        let quotes = match self.quote_fetcher.get_path_quotes(&pool_infos, input_amount).await {
            Ok(q) => {
                // Log successful quote details for debugging
                debug!("=== QUOTE RESULTS ===");
                for (i, quote) in q.quotes.iter().enumerate() {
                    let rate = if quote.amount_in > U256::ZERO {
                        quote.amount_out.to::<u128>() as f64 / quote.amount_in.to::<u128>() as f64
                    } else {
                        0.0
                    };
                    debug!(
                        "  Hop {}: {} {} -> {} {} | rate={:.8} | fee={} bps",
                        i + 1,
                        quote.amount_in,
                        tokens::symbol(quote.token_in),
                        quote.amount_out,
                        tokens::symbol(quote.token_out),
                        rate,
                        quote.fee_bps
                    );
                }
                let final_output = q.final_amount_out();
                let gross_ratio = if input_amount > U256::ZERO {
                    final_output.to::<u128>() as f64 / input_amount.to::<u128>() as f64
                } else {
                    0.0
                };
                debug!(
                    "  TOTAL: {} -> {} (ratio: {:.6}, {:+.2}%)",
                    input_amount,
                    final_output,
                    gross_ratio,
                    (gross_ratio - 1.0) * 100.0
                );
                q
            }
            Err(e) => {
                // Enhanced diagnostic logging for quote failures
                warn!(
                    "Quote failed for cycle {} via {}: {}",
                    cycle.token_path(),
                    cycle.dex_path(),
                    e
                );

                // Log detailed pool information for debugging
                for (i, pool_info) in pool_infos.iter().enumerate() {
                    let liq = liquidity_info.get(i);
                    debug!(
                        "  Pool {}: {} ({}) | token0={} ({}d), token1={} ({}d) | fee={} | liq={} | zero_for_one={}",
                        i,
                        pool_info.address,
                        pool_info.dex,
                        tokens::symbol(pool_info.token0),
                        tokens::decimals(pool_info.token0),
                        tokens::symbol(pool_info.token1),
                        tokens::decimals(pool_info.token1),
                        pool_info.fee,
                        liq.map(|l| l.total_liquidity.to_string()).unwrap_or_else(|| "N/A".to_string()),
                        pool_info.zero_for_one
                    );
                }

                return Ok(self.create_rejected_result(
                    cycle,
                    input_amount,
                    &liquidity_info,
                    format!("Quote failed: {}", e),
                ));
            }
        };

        // 4. Calculate total fees from actual quotes
        let total_dex_fees_bps: u32 = quotes.quotes.iter().map(|q| q.fee_bps).sum();

        // 5. Get gas price and estimate
        let gas_price_u128 = self.provider.get_gas_price().await.unwrap_or(50u128 * 10u128.pow(9));
        let gas_price = U256::from(gas_price_u128);
        let gas_units = quotes.total_gas_estimate + 50_000; // Add buffer for flash loan

        // 5a. Sanity check: reject if gas estimate is absurdly high
        // When eth_estimateGas reverts, it returns huge values (e.g., 4.73 MON worth of gas)
        // A typical 2-3 hop swap uses 300-500k gas, so 1M is a generous upper bound
        if gas_units > MAX_REASONABLE_GAS {
            warn!(
                "Gas estimate {} exceeds max reasonable {} - likely revert in quoter",
                gas_units, MAX_REASONABLE_GAS
            );
            return Ok(self.create_rejected_result(
                cycle,
                input_amount,
                &liquidity_info,
                format!(
                    "Gas estimate too high ({} > {}), likely swap would revert",
                    gas_units, MAX_REASONABLE_GAS
                ),
            ));
        }

        // 6. Calculate profit breakdown
        let gross_output = quotes.final_amount_out();

        // 6a. Sanity check: reject if gross output is extremely low (indicates bad quote)
        // If output < 50% of input, something is very wrong (e.g., -8013 bps = output is 20% of input)
        // Real arbitrage opportunities should have gross output close to input (within a few %)
        let min_reasonable_output = input_amount / U256::from(2); // 50% of input
        if gross_output < min_reasonable_output {
            let loss_bps = if gross_output > U256::ZERO {
                let ratio = (input_amount - gross_output).to::<u128>() * 10000 / input_amount.to::<u128>();
                ratio as i32
            } else {
                10000 // 100% loss
            };
            warn!(
                "Gross output {} is < 50% of input {} (-{} bps) - likely bad quote from V4/hooks",
                gross_output, input_amount, loss_bps
            );
            return Ok(self.create_rejected_result(
                cycle,
                input_amount,
                &liquidity_info,
                format!(
                    "Gross output too low (-{} bps), likely pool returned bad quote",
                    loss_bps
                ),
            ));
        }

        let profit_breakdown = self.profit_calculator.calculate(
            input_amount,
            gross_output,
            total_dex_fees_bps,
            gas_units,
            gas_price,
        );

        // 7. Determine confidence level
        let confidence = self.determine_confidence(&quotes, &liquidity_info);

        // 8. Build rejection reason if not profitable
        let rejection_reason = if !profit_breakdown.above_threshold {
            Some(self.build_rejection_reason(&profit_breakdown))
        } else {
            None
        };

        Ok(SimulationResult {
            path: cycle.path.clone(),
            pools: cycle.pools.clone(),
            dexes: cycle.dexes.clone(),
            input_amount,
            output_amount: profit_breakdown.net_output,
            gross_profit_bps: profit_breakdown.gross_profit_bps,
            net_profit_bps: profit_breakdown.net_profit_bps,
            total_dex_fees_bps,
            flash_loan_fee_bps: profit_breakdown.flash_loan_fee_bps,
            gas_cost_wei: profit_breakdown.gas_cost,
            gas_units,
            is_profitable: profit_breakdown.is_profitable,
            above_threshold: profit_breakdown.above_threshold,
            confidence,
            block_number: quotes.block_number,
            profit_breakdown,
            liquidity_info,
            quotes,
            rejection_reason,
        })
    }

    /// Build pool info list with correct swap directions
    fn build_pool_infos(&self, cycle: &ArbitrageCycle) -> Result<Vec<PoolInfo>> {
        let mut pool_infos = Vec::with_capacity(cycle.pools.len());

        for i in 0..cycle.pools.len() {
            let token_in = cycle.path[i];
            let token_out = cycle.path[i + 1];

            // For each pool, we need to determine which direction to swap
            // Pools store token0 < token1 by convention
            let (token0, token1) = if token_in < token_out {
                (token_in, token_out)
            } else {
                (token_out, token_in)
            };

            let zero_for_one = token_in == token0;

            pool_infos.push(PoolInfo {
                address: cycle.pools[i],
                dex: cycle.dexes[i],
                token0,
                token1,
                fee: cycle.fees[i],
                liquidity: U256::ZERO, // Will be fetched by quoter
                zero_for_one,
                tick_spacing: None,
                hooks: None,
            });
        }

        Ok(pool_infos)
    }

    /// Check liquidity for all pools in the path
    async fn check_liquidity(
        &self,
        pools: &[Address],
        dexes: &[Dex],
    ) -> Result<Vec<LiquidityInfo>> {
        let mut liquidity_info = Vec::with_capacity(pools.len());

        for (address, dex) in pools.iter().zip(dexes.iter()) {
            match self.liquidity_analyzer.get_liquidity(*address, *dex, None).await {
                Ok(info) => liquidity_info.push(info),
                Err(e) => {
                    warn!("Failed to get liquidity for pool {}: {}", address, e);
                    // Create empty liquidity info
                    liquidity_info.push(LiquidityInfo {
                        pool_address: *address,
                        dex: *dex,
                        total_liquidity: U256::ZERO,
                        liquidity_usd: 0.0,
                        max_trade_05pct_slippage: U256::ZERO,
                        max_trade_1pct_slippage: U256::ZERO,
                        max_trade_2pct_slippage: U256::ZERO,
                        current_tick: None,
                        active_bin_id: None,
                    });
                }
            }
        }

        Ok(liquidity_info)
    }

    /// Determine simulation confidence based on data quality
    fn determine_confidence(
        &self,
        quotes: &AtomicQuote,
        liquidity_info: &[LiquidityInfo],
    ) -> SimulationConfidence {
        // Check if all quotes are valid
        if quotes.quotes.iter().any(|q| q.amount_out.is_zero()) {
            return SimulationConfidence::Low;
        }

        // Check if all pools have sufficient liquidity data
        if liquidity_info.iter().any(|l| l.total_liquidity.is_zero()) {
            return SimulationConfidence::Low;
        }

        // Check quote age (should be recent)
        // In production, would compare against current block

        // All checks passed - medium confidence (would be high with eth_call)
        SimulationConfidence::Medium
    }

    /// Build rejection reason string
    fn build_rejection_reason(&self, breakdown: &ProfitBreakdown) -> String {
        if breakdown.net_profit_bps < 0 {
            format!(
                "Net loss of {} bps (DEX fees: {} bps, flash loan: {} bps, gas: {} wei)",
                -breakdown.net_profit_bps,
                breakdown.total_dex_fees_bps,
                breakdown.flash_loan_fee_bps,
                breakdown.gas_cost
            )
        } else {
            format!(
                "Profit {} bps below threshold {} bps",
                breakdown.net_profit_bps,
                breakdown.min_profit_threshold_bps
            )
        }
    }

    /// Create a rejected simulation result
    fn create_rejected_result(
        &self,
        cycle: &ArbitrageCycle,
        input_amount: U256,
        liquidity_info: &[LiquidityInfo],
        reason: String,
    ) -> SimulationResult {
        let empty_breakdown = ProfitBreakdown {
            input_amount,
            gross_output: U256::ZERO,
            total_dex_fees: U256::ZERO,
            total_dex_fees_bps: 0,
            flash_loan_fee: U256::ZERO,
            flash_loan_fee_bps: 0,
            gas_cost: U256::ZERO,
            gas_price: U256::ZERO,
            gas_units: 0,
            net_output: U256::ZERO,
            gross_profit: 0,
            net_profit: 0,
            gross_profit_bps: 0,
            net_profit_bps: 0,
            is_profitable: false,
            min_profit_threshold_bps: 0,
            above_threshold: false,
        };

        let empty_quotes = AtomicQuote {
            block_number: 0,
            timestamp: 0,
            quotes: vec![],
            total_gas_estimate: 0,
        };

        SimulationResult {
            path: cycle.path.clone(),
            pools: cycle.pools.clone(),
            dexes: cycle.dexes.clone(),
            input_amount,
            output_amount: U256::ZERO,
            gross_profit_bps: 0,
            net_profit_bps: 0,
            total_dex_fees_bps: 0,
            flash_loan_fee_bps: 0,
            gas_cost_wei: U256::ZERO,
            gas_units: 0,
            is_profitable: false,
            above_threshold: false,
            confidence: SimulationConfidence::Failed,
            block_number: 0,
            profit_breakdown: empty_breakdown,
            liquidity_info: liquidity_info.to_vec(),
            quotes: empty_quotes,
            rejection_reason: Some(reason),
        }
    }
}

impl SimulationResult {
    /// Get a summary string for logging
    pub fn summary(&self) -> String {
        let status = if self.above_threshold {
            "VERIFIED"
        } else if self.is_profitable {
            "MARGINAL"
        } else {
            "REJECTED"
        };

        format!(
            "[{}] {} | Net: {} bps | Confidence: {}",
            status,
            self.token_path(),
            self.net_profit_bps,
            self.confidence
        )
    }

    /// Get formatted token path
    pub fn token_path(&self) -> String {
        self.path
            .iter()
            .map(|addr| tokens::symbol(*addr))
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    /// Get formatted DEX path
    pub fn dex_path(&self) -> String {
        self.dexes
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    /// Print detailed simulation report
    pub fn detailed_report(&self) -> String {
        let status_line = if self.above_threshold {
            "STATUS: VERIFIED - PROFITABLE"
        } else if self.is_profitable {
            "STATUS: MARGINAL - Below threshold"
        } else {
            "STATUS: REJECTED - Not profitable"
        };

        let mut report = format!(
            r#"
========================================
 SIMULATION RESULT
========================================
   Path: {}
   DEXes: {}
   Block: {}
   Confidence: {}
   {}
"#,
            self.token_path(),
            self.dex_path(),
            self.block_number,
            self.confidence,
            status_line
        );

        // Add pool details
        for (i, quote) in self.quotes.quotes.iter().enumerate() {
            report.push_str(&format!(
                r#"
   Pool {}: {} ({})
     - Fee: {} bps
     - Quote: {} -> {}
     - Liquidity: {}
"#,
                i + 1,
                self.pools.get(i).map(|p| format!("{}", p)).unwrap_or_default(),
                self.dexes.get(i).map(|d| d.to_string()).unwrap_or_default(),
                quote.fee_bps,
                quote.amount_in,
                quote.amount_out,
                self.liquidity_info.get(i).map(|l| l.description()).unwrap_or_default(),
            ));
        }

        // Add profit breakdown
        report.push_str(&format!(
            r#"
   ─────────────────────────────────────
   PROFIT BREAKDOWN
   ─────────────────────────────────────
   Gross Profit:     {} bps
   DEX Fees:         -{} bps
   Flash Loan Fee:   -{} bps (Neverland)
   Gas Cost:         ~{} MON
   ─────────────────────────────────────
   NET PROFIT:       {} bps
"#,
            self.gross_profit_bps,
            self.total_dex_fees_bps,
            self.flash_loan_fee_bps,
            format_mon(self.gas_cost_wei),
            self.net_profit_bps
        ));

        if let Some(reason) = &self.rejection_reason {
            report.push_str(&format!("\n   Rejection: {}\n", reason));
        }

        report.push_str("\n========================================\n");

        report
    }
}

/// Format MON amount (18 decimals)
fn format_mon(amount: U256) -> String {
    let amount_u128 = amount.to::<u128>();
    if amount_u128 == 0 {
        return "0".to_string();
    }

    let whole = amount_u128 / 10u128.pow(18);
    let frac = (amount_u128 % 10u128.pow(18)) / 10u128.pow(14);

    if whole > 0 {
        format!("{}.{:04}", whole, frac)
    } else {
        format!("0.{:04}", frac)
    }
}
