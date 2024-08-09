pub struct Config {
    pub http: Arc<Provider<Http>>,  // No more signer middleware
    pub wss: Arc<Provider<Ws>>,
}

impl Config {
    pub async fn new() -> Self {
        dotenv::dotenv().ok();
        let network = std::env::var("NETWORK_RPC").expect("missing NETWORK_RPC");
        let provider: Provider<Http> = Provider::<Http>::try_from(network).unwrap();
        
        let ws_network = std::env::var("NETWORK_WSS").expect("missing NETWORK_WSS");
        let ws_provider: Provider<Ws> = Provider::<Ws>::connect(ws_network).await.unwrap();

        Self {
            http: Arc::new(provider),
            wss: Arc::new(ws_provider),
        }
    }
}