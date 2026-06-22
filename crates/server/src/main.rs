mod app;
mod model;
mod repository;
mod ws;

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::get;
use axum::Router;
use herdcore_protocol::v1;
use repository::Repository;
use tower_http::cors::CorsLayer;
use ws::{ws_handler, ProviderRegistry, WsState};

/// The bots offered to players. One for now: "Greedy Greg". The `address` is
/// informational; a bot service serving this `id` plays the seats.
fn bot_catalogue() -> Vec<v1::BotKind> {
    vec![v1::BotKind {
        id: "greedy-v1".to_owned(),
        name: "Greedy Greg".to_owned(),
        address: env::var("HERDCORE_GREEDY_ADDR").unwrap_or_else(|_| "127.0.0.1".to_owned()),
    }]
}

#[tokio::main]
async fn main() -> Result<()> {
    let address: SocketAddr = env::var("HERDCORE_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:55051".to_owned())
        .parse()?;
    // URL the bot service dials to play CPU seats; defaults to this server.
    let ws_url =
        env::var("HERDCORE_PUBLIC_WS_URL").unwrap_or_else(|_| format!("ws://{address}/ws"));
    let database_path = env::var("HERDCORE_DB").unwrap_or_else(|_| "target/herdcore.db".to_owned());

    let repository = Repository::open(&database_path).await?;
    let app = app::App::load(repository).await?;
    // Recovered lobbies' CPU seats are (re)assigned when a bot service connects.
    let state = WsState {
        app,
        ws_url,
        catalogue: Arc::new(bot_catalogue()),
        providers: ProviderRegistry::default(),
    };

    let router = Router::new()
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("Herdcore server listening on {address} (WebSocket at /ws)");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            println!("Shutting down Herdcore server…");
        })
        .await?;
    Ok(())
}
