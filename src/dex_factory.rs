use itertools::Itertools;
use std::sync::Arc;

use crate::
    address_book::{UniQuery, UniV2Factory, WETH_ADDRESS
    };
use ethers::prelude::*;

pub async fn get_markets_by_token<M>(
    factory_addresses: Vec<Address>,
    flash_query_contract: &UniQuery<M>,
    client: Arc<M>,
) -> Vec<(H160, Vec<[H160; 3]>)>
where
    M: Middleware,
{
    let factories: Vec<DexFactory<M>> = factory_addresses
        .into_iter()
        .map(|address| DexFactory::new(address, flash_query_contract, client.clone()))
        .collect();

    let weth_address = &WETH_ADDRESS.parse::<Address>().unwrap();

    let mut pairs: Vec<[H160; 3]> = vec![];
    for (i, factory) in factories.into_iter().enumerate() {
        println!("[V2] Factory {}: {:?}", i + 1, factory.factory_contract.address());
        let market_pairs = factory
            .get_markets()
            .await
            .into_iter()
            .collect::<Vec<[H160; 3]>>();

        println!("[V2] Factory {}: {} pairs fetched.", i + 1, market_pairs.len());
        for pair in market_pairs {
            pairs.push(pair.to_owned());
        }
    }

    let grouped_pairs: Vec<(H160, Vec<[H160; 3]>)> = pairs
        .into_iter()
        .filter(|pair| {
            pair[0].eq(weth_address) || pair[1].eq(weth_address)
        })
        .sorted_by(|pair0, pair1| {
            let non_weth0 = if pair0[0].eq(weth_address) {
                pair0[1]
            } else {
                pair0[0]
            };
            let non_weth1 = if pair1[0].eq(weth_address) {
                pair1[1]
            } else {
                pair1[0]
            };
            non_weth1.cmp(&non_weth0)
        })
        .group_by(|pair| {
            if pair[0].eq(weth_address) {
                pair[1]
            } else {
                pair[0]
            }
        })
        .into_iter()
        .map(|(key, group)| {
            let pairs = group.collect::<Vec<[H160; 3]>>();
            (key, pairs)
        })
        .filter(|(_, pairs)| pairs.len() > (1_usize))
        .collect::<Vec<(H160, Vec<[H160; 3]>)>>();

    grouped_pairs
}

pub struct DexFactory<'a, M> {
    factory_contract: UniV2Factory<M>,
    flash_query_contract: &'a UniQuery<M>,
}

impl<'a, M> DexFactory<'a, M>
where
    M: Middleware,
{
    pub fn new(
        pair_address: Address,
        flash_query_contract: &'a UniQuery<M>,
        client: Arc<M>,
    ) -> Self {
        let contract = UniV2Factory::new(pair_address, client);
        Self {
            factory_contract: contract,
            flash_query_contract,
        }
    }

    pub async fn get_markets(&self) -> Vec<[H160; 3]> {
        let batch_size = U256::from(500u32);
        let mut start = U256::from(0u32);
        let mut batch_num = 0usize;
        let pair_limit = std::env::var("PAIR_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok());

        let mut markets: Vec<[H160; 3]> = vec![];
        loop {
            let stop = start + batch_size;
            batch_num += 1;

            let pairs = self
                .flash_query_contract
                .get_pairs_by_index_range(self.factory_contract.address(), start, stop)
                .call()
                .await
                .unwrap();

            start = stop;

            let pair_length = pairs.len();
            markets.extend(pairs);

            if batch_num % 50 == 0 {
                println!("[V2]   ... batch {}, {} pairs so far", batch_num, markets.len());
            }

            if pair_length < batch_size.as_usize() {
                break;
            }

            if let Some(limit) = pair_limit {
                if markets.len() >= limit {
                    markets.truncate(limit);
                    println!("[V2]   PAIR_LIMIT={} reached, stopping early.", limit);
                    break;
                }
            }
        }
        markets
    }
}
