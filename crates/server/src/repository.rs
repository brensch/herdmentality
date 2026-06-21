use std::sync::Arc;

use anyhow::{Context, Result};
use herdcore_protocol::{action_from_proto, action_to_proto, v1};
use prost::Message;
use tokio::sync::Mutex;
use turso::{params, Builder, Connection};

use crate::model::{InternalPlayer, LobbyState, PendingMove};

#[derive(Clone)]
pub struct Repository {
    connection: Arc<Mutex<Connection>>,
}

impl Repository {
    pub async fn open(path: &str) -> Result<Self> {
        let database = Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        for pragma in [
            "PRAGMA journal_mode = WAL",
            "PRAGMA synchronous = FULL",
            "PRAGMA foreign_keys = ON",
            "PRAGMA busy_timeout = 5000",
        ] {
            let mut rows = connection.query(pragma, ()).await?;
            while rows.next().await?.is_some() {}
        }
        let repository = Self {
            connection: Arc::new(Mutex::new(connection)),
        };
        repository.migrate().await?;
        Ok(repository)
    }

    async fn migrate(&self) -> Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS lobbies (
                    lobby_id TEXT PRIMARY KEY,
                    lobby_code TEXT NOT NULL UNIQUE,
                    phase INTEGER NOT NULL,
                    public_version INTEGER NOT NULL,
                    game_id INTEGER NOT NULL,
                    deadline_unix_ms INTEGER NOT NULL,
                    snapshot BLOB NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS players (
                    player_id TEXT PRIMARY KEY,
                    lobby_id TEXT NOT NULL,
                    display_name TEXT NOT NULL,
                    kind INTEGER NOT NULL,
                    seat INTEGER,
                    connected INTEGER NOT NULL,
                    session_token TEXT NOT NULL,
                    bot_type_id TEXT,
                    FOREIGN KEY(lobby_id) REFERENCES lobbies(lobby_id)
                 );
                 CREATE INDEX IF NOT EXISTS players_lobby_idx ON players(lobby_id);
                 CREATE TABLE IF NOT EXISTS move_submissions (
                    lobby_id TEXT NOT NULL,
                    game_id INTEGER NOT NULL,
                    turn INTEGER NOT NULL,
                    player_id TEXT NOT NULL,
                    action INTEGER NOT NULL,
                    request_id TEXT NOT NULL,
                    received_at_ms INTEGER NOT NULL,
                    PRIMARY KEY(lobby_id, game_id, turn, player_id),
                    UNIQUE(lobby_id, request_id)
                 );
                 CREATE TABLE IF NOT EXISTS turn_results (
                    lobby_id TEXT NOT NULL,
                    game_id INTEGER NOT NULL,
                    turn INTEGER NOT NULL,
                    resolved_at_ms INTEGER NOT NULL,
                    event BLOB NOT NULL,
                    PRIMARY KEY(lobby_id, game_id, turn)
                 );
                 CREATE TABLE IF NOT EXISTS outbox_events (
                    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    lobby_id TEXT NOT NULL,
                    public_version INTEGER NOT NULL,
                    event BLOB NOT NULL,
                    published INTEGER NOT NULL DEFAULT 0
                 );",
            )
            .await?;
        let mut columns = connection.query("PRAGMA table_info(players)", ()).await?;
        let mut has_bot_type = false;
        while let Some(row) = columns.next().await? {
            let name: String = row.get(1)?;
            has_bot_type |= name == "bot_type_id";
        }
        if !has_bot_type {
            connection
                .execute("ALTER TABLE players ADD COLUMN bot_type_id TEXT", ())
                .await?;
        }
        Ok(())
    }

    pub async fn create_lobby(&self, state: &LobbyState) -> Result<()> {
        let snapshot = state.snapshot();
        let bytes = snapshot.encode_to_vec();
        let mut connection = self.connection.lock().await;
        let transaction = connection.transaction().await?;
        transaction
            .execute(
                "INSERT INTO lobbies
                 (lobby_id, lobby_code, phase, public_version, game_id, deadline_unix_ms, snapshot)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    state.lobby_id.clone(),
                    state.lobby_code.clone(),
                    state.phase as i64,
                    state.public_version as i64,
                    state.game_id as i64,
                    state.deadline_unix_ms,
                    bytes,
                ],
            )
            .await?;
        for player in &state.players {
            insert_player(&transaction, &state.lobby_id, player).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn persist_snapshot(&self, state: &LobbyState) -> Result<()> {
        let snapshot = state.snapshot();
        let bytes = snapshot.encode_to_vec();
        let mut connection = self.connection.lock().await;
        let transaction = connection.transaction().await?;
        transaction
            .execute(
                "UPDATE lobbies SET phase = ?, public_version = ?, game_id = ?,
                 deadline_unix_ms = ?, snapshot = ? WHERE lobby_id = ?",
                params![
                    state.phase as i64,
                    state.public_version as i64,
                    state.game_id as i64,
                    state.deadline_unix_ms,
                    bytes,
                    state.lobby_id.clone(),
                ],
            )
            .await?;
        // The lobby snapshot owns the complete player set. Replacing these
        // rows makes failed bot admission rollback durable as one transaction.
        transaction
            .execute(
                "DELETE FROM players WHERE lobby_id = ?",
                params![state.lobby_id.clone()],
            )
            .await?;
        for player in &state.players {
            transaction
                .execute(
                    "INSERT INTO players
                    (player_id, lobby_id, display_name, kind, seat, connected, session_token, bot_type_id)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(player_id) DO UPDATE SET
                     display_name=excluded.display_name, kind=excluded.kind, seat=excluded.seat,
                     connected=excluded.connected, session_token=excluded.session_token,
                     bot_type_id=excluded.bot_type_id",
                    params![
                        player.player_id.clone(),
                        state.lobby_id.clone(),
                        player.display_name.clone(),
                        player.kind as i64,
                        player.seat.map(i64::from),
                        i64::from(player.connected),
                        player.session_token.clone(),
                        player.bot_type_id.clone(),
                    ],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn persist_move(
        &self,
        lobby_id: &str,
        game_id: u64,
        turn: u64,
        player_id: &str,
        pending: &PendingMove,
    ) -> Result<bool> {
        let connection = self.connection.lock().await;
        let affected = connection
            .execute(
                "INSERT OR IGNORE INTO move_submissions
                 (lobby_id, game_id, turn, player_id, action, request_id, received_at_ms)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    lobby_id.to_owned(),
                    game_id as i64,
                    i64::try_from(turn)?,
                    player_id.to_owned(),
                    action_to_proto(pending.action) as i64,
                    pending.request_id.clone(),
                    pending.received_at_ms,
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    pub async fn persist_resolution(
        &self,
        state: &LobbyState,
        resolved_turn: u64,
        event: &v1::LobbyEvent,
        resolved_at_ms: i64,
    ) -> Result<()> {
        let snapshot_bytes = state.snapshot().encode_to_vec();
        let event_bytes = event.encode_to_vec();
        let mut connection = self.connection.lock().await;
        let transaction = connection.transaction().await?;
        transaction
            .execute(
                "UPDATE lobbies SET phase = ?, public_version = ?, game_id = ?,
                 deadline_unix_ms = ?, snapshot = ? WHERE lobby_id = ?",
                params![
                    state.phase as i64,
                    state.public_version as i64,
                    state.game_id as i64,
                    state.deadline_unix_ms,
                    snapshot_bytes,
                    state.lobby_id.clone(),
                ],
            )
            .await?;
        transaction
            .execute(
                "INSERT INTO turn_results
                 (lobby_id, game_id, turn, resolved_at_ms, event) VALUES (?, ?, ?, ?, ?)",
                params![
                    state.lobby_id.clone(),
                    state.game_id as i64,
                    i64::try_from(resolved_turn)?,
                    resolved_at_ms,
                    event_bytes.clone(),
                ],
            )
            .await?;
        transaction
            .execute(
                "INSERT INTO outbox_events (lobby_id, public_version, event, published)
                 VALUES (?, ?, ?, 0)",
                params![
                    state.lobby_id.clone(),
                    state.public_version as i64,
                    event_bytes,
                ],
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn mark_event_published(&self, lobby_id: &str, version: u64) -> Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "UPDATE outbox_events SET published = 1
                 WHERE lobby_id = ? AND public_version = ?",
                params![lobby_id.to_owned(), version as i64],
            )
            .await?;
        Ok(())
    }

    pub async fn load_lobbies(&self) -> Result<Vec<LobbyState>> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query("SELECT snapshot FROM lobbies ORDER BY lobby_id", ())
            .await?;
        let mut states = Vec::new();
        while let Some(row) = rows.next().await? {
            let bytes: Vec<u8> = row.get(0)?;
            let snapshot = v1::LobbySnapshot::decode(bytes.as_slice())?;
            let mut player_rows = connection
                .query(
                    "SELECT player_id, display_name, kind, seat, connected, session_token, bot_type_id
                     FROM players WHERE lobby_id = ? ORDER BY rowid",
                    params![snapshot.lobby_id.clone()],
                )
                .await?;
            let mut players = Vec::new();
            while let Some(player) = player_rows.next().await? {
                let seat_value = player.get_value(3)?;
                let seat = match seat_value {
                    turso::Value::Null => None,
                    turso::Value::Integer(value) => Some(u8::try_from(value)?),
                    _ => anyhow::bail!("invalid seat value"),
                };
                players.push(InternalPlayer {
                    player_id: player.get(0)?,
                    display_name: player.get(1)?,
                    kind: v1::PlayerKind::try_from(player.get::<i64>(2)? as i32)
                        .unwrap_or(v1::PlayerKind::Unspecified),
                    seat,
                    connected: player.get::<i64>(4)? != 0,
                    session_token: player.get(5)?,
                    bot_type_id: match player.get_value(6)? {
                        turso::Value::Null => None,
                        turso::Value::Text(value) => Some(value),
                        _ => anyhow::bail!("invalid bot type value"),
                    },
                });
            }
            let game = snapshot
                .game
                .as_ref()
                .map(herdcore_protocol::game_from_proto)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let mut state = LobbyState {
                lobby_id: snapshot.lobby_id,
                lobby_code: snapshot.lobby_code,
                phase: v1::LobbyPhase::try_from(snapshot.phase).unwrap_or(v1::LobbyPhase::Waiting),
                host_player_id: snapshot.host_player_id,
                max_players: u8::try_from(snapshot.max_players)?,
                turn_seconds: u16::try_from(snapshot.turn_seconds)?,
                public_version: snapshot.public_version,
                game_id: snapshot.game_id,
                deadline_unix_ms: snapshot.deadline_unix_ms,
                players,
                game,
                pending: Default::default(),
            };
            if state.phase == v1::LobbyPhase::Playing {
                if let Some(game) = &state.game {
                    let mut pending_rows = connection
                        .query(
                            "SELECT player_id, action, request_id, received_at_ms
                             FROM move_submissions WHERE lobby_id = ? AND game_id = ? AND turn = ?",
                            params![
                                state.lobby_id.clone(),
                                state.game_id as i64,
                                i64::try_from(game.turn)?,
                            ],
                        )
                        .await?;
                    while let Some(pending) = pending_rows.next().await? {
                        let action_proto = v1::Action::try_from(pending.get::<i64>(1)? as i32)
                            .unwrap_or(v1::Action::Unspecified);
                        let action = action_from_proto(action_proto).context("stored action")?;
                        state.pending.insert(
                            pending.get(0)?,
                            PendingMove {
                                action,
                                request_id: pending.get(2)?,
                                received_at_ms: pending.get(3)?,
                            },
                        );
                    }
                }
            }
            states.push(state);
        }
        Ok(states)
    }
}

async fn insert_player(
    transaction: &turso::transaction::Transaction<'_>,
    lobby_id: &str,
    player: &InternalPlayer,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO players
             (player_id, lobby_id, display_name, kind, seat, connected, session_token, bot_type_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                player.player_id.clone(),
                lobby_id.to_owned(),
                player.display_name.clone(),
                player.kind as i64,
                player.seat.map(i64::from),
                i64::from(player.connected),
                player.session_token.clone(),
                player.bot_type_id.clone(),
            ],
        )
        .await?;
    Ok(())
}
