# DEX Arbitrage Analysis Tool  ![license](https://img.shields.io/badge/License-MIT-green.svg?label=license)

This tool will attempt to detect a profitable transaction and upload it to the next block in Ethereum Mainet by using DEX arbitrage. This fork of the original [rusty-john](https://github.com/RenatoDev3/rusty-john) repo does away with the MEV bot deployment and adapts the source to analyse potential arb opportunities without execution instead. The tool can be used for free (without any wallet balance).

## Features

- Query Uniswap pairs available
- Find matching pairs across different exchanges (UniswapV2, UniswapV3, Sushiswap)
- Update pair reserves
- Find arbitrage opportunities using optimal input

## Goals

- Extend implementation to other DEXes
- Extend portability to other networks other than Ethereum Mainnet

## Environment setup

Make a API key to query the chain with [Infura](https://app.infura.io/).

Create a dummy ETH wallet with any wallet provider and retrieve the private key.

Setup environment variables by running:

    bash populate_env.sh

Compile crates and run:

    cargo run
