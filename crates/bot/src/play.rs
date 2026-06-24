//! Gameplay client: connect over the WebSocket as a player, receive game
//! states, and reply with moves. Used by the provider for each assigned seat.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use herdcore_core::bot;
use herdcore_protocol::v1::{client_frame, server_frame};
use herdcore_protocol::{decode_frame, encode_frame, v1};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// Play one seat until removed from the lobby or the session is rejected.
/// Reconnects on transient drops, gives up after repeated failures.
pub async fn run_bot(
    ws_url: &str,
    lobby_id: &str,
    player_id: &str,
    token: &str,
    bot_type_id: &str,
) {
    let mut failures = 0u32;
    loop {
        match play_once(ws_url, lobby_id, player_id, token, bot_type_id).await {
            Ok(Outcome::Stop) => return,
            Ok(Outcome::Reconnect) => failures = 0,
            Err(_) => {
                failures += 1;
                if failures >= 5 {
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

enum Outcome {
    Stop,
    Reconnect,
}

async fn play_once(
    ws_url: &str,
    lobby_id: &str,
    player_id: &str,
    token: &str,
    bot_type_id: &str,
) -> Result<Outcome> {
    let (mut socket, _) = connect_async(ws_url).await?;
    socket
        .send(Message::Binary(encode_frame(&resume(
            lobby_id, player_id, token,
        ))))
        .await?;

    // One move per (game, turn); the snapshot in each frame is enough to decide.
    let mut acted: Option<(u64, u64)> = None;

    while let Some(message) = socket.next().await {
        let bytes = match message? {
            Message::Binary(bytes) => bytes,
            Message::Close(_) => return Ok(Outcome::Reconnect),
            _ => continue,
        };
        let Ok(frame) = decode_frame::<v1::ServerFrame>(&bytes) else {
            continue;
        };
        let lobby = match frame.body {
            Some(server_frame::Body::Welcome(welcome)) => welcome.lobby,
            Some(server_frame::Body::Update(update)) => update.lobby,
            Some(server_frame::Body::Error(error)) if error.fatal => return Ok(Outcome::Stop),
            Some(server_frame::Body::Bye(_)) => return Ok(Outcome::Stop),
            _ => continue,
        };
        let Some(lobby) = lobby else {
            continue;
        };
        // Removed from the roster.
        let Some(player) = lobby.players.iter().find(|p| p.player_id == player_id) else {
            return Ok(Outcome::Stop);
        };
        if lobby.phase != v1::LobbyPhase::Playing as i32 {
            continue;
        }
        let (Some(seat), Some(game_proto)) = (
            player.seat.and_then(|seat| u8::try_from(seat).ok()),
            lobby.game.as_ref(),
        ) else {
            continue;
        };
        let Ok(game) = herdcore_protocol::game_from_proto(game_proto) else {
            continue;
        };
        let turn_key = (lobby.game_id, game.turn);
        if acted == Some(turn_key) {
            continue;
        }
        acted = Some(turn_key);

        // Decide within the turn: spend most of the time left before the
        // deadline, leaving headroom for the round trip. Without this a deep
        // search can overrun a short turn and the move is never counted.
        let budget = move_budget(lobby.deadline_unix_ms);
        let mut action = bot::choose_action_for_within(&game, seat, bot_type_id, budget);
        // Keep bots lively: never sit still when a real move is available. If the
        // strategy decides to stay, jump in a random legal direction instead.
        if action == herdcore_core::Action::Stay {
            let mut moves: Vec<_> = herdcore_core::legal_actions(&game, seat)
                .into_iter()
                .filter(|candidate| *candidate != herdcore_core::Action::Stay)
                .collect();
            if !moves.is_empty() {
                action = moves.swap_remove(random_index(moves.len()));
            }
        }
        tracing::trace!(
            player = %player_id,
            lobby = %lobby_id,
            game_id = lobby.game_id,
            turn = game.turn,
            seat,
            ?action,
            "submitting move"
        );
        // Submitting can lose a race with the deadline; ignore and wait for next.
        let _ = socket
            .send(Message::Binary(encode_frame(&make_move(
                &lobby, &game, action,
            ))))
            .await;
    }
    Ok(Outcome::Reconnect)
}

/// A cheap, dependency-free random index in `0..len`, seeded from the clock.
/// Good enough for picking a fallback direction; not security-sensitive.
fn random_index(len: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos as usize % len
}

/// Time to spend choosing a move: 60% of the time left until the deadline,
/// clamped so we always think a little but never risk missing the turn.
fn move_budget(deadline_unix_ms: i64) -> std::time::Duration {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let remaining = (deadline_unix_ms - now_ms).max(0);
    let budget = (remaining * 6 / 10).clamp(50, 5_000);
    std::time::Duration::from_millis(budget as u64)
}

fn resume(lobby_id: &str, player_id: &str, token: &str) -> v1::ClientFrame {
    v1::ClientFrame {
        body: Some(client_frame::Body::Resume(v1::Resume {
            lobby_id: lobby_id.to_owned(),
            player_id: player_id.to_owned(),
            session_token: token.to_owned(),
            after_version: 0,
        })),
    }
}

fn make_move(
    lobby: &v1::LobbySnapshot,
    game: &herdcore_core::GameState,
    action: herdcore_core::Action,
) -> v1::ClientFrame {
    v1::ClientFrame {
        body: Some(client_frame::Body::Move(v1::MoveCommand {
            game_id: lobby.game_id,
            turn: game.turn,
            action: herdcore_protocol::action_to_proto(action) as i32,
            request_id: uuid::Uuid::new_v4().to_string(),
        })),
    }
}
