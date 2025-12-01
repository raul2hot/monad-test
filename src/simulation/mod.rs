//! Arbitrage Opportunity Simulation & Verification Module
//!
//! This module provides comprehensive simulation and verification for arbitrage opportunities:
//! - Fee validation from on-chain pool contracts
//! - Atomic multi-pool quotes (same block)
//! - Liquidity depth analysis
//! - Net profit calculation including all costs
//! - eth_call simulation for execution verification

pub mod fee_validator;
pub mod liquidity;
pub mod profit_calculator;
pub mod quote_fetcher;
pub mod simulator;

// Re-exports for external use
pub use fee_validator::{FeeValidator, PoolFeeInfo};
pub use liquidity::{LiquidityAnalyzer, LiquidityInfo};
pub use profit_calculator::{ProfitCalculator, ProfitBreakdown};
pub use quote_fetcher::{AtomicQuote, PoolQuote, QuoteFetcher};
pub use simulator::{SimulationConfidence, SimulationResult, Simulator};
