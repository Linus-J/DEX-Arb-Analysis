# DEX Arbitrage Analysis Tool  ![license](https://img.shields.io/badge/License-MIT-green.svg?label=license)

This tool will attempt to detect a profitable transaction and upload it to the next block in Ethereum Mainet by using DEX arbitrage. This fork of the original [rusty-john](https://github.com/RenatoDev3/rusty-john) repo does away with the MEV bot deployment and adapts the source to analyse potential arb opportunities without execution instead. Now, we don't require any wallet and can use the adapted source to inspect the algorithm's behaviour.

## Features

- Query Uniswap pairs available
- Find matching pairs across different exchanges (UniswapV2, UniswapV3, Sushiswap)
- Update pair reserves
- Find arbitrage opportunities using optimal input

## Goals

- Extend implementation to other DEXes
- Extend portability to chains other than Ethereum
- Implement gas fee subtraction or actual profit simulation without writing to the chain

## Environment setup

    bash populate_env.sh

Compile crates and run

    cargo run
