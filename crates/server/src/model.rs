use std::collections::BTreeMap;

use herdcore_core::{Action, GameState};
use herdcore_protocol::{game_to_proto, v1};

#[derive(Clone, Debug)]
pub struct InternalPlayer {
    pub player_id: String,
    pub display_name: String,
    pub kind: v1::PlayerKind,
    pub seat: Option<u8>,
    pub connected: bool,
    pub session_token: String,
    pub bot_type_id: Option<String>,
}

/// A row of a lobby's game history, surfaced to the lobby page.
#[derive(Clone, Debug)]
pub struct GameRecord {
    pub game_id: u64,
    pub status: i32,
    pub winners: Vec<u32>,
    pub ended_unix_ms: i64,
}

#[derive(Clone, Debug)]
pub struct PendingMove {
    pub action: Action,
    pub request_id: String,
    pub received_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct LobbyState {
    pub lobby_id: String,
    pub lobby_code: String,
    pub phase: v1::LobbyPhase,
    pub host_player_id: String,
    pub max_players: u8,
    pub turn_seconds: u16,
    pub public_version: u64,
    pub game_id: u64,
    pub deadline_unix_ms: i64,
    pub players: Vec<InternalPlayer>,
    pub game: Option<GameState>,
    pub pending: BTreeMap<String, PendingMove>,
}

impl LobbyState {
    pub fn snapshot(&self) -> v1::LobbySnapshot {
        v1::LobbySnapshot {
            lobby_id: self.lobby_id.clone(),
            lobby_code: self.lobby_code.clone(),
            phase: self.phase as i32,
            host_player_id: self.host_player_id.clone(),
            max_players: u32::from(self.max_players),
            turn_seconds: u32::from(self.turn_seconds),
            public_version: self.public_version,
            game_id: self.game_id,
            deadline_unix_ms: self.deadline_unix_ms,
            players: self
                .players
                .iter()
                .map(|player| v1::LobbyPlayer {
                    player_id: player.player_id.clone(),
                    display_name: player.display_name.clone(),
                    kind: player.kind as i32,
                    seat: player.seat.map(u32::from),
                    connected: player.connected,
                    bot_type_id: player.bot_type_id.clone().unwrap_or_default(),
                })
                .collect(),
            game: self.game.as_ref().map(game_to_proto),
        }
    }

    pub fn authenticate(&self, player_id: &str, token: &str) -> Option<&InternalPlayer> {
        self.players
            .iter()
            .find(|player| player.player_id == player_id && player.session_token == token)
    }
}
