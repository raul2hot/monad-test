#!/usr/bin/env bash
# verify_lfj_bin_step.sh
#
# This script queries the LFJ pool to get the actual bin step.
# The bin step MUST match exactly in your swap transactions.
#
# Usage: ./verify_lfj_bin_step.sh <RPC_URL> <POOL_ADDRESS>
#
# Example:
#   ./verify_lfj_bin_step.sh "https://your-monad-rpc.com" "0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22"

RPC_URL="${1:-$MONAD_RPC_URL}"
POOL_ADDRESS="${2:-0x5e60bc3f7a7303bc4dfe4dc2220bdc90bc04fe22}"

if [ -z "$RPC_URL" ]; then
    echo "Error: RPC_URL not provided. Set MONAD_RPC_URL or pass as first argument."
    exit 1
fi

echo "=============================================="
echo "LFJ Pool Verification"
echo "=============================================="
echo "RPC URL: $RPC_URL"
echo "Pool Address: $POOL_ADDRESS"
echo "=============================================="

# getBinStep() function selector: 0x17f11ecc
echo ""
echo "Calling getBinStep()..."
BIN_STEP_RESULT=$(curl -s -X POST "$RPC_URL" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_call\",\"params\":[{\"to\":\"$POOL_ADDRESS\",\"data\":\"0x17f11ecc\"},\"latest\"],\"id\":1}" | jq -r '.result')

if [ "$BIN_STEP_RESULT" != "null" ] && [ -n "$BIN_STEP_RESULT" ]; then
    # Convert hex to decimal (bin step is uint16, so last 4 hex chars)
    BIN_STEP_HEX="${BIN_STEP_RESULT: -4}"
    BIN_STEP=$((16#$BIN_STEP_HEX))
    echo "✓ Bin Step: $BIN_STEP"
else
    echo "✗ Failed to get bin step. Check pool address and RPC."
fi

# getActiveId() function selector: 0xdbe65edc
echo ""
echo "Calling getActiveId()..."
ACTIVE_ID_RESULT=$(curl -s -X POST "$RPC_URL" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_call\",\"params\":[{\"to\":\"$POOL_ADDRESS\",\"data\":\"0xdbe65edc\"},\"latest\"],\"id\":1}" | jq -r '.result')

if [ "$ACTIVE_ID_RESULT" != "null" ] && [ -n "$ACTIVE_ID_RESULT" ]; then
    # Convert hex to decimal (active id is uint24, so last 6 hex chars)
    ACTIVE_ID_HEX="${ACTIVE_ID_RESULT: -6}"
    ACTIVE_ID=$((16#$ACTIVE_ID_HEX))
    echo "✓ Active Bin ID: $ACTIVE_ID"
else
    echo "✗ Failed to get active ID. Check pool address and RPC."
fi

echo ""
echo "=============================================="
echo "CONFIGURATION UPDATE NEEDED"
echo "=============================================="
echo ""
echo "Update your config.rs with the correct bin step:"
echo ""
echo "RouterConfig {"
echo "    name: \"LFJ\","
echo "    address: LFJ_LB_ROUTER,"
echo "    router_type: RouterType::LfjLB,"
echo "    pool_address: alloy::primitives::address!(\"$POOL_ADDRESS\"),"
echo "    pool_fee: $BIN_STEP,  // ← This is the bin step!"
echo "}"
echo ""
echo "=============================================="
