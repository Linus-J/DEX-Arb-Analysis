#![allow(unused)]
use crate::address_book::{
    UniQuery, UniV3Pool as UniV3PoolContract,
    MIN_PROFIT_WETH, MIN_WETH_LIQUIDITY, UNISWAP_ROUTER, PANCAKESWAP_ROUTER,
    SUSHISWAP_ROUTER, WETH_ADDRESS,
};
use crate::utils::*;
use crate::v3_pool::V3Pool;
use ethers::{abi::ethereum_types::U512, prelude::*, utils::{format_ether, parse_ether}};

/// Returns true when the V2 pool's stored reserves appear stale relative to the V3 spot price.
/// This happens with rebase tokens (e.g. AMPL) whose pool balance diverges from reserves.
///
/// Computes the tokens-per-WETH ratio from both sources using cross-multiplication
/// (U512 to avoid overflow) and flags divergences greater than 1.5x.
fn v2_reserves_stale(reserve0_weth: U256, reserve1_token: U256, pool: &V3Pool, weth: H160) -> bool {
    if pool.sqrt_price_x96.is_zero() || reserve0_weth.is_zero() || reserve1_token.is_zero() {
        return false;
    }
    // Scale sqrt down by 32 bits before squaring to stay within U512.
    // This preserves enough precision for a ratio comparison with 1.5x tolerance.
    let sqrt = U512::from(pool.sqrt_price_x96 >> 32u32);
    let q128 = U512::one() << 128u32; // compensates for two 32-bit shifts
    // v3 tokens-per-WETH  = sqrt^2 / Q128  (if WETH=token0)
    //                      = Q128 / sqrt^2  (if WETH=token1)
    let (v3_num, v3_den) = if pool.token0 == weth {
        (sqrt * sqrt, q128)
    } else {
        let sq = sqrt * sqrt;
        if sq.is_zero() { return false; }
        (q128, sq)
    };
    // v2 tokens-per-WETH = reserve1 / reserve0
    // Cross-multiply to compare without division:
    //   v2/v3 = (reserve1 * v3_den) / (reserve0 * v3_num)
    let v2_side = U512::from(reserve1_token) * v3_den;
    let v3_side = U512::from(reserve0_weth) * v3_num;
    if v3_side.is_zero() || v2_side.is_zero() {
        return false;
    }
    // Flag if either side is more than 1.5x the other.
    let scale = U512::from(3u32);
    let two   = U512::from(2u32);
    v2_side * two > v3_side * scale || v3_side * two > v2_side * scale
}

#[derive(Debug)]
pub struct CrossedPairManager<'a, M>
where
    M: Middleware,
{
    flash_query_contract: &'a UniQuery<M>,
    markets: Vec<TokenMarket<'a>>,
    v3_pools: Vec<V3Pool>,
    /// Precomputed index: non-WETH token → indices into `v3_pools`.
    /// Rebuilt whenever `v3_pools` changes (only at startup).
    v3_by_token: std::collections::HashMap<H160, Vec<usize>>,
}

impl<'a, M> CrossedPairManager<'a, M>
where
    M: Middleware,
{
    pub fn new(
        grouped_pairs: &'a [(H160, Vec<[H160; 3]>)],
        flash_query_contract: &'a UniQuery<M>,
        v3_pools: Vec<V3Pool>,
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
        let v3_by_token = Self::build_v3_index(&v3_pools);
        Self {
            markets: pairs,
            flash_query_contract,
            v3_pools,
            v3_by_token,
        }
    }

    /// Build the non-WETH token → V3 pool index from a slice of pools.
    fn build_v3_index(pools: &[V3Pool]) -> std::collections::HashMap<H160, Vec<usize>> {
        let weth_addr: H160 = WETH_ADDRESS.parse().unwrap();
        let mut map: std::collections::HashMap<H160, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, pool) in pools.iter().enumerate() {
            let token = if pool.token0 == weth_addr {
                pool.token1
            } else {
                pool.token0
            };
            map.entry(token).or_default().push(i);
        }
        map
    }

    pub fn write_tokens(&mut self) {
        let tokens: Vec<H160> = self.markets.iter().map(|market| *market.token).collect();
        dbg!(tokens.len());
        if let Err(e) = write_tokens_to_file(tokens.clone()) {
            eprintln!("Failed to write tokens to file: {}", e);
        }
    }

    pub async fn update_reserve(&mut self, block: Option<u64>) {
        let reserves = self
            .get_all_pair_addresses()
            .iter()
            .map(|pair| pair.address)
            .collect::<Vec<H160>>();

        let call = self
            .flash_query_contract
            .get_reserves_by_pairs(reserves);
        let call = match block {
            Some(n) => call.block(BlockId::Number(n.into())),
            None    => call,
        };
        let reserves = call.call().await.unwrap();

        for (new_reserve, pair) in std::iter::zip(&reserves, self.get_all_pair_addresses()) {
            let weth_address = &WETH_ADDRESS.parse::<Address>().unwrap();
            // Normalise so that reserve0 = WETH, reserve1 = token
            let (reserve0, reserve1) = if &pair.token0 == weth_address {
                (new_reserve[0], new_reserve[1])
            } else {
                (new_reserve[1], new_reserve[0])
            };

            // Only keep pairs with at least MIN_WETH_LIQUIDITY on the WETH side.
            if reserve0 >= MIN_WETH_LIQUIDITY {
                pair.reserve = Some(Reserve {
                    reserve0,
                    reserve1,
                });
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

    /// Refresh each V3 pool's spot price (slot0 + liquidity) at a specific historical block.
    /// Ticks are not refetched — they change rarely and the snapshot approximation is sufficient.
    pub async fn update_v3_spot_prices(&mut self, block: u64) {
        let client = self.flash_query_contract.client();
        let block_id = BlockId::Number(block.into());
        for pool in &mut self.v3_pools {
            let contract = UniV3PoolContract::new(pool.address, client.clone());
            if let Ok(slot0) = contract.slot_0().block(block_id).call().await {
                pool.sqrt_price_x96 = slot0.0;
                pool.tick = slot0.1;
            }
            if let Ok(liq) = contract.liquidity().block(block_id).call().await {
                pool.liquidity = liq;
            }
        }
    }

    pub fn find_arbitrage_opportunities(&mut self, max_bal: u64, gas_price: U256) {
        let mb = parse_ether(max_bal).unwrap();
        let debug_arb = std::env::var("DEBUG_ARB").as_deref() == Ok("true");

        // --- V2 ↔ V2 arb ---
        let gas_limit_v2 = U256::from(180_000u64);
        let gas_cost_v2 = gas_price * gas_limit_v2;
        if gas_cost_v2 >= mb {
            println!(
                "Gas cost {} ETH exceeds max balance {} ETH.",
                format_ether(gas_cost_v2),
                format_ether(mb)
            );
        } else {
            let adjusted_bal = mb - gas_cost_v2;
            for market in &self.markets {
                market.find_arbitrage_opportunity(adjusted_bal);
            }
        }

        // --- Mixed V2 ↔ V3 and V3 ↔ V3 arb ---

        // Gas estimate for mixed/V3 arbs.
        let gas_limit_v3 = U256::from(200_000u64);
        let gas_cost_v3 = gas_price * gas_limit_v3;
        if gas_cost_v3 >= mb {
            return;
        }
        let adjusted_bal_v3 = mb - gas_cost_v3;

        let weth_addr: H160 = WETH_ADDRESS.parse().unwrap();
        for market in &self.markets {
            let indices = match self.v3_by_token.get(market.token) {
                Some(p) => p,
                None => continue,
            };
            let v3_pools: Vec<&V3Pool> =
                indices.iter().map(|&i| &self.v3_pools[i]).collect();

            // V2 ↔ V3
            for v2_pair in &market.pairs {
                let reserve = match v2_pair.reserve.as_ref() {
                    Some(r) => r,
                    None => continue,
                };

                for v3_pool in v3_pools.iter() {
                    let weth_in = adjusted_bal_v3;

                    // Skip this pair entirely if V2 reserves are temporally mismatched with V3.
                    if v2_reserves_stale(reserve.reserve0, reserve.reserve1, v3_pool, weth_addr) {
                        if debug_arb {
                            println!("  [SKIP V2↔V3] Stale V2 reserves (V2/V3 price divergence >1.5x). Token {:?}", market.token);
                        }
                        continue;
                    }

                    // Buy token on V2, sell token on V3.
                    let token_out_v2 =
                        v2_amount_out(weth_in, reserve.reserve0, reserve.reserve1);
                    if !token_out_v2.is_zero() {
                        if let Some(weth_out_v3) =
                            v3_pool.simulate_swap_token_in(token_out_v2)
                        {
                            if weth_out_v3 > weth_in {
                                let arb_profit = weth_out_v3 - weth_in;
                                if arb_profit >= MIN_PROFIT_WETH {
                                    println!(
                                        "\n----------- SIMULATED ARB (V2 buy → V3 sell) -----------"
                                    );
                                    println!("Token:     {:?}", market.token);
                                    println!("V2 pair:   {:?}", v2_pair.address);
                                    println!("V3 pool:   {:?}", v3_pool.address);
                                    println!(
                                        "Send {} WETH → potential profit {} WETH",
                                        format_ether(weth_in),
                                        format_ether(arb_profit)
                                    );
                                    println!("---------------------------------------------------------");
                                }
                            }
                        }
                    }

                    // Buy token on V3, sell token on V2.
                    if let Some(token_out_v3) = v3_pool.simulate_swap_weth_in(weth_in) {
                        if !token_out_v3.is_zero() {
                            let weth_out_v2 =
                                v2_amount_out(token_out_v3, reserve.reserve1, reserve.reserve0);
                            if weth_out_v2 > weth_in {
                                let arb_profit = weth_out_v2 - weth_in;
                                // Suppress if V2 reserves appear stale (e.g. rebase tokens).
                                if v2_reserves_stale(reserve.reserve0, reserve.reserve1, v3_pool, weth_addr) {
                                    if debug_arb {
                                        println!("  [SKIP V3→V2] Stale V2 reserves detected (V2/V3 price divergence >1.5x). Token {:?}", market.token);
                                    }
                                } else if arb_profit >= MIN_PROFIT_WETH {
                                    println!(
                                        "\n----------- SIMULATED ARB (V3 buy → V2 sell) -----------"
                                    );
                                    println!("Token:     {:?}", market.token);
                                    println!("V3 pool:   {:?}", v3_pool.address);
                                    println!("V2 pair:   {:?}", v2_pair.address);
                                    println!(
                                        "Send {} WETH → potential profit {} WETH",
                                        format_ether(weth_in),
                                        format_ether(arb_profit)
                                    );
                                    println!("---------------------------------------------------------");
                                    if debug_arb {
                                        let v3_rate = if !token_out_v3.is_zero() { weth_in / token_out_v3 } else { U256::zero() };
                                        let v2_rate = if !reserve.reserve1.is_zero() { reserve.reserve0 / reserve.reserve1 } else { U256::zero() };
                                        println!("  [DEBUG V3→V2]");
                                        println!("    V3 fee={} tick={} liquidity={}", v3_pool.fee, v3_pool.tick, v3_pool.liquidity);
                                        println!("    WETH in:               {} ({} WETH)", weth_in, format_ether(weth_in));
                                        println!("    token_out_v3 (raw):    {}", token_out_v3);
                                        println!("    V3 rate (wei/raw_tok): {}", v3_rate);
                                        println!("    V2 reserve0 (WETH):    {} ({} WETH)", reserve.reserve0, format_ether(reserve.reserve0));
                                        println!("    V2 reserve1 (token):   {} (raw)", reserve.reserve1);
                                        println!("    V2 rate (wei/raw_tok): {}", v2_rate);
                                        println!("    weth_out_v2:           {} ({} WETH)", weth_out_v2, format_ether(weth_out_v2));
                                        if !v3_rate.is_zero() && v2_rate > v3_rate * U256::from(2u32) {
                                            println!("    WARNING: V2 rate >2x V3 — reserves likely stale (rebase token?).");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // V3 ↔ V3 (different fee tiers for the same token)
            for (i, pool_a) in v3_pools.iter().enumerate() {
                for pool_b in v3_pools[i + 1..].iter() {
                    let weth_in = adjusted_bal_v3;

                    // Buy on pool_a, sell on pool_b.
                    if let Some(token_out) = pool_a.simulate_swap_weth_in(weth_in) {
                        if !token_out.is_zero() {
                            if let Some(weth_out) = pool_b.simulate_swap_token_in(token_out) {
                                if weth_out > weth_in {
                                    let arb_profit = weth_out - weth_in;
                                    if arb_profit >= MIN_PROFIT_WETH {
                                        println!(
                                            "\n----------- SIMULATED ARB (V3 ↔ V3) -----------"
                                        );
                                        println!("Token:      {:?}", market.token);
                                        println!("Buy  pool:  {:?} (fee {})", pool_a.address, pool_a.fee);
                                        println!("Sell pool:  {:?} (fee {})", pool_b.address, pool_b.fee);
                                        println!(
                                            "Send {} WETH → potential profit {} WETH",
                                            format_ether(weth_in),
                                            format_ether(arb_profit)
                                        );
                                        println!("-------------------------------------------------");
                                    }
                                }
                            }
                        }
                    }

                    // Buy on pool_b, sell on pool_a.
                    if let Some(token_out) = pool_b.simulate_swap_weth_in(weth_in) {
                        if !token_out.is_zero() {
                            if let Some(weth_out) = pool_a.simulate_swap_token_in(token_out) {
                                if weth_out > weth_in {
                                    let arb_profit = weth_out - weth_in;
                                    if arb_profit >= MIN_PROFIT_WETH {
                                        println!(
                                            "\n----------- SIMULATED ARB (V3 ↔ V3) -----------"
                                        );
                                        println!("Token:      {:?}", market.token);
                                        println!("Buy  pool:  {:?} (fee {})", pool_b.address, pool_b.fee);
                                        println!("Sell pool:  {:?} (fee {})", pool_a.address, pool_a.fee);
                                        println!(
                                            "Send {} WETH → potential profit {} WETH",
                                            format_ether(weth_in),
                                            format_ether(arb_profit)
                                        );
                                        println!("-------------------------------------------------");
                                    }
                                }
                            }
                        }
                    }
                }
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
    pub fn find_arbitrage_opportunity(&self, max_bal: U256) {
        for (i, pair_a) in self.pairs.iter().enumerate() {
            for (j, pair_b) in self.pairs.iter().enumerate() {
                if i == j {
                    continue;
                }
                if let Some((x, arb_profit)) = profit(
                    pair_a.reserve.as_ref().unwrap(),
                    pair_b.reserve.as_ref().unwrap(),
                    max_bal,
                ) {
                    if arb_profit >= MIN_PROFIT_WETH {
                        println!(
                            "\n---------------------------------- SIMULATED ARB ----------------------------------------"
                        );
                        println!("Token: {:?}", self.token);
                        println!("Pair 1 (sell token): {:?}", pair_a.address);
                        println!("Pair 2 (buy token):  {:?}", pair_b.address);
                        println!(
                            "Send {} WETH to receive potential profit of {} WETH",
                            format_ether(x),
                            format_ether(arb_profit)
                        );
                        println!("------------------------------------------------------------------------------------------");
                    }
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
    pub reserve0: U256,
    pub reserve1: U256,
}

impl Reserve {
    pub fn new(reserve0: U256, reserve1: U256) -> Self {
        Self { reserve0, reserve1 }
    }
}

/// Compute the output of a single Uniswap V2 swap (0.3% fee).
pub fn v2_amount_out(amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::zero();
    }
    let amount_in_with_fee = amount_in * U256::from(997u64);
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * U256::from(1000u64) + amount_in_with_fee;
    numerator / denominator
}

/// Compute the optimal two-hop arbitrage input and expected profit for a V2 ↔ V2 pair.
///
/// Convention (matching test expectations):
///   - pair_a: the **sell** pool — token is expensive here (high WETH / low token)
///   - pair_b: the **buy** pool  — token is cheap here (low WETH / high token)
/// Direction: buy token cheaply on pair_b (WETH → token), then sell on pair_a (token → WETH).
pub fn profit(pair_a: &Reserve, pair_b: &Reserve, max_bal: U256) -> Option<(U256, U256)> {
    // reserve0 = WETH, reserve1 = token (both pairs normalised by caller)
    // Uniswap V2 fee: 0.3%, effective multiplier is 997/1000

    let ra0 = pair_a.reserve0; // WETH in pair_a (sell pool)
    let ra1 = pair_a.reserve1; // token in pair_a (sell pool)
    let rb0 = pair_b.reserve0; // WETH in pair_b (buy pool)
    let rb1 = pair_b.reserve1; // token in pair_b (buy pool)

    // Opportunity exists only when pair_a has a higher WETH/token price than pair_b.
    // price_a = ra0/ra1, price_b = rb0/rb1  →  profit when ra0*rb1 > rb0*ra1.
    // Use U512 to avoid overflow when multiplying two U256 reserve values.
    let ra0_512 = U512::from(ra0);
    let ra1_512 = U512::from(ra1);
    let rb0_512 = U512::from(rb0);
    let rb1_512 = U512::from(rb1);
    if ra0_512 * rb1_512 <= rb0_512 * ra1_512 {
        return None;
    }

    // Optimal input (x*) using the standard two-hop formula with 0.3% fee per hop,
    // where pair_b is the buy pool and pair_a is the sell pool:
    // x* = (sqrt(997^2 * rb0 * rb1 * ra0 * ra1) - 1000 * rb0 * ra1) / (997 * rb1 + 1000 * ra0)
    let f = U512::from(997u64);
    let g = U512::from(1000u64);

    let ra0_ = ra0_512;
    let ra1_ = ra1_512;
    let rb0_ = rb0_512;
    let rb1_ = rb1_512;

    let under_sqrt = f * f * rb0_ * rb1_ * ra0_ * ra1_;
    let sqrt_val = under_sqrt.integer_sqrt();
    let sub = g * rb0_ * ra1_;

    if sqrt_val <= sub {
        return None;
    }

    let denom = f * rb1_ + g * ra0_;
    if denom.is_zero() {
        return None;
    }

    let x_opt_512 = (sqrt_val - sub) / denom;

    // Clamp to max_bal.
    let max_512 = U512::from(max_bal);
    let x_opt_512 = x_opt_512.min(max_512);

    if x_opt_512.is_zero() {
        return None;
    }

    // Simulate actual two-hop swap to get real profit.
    // Step 1: WETH → token on pair_b (buy pool, cheap token).
    let weth_in_997 = f * x_opt_512;
    let token_out = weth_in_997 * rb1_ / (g * rb0_ + weth_in_997);

    // Step 2: token → WETH on pair_a (sell pool, expensive token).
    let token_in_997 = f * token_out;
    let weth_out = token_in_997 * ra0_ / (g * ra1_ + token_in_997);

    if weth_out <= x_opt_512 {
        return None;
    }

    let profit_512 = weth_out - x_opt_512;

    // Convert back to U256 (safe since these are bounded by reserves which are U256).
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

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::utils::parse_ether;
    use std::collections::BTreeMap;
    use crate::v3_pool::V3Pool;

    /// Build a minimal V3Pool with token0=WETH, token1=fake_token at the given sqrt price.
    /// The pool has a single full-range position so any swap completes in one step.
    fn make_v3_pool(sqrt_price_x96: U256, tick: i32, liquidity: u128) -> V3Pool {
        let weth: H160 = crate::address_book::WETH_ADDRESS.parse().unwrap();
        let fake_token: H160 = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let liq_i = liquidity as i128;
        let mut ticks = BTreeMap::new();
        // Full-range position: MIN_TICK lower, MAX_TICK upper.
        ticks.insert(-887272i32, (liq_i, liquidity));
        ticks.insert(887272i32, (-liq_i, liquidity));
        V3Pool {
            address: H160::zero(),
            token0: weth,
            token1: fake_token,
            fee: 3000,
            sqrt_price_x96,
            tick,
            liquidity,
            tick_spacing: 60,
            ticks,
        }
    }

    // 1. Balanced pools → no opportunity
    #[test]
    fn test_no_arb_equal_prices() {
        let r = Reserve::new(parse_ether(1000).unwrap(), parse_ether(1000).unwrap());
        assert!(profit(&r, &r, parse_ether(10).unwrap()).is_none());
    }

    // 2. Clear price discrepancy → opportunity detected
    #[test]
    fn test_arb_detected() {
        // pair_a: expensive token (sell here) — high WETH reserve, low token reserve
        let pair_a = Reserve::new(parse_ether(1000).unwrap(), parse_ether(500).unwrap());
        // pair_b: cheap token (buy here) — low WETH reserve, high token reserve
        let pair_b = Reserve::new(parse_ether(500).unwrap(), parse_ether(1000).unwrap());
        let result = profit(&pair_a, &pair_b, parse_ether(100).unwrap());
        assert!(result.is_some());
        let (x, p) = result.unwrap();
        assert!(x > U256::zero());
        assert!(p > U256::zero());
    }

    // 3. Profit is bounded by max_bal
    #[test]
    fn test_arb_clamped_by_max_bal() {
        let pair_a = Reserve::new(parse_ether(1000).unwrap(), parse_ether(500).unwrap());
        let pair_b = Reserve::new(parse_ether(500).unwrap(), parse_ether(1000).unwrap());
        let tiny_bal = parse_ether(1).unwrap();
        let large_bal = parse_ether(500).unwrap();
        let (x_small, _) = profit(&pair_a, &pair_b, tiny_bal).unwrap();
        let (x_large, _) = profit(&pair_a, &pair_b, large_bal).unwrap();
        assert!(x_small <= tiny_bal);
        assert!(x_large <= large_bal);
    }

    // 4. Wrong direction (pair_b cheaper than pair_a) → no opportunity
    #[test]
    fn test_no_arb_wrong_direction() {
        let pair_a = Reserve::new(parse_ether(500).unwrap(), parse_ether(1000).unwrap());
        let pair_b = Reserve::new(parse_ether(1000).unwrap(), parse_ether(500).unwrap());
        assert!(profit(&pair_a, &pair_b, parse_ether(100).unwrap()).is_none());
    }

    // 5. Zero liquidity edge case
    #[test]
    fn test_zero_reserve_returns_none() {
        let pair_a = Reserve::new(U256::zero(), parse_ether(1000).unwrap());
        let pair_b = Reserve::new(parse_ether(1000).unwrap(), parse_ether(500).unwrap());
        assert!(profit(&pair_a, &pair_b, parse_ether(10).unwrap()).is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // V2 ↔ V3 detection tests
    // ─────────────────────────────────────────────────────────────────────────

    // Q96 constant used across V3 tests: 2^96
    fn q96() -> U256 {
        U256::from(1u64) << 96u32
    }

    // 6. Token cheaper on V2 → buy V2, sell V3 should profit
    // V2: 120 tokens/WETH  (lots of token relative to WETH)
    // V3: 100 tokens/WETH  (fewer tokens for WETH → token is more valuable here)
    // Strategy: spend 1 WETH on V2 (~119.5 tokens), then sell those tokens on V3 → > 1 WETH back
    #[test]
    fn test_v2_buy_v3_sell_arb_detected() {
        // sqrt_price_x96 = 10 * Q96  →  P = 100 tokens/WETH (token0=WETH)
        let pool = make_v3_pool(q96() * U256::from(10u64), 46054, 1_000_000_000_000_000_000_000_000u128);

        let weth_reserve  = parse_ether(1000).unwrap();
        let token_reserve = parse_ether(120_000).unwrap(); // 120 tokens per WETH on V2
        let weth_in = parse_ether(1).unwrap();

        // Step 1: buy token on V2 (cheaper source)
        let token_out = v2_amount_out(weth_in, weth_reserve, token_reserve);
        assert!(!token_out.is_zero(), "V2 should produce tokens");

        // Step 2: sell token on V3 (where token is worth more WETH)
        let weth_out = pool.simulate_swap_token_in(token_out).expect("V3 sim must succeed");
        assert!(
            weth_out > weth_in,
            "V2 buy → V3 sell: expected profit, got weth_out={weth_out} <= weth_in={weth_in}"
        );
    }

    // 7. Token cheaper on V3 → buy V3, sell V2 should profit
    // V3: 121 tokens/WETH  (sqrt=11*Q96 → P=121)
    // V2: 100 tokens/WETH
    // Strategy: spend 1 WETH on V3 (~120.6 tokens), then sell those tokens on V2 → > 1 WETH back
    #[test]
    fn test_v3_buy_v2_sell_arb_detected() {
        // sqrt_price_x96 = 11 * Q96  →  P = 121 tokens/WETH
        let pool = make_v3_pool(q96() * U256::from(11u64), 47960, 1_000_000_000_000_000_000_000_000u128);

        let weth_reserve  = parse_ether(1000).unwrap();
        let token_reserve = parse_ether(100_000).unwrap(); // 100 tokens per WETH on V2
        let weth_in = parse_ether(1).unwrap();

        // Step 1: buy token on V3 (cheaper source, 121 tokens/WETH)
        let token_out = pool.simulate_swap_weth_in(weth_in).expect("V3 sim must succeed");
        assert!(!token_out.is_zero(), "V3 should produce tokens");

        // Step 2: sell token on V2 (where token is worth more WETH)
        let weth_out = v2_amount_out(token_out, token_reserve, weth_reserve);
        assert!(
            weth_out > weth_in,
            "V3 buy → V2 sell: expected profit, got weth_out={weth_out} <= weth_in={weth_in}"
        );
    }

    // 8. Equal prices V2 = V3 → neither direction profits (fees eat any gain)
    #[test]
    fn test_no_arb_equal_v2_v3_prices() {
        // Both V2 and V3 at 100 tokens/WETH
        let pool = make_v3_pool(q96() * U256::from(10u64), 46054, 1_000_000_000_000_000_000_000_000u128);

        let weth_reserve  = parse_ether(1000).unwrap();
        let token_reserve = parse_ether(100_000).unwrap();
        let weth_in = parse_ether(1).unwrap();

        // V2 buy → V3 sell: fees on both legs should leave us with less than we started
        let token_from_v2 = v2_amount_out(weth_in, weth_reserve, token_reserve);
        let weth_back_v3 = pool
            .simulate_swap_token_in(token_from_v2)
            .unwrap_or(U256::zero());
        assert!(
            weth_back_v3 < weth_in,
            "Equal prices V2→V3: should not profit (fees), got weth_back={weth_back_v3}"
        );

        // V3 buy → V2 sell: same expectation
        let token_from_v3 = pool
            .simulate_swap_weth_in(weth_in)
            .unwrap_or(U256::zero());
        let weth_back_v2 = v2_amount_out(token_from_v3, token_reserve, weth_reserve);
        assert!(
            weth_back_v2 < weth_in,
            "Equal prices V3→V2: should not profit (fees), got weth_back={weth_back_v2}"
        );
    }

    // 9. v2_reserves_stale correctly flags a 2× price divergence
    #[test]
    fn test_stale_reserves_detected_at_2x() {
        let weth: H160 = crate::address_book::WETH_ADDRESS.parse().unwrap();
        // V3 at 100 tokens/WETH (token0 = WETH)
        let pool = make_v3_pool(q96() * U256::from(10u64), 46054, 1_000_000u128);

        // V2 at 200 tokens/WETH → 2× divergence → stale
        let weth_res = parse_ether(1000).unwrap();
        let tok_res  = parse_ether(200_000).unwrap();
        assert!(
            v2_reserves_stale(weth_res, tok_res, &pool, weth),
            "2× price divergence must be flagged as stale"
        );
    }

    // 10. v2_reserves_stale does NOT flag prices within 1.5× (including equal)
    #[test]
    fn test_stale_reserves_ok_within_threshold() {
        let weth: H160 = crate::address_book::WETH_ADDRESS.parse().unwrap();
        // V3 at 100 tokens/WETH
        let pool = make_v3_pool(q96() * U256::from(10u64), 46054, 1_000_000u128);
        let weth_res = parse_ether(1000).unwrap();

        // Equal prices → not stale
        let tok_equal = parse_ether(100_000).unwrap();
        assert!(
            !v2_reserves_stale(weth_res, tok_equal, &pool, weth),
            "Equal prices must not be flagged as stale"
        );

        // 1.4× divergence → still within 1.5× threshold → not stale
        let tok_14x = parse_ether(140_000).unwrap();
        assert!(
            !v2_reserves_stale(weth_res, tok_14x, &pool, weth),
            "1.4× divergence must not be flagged as stale"
        );
    }
}
