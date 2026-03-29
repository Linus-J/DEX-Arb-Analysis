#![allow(unused)]

use ethers::prelude::*;
use futures::StreamExt;
use rusty_john::{utils::*, crossed_pair::*, dex_factory::*, address_book::*, v3_pool::*};

const SNAPSHOT_PATH: &str = "snapshot.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    /// Block number at which the V3 tick data was fetched.
    snapshot_block: u64,
    grouped_pairs: Vec<(H160, Vec<[H160; 3]>)>,
    v3_pools: Vec<V3Pool>,
}

fn load_snapshot() -> Option<Snapshot> {
    let data = std::fs::read_to_string(SNAPSHOT_PATH).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_snapshot(snapshot_block: u64, grouped_pairs: &[(H160, Vec<[H160; 3]>)], v3_pools: &[V3Pool]) {
    let snap = Snapshot {
        snapshot_block,
        grouped_pairs: grouped_pairs.to_vec(),
        v3_pools: v3_pools.to_vec(),
    };
    match serde_json::to_string(&snap) {
        Ok(json) => {
            if let Err(e) = std::fs::write(SNAPSHOT_PATH, json) {
                eprintln!("[SNAPSHOT] Failed to write {}: {}", SNAPSHOT_PATH, e);
            } else {
                println!("[SNAPSHOT] Saved to {}", SNAPSHOT_PATH);
            }
        }
        Err(e) => eprintln!("[SNAPSHOT] Serialization failed: {}", e),
    }
}

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

    println!("[V2] Fetching markets from {} DEX factories...", factory_addresses.len());
    let flash_query_address = QUERY_CONTRACT.parse::<Address>().unwrap();
    let flash_query_contract = UniQuery::new(flash_query_address, config.http.clone());

    let refresh = std::env::var("REFRESH_CACHE").as_deref() == Ok("true");
    let snapshot = if !refresh { load_snapshot() } else { None };

    let (snapshot_block, grouped_pairs, v3_pools) = if let Some(snap) = snapshot {
        println!("[SNAPSHOT] Loaded from {} (block {}) — skipping chain fetch. Set REFRESH_CACHE=true to re-fetch.",
            SNAPSHOT_PATH, snap.snapshot_block);
        (snap.snapshot_block, snap.grouped_pairs, snap.v3_pools)
    } else {
        let gp = get_markets_by_token(factory_addresses, &flash_query_contract, config.http.clone()).await;
        println!("[V2] Done. {} unique tokens with multi-pool WETH markets.", gp.len());

        let token_limit = std::env::var("TOKEN_LIMIT").ok().and_then(|v| v.parse::<usize>().ok());
        let mut token_list: Vec<H160> = gp.iter().map(|(t, _)| *t).collect();
        if let Some(limit) = token_limit {
            token_list.truncate(limit);
            println!("[V3] TOKEN_LIMIT={} — capping token scan for test run.", limit);
        }
        println!("[V3] Fetching V3 pools for {} tokens (3 fee tiers each)...", token_list.len());
        let v3_factory = UNISWAP_V3_FACTORY.parse::<Address>().unwrap();
        let pools = fetch_v3_pools_for_tokens(
            &token_list,
            WETH_ADDRESS.parse().unwrap(),
            v3_factory,
            config.http.clone(),
        )
        .await;
        println!("[V3] Done. {} V3 pools found.", pools.len());

        let current_block = config.http.get_block_number().await
            .map(|b| b.as_u64())
            .unwrap_or(0);
        save_snapshot(current_block, &gp, &pools);
        (current_block, gp, pools)
    };

    println!("[SETUP] Building crossed pair manager...");
    let mut crossed_pair = CrossedPairManager::new(&grouped_pairs, &flash_query_contract, v3_pools);
    crossed_pair.write_tokens();

    // Parse optional backtest range: BACKTEST_BLOCKS=21900000:21900100
    let backtest = std::env::var("BACKTEST_BLOCKS").ok().and_then(|v| {
        let mut parts = v.splitn(2, ':');
        let start = parts.next()?.parse::<u64>().ok()?;
        let end   = parts.next()?.parse::<u64>().ok()?;
        Some((start, end))
    });

    // Warn if backtest range is far from the snapshot block (ticks may be inaccurate).
    const MAX_SAFE_BLOCK_DRIFT: u64 = 50_000;
    if let Some((start, end)) = backtest {
        let drift = snapshot_block.saturating_sub(start).max(end.saturating_sub(snapshot_block));
        if drift > MAX_SAFE_BLOCK_DRIFT {
            println!(
                "[BACKTEST] WARNING: backtest range ({}-{}) is {} blocks from snapshot block {}.",
                start, end, drift, snapshot_block
            );
            println!("[BACKTEST] WARNING: V3 tick data may be inaccurate. Re-fetch snapshot closer to this range with REFRESH_CACHE=true.");
        }
        println!("[BACKTEST] Replaying blocks {} → {} (snapshot block: {})", start, end, snapshot_block);
        for block_num in start..=end {
            println!("[BACKTEST] Block #{} — fetching reserves + V3 spot prices...", block_num);
            crossed_pair.update_reserve(Some(block_num)).await;
            crossed_pair.update_v3_spot_prices(block_num).await;
            let gas_price = U256::from(config.http.get_gas_price().await.unwrap_or_default());
            crossed_pair.find_arbitrage_opportunities(config.max_bal, gas_price);
        }
        println!("[BACKTEST] Done.");
    } else {
        // Live mode: initial fetch then subscribe to new blocks.
        println!("[SETUP] Fetching initial reserves...");
        crossed_pair.update_reserve(None).await;
        println!("[SETUP] Running initial arbitrage scan...");
        let gas_price = U256::from(config.http.get_gas_price().await.unwrap_or_default());
        crossed_pair.find_arbitrage_opportunities(config.max_bal, gas_price);

        println!("[WATCHING] Subscribed to new blocks.");
        let mut stream = config.wss.subscribe_blocks().await?;
        let mut block_count = 0u64;
        while let Some(block) = stream.next().await {
            block_count += 1;
            let block_num = block.number.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
            println!("[BLOCK {}] #{} — updating reserves...", block_count, block_num);
            crossed_pair.update_reserve(None).await;
            println!("[BLOCK {}] #{} — scanning for arbitrage...", block_count, block_num);
            let gas_price = U256::from(config.http.get_gas_price().await.unwrap_or_default());
            crossed_pair.find_arbitrage_opportunities(config.max_bal, gas_price);
        }
    }

    Ok(())
}
