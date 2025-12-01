# Uniswap V4 on Monad: Implementation Guide for Rust Arbitrage Bots

**Uniswap V4 is not yet deployed on Monad Mainnet (Chain ID 143).** As of December 2025, only Uniswap V2 and V3 are live on Monad, with V4 deployment pending. This documentation covers verified V3 addresses on Monad, the complete V4 architecture for future implementation, and the Solidity interfaces needed for Rust/alloy bindings.

Monad launched its mainnet on November 24, 2025, with Uniswap Labs supporting the network from day one. The ecosystem currently includes **$59.62M TVL** across Uniswap protocols, with V3 handling approximately $27M. While V4 is deployed on 14+ other chains (Ethereum, Base, Arbitrum, Optimism, etc.), Monad is not yet on the official V4 deployment list.

---

## Verified contract addresses on Monad Mainnet

The following addresses are confirmed through official Monad documentation and block explorer verification:

### Canonical Monad contracts
| Contract | Address |
|----------|---------|
| **WMON** (Wrapped MON) | `0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A` |
| **USDC** | `0x754704Bc059F8C67012fEd69BC8A327a5aafb603` |
| Permit2 | `0x000000000022d473030f116ddee9f6b43ac78ba3` |
| Multicall3 | `0xcA11bde05977b3631167028862bE2a173976CA11` |
| EntryPoint v0.7 | `0x0000000071727De22E5E9d8BAf0edAc6f37da032` |

### DEX addresses requiring on-chain verification
The addresses provided in your query could not be definitively verified through official documentation. **Verify these on MonadVision block explorer before production use:**

| Protocol | Contract | Provided Address | Status |
|----------|----------|------------------|--------|
| Uniswap V3 | Factory | `0x204faca1764b154221e35c0d20abb3c525710498` | Unverified |
| Uniswap V3 | SwapRouter | `0xd6145b2d3f379919e8cdeda7b97e37c4b2ca9c40` | Unverified |
| PancakeSwap V3 | Factory | `0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865` | Likely correct (canonical) |
| PancakeSwap V3 | SwapRouter | `0x1b81D678ffb9C0263b24A97847620C99d213eB14` | Likely correct (canonical) |
| LFJ (TraderJoe) | LB_FACTORY | `0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c` | Unverified |
| LFJ | LB_ROUTER | `0x18556DA13313f3532c54711497A8FedAC273220E` | Unverified |

**Verification sources:** Check `github.com/monad-crypto/protocols` mainnet folder for official protocol JSON files, or query contracts directly via MonadVision at `monadvision.com`.

---

Uniswap V4

Monad: 143
Contract	Address
PoolManager	0x188d586ddcf52439676ca21a244753fa19f9ea8e
PositionDescriptor	0x5770d2914355a6d0a39a70aeea9bcce55df4201b
PositionManager	0x5b7ec4a94ff9bedb700fb82ab09d5846972f4016
Quoter	0xa222dd357a9076d1091ed6aa2e16c9742dd26891
StateView	0x77395f3b2e73ae90843717371294fa97cc419d64
Universal Router	0x0d97dc33264bfc1c226207428a79b26757fb9dc3
Permit2	0x000000000022D473030F116dDEE9F6B43aC78BA3

## Uniswap V4 architecture: critical differences from V3

Understanding V4's singleton pattern is essential for arbitrage implementation. The architecture fundamentally changes how pools are accessed and swaps are executed.

### Singleton PoolManager replaces individual pool contracts

V3 deployed a separate `UniswapV3Pool` contract for each token pair and fee tier. V4 consolidates **all pools into a single PoolManager contract**, storing pool state in a mapping:

```solidity
contract PoolManager {
    mapping(PoolId => Pool.State) internal _pools;
}
```

This means:
- No `factory.getPool()` lookup—you construct the `PoolKey` and derive `PoolId` via `keccak256`
- Pool creation costs ~90% less gas (state update vs. contract deployment)
- Multi-hop swaps require only 2 token transfers total (flash accounting)

### PoolKey structure defines pool identity

Every V4 pool is uniquely identified by a `PoolKey` struct:

```solidity
struct PoolKey {
    Currency currency0;      // Lower address token (address(0) = native ETH)
    Currency currency1;      // Higher address token
    uint24 fee;              // LP fee in pips (3000 = 0.30%), or 0x800000 for dynamic
    int24 tickSpacing;       // Tick spacing (1, 10, 60, 200 common)
    IHooks hooks;            // Hook contract address (address(0) for none)
}
```

**Critical constraint:** `currency0` must be numerically less than `currency1`. Tokens are sorted by address value, not symbol.

### PoolId calculation for Rust implementation

```rust
use alloy_primitives::{keccak256, B256};
use alloy_sol_types::SolValue;

fn compute_pool_id(pool_key: &PoolKey) -> B256 {
    let encoded = pool_key.abi_encode();
    keccak256(&encoded)
}
```

The PoolId is simply `keccak256(abi.encode(PoolKey))`, hashing 5 slots (160 bytes) of the struct.

### Price representation unchanged from V3

V4 maintains the **sqrtPriceX96** format—a Q64.96 fixed-point representation of `sqrt(price)`:

```rust
// Convert sqrtPriceX96 to human-readable price
fn sqrt_price_to_price(sqrt_price_x96: U256) -> f64 {
    let price_x192 = sqrt_price_x96 * sqrt_price_x96;
    let price = price_x192.to::<f64>() / 2f64.powi(192);
    price
}

// TickMath constants (identical to V3)
const MIN_SQRT_PRICE: U160 = 4295128739;
const MAX_SQRT_PRICE: U160 = 1461446703485210103287273052203988822378723970342;
const MIN_TICK: i24 = -887272;
const MAX_TICK: i24 = 887272;
```

### Fee structure allows any value plus dynamic fees

V3 restricted fees to fixed tiers (500, 3000, 10000). V4 permits:
- Any static fee from 0 to 1,000,000 (100%)
- Dynamic fees via `0x800000` flag, updated per-swap by hooks

```rust
const DYNAMIC_FEE_FLAG: u32 = 0x800000;
const MAX_LP_FEE: u32 = 1_000_000;

fn is_dynamic_fee(fee: u32) -> bool {
    fee & DYNAMIC_FEE_FLAG != 0
}
```

---

## Flash accounting enables efficient multi-hop arbitrage

V4's flash accounting system is the key innovation for arbitrage bots. Instead of transferring tokens between pools during multi-hop swaps, V4 tracks **deltas** (credits/debits) in transient storage and settles only the net balance at transaction end.

### The unlock callback pattern

All V4 operations must occur within an `unlockCallback`:

```solidity
// 1. Entry point
poolManager.unlock(encodedData);

// 2. Callback executes your logic
function unlockCallback(bytes calldata data) external returns (bytes memory) {
    // Perform swaps - only updates deltas, no transfers
    BalanceDelta delta1 = poolManager.swap(key1, params1, "");
    BalanceDelta delta2 = poolManager.swap(key2, params2, "");
    
    // 3. Settle net position at the end
    poolManager.sync(inputCurrency);    // Mark incoming
    // Transfer input tokens to PoolManager
    poolManager.settle(inputCurrency);   // Settle debt
    
    poolManager.take(outputCurrency, recipient, amount);  // Receive output
    
    return "";
}
// 4. PoolManager verifies all deltas = 0, reverts if not
```

### Arbitrage implications

This architecture provides three major benefits:
- **Gas efficiency:** A 5-hop arbitrage path requires only 2 token transfers (input + output)
- **Atomicity:** All operations succeed or all revert—no partial execution risk
- **Implicit flash loans:** Use output tokens before paying input tokens

---

## Solidity interfaces for alloy bindings

Generate Rust bindings using `alloy::sol!` macro with these interface definitions:

### Core types and PoolKey

```solidity
// Types for alloy binding generation
type Currency is address;
type PoolId is bytes32;
type BalanceDelta is int256;

struct PoolKey {
    Currency currency0;
    Currency currency1;
    uint24 fee;
    int24 tickSpacing;
    address hooks;  // IHooks simplified to address
}

struct SwapParams {
    bool zeroForOne;           // true = token0→token1
    int256 amountSpecified;    // negative = exactInput, positive = exactOutput
    uint160 sqrtPriceLimitX96; // Price limit for slippage protection
}
```

### IPoolManager interface

```solidity
interface IPoolManager {
    function unlock(bytes calldata data) external returns (bytes memory);
    
    function swap(
        PoolKey memory key,
        SwapParams memory params,
        bytes calldata hookData
    ) external returns (int256 delta0, int256 delta1);
    
    function sync(address currency) external;
    function settle() external payable returns (uint256);
    function take(address currency, address to, uint256 amount) external;
    
    // State reading via extsload
    function extsload(bytes32 slot) external view returns (bytes32);
    function extsload(bytes32[] calldata slots) external view returns (bytes32[]);
}
```

### StateView for off-chain price queries

```solidity
interface IStateView {
    function getSlot0(bytes32 poolId) external view returns (
        uint160 sqrtPriceX96,
        int24 tick,
        uint24 protocolFee,
        uint24 lpFee
    );
    
    function getLiquidity(bytes32 poolId) external view returns (uint128);
    
    function getTickInfo(bytes32 poolId, int24 tick) external view returns (
        uint128 liquidityGross,
        int128 liquidityNet,
        uint256 feeGrowthOutside0X128,
        uint256 feeGrowthOutside1X128
    );
}
```

### Rust alloy binding example

```rust
use alloy::sol;

sol! {
    #[derive(Debug)]
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }
    
    #[derive(Debug)]
    struct SwapParams {
        bool zeroForOne;
        int256 amountSpecified;
        uint160 sqrtPriceLimitX96;
    }

    #[sol(rpc)]
    interface IPoolManager {
        function unlock(bytes calldata data) external returns (bytes memory);
        function extsload(bytes32 slot) external view returns (bytes32);
    }
    
    #[sol(rpc)]
    interface IStateView {
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
        function getLiquidity(bytes32 poolId) external view returns (uint128);
    }
}
```

---

## Hook system and arbitrage risk considerations

V4 hooks introduce programmable logic at swap lifecycle points. **For arbitrage, prefer pools with `hooks = address(0)`** to avoid:

- **Custom pricing curves:** `beforeSwap` hooks can override concentrated liquidity pricing entirely
- **Dynamic fee extraction:** Hooks may increase fees during volatile periods
- **Return delta manipulation:** Hooks with `BEFORE_SWAP_RETURNS_DELTA_FLAG` can front-run
- **DoS risk:** Malicious hooks may revert or consume excessive gas

Hook permissions are encoded in the **least significant 14 bits** of the hook contract address:

```rust
const BEFORE_SWAP_FLAG: u160 = 1 << 7;
const AFTER_SWAP_FLAG: u160 = 1 << 6;

fn has_before_swap_hook(hook_address: Address) -> bool {
    u160::from(hook_address) & BEFORE_SWAP_FLAG != 0
}
```

---

## Implementation roadmap for Monad V4 support

Since V4 is not yet deployed on Monad, prepare your codebase with these steps:

1. **Monitor deployment announcements** at `docs.uniswap.org/contracts/v4/deployments`
2. **Use V3 for current Monad arbitrage** with the addresses in this document
3. **Implement V4 interfaces now** using the Solidity ABIs above—they're chain-agnostic
4. **Add chain-specific address configuration** that can be updated when V4 deploys
5. **Test V4 logic on Ethereum Sepolia** where V4 is already live

### Expected V4 contract addresses (when deployed)
Based on Uniswap's deployment patterns, expect these contracts on Monad V4:
- PoolManager (singleton—one per chain)
- PositionManager (ERC721 for LP positions)
- StateView (for off-chain state queries)
- V4Router or UniversalRouter (swap execution)

---

## Conclusion
The interfaces and patterns documented here are ready for integration—only the contract addresses need updating when V4 goes live on Chain ID 143. Verify all addresses against `github.com/monad-crypto/protocols` and MonadVision before production deployment.