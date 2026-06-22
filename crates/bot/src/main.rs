//! The Herdcore bot service: an independent process that plays CPU seats for a
//! game server entirely over WebSockets.

use std::env;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Defaults to showing the bot's per-move TRACE logs; override with RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("herdcore_bot=trace,info")),
        )
        .init();

    let server_url =
        env::var("HERDCORE_WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:55051/ws".to_owned());
    let bot_types = env::var("HERDCORE_BOT_TYPES")
        .or_else(|_| env::var("HERDCORE_BOT_TYPE"))
        .unwrap_or_else(|_| "greedy-v1,lookahead-v1".to_owned());
    let bot_types: Vec<_> = bot_types
        .split(',')
        .map(str::trim)
        .filter(|bot_type| !bot_type.is_empty())
        .collect();
    tracing::info!(%server_url, ?bot_types, "herdcore-bot service starting");
    futures_util::future::join_all(
        bot_types
            .iter()
            .map(|bot_type| herdcore_bot::provider::run_service(&server_url, bot_type)),
    )
    .await;
}
