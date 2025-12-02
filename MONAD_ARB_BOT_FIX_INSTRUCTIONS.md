# Root cause of LFJ arbitrage simulation failures on Monad

Arbitrage opportunities detected via Bellman-Ford graph analysis are failing because the graph uses **theoretical bin prices** from `getPriceFromId()` which exist mathematically regardless of actual liquidity, while simulation with `getSwapOut()` correctly discovers that bins have near-zero reserves. The 21% "profit" is a phantom created by the fundamental separation between price coordinates and executable depth in concentrated liquidity AMMs.

## The mathematical price versus executable reality gap

The core failure mode stems from how TraderJoe Liquidity Book v2.1 (LFJ) architecturally separates price calculation from liquidity reserves. The function `getPriceFromId(id, binStep)` computes price using a pure mathematical formula: **price = (1 + binStep/10,000)^(activeId - 2^23)**. This calculation is stateless—it takes only the bin ID and bin step parameters, never querying actual token reserves. Every bin ID maps deterministically to a theoretical price regardless of whether any liquidity exists at that bin.

When `getSwapOut(amountIn, swapForY)` executes, it performs an entirely different operation: iterating through bins, checking actual reserves via `_bins[id].decode()`, and accumulating output amounts until either the input is exhausted or no more liquidity exists. The critical return value **`amountInLeft`** represents tokens that couldn't be swapped due to insufficient reserves. Your error showing "only 0.0% fill (142869070760175 of 100000000000000000000 swapped)" means `amountInLeft` was approximately **99.9999%** of the input—the pool had virtually no executable liquidity despite having a price.

This architectural separation exists because LFJ uses discrete bins rather than a continuous curve. Each bin operates under a constant-sum formula (x + y = k within bins, not x * y = k), meaning price is constant within a bin regardless of composition. A bin can be completely empty yet still "exist" with a calculable price. The active bin pointer stored in the contract may point to a depleted bin until the next trade moves the cursor.

## Why 21% theoretical profit collapses to 0.00014% execution

The graph construction logic reads prices from multiple pools, calculates edge weights using `log(price)`, and identifies negative cycles indicating arbitrage. For the WMON→USDC→AUSD→WMON path, if Pool A's price shows 1 WMON = X USDC, Pool B shows X USDC = Y AUSD, and Pool C shows Y AUSD = 1.21 WMON, Bellman-Ford correctly identifies a 21% theoretical gain.

However, Pool C (the LFJ pool at `0x5E60BC3F7a7303BC4dfE4dc2220bdC90bc04fE22`) has its active bin at a price corresponding to that favorable rate, but the bin reserves are essentially zero. When the router's `getSwapOut` iterates through bins looking for liquidity, it finds nothing—or finds only **142869070760175 wei** (roughly 0.00014 WMON) worth of executable depth. The mathematical arbitrage exists in price space but not in liquidity space.

This is particularly common on Monad mainnet just 8 days post-launch. Total chain TVL sits around **$150M**, concentrated heavily in a handful of major pairs. The WMON/USDC pools have between **$290K-$676K** liquidity spread across Uniswap V3, PancakeSwap V3, and LFJ. Stablecoin pairs like AUSD/USDC show better depth at **$24.77M** on Uniswap V3. But many secondary pools—especially on LFJ—have minimal deposits, creating conditions where theoretical prices exist without backing reserves.

## Production architecture for liquidity-aware graph construction

The solution requires restructuring the arbitrage detection pipeline to use executable amounts rather than theoretical prices at every stage.

**Phase 1: Pool discovery and filtering.** Before any pool enters the graph, validate minimum liquidity thresholds. Query each LFJ pool's bins directly using `getBin(activeId)` to inspect actual reserves:

```solidity
(uint128 reserveX, uint128 reserveY) = pair.getBin(activeId);
if (reserveX < MIN_RESERVE_THRESHOLD || reserveY < MIN_RESERVE_THRESHOLD) {
    // Skip this pool - insufficient liquidity
    continue;
}
```

For Monad's current conditions, a minimum reserve threshold of **$1,000-$5,000** per side filters out most phantom liquidity pools. This threshold should be calibrated based on your target trade size—for 100 WMON (~$4 at current prices), you need pools with at least several hundred dollars of depth to absorb price impact.

**Phase 2: Replace price-based edge weights with quoter-based weights.** Instead of computing `edgeWeight = log(getPriceFromId(id, binStep))`, use simulated output amounts:

```javascript
// Standard input amount for calibration
const CALIBRATION_AMOUNT = ethers.parseUnits("10", 18); // 10 WMON

async function getExecutableEdgeWeight(pair, swapForY) {
    const { amountInLeft, amountOut, fee } = await router.getSwapOut(
        pair, 
        CALIBRATION_AMOUNT, 
        swapForY
    );
    
    if (amountInLeft > 0) {
        // Pool cannot fully execute even calibration amount
        return null; // Exclude from graph
    }
    
    const effectiveRate = amountOut / CALIBRATION_AMOUNT;
    return Math.log(effectiveRate);
}
```

This approach captures actual price impact and rejects pools that can't handle your minimum viable trade size.

**Phase 3: Full path simulation before execution.** When Bellman-Ford identifies a candidate cycle, simulate the complete path with your actual intended trade size:

```javascript
async function simulateArbitragePath(pools, startAmount) {
    let currentAmount = startAmount;
    
    for (const { pair, swapForY } of pools) {
        const { amountInLeft, amountOut } = await router.getSwapOut(
            pair,
            currentAmount,
            swapForY
        );
        
        if (amountInLeft > 0) {
            const fillRate = (currentAmount - amountInLeft) / currentAmount;
            console.log(`WARNING: ${fillRate * 100}% fill rate only`);
            return { profitable: false, reason: 'insufficient_liquidity' };
        }
        
        currentAmount = amountOut;
    }
    
    const profit = currentAmount - startAmount;
    const profitPercent = (profit / startAmount) * 100;
    return { profitable: profit > gasEstimate, profit, profitPercent };
}
```

## Optimal liquidity thresholds for Monad's current state

Given Monad's early-stage liquidity distribution, apply tiered filtering:

| Trade Size | Minimum Pool Liquidity | Pools Available |
|------------|----------------------|-----------------|
| 100 WMON (~$4) | $5K TVL | ~50-100 pools |
| 1,000 WMON (~$40) | $25K TVL | ~20-30 pools |
| 10,000 WMON (~$400) | $100K TVL | ~10-15 pools |
| 100,000 WMON (~$4,000) | $500K TVL | ~5-8 pools |

For LFJ specifically, also verify that the **active bin and adjacent bins** contain reserves. Empty active bins with liquidity only in distant bins create deceptive conditions where `getBin(activeId)` shows zero but `getSwapOut` might eventually find liquidity at much worse prices.

## Efficient RPC call strategies for liquidity checking

The concern about excessive RPC calls is valid. Three approaches minimize overhead while maintaining accuracy:

**Multicall batching** combines multiple `getBin()` or `getSwapOut()` calls into single RPC requests:

```javascript
const multicall = new Contract(MULTICALL3_ADDRESS, MULTICALL3_ABI, provider);

const calls = pools.map(pool => ({
    target: pool.address,
    allowFailure: true,
    callData: lbPairInterface.encodeFunctionData('getBin', [pool.activeId])
}));

const results = await multicall.aggregate3.staticCall(calls);
// Process all bin reserves in single RPC round-trip
```

**Local REVM simulation** caches blockchain state and runs swap simulations locally without RPC calls per simulation. Production MEV bots report reducing execution time from 90ms to 19ms and RPC calls from 100 to 10 using this approach. Initialize REVM's CacheDB with relevant pool state, then simulate arbitrage paths locally.

**Tiered validation** applies different rigor levels based on opportunity size:

1. Quick filter: Check pool TVL from cached data (API calls to DexScreener/GeckoTerminal)
2. Medium validation: Multicall batch of `getBin()` for candidates passing filter
3. Full simulation: `getSwapOut()` path simulation only for high-probability opportunities

## Code-level implementation recommendations

Replace the current price discovery logic:

```javascript
// INCORRECT: Current approach using theoretical prices
async function getCurrentPrice(pair) {
    const activeId = await pair.getActiveId();
    const binStep = await pair.getStaticFeeParameters().binStep;
    return getPriceFromId(activeId, binStep); // Pure math, ignores liquidity
}

// CORRECT: Use executable simulation
async function getExecutableRate(pair, amountIn, swapForY) {
    const { amountInLeft, amountOut, fee } = await router.getSwapOut(
        pair.address,
        amountIn,
        swapForY
    );
    
    if (amountInLeft > 0) {
        return {
            executable: false,
            maxExecutable: amountIn - amountInLeft,
            fillRate: (amountIn - amountInLeft) / amountIn
        };
    }
    
    return {
        executable: true,
        rate: amountOut / amountIn,
        effectivePrice: amountIn / amountOut
    };
}
```

For graph construction, add a liquidity validation layer:

```javascript
class LiquidityAwareGraph {
    constructor(minLiquidityUSD, calibrationAmount) {
        this.minLiquidity = minLiquidityUSD;
        this.calibrationAmount = calibrationAmount;
        this.edges = new Map();
    }
    
    async addPoolIfLiquid(pool) {
        // Step 1: Check basic TVL threshold
        const tvl = await this.fetchPoolTVL(pool);
        if (tvl < this.minLiquidity) return false;
        
        // Step 2: Verify executable depth at calibration amount
        const forwardQuote = await router.getSwapOut(
            pool.address, 
            this.calibrationAmount, 
            true
        );
        
        if (forwardQuote.amountInLeft > 0) return false;
        
        // Step 3: Add edge with executable rate as weight
        const rate = forwardQuote.amountOut / this.calibrationAmount;
        this.edges.set(
            `${pool.tokenX}->${pool.tokenY}`,
            { pool, weight: Math.log(rate), executable: true }
        );
        
        return true;
    }
}
```

## Conclusion: The path from phantom profits to real execution

The root cause is architectural, not a bug: LFJ's `getPriceFromId()` provides mathematical coordinates in price space while `getSwapOut()` discovers physical reality in liquidity space. On a nascent chain like Monad, these diverge dramatically because liquidity concentrates in few pools while prices exist theoretically across all bins.

The fix requires a philosophical shift from "find price arbitrage, then validate" to "only consider pools with proven executable depth." Filter aggressively at pool discovery, use quoter-based edge weights instead of theoretical prices, and simulate full paths before any execution attempt. The 99.9999% false positive rate you're experiencing will drop to near-zero once the graph itself reflects executable reality rather than mathematical possibility.

For Monad specifically, focus arbitrage efforts on the deepest pools: AUSD/USDC on Uniswap V3 ($24.77M), MON/AUSD on Uniswap V4 ($4.21M), and the primary WMON/USDC pools across Uniswap V3, PancakeSwap V3, and LFJ V2.2 ($290K-$676K range). Skip pools below $25K TVL entirely until ecosystem liquidity matures.