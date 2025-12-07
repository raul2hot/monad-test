// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "./interfaces/IERC20.sol";

/// @title MonadAtomicArb
/// @notice Atomic arbitrage contract for Monad mainnet
/// @dev Executes two swaps in single TX, reverts if unprofitable
contract MonadAtomicArb {
    address public immutable owner;

    // Token addresses (Monad mainnet)
    address public constant WMON = 0x3bd359C1119dA7Da1D913D1C4D2B7c461115433A;
    address public constant USDC = 0x754704Bc059F8C67012fEd69BC8A327a5aafb603;

    // Router addresses (Monad mainnet)
    address public constant UNISWAP_ROUTER = 0xfE31F71C1b106EAc32F1A19239c9a9A72ddfb900;
    address public constant PANCAKE_ROUTER = 0x21114915Ac6d5A2e156931e20B20b038dEd0Be7C;
    address public constant MONDAY_ROUTER = 0xFE951b693A2FE54BE5148614B109E316B567632F;
    address public constant LFJ_ROUTER = 0x18556DA13313f3532c54711497A8FedAC273220E;

    // Router enum matching Rust RouterType
    enum Router { Uniswap, PancakeSwap, MondayTrade, LFJ }

    // LFJ Path struct for Liquidity Book routing
    struct LFJPath {
        uint256[] pairBinSteps;
        uint8[] versions;
        address[] tokenPath;
    }

    error OnlyOwner();
    error SwapFailed(uint8 swapIndex);
    error Unprofitable(uint256 wmonBefore, uint256 wmonAfter);
    error InvalidRouter();

    event ArbExecuted(
        uint8 indexed sellRouter,
        uint8 indexed buyRouter,
        uint256 wmonIn,
        uint256 wmonOut,
        int256 profit
    );

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert OnlyOwner();
        _;
    }

    /// @notice Get router address from enum
    function _getRouterAddress(Router router) internal pure returns (address) {
        if (router == Router.Uniswap) return UNISWAP_ROUTER;
        if (router == Router.PancakeSwap) return PANCAKE_ROUTER;
        if (router == Router.MondayTrade) return MONDAY_ROUTER;
        if (router == Router.LFJ) return LFJ_ROUTER;
        revert InvalidRouter();
    }

    /// @notice Execute swap 1 (sell WMON for USDC)
    function _executeSwap1(Router sellRouter, bytes calldata sellRouterData) internal {
        (bool success,) = _getRouterAddress(sellRouter).call(sellRouterData);
        if (!success) revert SwapFailed(1);
    }

    /// @notice Execute swap 2 (buy WMON with USDC) using actual USDC balance
    function _executeSwap2(Router buyRouter, uint24 buyPoolFee, uint256 minWmonOut) internal {
        uint256 usdcToSwap = IERC20(USDC).balanceOf(address(this));
        bytes memory buyCalldata = _buildBuyCalldata(buyRouter, usdcToSwap, minWmonOut, buyPoolFee);
        (bool success,) = _getRouterAddress(buyRouter).call(buyCalldata);
        if (!success) revert SwapFailed(2);
    }

    /// @notice Build exactInputSingle calldata for V3-style routers
    /// @dev Each router has a different ABI for exactInputSingle
    function _buildBuyCalldata(
        Router router,
        uint256 amountIn,
        uint256 amountOutMin,
        uint24 fee
    ) internal view returns (bytes memory) {
        if (router == Router.Uniswap) {
            // Uniswap SwapRouter02: exactInputSingle WITHOUT deadline in struct (7 fields)
            // Selector: 0x04e45aaf
            return abi.encodeWithSelector(
                bytes4(0x04e45aaf),
                USDC,              // tokenIn
                WMON,              // tokenOut
                fee,               // fee tier
                address(this),     // recipient
                amountIn,          // amountIn
                amountOutMin,      // amountOutMinimum
                uint160(0)         // sqrtPriceLimitX96
            );
        } else if (router == Router.MondayTrade) {
            // MondayTrade uses original ISwapRouter: deadline IS in struct (8 fields)
            // Selector: 0x414bf389
            return abi.encodeWithSelector(
                bytes4(0x414bf389),
                USDC,              // tokenIn
                WMON,              // tokenOut
                fee,               // fee tier
                address(this),     // recipient
                block.timestamp + 300, // deadline (INSIDE struct for Monday!)
                amountIn,          // amountIn
                amountOutMin,      // amountOutMinimum
                uint160(0)         // sqrtPriceLimitX96
            );
        } else if (router == Router.PancakeSwap) {
            // PancakeSwap: wrap in multicall(deadline, data[])
            bytes memory innerCall = abi.encodeWithSelector(
                bytes4(0x04e45aaf), // exactInputSingle selector
                USDC,
                WMON,
                fee,
                address(this),
                amountIn,
                amountOutMin,
                uint160(0)
            );
            bytes[] memory calls = new bytes[](1);
            calls[0] = innerCall;
            return abi.encodeWithSelector(
                bytes4(0x5ae401dc), // multicall(uint256,bytes[]) selector
                block.timestamp + 300, // deadline
                calls
            );
        } else if (router == Router.LFJ) {
            // LFJ: swapExactTokensForTokens(uint256, uint256, Path memory, address, uint256)
            // Path struct must be encoded as a single struct, NOT separate arrays
            uint256[] memory binSteps = new uint256[](1);
            binSteps[0] = uint256(fee); // fee is binStep for LFJ
            uint8[] memory versions = new uint8[](1);
            versions[0] = 3; // V2_2
            address[] memory tokenPath = new address[](2);
            tokenPath[0] = USDC;
            tokenPath[1] = WMON;

            // Create the Path struct
            LFJPath memory lfjPath = LFJPath({
                pairBinSteps: binSteps,
                versions: versions,
                tokenPath: tokenPath
            });

            return abi.encodeWithSelector(
                bytes4(0x4b126ad4), // swapExactTokensForTokens selector
                amountIn,
                amountOutMin,
                lfjPath,           // Pass as struct, not separate arrays!
                address(this),
                block.timestamp + 300
            );
        }
        revert InvalidRouter();
    }

    /// @notice Setup max approvals for all routers (call once after deployment)
    function setupApprovals() external onlyOwner {
        // Approve WMON to all routers
        IERC20(WMON).approve(UNISWAP_ROUTER, type(uint256).max);
        IERC20(WMON).approve(PANCAKE_ROUTER, type(uint256).max);
        IERC20(WMON).approve(MONDAY_ROUTER, type(uint256).max);
        IERC20(WMON).approve(LFJ_ROUTER, type(uint256).max);

        // Approve USDC to all routers
        IERC20(USDC).approve(UNISWAP_ROUTER, type(uint256).max);
        IERC20(USDC).approve(PANCAKE_ROUTER, type(uint256).max);
        IERC20(USDC).approve(MONDAY_ROUTER, type(uint256).max);
        IERC20(USDC).approve(LFJ_ROUTER, type(uint256).max);
    }

    /// @notice Execute atomic arbitrage: WMON -> USDC -> WMON (with profit check)
    /// @param sellRouter Router to sell WMON for USDC (higher price)
    /// @param sellRouterData Pre-encoded calldata for sell swap
    /// @param buyRouter Router to buy WMON with USDC (lower price)
    /// @param buyPoolFee Pool fee tier for buy swap (used to build calldata on-chain)
    /// @param minWmonOut Minimum WMON output for slippage protection on buy swap
    /// @param minProfit Minimum WMON profit required (reverts if not met)
    /// @return profit The WMON profit achieved
    function executeArb(
        Router sellRouter,
        bytes calldata sellRouterData,
        Router buyRouter,
        uint24 buyPoolFee,
        uint256 minWmonOut,
        uint256 minProfit
    ) external onlyOwner returns (int256 profit) {
        uint256 wmonBefore = IERC20(WMON).balanceOf(address(this));

        // Execute both swaps using helper functions
        _executeSwap1(sellRouter, sellRouterData);
        _executeSwap2(buyRouter, buyPoolFee, minWmonOut);

        uint256 wmonAfter = IERC20(WMON).balanceOf(address(this));
        profit = int256(wmonAfter) - int256(wmonBefore);

        if (wmonAfter < wmonBefore + minProfit) {
            revert Unprofitable(wmonBefore, wmonAfter);
        }

        emit ArbExecuted(uint8(sellRouter), uint8(buyRouter), wmonBefore, wmonAfter, profit);
    }

    /// @notice Execute atomic arbitrage WITHOUT profit check (for testing)
    /// @dev Only ensures both swaps succeed atomically. Monitor should check profitability.
    function executeArbUnchecked(
        Router sellRouter,
        bytes calldata sellRouterData,
        Router buyRouter,
        uint24 buyPoolFee,
        uint256 minWmonOut
    ) external onlyOwner returns (int256 profit) {
        uint256 wmonBefore = IERC20(WMON).balanceOf(address(this));

        // Execute both swaps using helper functions
        _executeSwap1(sellRouter, sellRouterData);
        _executeSwap2(buyRouter, buyPoolFee, minWmonOut);

        uint256 wmonAfter = IERC20(WMON).balanceOf(address(this));
        profit = int256(wmonAfter) - int256(wmonBefore);

        emit ArbExecuted(uint8(sellRouter), uint8(buyRouter), wmonBefore, wmonAfter, profit);
    }

    /// @notice Withdraw tokens (emergency or profit collection)
    function withdrawToken(address token, uint256 amount) external onlyOwner {
        IERC20(token).transfer(owner, amount);
    }

    /// @notice Withdraw all of a token
    function withdrawAllToken(address token) external onlyOwner {
        uint256 balance = IERC20(token).balanceOf(address(this));
        IERC20(token).transfer(owner, balance);
    }

    /// @notice Check current balances
    function getBalances() external view returns (uint256 wmon, uint256 usdc) {
        wmon = IERC20(WMON).balanceOf(address(this));
        usdc = IERC20(USDC).balanceOf(address(this));
    }

    receive() external payable {}
}
