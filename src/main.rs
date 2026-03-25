#![allow(unused)]

use ethers::prelude::*;
use futures::StreamExt;
use rusty_john::{utils::*, crossed_pair::*, dex_factory::*, address_book::*, v3_pool::*};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv::dotenv().ok();
    let config = Config::new().await;
    println!("[STARTING]");

    let factory_addresses = vec![
        UNISWAP_FACTORY,
        PANCAKESWAP_FACTORY,
        SUSHISWAP_FACTORY,
    ]
    .into_iter()
    .map(|address| {
        address
            .parse::<Address>()
            .expect("parse factory address failed")
    })
    .collect::<Vec<Address>>();

    let flash_query_address = QUERY_CONTRACT.parse::<Address>().unwrap();
    let flash_query_contract = UniQuery::new(flash_query_address, config.http.clone());
    let grouped_pairs =
        get_markets_by_token(factory_addresses, &flash_query_contract, config.http.clone()).await;

    // Discover V3 pools for all tokens found in V2 markets.
    let token_list: Vec<H160> = grouped_pairs.iter().map(|(t, _)| *t).collect();
    let v3_factory = UNISWAP_V3_FACTORY.parse::<Address>().unwrap();
    let v3_pools = fetch_v3_pools_for_tokens(
        &token_list,
        WETH_ADDRESS.parse().unwrap(),
        v3_factory,
        config.http.clone(),
    )
    .await;

    let mut crossed_pair = CrossedPairManager::new(&grouped_pairs, &flash_query_contract, v3_pools);
    crossed_pair.write_tokens();

    // Initial snapshot.
    crossed_pair.update_reserve().await;
    crossed_pair.find_arbitrage_opportunities(config.max_bal).await;

    // Subscribe to new blocks and refresh reserves + run analysis on every block.
    let mut stream = config.wss.subscribe_blocks().await?;
    while let Some(_block) = stream.next().await {
        crossed_pair.update_reserve().await;
        crossed_pair.find_arbitrage_opportunities(config.max_bal).await;
    }

    Ok(())
}
