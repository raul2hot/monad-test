# Root causes of arbitrage simulation false rejections on Monad

Your arbitrage bot's graph-based cycle detection finding **20-26% profit opportunities** while simulation rejects **all** of them indicates systematic errors in the liquidity validation layer—not actual insufficient liquidity. Based on deep technical research into LFJ, Uniswap V3, decimal handling, and Monad-specific behaviors, the root causes are: incorrect decimal normalization (USDC 6 vs WMON 18), misuse of V3 liquidity metrics, LFJ `amountInLeft` misinterpretation, and overly strict absolute thresholds instead of value-based checks.

## The decimal normalization bug is likely your primary culprit

When comparing liquidity across tokens with different decimals, a **single fixed threshold causes 10^12x errors**. Consider a minimum liquidity threshold of `1e18`:

- For WMON (18 decimals): `1e18` = 1 token ✓
- For USDC (6 decimals): `1e18` = **1,000,000,000,000 tokens** ($1 trillion!) ✗

Any pool containing USDC will fail this check because the threshold demands impossibly high liquidity. This explains why **all** opportunities are rejected—your arbitrage routes likely include USDC pairs.

**The fix requires normalizing all amounts to a common base before comparison:**

```javascript
function normalizeToWad(rawAmount, tokenDecimals) {
  if (tokenDecimals < 18) {
    return rawAmount * BigInt(10 ** (18 - tokenDecimals));
  } else if (tokenDecimals > 18) {
    return rawAmount / BigInt(10 ** (tokenDecimals - 18));
  }
  return rawAmount;
}

// Correct threshold comparison
const normalizedReserve = normalizeToWad(reserve, tokenDecimals);
const MIN_LIQUIDITY_WAD = BigInt(1e18); // 1 token normalized
if (normalizedReserve < MIN_LIQUIDITY_WAD) {
  return { reject: true, reason: "INSUFFICIENT_LIQUIDITY" };
}
```

Better still, use **value-based thresholds in USD** rather than token amounts:

```javascript
const reserveUSD = (reserve * tokenPriceUSD) / BigInt(10 ** tokenDecimals);
const MIN_LIQUIDITY_USD = BigInt(10000e18); // $10,000 minimum
if (reserveUSD < MIN_LIQUIDITY_USD) return false;
```

## LFJ getSwapOut() requires correct interpretation of amountInLeft

The Liquidity Book's `getSwapOut()` function returns three values that are frequently misinterpreted:

| Return Value | Type | Meaning |
|-------------|------|---------|
| `amountInLeft` | uint128 | Amount that **could not be swapped** due to liquidity limits |
| `amountOut` | uint128 | Output tokens received |
| `fee` | uint128 | Fees paid |

**Critical insight**: `amountInLeft > 0` does **not** always mean "reject the trade." It indicates the pool cannot handle the **full** requested amount, but a **partial fill** may still be profitable. The correct validation pattern:

```javascript
const [amountInLeft, amountOut, fee] = await router.getSwapOut(lbPair, amountIn, swapForY);

if (amountInLeft === amountIn && amountOut === 0n) {
  // NO liquidity at all - legitimate rejection
  return { canExecute: false, reason: "NO_LIQUIDITY" };
}

if (amountInLeft > 0n && amountOut > 0n) {
  // PARTIAL fill available - calculate if reduced trade is still profitable
  const maxSwappable = amountIn - amountInLeft;
  const reducedProfit = calculateProfit(maxSwappable, amountOut);
  if (reducedProfit > minProfitThreshold) {
    return { canExecute: true, adjustedAmount: maxSwappable };
  }
}

if (amountInLeft === 0n) {
  // Full swap possible
  return { canExecute: true, expectedOutput: amountOut };
}
```

LFJ's bin-based model differs fundamentally from V3: it uses **constant sum** (`X + Y = k`) within bins rather than constant product. Swaps traverse bins sequentially with zero slippage inside each bin, and `amountInLeft > 0` occurs only when all bins in the swap direction are exhausted.

## Uniswap V3 pool.liquidity does not indicate swap capacity

The most dangerous misconception is treating `pool.liquidity` as a threshold for swap feasibility. **This metric represents only the sum of liquidity from positions containing the current price**—it tells you nothing about:

1. Whether adjacent tick ranges have liquidity
2. How much can actually be swapped in your direction
3. Whether the swap will cross tick boundaries

The relationship between L and actual reserves within a tick range is:

```
x_real = L × (√Pb - √P) / (√P × √Pb)  // Token0 reserves
y_real = L × (√P - √Pa)                // Token1 reserves
```

where `Pa` and `Pb` are the price bounds of the current tick range. **These are "virtual reserves" that exist only within the current tick**—a large swap will exhaust them and cross into adjacent ticks where liquidity may be zero.

**The only reliable validation method is using the Quoter contract:**

```javascript
const quoter = new ethers.Contract(QUOTER_V2_ADDRESS, QuoterV2ABI, provider);

try {
  const quote = await quoter.quoteExactInputSingle.staticCall({
    tokenIn: tokenInAddress,
    tokenOut: tokenOutAddress,
    fee: poolFee,
    amountIn: swapAmount,
    sqrtPriceLimitX96: 0
  });
  
  return {
    canExecute: true,
    amountOut: quote.amountOut,
    ticksCrossed: quote.initializedTicksCrossed,
    priceImpact: calculatePriceImpact(quote)
  };
} catch (error) {
  // Only NOW can you legitimately reject
  return { canExecute: false, reason: parseQuoterError(error) };
}
```

The Quoter internally executes the actual swap logic and reverts to return the result—it's the only accurate simulation for concentrated liquidity pools.

## Monad's gas model adds unique constraints

Monad mainnet (launched November 24, 2025) has several differences from Ethereum that affect arbitrage simulation:

**No gas refunds**: Users are charged based on `gasLimit`, not `gasUsed`. Over-estimating gas locks in costs, making accurate simulation critical for profitability calculations.

**State lag**: Due to deferred execution, when building block N, confirmed state may only be at N-2. Simulations against "current" state may fail because pool reserves changed. This requires probabilistic strategies rather than deterministic arbitrage.

**Reserve balance protection**: Transactions that would reduce sender's balance below 10 MON (after execution) automatically revert. Factor this into balance checks.

**Key Monad addresses:**
- WMON: `0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A`
- USDC: `0x754704Bc059F8C67012fEd69BC8A327a5aafb603`

## Recommended simulation module architecture

Replace the current liquidity-check-first approach with a **simulation-first pipeline**:

```javascript
class ArbitrageValidator {
  async validate(opportunity) {
    // Stage 1: Quick filters (no RPC, instant)
    if (!this.passesQuickFilters(opportunity)) {
      return { rejected: true, stage: "QUICK_FILTER" };
    }
    
    // Stage 2: Actual swap simulation (one RPC per pool)
    const simResults = await Promise.all(
      opportunity.route.map(hop => this.simulateHop(hop))
    );
    
    const failedHop = simResults.find(r => !r.success);
    if (failedHop) {
      return { rejected: true, stage: "SIMULATION", hop: failedHop };
    }
    
    // Stage 3: Profitability check (after simulation confirms feasibility)
    const totalOutput = simResults[simResults.length - 1].amountOut;
    const gasCost = this.estimateGasCost(simResults);
    const netProfit = totalOutput - opportunity.inputAmount - gasCost;
    
    if (netProfit < this.minProfitThreshold) {
      return { rejected: true, stage: "PROFITABILITY", netProfit };
    }
    
    return { rejected: false, simulation: simResults, netProfit };
  }
  
  async simulateHop(hop) {
    switch (hop.poolType) {
      case "LFJ":
        return this.simulateLFJ(hop);
      case "UniswapV3":
      case "PancakeSwapV3":
        return this.simulateV3(hop);
    }
  }
  
  async simulateLFJ(hop) {
    const [amountInLeft, amountOut, fee] = await this.lfjRouter.getSwapOut(
      hop.pair, hop.amountIn, hop.swapForY
    );
    
    // Accept partial fills if still profitable
    if (amountInLeft > 0n && amountOut > 0n) {
      const maxSwappable = hop.amountIn - amountInLeft;
      return { success: true, amountOut, adjustedInput: maxSwappable, partial: true };
    }
    
    return { success: amountInLeft === 0n, amountOut };
  }
  
  async simulateV3(hop) {
    try {
      const quote = await this.quoter.quoteExactInputSingle.staticCall({
        tokenIn: hop.tokenIn,
        tokenOut: hop.tokenOut,
        fee: hop.fee,
        amountIn: hop.amountIn,
        sqrtPriceLimitX96: 0
      });
      return { success: true, amountOut: quote.amountOut };
    } catch {
      return { success: false };
    }
  }
}
```

## Specific code changes checklist

**1. Fix decimal normalization everywhere:**
```javascript
// Before any comparison or threshold check:
const normalizedAmount = rawAmount * BigInt(10 ** (18 - tokenDecimals));
```

**2. Replace liquidity threshold checks with simulation:**
```javascript
// REMOVE this pattern:
if (pool.liquidity < MIN_LIQUIDITY) return false;

// USE this pattern:
const simulation = await quoter.quoteExactInputSingle.staticCall(params);
if (!simulation) return false;
```

**3. Handle LFJ partial fills:**
```javascript
// REMOVE this pattern:
if (amountInLeft > 0) return false;

// USE this pattern:
if (amountInLeft === amountIn && amountOut === 0n) return false;
// Otherwise, check if partial fill is profitable
```

**4. Use value-based thresholds:**
```javascript
// REMOVE:
const MIN_RESERVE = 1e18n;

// USE:
const MIN_RESERVE_USD = 10000e18n; // $10k, normalized to 18 decimals
const reserveUSD = (reserve * priceUSD) / BigInt(10 ** decimals);
```

**5. Add comprehensive rejection logging:**
```javascript
console.log({
  opportunity: opp.id,
  stage: "LIQUIDITY_CHECK",
  poolType: pool.type,
  tokenDecimals: [token0.decimals, token1.decimals],
  rawReserves: [reserve0, reserve1],
  normalizedReserves: [normalized0, normalized1],
  threshold: currentThreshold,
  verdict: passed ? "PASSED" : "REJECTED"
});
```

## Diagnostic approach for existing rejections

To confirm these are the issues, add logging to capture:

1. **Token decimals** for every pool in rejected routes—look for 6-decimal tokens
2. **Raw vs normalized reserve comparisons**—identify 10^12x mismatches
3. **LFJ getSwapOut full return tuple**—check if `amountOut > 0` when rejected
4. **V3 quoter results**—verify quoter succeeds where liquidity checks fail

A single test case swapping **1 USDC → WMON** through each DEX will likely reveal the decimal bug immediately: the USDC side will show "insufficient liquidity" despite adequate reserves because `1e6` (1 USDC raw) compared to a `1e18` threshold appears to be zero liquidity.

The combination of proper decimal normalization, simulation-first validation, and correct LFJ partial-fill handling should convert your 0% success rate to match the 20-26% profit rate from your graph-based detection.