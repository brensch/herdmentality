//! Provider service: a standalone process that registers with the game server
//! over a WebSocket and, for each Assign it receives, opens a separate gameplay
//! connection to play that seat. This is the proof that the WebSocket protocol
//! carries everything between independent processes — control and gameplay.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use herdcore_protocol::v1::{client_frame, server_frame};
use herdcore_protocol::{decode_frame, encode_frame, v1};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::play::run_bot;

/// Stay connected to the game server forever, reconnecting on drops. `bot_type`
/// is the catalogue id this service plays (e.g. "greedy-v1").
pub async fn run_service(server_url: &str, bot_type: &str) {
    loop {
        if let Err(error) = serve(server_url, bot_type).await {
            tracing::warn!(%error, "provider connection lost; reconnecting");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn serve(server_url: &str, bot_type: &str) -> Result<()> {
    let (mut socket, _) = connect_async(server_url).await?;
    socket
        .send(Message::Binary(encode_frame(&register(bot_type))))
        .await?;
    tracing::info!(%server_url, "registered as bot provider");

    // Seats currently being played, so a re-sent Assign doesn't double up.
    let active: Arc<Mutex<HashSet<String>>> = Arc::default();

    while let Some(message) = socket.next().await {
        let bytes = match message? {
            Message::Binary(bytes) => bytes,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(frame) = decode_frame::<v1::ServerFrame>(&bytes) else {
            continue;
        };
        if let Some(server_frame::Body::Assign(assign)) = frame.body {
            if active.lock().await.insert(assign.player_id.clone()) {
                tracing::info!(player = %assign.player_id, lobby = %assign.lobby_id, "playing seat");
                let active = active.clone();
                tokio::spawn(async move {
                    run_bot(
                        &assign.game_server_url,
                        &assign.lobby_id,
                        &assign.player_id,
                        &assign.session_token,
                        &assign.bot_type_id,
                    )
                    .await;
                    active.lock().await.remove(&assign.player_id);
                });
            }
        }
    }
    Ok(())
}

fn register(bot_type: &str) -> v1::ClientFrame {
    v1::ClientFrame {
        body: Some(client_frame::Body::RegisterProvider(v1::RegisterProvider {
            bot_type_id: bot_type.to_owned(),
        })),
    }
}
