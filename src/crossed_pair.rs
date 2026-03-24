#![allow(unused)]
use crate::address_book::{UniQuery, UNISWAP_ROUTER, PANCAKESWAP_ROUTER, SUSHISWAP_ROUTER, WETH_ADDRESS};
use crate::utils::*;
use ethers::{abi::ethereum_types::U512, prelude::*, utils::{format_ether, parse_ether}};

#[derive(Debug)]
pub struct CrossedPairManager<'a, M>
where
    M: Middleware,
{
    flash_query_contract: &'a UniQuery<M>,
    markets: Vec<TokenMarket<'a>>,
}

impl<'a, M> CrossedPairManager<'a, M>
where
    M: Middleware,
{
    pub fn new(
        grouped_pairs: &'a [(H160, Vec<[H160; 3]>)],
        flash_query_contract: &'a UniQuery<M>,
    ) -> Self {
        let pairs = grouped_pairs
            .iter()
            .map(|(token, pairs)| TokenMarket {
                token,
                pairs: pairs
                    .iter()
                    .copied()
                    .map(|[token0, token1, address]| Pair {
                        address,
                        token0,
                        token1,
                        reserve: None,
                    })
                    .collect::<Vec<Pair>>(),
            })
            .collect::<Vec<TokenMarket>>();
        Self {
            markets: pairs,
            flash_query_contract,
        }
    }

    pub fn write_tokens(&mut self) {
        let tokens: Vec<H160> = self.markets.iter().map(|market| *market.token).collect();
        dbg!(tokens.len());
        if let Err(e) = write_tokens_to_file(tokens.clone()) {
            eprintln!("Failed to write tokens to file: {}", e);
            }
        }

    pub async fn update_reserve(&mut self) {
        let reserves = self
            .get_all_pair_addresses()
            .iter()
            .map(|pair| pair.address)
            .collect::<Vec<H160>>();

        let reserves = self
            .flash_query_contract
            .get_reserves_by_pairs(reserves)
            .call()
            .await
            .unwrap();

        let min_weth = parse_ether(500).unwrap();   // Filter out pairs that have less than 500 WETH

        for (new_reserve, pair) in std::iter::zip(&reserves, self.get_all_pair_addresses()) {
            let weth_address = &WETH_ADDRESS.parse::<Address>().unwrap();
            // Normalise so that reserve0 = WETH, reserve1 = token
            let (reserve0, reserve1) = if &pair.token0 == weth_address {
                (new_reserve[0], new_reserve[1])   // token0 is WETH → reserve0 is WETH
            } else {
                (new_reserve[1], new_reserve[0])   // token1 is WETH → reserve0 is WETH
            };

            // Only keep the pair if it has at least 500 WETH liquidity on the WETH side
            if reserve0 >= min_weth {
                let updated_reserve = Reserve {
                    reserve0,
                    reserve1,
                    // block_timestamp_last: new_reserve[2],
                };
    
                pair.reserve = Some(updated_reserve);
            } else {
                pair.reserve = None;
            }
        }
        for market in &mut self.markets {
            market.pairs.retain(|pair| pair.reserve.is_some());
        }
    }

    pub fn get_all_pair_addresses(&mut self) -> Vec<&mut Pair> {
        self.markets
            .iter_mut()
            .flat_map(|token_market| &mut token_market.pairs)
            .collect::<Vec<&mut Pair>>()
    }

    pub async fn find_arbitrage_opportunities(&mut self, max_bal: u64) {
        let config = Config::new().await;
        let gas_price = U256::from(config.http.get_gas_price().await.unwrap());
        let mb = parse_ether(max_bal).unwrap();
        let gas_limit = U256::from(300_000u64); // conservative estimate for a 2-hop arb
        let gas_cost = gas_price * gas_limit;
        if gas_cost >= mb {
            println!("Gas cost {} ETH exceeds max balance {} ETH.", format_ether(gas_cost), format_ether(mb));
        } else {
            let adjusted_bal = mb - gas_cost;
            for market in &mut self.markets {
                market.find_arbitrage_opportunity(adjusted_bal).await;
            }
        }
    }
}


#[derive(Debug)]
pub struct TokenMarket<'a> {
    token: &'a H160,
    pairs: Vec<Pair>,
}

impl<'a> TokenMarket<'a> {
    pub async fn find_arbitrage_opportunity(&self, max_bal: U256) {
        for (i, pair_a) in self.pairs.iter().enumerate() {
            for (j, pair_b) in self.pairs.iter().enumerate() {
                if i == j { continue; }
                if let Some((x, profit)) = profit(
                    pair_a.reserve.as_ref().unwrap(),
                    pair_b.reserve.as_ref().unwrap(),
                    max_bal,
                ) { 
                    println!("\n---------------------------------- SIMULATED ARB ----------------------------------------");
                    println!("Token: {:?}", self.token);
                    println!("Pair 1 (buy token): {:?}", pair_a.address);
                    println!("Pair 2 (sell token): {:?}", pair_b.address);
                    println!("Send {} WETH to receive potential profit of {} WETH", format_ether(x), format_ether(profit));
                    println!("------------------------------------------------------------------------------------------");
                }
            }
        }
    }
}



use std::{sync::Arc, ops::Add};

#[derive(Debug)]
#[allow(dead_code)]
pub struct Pair {
    address: H160,
    token0: H160,
    token1: H160,
    reserve: Option<Reserve>,
}

#[derive(Debug)]
pub struct Reserve {
    reserve0: U256,
    reserve1: U256,
}

impl Reserve {
    pub fn new(reserve0: U256, reserve1: U256) -> Self {
        Self { reserve0, reserve1 }
    }
}

//Fix accuracy of profit function
pub fn profit(pair_a: &Reserve, pair_b: &Reserve, max_bal: U256) -> Option<(U256, U256)> {
    // reserve0 = WETH, reserve1 = token (both pairs normalised by caller)
    // Direction: buy token on pair_a (WETH → token), sell token on pair_b (token → WETH)
    // Uniswap V2 fee: 0.3%, effective multiplier is 997/1000

    let ra0 = pair_a.reserve0; // WETH in pair_a
    let ra1 = pair_a.reserve1; // token in pair_a
    let rb0 = pair_b.reserve0; // WETH in pair_b
    let rb1 = pair_b.reserve1; // token in pair_b

    // No opportunity if price on pair_b is not higher than pair_a
    // price_a = ra0/ra1, price_b = rb0/rb1
    // Need rb0*ra1 > ra0*rb1 to profit by buying on pair_a and selling on pair_b
    // Use U512 to avoid overflow when multiplying two U256 reserve values
    let ra0_512 = U512::from(ra0);
    let ra1_512 = U512::from(ra1);
    let rb0_512 = U512::from(rb0);
    let rb1_512 = U512::from(rb1);
    if rb0_512 * ra1_512 <= ra0_512 * rb1_512 {
        return None;
    }

    // Optimal input using the standard two-hop formula with 0.3% fee per hop:
    // x* = (sqrt(997^2 * ra0 * ra1 * rb0 * rb1) - 1000 * ra0 * rb1) / (997 * ra1 + 1000 * rb0)
    // Use U512 to avoid overflow
    let f = U512::from(997u64);
    let g = U512::from(1000u64);

    let ra0_ = ra0_512;
    let ra1_ = ra1_512;
    let rb0_ = rb0_512;
    let rb1_ = rb1_512;

    let under_sqrt = f * f * ra0_ * ra1_ * rb0_ * rb1_;
    let sqrt_val = under_sqrt.integer_sqrt();
    let sub = g * ra0_ * rb1_;

    if sqrt_val <= sub {
        return None;
    }

    let denom = f * ra1_ + g * rb0_;
    if denom.is_zero() {
        return None;
    }

    let x_opt_512 = (sqrt_val - sub) / denom;

    // Clamp to max_bal
    let max_512 = U512::from(max_bal);
    let x_opt_512 = x_opt_512.min(max_512);

    if x_opt_512.is_zero() {
        return None;
    }

    // Simulate actual two-hop swap to get real profit
    // Step 1: WETH → token on pair_a
    // amount_out = (fee * amount_in * reserve_out) / (1000 * reserve_in + fee * amount_in)
    let weth_in_997 = f * x_opt_512;
    let token_out = weth_in_997 * ra1_ / (g * ra0_ + weth_in_997);

    // Step 2: token → WETH on pair_b
    // amount_out = (fee * amount_in * reserve_out) / (1000 * reserve_in + fee * amount_in)
    let token_in_997 = f * token_out;
    let weth_out = token_in_997 * rb0_ / (g * rb1_ + token_in_997);

    // Profit is only real if weth_out > x_opt
    if weth_out <= x_opt_512 {
        return None;
    }

    let profit_512 = weth_out - x_opt_512;

    // Convert back to U256 (safe since these are bounded by reserves which are U256)
    let x_opt = U256::try_from(x_opt_512).ok()?;
    let profit = U256::try_from(profit_512).ok()?;

    Some((x_opt, profit))
}

use std::fs::File;
use std::io::prelude::*;
fn write_tokens_to_file(tokens: Vec<H160>) -> std::io::Result<()> {
    let mut file = File::create("src/tokens.txt")?;
    let tokens_str = tokens
        .iter()
        .map(|token| format!("\"{:?}\"", token))
        .collect::<Vec<String>>()
        .join(", ");
    let tokens_line = format!("[{}]", tokens_str);
    writeln!(file, "{}", tokens_line)?;

    Ok(())
}
