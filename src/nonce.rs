use alloy::primitives::Address;
use alloy::providers::Provider;
use eyre::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Global nonce manager - initialized once, used for all transactions
static NONCE: OnceLock<AtomicU64> = OnceLock::new();
static WALLET_ADDRESS: OnceLock<Address> = OnceLock::new();

/// Initialize the nonce manager by fetching current nonce from RPC.
/// Must be called once at startup before any transactions.
/// Safe to call multiple times - subsequent calls are no-ops.
pub async fn init_nonce<P: Provider>(provider: &P, wallet_address: Address) -> Result<u64> {
    // Store wallet address for validation
    let _ = WALLET_ADDRESS.set(wallet_address);

    // If already initialized, return current value
    if let Some(nonce) = NONCE.get() {
        return Ok(nonce.load(Ordering::SeqCst));
    }

    // Fetch from RPC
    let nonce = provider.get_transaction_count(wallet_address).await?;

    // Initialize atomic counter
    let _ = NONCE.set(AtomicU64::new(nonce));

    Ok(nonce)
}

/// Get the next nonce and increment the counter atomically.
/// Panics if init_nonce() was not called first.
pub fn next_nonce() -> u64 {
    NONCE
        .get()
        .expect("Nonce manager not initialized. Call init_nonce() first.")
        .fetch_add(1, Ordering::SeqCst)
}

/// Get current nonce without incrementing (for debugging/display).
#[allow(dead_code)]
pub fn current_nonce() -> u64 {
    NONCE
        .get()
        .expect("Nonce manager not initialized. Call init_nonce() first.")
        .load(Ordering::SeqCst)
}

/// Reset nonce by re-fetching from RPC. Use if transaction failed.
#[allow(dead_code)]
pub async fn reset_nonce<P: Provider>(provider: &P) -> Result<u64> {
    let wallet_address = *WALLET_ADDRESS
        .get()
        .expect("Nonce manager not initialized.");

    let nonce = provider.get_transaction_count(wallet_address).await?;

    if let Some(atomic_nonce) = NONCE.get() {
        atomic_nonce.store(nonce, Ordering::SeqCst);
    }

    Ok(nonce)
}
