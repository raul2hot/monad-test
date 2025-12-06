// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import "../src/MonadAtomicArb.sol";

contract DeployScript is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(deployerPrivateKey);

        MonadAtomicArb arb = new MonadAtomicArb();
        console.log("MonadAtomicArb deployed at:", address(arb));

        // Setup approvals
        arb.setupApprovals();
        console.log("Approvals configured");

        vm.stopBroadcast();
    }
}
