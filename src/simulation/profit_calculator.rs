//! Profit Calculator Module
//!
//! Calculates net profit after all costs including:
//! - DEX swap fees
//! - Flash loan fees
//! - Gas costs
//! - Slippage

use alloy::primitives::U256;

/// Flash loan provider information
#[derive(Debug, Clone, Copy)]
pub enum FlashLoanProvider {
    /// Neverland flash loan (9 bps fee)
    Neverland,
    /// Aave V3 (5 bps fee)
    AaveV3,
    /// No flash loan (using own capital)
    None,
}

impl FlashLoanProvider {
    /// Get the flash loan fee in basis points
    pub fn fee_bps(&self) -> u32 {
        match self {
            FlashLoanProvider::Neverland => 9,  // 0.09%
            FlashLoanProvider::AaveV3 => 5,     // 0.05%
            FlashLoanProvider::None => 0,
        }
    }

    /// Get provider name
    pub fn name(&self) -> &'static str {
        match self {
            FlashLoanProvider::Neverland => "Neverland",
            FlashLoanProvider::AaveV3 => "Aave V3",
            FlashLoanProvider::None => "None",
        }
    }
}

/// Breakdown of all costs in an arbitrage execution
#[derive(Debug, Clone)]
pub struct ProfitBreakdown {
    /// Input amount (borrowed or own capital)
    pub input_amount: U256,
    /// Gross output from swaps (before any deductions)
    pub gross_output: U256,
    /// Total DEX fees paid (sum across all swaps)
    pub total_dex_fees: U256,
    /// Total DEX fees in basis points
    pub total_dex_fees_bps: u32,
    /// Flash loan fee paid
    pub flash_loan_fee: U256,
    /// Flash loan fee in basis points
    pub flash_loan_fee_bps: u32,
    /// Estimated gas cost in native token (MON)
    pub gas_cost: U256,
    /// Gas price used for estimation
    pub gas_price: U256,
    /// Gas units estimated
    pub gas_units: u64,
    /// Net output after all costs
    pub net_output: U256,
    /// Gross profit (gross_output - input_amount)
    pub gross_profit: i128,
    /// Net profit (net_output - input_amount)
    pub net_profit: i128,
    /// Gross profit in basis points
    pub gross_profit_bps: i32,
    /// Net profit in basis points
    pub net_profit_bps: i32,
    /// Is the opportunity profitable?
    pub is_profitable: bool,
    /// Minimum profit required (in bps) for execution
    pub min_profit_threshold_bps: u32,
    /// Is profit above threshold?
    pub above_threshold: bool,
}

/// Profit calculator for arbitrage opportunities
#[derive(Debug, Clone)]
pub struct ProfitCalculator {
    /// Flash loan provider
    flash_loan_provider: FlashLoanProvider,
    /// Minimum profit threshold in bps
    min_profit_bps: u32,
    /// Safety margin to add to required profit (buffer for slippage)
    safety_margin_bps: u32,
}

impl Default for ProfitCalculator {
    fn default() -> Self {
        Self {
            flash_loan_provider: FlashLoanProvider::Neverland,
            min_profit_bps: 10, // 0.1% minimum
            safety_margin_bps: 5, // 0.05% safety buffer
        }
    }
}

impl ProfitCalculator {
    pub fn new(
        flash_loan_provider: FlashLoanProvider,
        min_profit_bps: u32,
        safety_margin_bps: u32,
    ) -> Self {
        Self {
            flash_loan_provider,
            min_profit_bps,
            safety_margin_bps,
        }
    }

    /// Calculate complete profit breakdown for an arbitrage opportunity
    pub fn calculate(
        &self,
        input_amount: U256,
        gross_output: U256,
        total_dex_fees_bps: u32,
        gas_units: u64,
        gas_price: U256,
    ) -> ProfitBreakdown {
        // Calculate DEX fees (already deducted from gross_output, but we track them)
        let total_dex_fees = input_amount * U256::from(total_dex_fees_bps) / U256::from(10000);

        // Calculate flash loan fee
        let flash_loan_fee_bps = self.flash_loan_provider.fee_bps();
        let flash_loan_fee = input_amount * U256::from(flash_loan_fee_bps) / U256::from(10000);

        // Calculate gas cost
        let gas_cost = U256::from(gas_units) * gas_price;

        // Calculate net output
        // gross_output already has swap fees deducted (via quoter)
        // We need to subtract flash loan fee and gas cost
        let net_output = gross_output
            .saturating_sub(flash_loan_fee)
            .saturating_sub(gas_cost);

        // Calculate profits
        let gross_profit = if gross_output >= input_amount {
            (gross_output - input_amount).to::<u128>() as i128
        } else {
            -((input_amount - gross_output).to::<u128>() as i128)
        };

        let net_profit = if net_output >= input_amount {
            (net_output - input_amount).to::<u128>() as i128
        } else {
            -((input_amount - net_output).to::<u128>() as i128)
        };

        // Calculate profit in basis points
        let input_u128 = input_amount.to::<u128>();
        let gross_profit_bps = if input_u128 > 0 {
            ((gross_profit * 10000) / input_u128 as i128) as i32
        } else {
            0
        };

        let net_profit_bps = if input_u128 > 0 {
            ((net_profit * 10000) / input_u128 as i128) as i32
        } else {
            0
        };

        let is_profitable = net_profit > 0;
        let above_threshold = net_profit_bps >= (self.min_profit_bps + self.safety_margin_bps) as i32;

        ProfitBreakdown {
            input_amount,
            gross_output,
            total_dex_fees,
            total_dex_fees_bps,
            flash_loan_fee,
            flash_loan_fee_bps,
            gas_cost,
            gas_price,
            gas_units,
            net_output,
            gross_profit,
            net_profit,
            gross_profit_bps,
            net_profit_bps,
            is_profitable,
            min_profit_threshold_bps: self.min_profit_bps + self.safety_margin_bps,
            above_threshold,
        }
    }

    /// Calculate minimum output needed for profitable execution
    pub fn minimum_output_for_profit(&self, input_amount: U256, gas_cost: U256) -> U256 {
        let flash_loan_fee = input_amount * U256::from(self.flash_loan_provider.fee_bps()) / U256::from(10000);
        let min_profit = input_amount * U256::from(self.min_profit_bps + self.safety_margin_bps) / U256::from(10000);

        input_amount + flash_loan_fee + gas_cost + min_profit
    }

    /// Calculate breakeven output (where net profit = 0)
    pub fn breakeven_output(&self, input_amount: U256, gas_cost: U256) -> U256 {
        let flash_loan_fee = input_amount * U256::from(self.flash_loan_provider.fee_bps()) / U256::from(10000);
        input_amount + flash_loan_fee + gas_cost
    }

    /// Estimate required gross profit to achieve target net profit
    pub fn required_gross_profit_bps(&self, gas_cost_bps: u32) -> u32 {
        self.flash_loan_provider.fee_bps()
            + gas_cost_bps
            + self.min_profit_bps
            + self.safety_margin_bps
    }

    /// Get flash loan provider
    pub fn flash_loan_provider(&self) -> FlashLoanProvider {
        self.flash_loan_provider
    }

    /// Update flash loan provider
    pub fn set_flash_loan_provider(&mut self, provider: FlashLoanProvider) {
        self.flash_loan_provider = provider;
    }
}

impl ProfitBreakdown {
    /// Get a summary string for logging
    pub fn summary(&self) -> String {
        format!(
            "Gross: {} bps | DEX Fees: -{} bps | Flash Loan: -{} bps | Gas: ~{} | Net: {} bps",
            self.gross_profit_bps,
            self.total_dex_fees_bps,
            self.flash_loan_fee_bps,
            format_gas_cost(self.gas_cost),
            self.net_profit_bps
        )
    }

    /// Get detailed breakdown string for logging
    pub fn detailed_breakdown(&self) -> String {
        format!(
            r#"
   Input Amount:    {}
   Gross Output:    {}
   ─────────────────────────
   DEX Fees:        -{} ({} bps)
   Flash Loan Fee:  -{} ({} bps)
   Gas Cost:        -{} (~{} gas @ {} gwei)
   ─────────────────────────
   Net Output:      {}
   Gross Profit:    {} bps
   NET PROFIT:      {} bps
   Status:          {}"#,
            format_amount(self.input_amount),
            format_amount(self.gross_output),
            format_amount(self.total_dex_fees),
            self.total_dex_fees_bps,
            format_amount(self.flash_loan_fee),
            self.flash_loan_fee_bps,
            format_amount(self.gas_cost),
            self.gas_units,
            self.gas_price / U256::from(10u128.pow(9)),
            format_amount(self.net_output),
            self.gross_profit_bps,
            self.net_profit_bps,
            if self.above_threshold {
                "PROFITABLE ✓"
            } else if self.is_profitable {
                "PROFITABLE (below threshold)"
            } else {
                "NOT PROFITABLE ✗"
            }
        )
    }

    /// Calculate effective return multiplier
    pub fn effective_return(&self) -> f64 {
        if self.input_amount.is_zero() {
            return 0.0;
        }
        self.net_output.to::<u128>() as f64 / self.input_amount.to::<u128>() as f64
    }
}

/// Format amount in human-readable form (assuming 18 decimals)
fn format_amount(amount: U256) -> String {
    let amount_u128 = amount.to::<u128>();
    let whole = amount_u128 / 10u128.pow(18);
    let frac = (amount_u128 % 10u128.pow(18)) / 10u128.pow(14); // 4 decimal places
    format!("{}.{:04}", whole, frac)
}

/// Format gas cost in human-readable form
fn format_gas_cost(gas_cost: U256) -> String {
    let cost_u128 = gas_cost.to::<u128>();
    if cost_u128 == 0 {
        return "~0".to_string();
    }

    let whole = cost_u128 / 10u128.pow(18);
    let frac = (cost_u128 % 10u128.pow(18)) / 10u128.pow(14);

    if whole > 0 {
        format!("{}.{:04} MON", whole, frac)
    } else {
        format!("0.{:04} MON", frac)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profit_calculation() {
        let calculator = ProfitCalculator::new(FlashLoanProvider::Neverland, 10, 5);

        let input = U256::from(1000u128 * 10u128.pow(18)); // 1000 tokens
        let output = U256::from(1010u128 * 10u128.pow(18)); // 1010 tokens (1% gross profit)
        let gas_units = 300_000u64;
        let gas_price = U256::from(50u128 * 10u128.pow(9)); // 50 gwei

        let breakdown = calculator.calculate(input, output, 60, gas_units, gas_price);

        assert!(breakdown.gross_profit_bps == 100); // 1%
        assert!(breakdown.is_profitable);
        println!("{}", breakdown.detailed_breakdown());
    }

    #[test]
    fn test_flash_loan_fees() {
        assert_eq!(FlashLoanProvider::Neverland.fee_bps(), 9);
        assert_eq!(FlashLoanProvider::AaveV3.fee_bps(), 5);
        assert_eq!(FlashLoanProvider::None.fee_bps(), 0);
    }

    #[test]
    fn test_breakeven_calculation() {
        let calculator = ProfitCalculator::new(FlashLoanProvider::Neverland, 10, 5);

        let input = U256::from(1000u128 * 10u128.pow(18));
        let gas_cost = U256::from(10u128.pow(15)); // 0.001 MON

        let breakeven = calculator.breakeven_output(input, gas_cost);

        // Should be input + flash loan fee (0.09%) + gas
        let expected_flash_fee = input * U256::from(9) / U256::from(10000);
        let expected = input + expected_flash_fee + gas_cost;

        assert_eq!(breakeven, expected);
    }
}
