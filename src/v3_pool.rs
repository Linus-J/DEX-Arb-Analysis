#![allow(unused)]
use std::collections::BTreeMap;
use std::sync::Arc;

use ethers::prelude::*;

use crate::address_book::{UniV3Factory, UniV3Pool as UniV3PoolContract, WETH_ADDRESS};

/// Sparse tick entry: (liquidityNet, liquidityGross).
type TickEntry = (i128, u128);

/// Off-chain representation of a Uniswap V3 pool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct V3Pool {
    pub address: H160,
    pub token0: H160,
    pub token1: H160,
    /// Fee tier in hundredths of a bip (e.g. 500, 3000, 10000).
    pub fee: u32,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub tick_spacing: i32,
    /// Sparse map of initialised ticks: tick index → (liquidityNet, liquidityGross).
    pub ticks: BTreeMap<i32, TickEntry>,
}

impl V3Pool {
    fn weth_zero_for_one(&self, weth_in: bool) -> Option<bool> {
        let weth: H160 = WETH_ADDRESS.parse().expect("valid WETH address");
        let token0_is_weth = self.token0 == weth;
        let token1_is_weth = self.token1 == weth;

        let zero_for_one = match (token0_is_weth, token1_is_weth, weth_in) {
            // WETH -> token
            (true, false, true) => true,
            (false, true, true) => false,
            // token -> WETH
            (true, false, false) => false,
            (false, true, false) => true,
            _ => {
                debug_assert!(
                    false,
                    "simulate_swap_*_weth API called for pool without exactly one WETH side"
                );
                return None;
            }
        };

        Some(zero_for_one)
    }

    /// Simulate a swap of `amount_in` of the WETH-side token through this pool.
    /// Returns the amount of the other token received, or None if the pool has no liquidity
    /// or if the pool does not contain exactly one WETH side.
    pub fn simulate_swap_weth_in(&self, amount_in: U256) -> Option<U256> {
        if self.liquidity == 0 {
            return None;
        }
        let zero_for_one = self.weth_zero_for_one(true)?;
        self.simulate_swap(amount_in, zero_for_one)
    }

    /// Simulate the reverse swap (non-WETH token → WETH). Returns WETH out.
    /// Returns None if the pool has no liquidity or does not contain exactly one WETH side.
    pub fn simulate_swap_token_in(&self, amount_in: U256) -> Option<U256> {
        if self.liquidity == 0 {
            return None;
        }
        let zero_for_one = self.weth_zero_for_one(false)?;
        self.simulate_swap(amount_in, zero_for_one)
    }

    /// Core off-chain swap simulation walking initialised ticks.
    fn simulate_swap(&self, amount_in: U256, zero_for_one: bool) -> Option<U256> {
        use uniswap_v3_math::{
            swap_math::compute_swap_step,
            tick_math::{
                get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio, MAX_SQRT_RATIO, MAX_TICK,
                MIN_SQRT_RATIO, MIN_TICK,
            },
        };
        // uniswap_v3_math depends on the published ethers-core, not the git version.
        // Use that crate's I256 type directly to avoid a type mismatch.
        use ethers_core::types::I256 as V3I256;

        if amount_in.is_zero() {
            return None;
        }

        let mut sqrt_price_current = self.sqrt_price_x96;
        let mut tick_current = self.tick;
        let mut liquidity_current = self.liquidity;
        let mut amount_remaining = amount_in;
        let mut amount_out_total = U256::zero();

        // Hard price limits — we never push the pool past these.
        let sqrt_price_limit = if zero_for_one {
            MIN_SQRT_RATIO + U256::one()
        } else {
            MAX_SQRT_RATIO - U256::one()
        };

        // Safety cap to prevent infinite loops on degenerate pool data.
        let max_iterations = 200usize;
        let mut iter = 0usize;

        while !amount_remaining.is_zero()
            && sqrt_price_current != sqrt_price_limit
            && iter < max_iterations
        {
            iter += 1;

            // Find next initialised tick in the swap direction.
            let tick_next = if zero_for_one {
                // Price decreasing → look for the largest tick strictly below current.
                self.ticks
                    .range(..tick_current)
                    .next_back()
                    .map(|(&t, _)| t)
                    .unwrap_or(MIN_TICK)
            } else {
                // Price increasing → look for the smallest tick strictly above current.
                self.ticks
                    .range((tick_current + 1)..)
                    .next()
                    .map(|(&t, _)| t)
                    .unwrap_or(MAX_TICK)
            };

            let tick_next = tick_next.clamp(MIN_TICK, MAX_TICK);
            let sqrt_price_next = get_sqrt_ratio_at_tick(tick_next).ok()?;

            // Clamp the target to the price limit.
            let sqrt_price_target = if zero_for_one {
                if sqrt_price_next < sqrt_price_limit {
                    sqrt_price_limit
                } else {
                    sqrt_price_next
                }
            } else {
                if sqrt_price_next > sqrt_price_limit {
                    sqrt_price_limit
                } else {
                    sqrt_price_next
                }
            };

            // V3I256::from_raw is safe for all realistic amounts (< 2^255).
            let amount_remaining_i256 = V3I256::from_raw(amount_remaining);
            let (next_sqrt_price, step_amount_in, step_amount_out, step_fee) =
                compute_swap_step(
                    sqrt_price_current,
                    sqrt_price_target,
                    liquidity_current,
                    amount_remaining_i256,
                    self.fee,
                )
                .ok()?;

            let consumed = step_amount_in.saturating_add(step_fee);
            amount_remaining = amount_remaining.saturating_sub(consumed);
            amount_out_total = amount_out_total.saturating_add(step_amount_out);
            sqrt_price_current = next_sqrt_price;

            // If we reached the tick boundary, apply the liquidity delta and cross the tick.
            if next_sqrt_price == sqrt_price_next {
                if let Some(&(liquidity_net, _)) = self.ticks.get(&tick_next) {
                    // Per Uniswap V3 convention, negate liquidityNet when moving left.
                    let delta = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };
                    if delta >= 0 {
                        liquidity_current =
                            liquidity_current.saturating_add(delta as u128);
                    } else {
                        liquidity_current =
                            liquidity_current.saturating_sub((-delta) as u128);
                    }
                }
                tick_current = if zero_for_one {
                    tick_next - 1
                } else {
                    tick_next
                };
            } else {
                tick_current =
                    get_tick_at_sqrt_ratio(next_sqrt_price).unwrap_or(tick_current);
            }

            // Stop if all liquidity has been consumed.
            if liquidity_current == 0 {
                break;
            }
        }

        // If the iteration cap was hit before the full input was consumed, the output
        // is only partial and would produce a false arb signal — return None.
        if iter == max_iterations && !amount_remaining.is_zero() {
            return None;
        }

        if amount_out_total.is_zero() {
            None
        } else {
            Some(amount_out_total)
        }
    }
}

/// Discover all V3 pools pairing each token in `tokens` with `weth` across all fee tiers.
/// Fetches pool state (slot0, liquidity, tick data) via on-chain calls.
pub async fn fetch_v3_pools_for_tokens<M: Middleware>(
    tokens: &[H160],
    weth: H160,
    factory_addr: H160,
    client: Arc<M>,
) -> Vec<V3Pool> {
    let fee_tiers: [u32; 3] = [500, 3000, 10000];
    let factory = UniV3Factory::new(factory_addr, client.clone());
    let mut pools = Vec::new();
    let total_tokens = tokens.len();

    for (token_idx, &token) in tokens.iter().enumerate() {
        if token_idx % 100 == 0 {
            println!("[V3]   {}/{} tokens processed, {} pools found so far...", token_idx, total_tokens, pools.len());
        }
        for &fee in &fee_tiers {
            // Query factory for pool address.
            let pool_addr = match factory.get_pool(token, weth, fee).call().await {
                Ok(addr) => addr,
                Err(_) => continue,
            };
            if pool_addr == H160::zero() {
                continue;
            }

            let pool_contract = UniV3PoolContract::new(pool_addr, client.clone());

            // Fetch slot0.
            let slot0 = match pool_contract.slot_0().call().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let sqrt_price_x96: U256 = slot0.0;
            let tick: i32 = slot0.1;

            // Skip uninitialized pools.
            if sqrt_price_x96.is_zero() {
                continue;
            }

            // Fetch current in-range liquidity.
            let liquidity: u128 = match pool_contract.liquidity().call().await {
                Ok(l) => l,
                Err(_) => continue,
            };

            // Fetch tick spacing.
            let tick_spacing: i32 = match pool_contract.tick_spacing().call().await {
                Ok(ts) => ts,
                Err(_) => continue,
            };
            if tick_spacing == 0 {
                continue;
            }

            // Fetch canonical token order from the pool itself.
            let token0 = match pool_contract.token_0().call().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            let token1 = match pool_contract.token_1().call().await {
                Ok(t) => t,
                Err(_) => continue,
            };

            // Discover initialised ticks via tickBitmap.
            // MAX_TICK = 887272 is the Uniswap V3 absolute tick boundary.
            let max_word =
                ((uniswap_v3_math::tick_math::MAX_TICK / tick_spacing) / 256 + 1)
                    .min(i16::MAX as i32) as i16;
            let current_compressed = tick.div_euclid(tick_spacing);
            let current_word = current_compressed.div_euclid(256).clamp(
                -(max_word as i32),
                max_word as i32,
            ) as i16;
            let configured_word_limit = std::env::var("V3_POOL_BITMAP_SCAN_WORD_LIMIT")
                .ok()
                .and_then(|v| v.parse::<u16>().ok())
                .filter(|&limit| limit > 0);
            let (scan_start_word, scan_end_word) = if let Some(limit) = configured_word_limit {
                let half_window = ((limit as i32) - 1) / 2;
                let mut start = current_word as i32 - half_window;
                let mut end = current_word as i32 + half_window;
                let max_word_i32 = max_word as i32;

                if start < -max_word_i32 {
                    end = (end + (-max_word_i32 - start)).min(max_word_i32);
                    start = -max_word_i32;
                }
                if end > max_word_i32 {
                    start = (start - (end - max_word_i32)).max(-max_word_i32);
                    end = max_word_i32;
                }

                (start as i16, end as i16)
            } else {
                (-max_word, max_word)
            };
            let scan_word_count = scan_end_word as i32 - scan_start_word as i32 + 1;
            let verbose_bitmap_scan = std::env::var("V3_POOL_VERBOSE_BITMAP_SCAN")
                .map(|v| {
                    let v = v.trim();
                    v == "1"
                        || v.eq_ignore_ascii_case("true")
                        || v.eq_ignore_ascii_case("yes")
                        || v.eq_ignore_ascii_case("on")
                })
                .unwrap_or(false);
            if verbose_bitmap_scan {
                println!(
                    "[V3]   Pool {:?} (fee {}) — scanning {} bitmap words for ticks...",
                    pool_addr,
                    fee,
                    scan_word_count,
                );
            }
            let mut ticks: BTreeMap<i32, TickEntry> = BTreeMap::new();
            for word_pos in scan_start_word..=scan_end_word {
                let bitmap = match pool_contract.tick_bitmap(word_pos).call().await {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                if bitmap.is_zero() {
                    continue;
                }
                for bit in 0u32..256 {
                    if (bitmap >> bit) & U256::one() == U256::one() {
                        let compressed =
                            word_pos as i32 * 256 + bit as i32;
                        let tick_idx = compressed * tick_spacing;
                        match pool_contract.ticks(tick_idx).call().await {
                            Ok(td) => {
                                // td.0 = liquidityGross (u128), td.1 = liquidityNet (i128)
                                ticks.insert(tick_idx, (td.1, td.0));
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }

            pools.push(V3Pool {
                address: pool_addr,
                token0,
                token1,
                fee,
                sqrt_price_x96,
                tick,
                liquidity,
                tick_spacing,
                ticks,
            });
        }
    }

    pools
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::types::U256;

    fn make_simple_pool(fee: u32, sqrt_price_x96: U256, tick: i32, liquidity: u128) -> V3Pool {
        use uniswap_v3_math::tick_math::{MIN_TICK, MAX_TICK};
        // A pool with a single liquidity range (no tick crossings needed).
        // Use MIN_TICK and MAX_TICK as the only ticks so the swap never crosses them
        // for small input amounts.
        let mut ticks = BTreeMap::new();
        // liquidityNet at lower bound is +liquidity, at upper bound is -liquidity.
        ticks.insert(MIN_TICK, (liquidity as i128, liquidity));
        ticks.insert(MAX_TICK, (-(liquidity as i128), liquidity));
        // Set token0 = WETH so simulate_swap_weth_in / simulate_swap_token_in resolve correctly.
        let weth: H160 = WETH_ADDRESS.parse().unwrap();
        let dummy_token: H160 = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        V3Pool {
            address: H160::zero(),
            token0: weth,
            token1: dummy_token,
            fee,
            sqrt_price_x96,
            tick,
            liquidity,
            tick_spacing: 60,
            ticks,
        }
    }

    /// sqrtPriceX96 for price = 1 (1:1 pool, simplest to reason about).
    /// 2^96 = 79228162514264337593543950336
    fn price_1_sqrt() -> U256 {
        U256::from_dec_str("79228162514264337593543950336").unwrap()
    }

    // 1. Non-zero swap in a liquid pool returns a non-zero output.
    #[test]
    fn test_v3_swap_returns_nonzero() {
        let pool =
            make_simple_pool(3000, price_1_sqrt(), 0, 1_000_000_000_000_000_000u128);
        let amount_in = U256::from(1_000_000u64); // small input
        let out = pool.simulate_swap_weth_in(amount_in);
        assert!(out.is_some());
        assert!(out.unwrap() > U256::zero());
    }

    // 2. Zero liquidity pool returns None.
    #[test]
    fn test_v3_swap_no_liquidity_returns_none() {
        let pool = make_simple_pool(3000, price_1_sqrt(), 0, 0);
        let out = pool.simulate_swap_weth_in(U256::from(1_000_000u64));
        assert!(out.is_none());
    }

    // 3. Swap output respects fee — higher fee tier produces less output.
    #[test]
    fn test_v3_higher_fee_produces_less_output() {
        let liq = 1_000_000_000_000_000_000u128;
        let price = price_1_sqrt();
        let pool_low_fee = make_simple_pool(500, price, 0, liq);
        let pool_high_fee = make_simple_pool(10000, price, 0, liq);
        let amount_in = U256::from(1_000_000u64);
        let out_low = pool_low_fee.simulate_swap_weth_in(amount_in).unwrap();
        let out_high = pool_high_fee.simulate_swap_weth_in(amount_in).unwrap();
        assert!(out_low > out_high, "lower fee should yield more output");
    }

    // 4. Reverse swap (token in → WETH out) returns non-zero.
    #[test]
    fn test_v3_reverse_swap_returns_nonzero() {
        let pool =
            make_simple_pool(3000, price_1_sqrt(), 0, 1_000_000_000_000_000_000u128);
        let out = pool.simulate_swap_token_in(U256::from(1_000_000u64));
        assert!(out.is_some());
        assert!(out.unwrap() > U256::zero());
    }
}
