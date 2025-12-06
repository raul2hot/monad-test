// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../src/MonadAtomicArb.sol";

/// @notice Simple deployment script - run with:
/// forge create --rpc-url $MONAD_RPC_URL --private-key $PRIVATE_KEY src/MonadAtomicArb.sol:MonadAtomicArb
///
/// Or use this script with:
/// forge script script/Deploy.s.sol:DeployScript --rpc-url $MONAD_RPC_URL --broadcast --private-key $PRIVATE_KEY
contract DeployScript {
    function run() external {
        // Deploy the contract
        MonadAtomicArb arb = new MonadAtomicArb();

        // Setup approvals
        arb.setupApprovals();
    }
}
