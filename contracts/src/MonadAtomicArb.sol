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

    /// @notice Build exactInputSingle calldata for V3-style routers
    /// @dev Works for Uniswap, PancakeSwap (wrapped in multicall), and Monday
    function _buildBuyCalldata(
        Router router,
        uint256 amountIn,
        uint256 amountOutMin,
        uint24 fee
    ) internal view returns (bytes memory) {
        if (router == Router.Uniswap || router == Router.MondayTrade) {
            // Uniswap/Monday: exactInputSingle with deadline in struct (Monday) or not (Uniswap)
            // For simplicity, use Uniswap format (no deadline in struct)
            return abi.encodeWithSelector(
                bytes4(0x04e45aaf), // exactInputSingle selector
                USDC,              // tokenIn
                WMON,              // tokenOut
                fee,               // fee tier
                address(this),     // recipient
                amountIn,          // amountIn
                amountOutMin,      // amountOutMinimum
                uint160(0)         // sqrtPriceLimitX96 (0 = no limit)
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
            // LFJ: swapExactTokensForTokens with Path struct
            // Path: pairBinSteps[], versions[], tokenPath[]
            uint256[] memory binSteps = new uint256[](1);
            binSteps[0] = uint256(fee); // fee is binStep for LFJ
            uint8[] memory versions = new uint8[](1);
            versions[0] = 3; // V2_2
            address[] memory path = new address[](2);
            path[0] = USDC;
            path[1] = WMON;

            return abi.encodeWithSelector(
                bytes4(0x4b126ad4), // swapExactTokensForTokens selector
                amountIn,
                amountOutMin,
                binSteps,
                versions,
                path,
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

        // Swap 1: WMON -> USDC on sellRouter (calldata pre-built by Rust)
        address sellAddr = _getRouterAddress(sellRouter);
        (bool success1,) = sellAddr.call(sellRouterData);
        if (!success1) revert SwapFailed(1);

        // Get ACTUAL USDC balance after swap 1
        uint256 usdcToSwap = IERC20(USDC).balanceOf(address(this));

        // Build swap 2 calldata ON-CHAIN using actual USDC
        bytes memory buyCalldata = _buildBuyCalldata(buyRouter, usdcToSwap, minWmonOut, buyPoolFee);

        // Swap 2: USDC -> WMON on buyRouter
        address buyAddr = _getRouterAddress(buyRouter);
        (bool success2,) = buyAddr.call(buyCalldata);
        if (!success2) revert SwapFailed(2);

        uint256 wmonAfter = IERC20(WMON).balanceOf(address(this));

        // Calculate profit
        profit = int256(wmonAfter) - int256(wmonBefore);

        // Revert if below minimum
        if (wmonAfter < wmonBefore + minProfit) {
            revert Unprofitable(wmonBefore, wmonAfter);
        }

        emit ArbExecuted(
            uint8(sellRouter),
            uint8(buyRouter),
            wmonBefore,
            wmonAfter,
            profit
        );
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

        // Swap 1: WMON -> USDC on sellRouter (calldata pre-built by Rust)
        address sellAddr = _getRouterAddress(sellRouter);
        (bool success1,) = sellAddr.call(sellRouterData);
        if (!success1) revert SwapFailed(1);

        // Get ACTUAL USDC balance after swap 1
        uint256 usdcToSwap = IERC20(USDC).balanceOf(address(this));

        // Build swap 2 calldata ON-CHAIN using actual USDC
        bytes memory buyCalldata = _buildBuyCalldata(buyRouter, usdcToSwap, minWmonOut, buyPoolFee);

        // Swap 2: USDC -> WMON on buyRouter
        address buyAddr = _getRouterAddress(buyRouter);
        (bool success2,) = buyAddr.call(buyCalldata);
        if (!success2) revert SwapFailed(2);

        uint256 wmonAfter = IERC20(WMON).balanceOf(address(this));

        // Calculate profit (can be negative) - NO REVERT on loss
        profit = int256(wmonAfter) - int256(wmonBefore);

        emit ArbExecuted(
            uint8(sellRouter),
            uint8(buyRouter),
            wmonBefore,
            wmonAfter,
            profit
        );
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

    /// @notice Get router address from enum
    function _getRouterAddress(Router router) internal pure returns (address) {
        if (router == Router.Uniswap) return UNISWAP_ROUTER;
        if (router == Router.PancakeSwap) return PANCAKE_ROUTER;
        if (router == Router.MondayTrade) return MONDAY_ROUTER;
        if (router == Router.LFJ) return LFJ_ROUTER;
        revert InvalidRouter();
    }

    /// @notice Check current balances
    function getBalances() external view returns (uint256 wmon, uint256 usdc) {
        wmon = IERC20(WMON).balanceOf(address(this));
        usdc = IERC20(USDC).balanceOf(address(this));
    }

    receive() external payable {}
}
