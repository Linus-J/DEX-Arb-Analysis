use ethers::prelude::*;

pub const UNISWAP_ROUTER: &str = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";
pub const UNISWAP_FACTORY: &str = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f";

pub const SUSHISWAP_ROUTER: &str = "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F";
pub const SUSHISWAP_FACTORY: &str = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac";

pub const PANCAKESWAP_ROUTER: &str = "0xEfF92A263d31888d860bD50809A8D171709b7b1c";
pub const PANCAKESWAP_FACTORY: &str = "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362";

pub const QUERY_CONTRACT: &str = "0x5EF1009b9FCD4fec3094a5564047e190D72Bd511";

pub const WETH_ADDRESS: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

pub const UNISWAP_V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

// Minimum WETH liquidity a pool must hold to be considered (1 ETH).
pub const MIN_WETH_LIQUIDITY: U256 = U256([1_000_000_000_000_000_000u64, 0, 0, 0]);

// Minimum profit (after gas) to report an opportunity (0.01 ETH).
pub const MIN_PROFIT_WETH: U256 = U256([10_000_000_000_000_000u64, 0, 0, 0]);

abigen!(UniV2Router, "src/abi/UniV2Router.json");
abigen!(UniV2Factory, "src/abi/UniV2Factory.json");
abigen!(UniV2Pair, "src/abi/UniV2Pair.json");
abigen!(UniQuery, "src/abi/UniQuery.json");
abigen!(UniV3Factory, "src/abi/UniV3Factory.json");
abigen!(UniV3Pool, "src/abi/UniV3Pool.json");
