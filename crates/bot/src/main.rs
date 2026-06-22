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
    let bot_type = env::var("HERDCORE_BOT_TYPE").unwrap_or_else(|_| "greedy-v1".to_owned());
    tracing::info!(%server_url, %bot_type, "herdcore-bot service starting");
    herdcore_bot::provider::run_service(&server_url, &bot_type).await;
}
