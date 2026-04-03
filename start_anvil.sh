#!/bin/bash
# Starts a local Anvil mainnet fork using the RPC URL from .env.
# Run this in a separate terminal before `USE_ANVIL=true cargo run`.
#
# Pin a specific block for a reproducible snapshot:
#   FORK_BLOCK=21950000 ./start_anvil.sh
# Leave unset to fork from the current chain tip.

if [ ! -f .env ]; then
  echo "Error: .env file not found. Run populate_env.sh first."
  exit 1
fi

if ! command -v anvil &> /dev/null; then
  echo "Error: anvil not found. Install Foundry: https://getfoundry.sh/"
  exit 1
fi

set -a
source .env
set +a

echo "Starting Anvil fork of mainnet..."
echo "Fork URL: $NETWORK_RPC"
echo "Listening on http://127.0.0.1:8545 (HTTP + WS)"
echo ""

BLOCK_ARG=""
if [ -n "$FORK_BLOCK" ]; then
  BLOCK_ARG="--fork-block-number $FORK_BLOCK"
  echo "Pinned to block: $FORK_BLOCK"
else
  echo "No FORK_BLOCK set — forking from current tip."
fi

anvil \
  --fork-url "$NETWORK_RPC" \
  --block-time 12 \
  --compute-units-per-second 200 \
  $BLOCK_ARG