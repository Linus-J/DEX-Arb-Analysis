# DEX Arbitrage Analysis Tool  ![license](https://img.shields.io/badge/License-MIT-green.svg?label=license)

This tool detects profitable arbitrage opportunities across decentralised exchanges on Ethereum Mainnet using fully **off-chain simulation** — no on-chain execution, no wallet balance required.

It is a fork of the original [rusty-john](https://github.com/RenatoDev3/rusty-john) repo, extended to drop the MEV-bot deployment path and to add real-time reserve tracking and Uniswap V3 support.

## Features

- **V2 pair discovery** — queries UniswapV2, PancakeSwapV2 and SushiSwapV2 factories via the bundled `FlashBotsUniswapQuery` contract (full pair space, no hard cap)
- **Uniswap V3 pool discovery** — queries `UniswapV3Factory.getPool` for all three fee tiers (0.05 %, 0.3 %, 1 %) and resolves tick data via the `tickBitmap` word scan
- **Off-chain swap simulation** — V2 uses the constant-product optimal-input formula; V3 uses tick-traversal simulation via the [`uniswap_v3_math`](https://docs.rs/uniswap_v3_math) crate (no Quoter contract / `eth_call` per opportunity)
- **Cross-protocol arb detection** — finds V2 ↔ V2, V2 ↔ V3 and V3 ↔ V3 opportunities for the same token in a single pass
- **Real-time reserve updates** — subscribes to `newHeads` via WebSocket and re-runs the full analysis on every new block so reserves are always at most one block stale
- **Configurable filters** — `MIN_WETH_LIQUIDITY` (default 1 ETH) and `MIN_PROFIT_WETH` (default 0.01 ETH) constants in `src/address_book.rs`
- **Gas-aware profit** — realistic gas estimates (180 k for V2↔V2, 200 k for mixed/V3 arbs) deducted before reporting

## Architecture

| Module | Responsibility |
|---|---|
| `src/main.rs` | Startup sequence, V3 discovery, `newHeads` subscription loop |
| `src/dex_factory.rs` | V2 pair discovery via the batch query contract |
| `src/v3_pool.rs` | `V3Pool` struct, off-chain swap simulation, V3 pool discovery |
| `src/crossed_pair.rs` | `CrossedPairManager` — reserve updates, V2/V3/mixed arb search |
| `src/address_book.rs` | Contract addresses, ABI bindings, configurable constants |
| `src/utils.rs` | `Config` (RPC providers, max balance) |

## Environment setup

Make an API key to query the chain with [Infura](https://app.infura.io/) or [Alchemy](https://dashboard.alchemy.com/?a=)— you need both an HTTP and a WebSocket endpoint.

Create a dummy ETH wallet with any wallet provider and retrieve the private key.

Setup environment variables by running:

    bash populate_env.sh

Compile crates and run:

    cargo run

### Optional environment variables

| Variable | Default | Description |
|---|---|---|
| `V3_POOL_BITMAP_SCAN_WORD_LIMIT` | *(full range)* | Limit the tick-bitmap scan window around the current tick (number of 256-tick word slots). Useful for faster startup on large token lists. |
| `V3_POOL_VERBOSE_BITMAP_SCAN` | `false` | Set to `1`/`true`/`yes` to print per-pool tick scan progress. |
| `DEBUG_ARB` | `false` | Set to `true` to print skipped cross-protocol pairs (e.g. stale V2 reserves). |

## Running tests

All tests are pure unit tests (no RPC calls):

    cargo test

## Future directions

- Extend to additional V3-compatible DEXes (e.g. PancakeSwap V3, SushiSwap V3)
- Add incremental tick-state updates via `Swap`/`Mint`/`Burn` event subscriptions to keep V3 tick maps fresh without a full re-fetch
- Extend portability to other EVM-compatible networks

