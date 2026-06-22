use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use herdcore_core::{initial_state_for_players, is_action_legal, step, Action};
use herdcore_protocol::{action_to_proto, v1};
use tokio::sync::{broadcast, Mutex, RwLock};
use uuid::Uuid;

use crate::model::{InternalPlayer, LobbyState, PendingMove};
use crate::repository::Repository;

/// Word-keyed lobbies are created with these defaults; the start screen does not
/// expose capacity or turn length.
const DEFAULT_MAX_PLAYERS: u8 = herdcore_core::MAX_PLAYERS;
const DEFAULT_TURN_SECONDS: u16 = 20;

pub struct Lobby {
    pub state: Mutex<LobbyState>,
    pub events: broadcast::Sender<v1::LobbyEvent>,
}

#[derive(Clone)]
pub struct App {
    repository: Repository,
    lobbies: Arc<RwLock<HashMap<String, Arc<Lobby>>>>,
    codes: Arc<RwLock<HashMap<String, String>>>,
}

pub struct BotAssignment {
    pub snapshot: v1::LobbySnapshot,
    pub lobby_id: String,
    pub player_id: String,
    pub session_token: String,
    pub display_name: String,
    pub bot_type_id: String,
}

impl App {
    pub async fn load(repository: Repository) -> Result<Arc<Self>> {
        let app = Arc::new(Self {
            repository,
            lobbies: Arc::new(RwLock::new(HashMap::new())),
            codes: Arc::new(RwLock::new(HashMap::new())),
        });
        let recovered = app.repository.load_lobbies().await?;
        for state in recovered {
            let id = state.lobby_id.clone();
            let code = state.lobby_code.clone();
            let game_id = state.game_id;
            let turn = state.game.as_ref().map(|game| game.turn);
            let deadline = state.deadline_unix_ms;
            let (events, _) = broadcast::channel(128);
            let lobby = Arc::new(Lobby {
                state: Mutex::new(state),
                events,
            });
            app.codes.write().await.insert(code, id.clone());
            app.lobbies.write().await.insert(id, Arc::clone(&lobby));
            if let Some(turn) = turn {
                app.schedule_deadline(lobby, game_id, turn, deadline);
            }
        }
        Ok(app)
    }

    pub async fn create_lobby(
        self: &Arc<Self>,
        display_name: String,
        max_players: u8,
        turn_seconds: u16,
    ) -> Result<(InternalPlayer, v1::LobbySnapshot)> {
        validate_name(&display_name)?;
        if !(herdcore_core::MIN_PLAYERS..=herdcore_core::MAX_PLAYERS).contains(&max_players) {
            bail!(
                "max players must be between {} and {}",
                herdcore_core::MIN_PLAYERS,
                herdcore_core::MAX_PLAYERS
            );
        }
        if !(3..=300).contains(&turn_seconds) {
            bail!("turn duration must be between 3 and 300 seconds");
        }

        let lobby_id = Uuid::new_v4().to_string();
        let lobby_code = self.unique_lobby_code().await;
        let host = new_player(display_name, v1::PlayerKind::Human, None);
        let state = LobbyState {
            lobby_id: lobby_id.clone(),
            lobby_code: lobby_code.clone(),
            phase: v1::LobbyPhase::Waiting,
            host_player_id: host.player_id.clone(),
            max_players,
            turn_seconds,
            public_version: 1,
            game_id: 0,
            deadline_unix_ms: 0,
            players: vec![host.clone()],
            game: None,
            pending: BTreeMap::new(),
        };
        self.repository.create_lobby(&state).await?;
        let snapshot = state.snapshot();
        let (events, _) = broadcast::channel(128);
        let lobby = Arc::new(Lobby {
            state: Mutex::new(state),
            events,
        });
        self.codes
            .write()
            .await
            .insert(lobby_code, lobby_id.clone());
        self.lobbies.write().await.insert(lobby_id, lobby);
        Ok((host, snapshot))
    }

    pub async fn join_lobby(
        &self,
        lobby_code: &str,
        display_name: String,
    ) -> Result<(InternalPlayer, v1::LobbySnapshot)> {
        validate_name(&display_name)?;
        let code = lobby_code.trim().to_ascii_uppercase();
        let lobby_id = self
            .codes
            .read()
            .await
            .get(&code)
            .cloned()
            .context("lobby not found")?;
        self.join_existing(&lobby_id, display_name).await
    }

    /// Join the lobby that owns `lobby_name`, creating it if no lobby uses that
    /// word yet. Lobbies are keyed by their normalized word so the same word
    /// always lands everyone in the same persistent room.
    pub async fn join_or_create_lobby(
        self: &Arc<Self>,
        lobby_name: &str,
        display_name: String,
    ) -> Result<(InternalPlayer, v1::LobbySnapshot)> {
        validate_name(&display_name)?;
        let slug = normalize_lobby_name(lobby_name)?;

        if let Some(lobby_id) = self.codes.read().await.get(&slug).cloned() {
            return self.join_existing(&lobby_id, display_name).await;
        }

        // Hold the code map for the whole create path so two players racing on
        // the same fresh word cannot both create a lobby for it.
        let mut codes = self.codes.write().await;
        if let Some(lobby_id) = codes.get(&slug).cloned() {
            drop(codes);
            return self.join_existing(&lobby_id, display_name).await;
        }

        let lobby_id = Uuid::new_v4().to_string();
        let host = new_player(display_name, v1::PlayerKind::Human, None);
        let state = LobbyState {
            lobby_id: lobby_id.clone(),
            lobby_code: slug.clone(),
            phase: v1::LobbyPhase::Waiting,
            host_player_id: host.player_id.clone(),
            max_players: DEFAULT_MAX_PLAYERS,
            turn_seconds: DEFAULT_TURN_SECONDS,
            public_version: 1,
            game_id: 0,
            deadline_unix_ms: 0,
            players: vec![host.clone()],
            game: None,
            pending: BTreeMap::new(),
        };
        self.repository.create_lobby(&state).await?;
        let snapshot = state.snapshot();
        let (events, _) = broadcast::channel(128);
        let lobby = Arc::new(Lobby {
            state: Mutex::new(state),
            events,
        });
        codes.insert(slug, lobby_id.clone());
        self.lobbies.write().await.insert(lobby_id, lobby);
        Ok((host, snapshot))
    }

    /// Add a human to an existing lobby in any phase. A player who joins while a
    /// game is running is seated only when the next game starts, so until then
    /// they spectate the live board.
    async fn join_existing(
        &self,
        lobby_id: &str,
        display_name: String,
    ) -> Result<(InternalPlayer, v1::LobbySnapshot)> {
        let lobby = self.lobby(lobby_id).await?;
        let mut state = lobby.state.lock().await;
        if state.players.len() >= usize::from(state.max_players) {
            bail!("lobby is full");
        }
        let player = new_player(display_name, v1::PlayerKind::Human, None);
        // A finished round has no active host responsibilities, so hand the room
        // to whoever shows up next; this keeps abandoned lobbies startable.
        if state.phase == v1::LobbyPhase::Finished {
            state.host_player_id = player.player_id.clone();
        }
        state.players.push(player.clone());
        state.public_version += 1;
        self.repository.persist_snapshot(&state).await?;
        let snapshot = state.snapshot();
        send_event(
            &lobby,
            v1::LobbyEventKind::LobbyUpdated,
            snapshot.clone(),
            Vec::new(),
        );
        Ok((player, snapshot))
    }

    /// Remove a player from a lobby. The host role passes to a remaining human
    /// when the host leaves, and an emptied lobby is torn down entirely so its
    /// word becomes free again.
    pub async fn leave_lobby(&self, lobby_id: &str, player_id: &str, token: &str) -> Result<bool> {
        let lobby = self.lobby(lobby_id).await?;
        let mut state = lobby.state.lock().await;
        if state.authenticate(player_id, token).is_none() {
            bail!("unauthorized");
        }
        let Some(index) = state
            .players
            .iter()
            .position(|player| player.player_id == player_id)
        else {
            return Ok(false);
        };
        state.players.remove(index);
        state.pending.remove(player_id);

        let remaining_humans: Vec<String> = state
            .players
            .iter()
            .filter(|player| player.kind == v1::PlayerKind::Human)
            .map(|player| player.player_id.clone())
            .collect();
        if remaining_humans.is_empty() {
            // Nobody human is left to drive the room; drop it and free the word.
            let code = state.lobby_code.clone();
            drop(state);
            self.codes.write().await.remove(&code);
            self.lobbies.write().await.remove(lobby_id);
            self.repository.delete_lobby(lobby_id).await?;
            return Ok(true);
        }
        if state.host_player_id == player_id {
            state.host_player_id = remaining_humans[0].clone();
        }
        state.public_version += 1;
        self.repository.persist_snapshot(&state).await?;
        send_event(
            &lobby,
            v1::LobbyEventKind::LobbyUpdated,
            state.snapshot(),
            Vec::new(),
        );
        Ok(true)
    }

    pub async fn get_private_snapshot(
        &self,
        lobby_id: &str,
        player_id: &str,
        token: &str,
    ) -> Result<v1::PrivateLobbySnapshot> {
        let lobby = self.lobby(lobby_id).await?;
        let state = lobby.state.lock().await;
        state
            .authenticate(player_id, token)
            .context("unauthorized")?;
        Ok(v1::PrivateLobbySnapshot {
            lobby: Some(state.snapshot()),
            player_id: player_id.to_owned(),
            my_move_submitted: state.pending.contains_key(player_id),
        })
    }

    pub async fn list_games(
        &self,
        lobby_id: &str,
        player_id: &str,
        token: &str,
    ) -> Result<Vec<v1::GameSummary>> {
        let lobby = self.lobby(lobby_id).await?;
        {
            let state = lobby.state.lock().await;
            state
                .authenticate(player_id, token)
                .context("unauthorized")?;
        }
        let records = self.repository.list_games(lobby_id).await?;
        Ok(records
            .into_iter()
            .map(|record| v1::GameSummary {
                game_id: record.game_id,
                status: record.status,
                winners: record.winners,
                ended_unix_ms: record.ended_unix_ms,
            })
            .collect())
    }

    pub async fn watch_lobby(
        &self,
        lobby_id: &str,
        player_id: &str,
        token: &str,
    ) -> Result<(Arc<Lobby>, v1::LobbyEvent)> {
        let lobby = self.lobby(lobby_id).await?;
        let state = lobby.state.lock().await;
        state
            .authenticate(player_id, token)
            .context("unauthorized")?;
        let initial = v1::LobbyEvent {
            version: state.public_version,
            kind: v1::LobbyEventKind::Snapshot as i32,
            lobby: Some(state.snapshot()),
            moves: Vec::new(),
        };
        drop(state);
        Ok((lobby, initial))
    }

    pub async fn start_game(
        self: &Arc<Self>,
        lobby_id: &str,
        player_id: &str,
        token: &str,
    ) -> Result<v1::LobbySnapshot> {
        let lobby = self.lobby(lobby_id).await?;
        let mut state = lobby.state.lock().await;
        authenticate_host(&state, player_id, token)?;
        // A lobby starts the next round either from its initial waiting room or
        // after a finished game; an in-progress game cannot be restarted.
        if state.phase == v1::LobbyPhase::Playing {
            bail!("a game is already in progress");
        }
        if state.players.len() < 2 {
            bail!("at least two players are required");
        }
        if state.players.len() > usize::from(herdcore_core::MAX_PLAYERS) {
            bail!("too many players for one game");
        }
        for (seat, player) in state.players.iter_mut().enumerate() {
            player.seat = Some(seat as u8);
        }
        state.game = Some(
            initial_state_for_players(state.players.len() as u8)
                .map_err(|_| anyhow::anyhow!("invalid player count"))?,
        );
        state.phase = v1::LobbyPhase::Playing;
        state.game_id += 1;
        state.pending.clear();
        state.deadline_unix_ms = now_ms() + i64::from(state.turn_seconds) * 1000;
        state.public_version += 1;
        self.repository.persist_snapshot(&state).await?;
        self.repository
            .record_game_started(&state.lobby_id, state.game_id)
            .await?;
        let snapshot = state.snapshot();
        let game_id = state.game_id;
        let turn = state.game.as_ref().unwrap().turn;
        let deadline = state.deadline_unix_ms;
        send_event(
            &lobby,
            v1::LobbyEventKind::GameStarted,
            snapshot.clone(),
            Vec::new(),
        );
        drop(state);
        self.schedule_deadline(lobby, game_id, turn, deadline);
        Ok(snapshot)
    }

    pub async fn submit_move(
        self: &Arc<Self>,
        request: v1::SubmitMoveRequest,
    ) -> Result<v1::SubmitMoveResponse> {
        let lobby = self.lobby(&request.lobby_id).await?;
        let mut state = lobby.state.lock().await;
        let player = state
            .authenticate(&request.player_id, &request.session_token)
            .context("unauthorized")?
            .clone();
        if state.phase != v1::LobbyPhase::Playing {
            bail!("game is not active");
        }
        if now_ms() > state.deadline_unix_ms {
            bail!("turn deadline has passed");
        }
        let game = state.game.as_ref().context("game state missing")?;
        if request.game_id != state.game_id || request.turn != game.turn {
            bail!("stale turn");
        }
        if request.request_id.is_empty() || request.request_id.len() > 128 {
            bail!("invalid request id");
        }
        if state.pending.contains_key(&request.player_id) {
            return Ok(v1::SubmitMoveResponse {
                accepted: true,
                already_submitted: true,
                turn: game.turn,
            });
        }
        let proto_action = v1::Action::try_from(request.action).context("invalid action")?;
        let action =
            herdcore_protocol::action_from_proto(proto_action).context("action required")?;
        let seat = player.seat.context("player has no seat")?;
        if !is_action_legal(game, seat, action) {
            bail!("illegal action");
        }
        let submitted_turn = game.turn;
        let pending = PendingMove {
            action,
            request_id: request.request_id,
            received_at_ms: now_ms(),
        };

        let inserted = self
            .repository
            .persist_move(
                &state.lobby_id,
                state.game_id,
                submitted_turn,
                &request.player_id,
                &pending,
            )
            .await?;
        if !inserted {
            return Ok(v1::SubmitMoveResponse {
                accepted: true,
                already_submitted: true,
                turn: game.turn,
            });
        }
        state.pending.insert(request.player_id, pending);
        let should_resolve = state.pending.len() == state.players.len();
        let schedule = if should_resolve {
            self.resolve_locked(&lobby, &mut state).await?
        } else {
            None
        };
        drop(state);
        if let Some((game_id, turn, deadline)) = schedule {
            self.schedule_deadline(lobby, game_id, turn, deadline);
        }
        Ok(v1::SubmitMoveResponse {
            accepted: true,
            already_submitted: false,
            turn: submitted_turn,
        })
    }

    pub async fn add_bot(&self, request: &v1::AddBotRequest) -> Result<BotAssignment> {
        let lobby = self.lobby(&request.lobby_id).await?;
        let mut state = lobby.state.lock().await;
        authenticate_host(&state, &request.player_id, &request.session_token)?;
        if state.phase != v1::LobbyPhase::Waiting {
            bail!("bots can only be added before the game");
        }
        if state.players.len() >= usize::from(state.max_players) {
            bail!("lobby is full");
        }
        let display_name = if request.display_name.trim().is_empty() {
            "CPU".to_owned()
        } else {
            request.display_name.clone()
        };
        validate_name(&display_name)?;
        let player = new_player(
            display_name.clone(),
            v1::PlayerKind::Bot,
            Some(request.bot_type_id.clone()),
        );
        state.players.push(player.clone());
        state.public_version += 1;
        self.repository.persist_snapshot(&state).await?;
        let snapshot = state.snapshot();
        send_event(
            &lobby,
            v1::LobbyEventKind::LobbyUpdated,
            snapshot.clone(),
            Vec::new(),
        );
        Ok(BotAssignment {
            snapshot,
            lobby_id: state.lobby_id.clone(),
            player_id: player.player_id,
            session_token: player.session_token,
            display_name,
            bot_type_id: request.bot_type_id.clone(),
        })
    }

    pub async fn remove_failed_bot(&self, lobby_id: &str, player_id: &str) -> Result<()> {
        let lobby = self.lobby(lobby_id).await?;
        let mut state = lobby.state.lock().await;
        if state.phase != v1::LobbyPhase::Waiting {
            return Ok(());
        }
        let Some(index) = state
            .players
            .iter()
            .position(|player| player.player_id == player_id && player.kind == v1::PlayerKind::Bot)
        else {
            return Ok(());
        };
        state.players.remove(index);
        state.public_version += 1;
        self.repository.persist_snapshot(&state).await?;
        send_event(
            &lobby,
            v1::LobbyEventKind::LobbyUpdated,
            state.snapshot(),
            Vec::new(),
        );
        Ok(())
    }

    pub async fn recoverable_bots(&self) -> Vec<BotAssignment> {
        let lobbies: Vec<Arc<Lobby>> = self.lobbies.read().await.values().cloned().collect();
        let mut assignments = Vec::new();
        for lobby in lobbies {
            let state = lobby.state.lock().await;
            if state.phase == v1::LobbyPhase::Finished {
                continue;
            }
            for player in &state.players {
                if player.kind == v1::PlayerKind::Bot {
                    assignments.push(BotAssignment {
                        snapshot: state.snapshot(),
                        lobby_id: state.lobby_id.clone(),
                        player_id: player.player_id.clone(),
                        session_token: player.session_token.clone(),
                        display_name: player.display_name.clone(),
                        bot_type_id: player
                            .bot_type_id
                            .clone()
                            .unwrap_or_else(|| "greedy-v1".to_owned()),
                    });
                }
            }
        }
        assignments
    }

    async fn resolve_deadline(
        self: Arc<Self>,
        lobby: Arc<Lobby>,
        game_id: u64,
        turn: u64,
    ) -> Result<()> {
        let mut state = lobby.state.lock().await;
        if state.phase != v1::LobbyPhase::Playing
            || state.game_id != game_id
            || state.game.as_ref().map(|game| game.turn) != Some(turn)
        {
            return Ok(());
        }
        let schedule = self.resolve_locked(&lobby, &mut state).await?;
        drop(state);
        if let Some((next_game_id, next_turn, deadline)) = schedule {
            self.schedule_deadline(lobby, next_game_id, next_turn, deadline);
        }
        Ok(())
    }

    async fn resolve_locked(
        &self,
        lobby: &Arc<Lobby>,
        state: &mut LobbyState,
    ) -> Result<Option<(u64, u64, i64)>> {
        let game = state.game.as_ref().context("game missing")?;
        let resolved_turn = game.turn;
        let mut actions = BTreeMap::new();
        let mut revealed = Vec::with_capacity(state.players.len());
        for player in &state.players {
            let seat = player.seat.context("seat missing")?;
            let action = state
                .pending
                .get(&player.player_id)
                .map(|pending| pending.action)
                .unwrap_or(Action::Stay);
            actions.insert(seat, action);
            revealed.push(v1::RevealedMove {
                player_id: player.player_id.clone(),
                seat: u32::from(seat),
                action: action_to_proto(action) as i32,
            });
        }
        let next_game = step(game, &actions)
            .map_err(|error| anyhow::anyhow!("resolution failed: {error:?}"))?;
        let mut candidate = state.clone();
        candidate.game = Some(next_game.clone());
        candidate.pending.clear();
        candidate.public_version += 1;
        if next_game.game_over {
            candidate.phase = v1::LobbyPhase::Finished;
            candidate.deadline_unix_ms = 0;
        } else {
            candidate.deadline_unix_ms = now_ms() + i64::from(candidate.turn_seconds) * 1000;
        }
        let event = v1::LobbyEvent {
            version: candidate.public_version,
            kind: v1::LobbyEventKind::TurnResolved as i32,
            lobby: Some(candidate.snapshot()),
            moves: revealed,
        };
        let resolved_at = now_ms();
        self.repository
            .persist_resolution(&candidate, resolved_turn, &event, resolved_at)
            .await?;
        *state = candidate;
        let _ = lobby.events.send(event);
        if let Err(error) = self
            .repository
            .mark_event_published(&state.lobby_id, state.public_version)
            .await
        {
            eprintln!("failed to mark outbox event published: {error:#}");
        }
        if next_game.game_over {
            let winners: Vec<u32> = next_game.winners.iter().map(|seat| u32::from(*seat)).collect();
            if let Err(error) = self
                .repository
                .record_game_finished(&state.lobby_id, state.game_id, &winners, resolved_at)
                .await
            {
                eprintln!("failed to record finished game: {error:#}");
            }
            Ok(None)
        } else {
            Ok(Some((
                state.game_id,
                next_game.turn,
                state.deadline_unix_ms,
            )))
        }
    }

    fn schedule_deadline(
        self: &Arc<Self>,
        lobby: Arc<Lobby>,
        game_id: u64,
        turn: u64,
        deadline_unix_ms: i64,
    ) {
        let app = Arc::clone(self);
        tokio::spawn(async move {
            let delay_ms = deadline_unix_ms.saturating_sub(now_ms()) as u64;
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            loop {
                match Arc::clone(&app)
                    .resolve_deadline(Arc::clone(&lobby), game_id, turn)
                    .await
                {
                    Ok(()) => break,
                    Err(error) => {
                        eprintln!("deadline resolution failed; retrying: {error:#}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        });
    }

    async fn lobby(&self, lobby_id: &str) -> Result<Arc<Lobby>> {
        self.lobbies
            .read()
            .await
            .get(lobby_id)
            .cloned()
            .context("lobby not found")
    }

    async fn unique_lobby_code(&self) -> String {
        loop {
            let code = Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(6)
                .collect::<String>()
                .to_ascii_uppercase();
            if !self.codes.read().await.contains_key(&code) {
                return code;
            }
        }
    }
}

fn new_player(
    display_name: String,
    kind: v1::PlayerKind,
    bot_type_id: Option<String>,
) -> InternalPlayer {
    InternalPlayer {
        player_id: Uuid::new_v4().to_string(),
        display_name,
        kind,
        seat: None,
        connected: true,
        session_token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        bot_type_id,
    }
}

fn authenticate_host(state: &LobbyState, player_id: &str, token: &str) -> Result<()> {
    state
        .authenticate(player_id, token)
        .context("unauthorized")?;
    if state.host_player_id != player_id {
        bail!("host permission required");
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 24 || trimmed.chars().any(char::is_control) {
        bail!("display name must contain 1 to 24 printable characters");
    }
    Ok(())
}

/// Collapse a human-typed lobby word into a stable key: lowercase, with runs of
/// non-alphanumeric characters folded to single dashes. Everyone who types the
/// same word (regardless of spacing or case) shares one lobby.
fn normalize_lobby_name(name: &str) -> Result<String> {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
            last_was_dash = false;
        } else if !slug.is_empty() && !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() || slug.chars().count() > 32 {
        bail!("lobby name must contain 1 to 32 letters or digits");
    }
    Ok(slug)
}

fn send_event(
    lobby: &Lobby,
    kind: v1::LobbyEventKind,
    snapshot: v1::LobbySnapshot,
    moves: Vec<v1::RevealedMove>,
) {
    let _ = lobby.events.send(v1::LobbyEvent {
        version: snapshot.public_version,
        kind: kind as i32,
        lobby: Some(snapshot),
        moves,
    });
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn submissions_are_private_and_durable_until_atomic_resolution() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("game.db");
        let repository = Repository::open(path.to_str().unwrap()).await.unwrap();
        let app = App::load(repository).await.unwrap();
        let (alice, created) = app.create_lobby("Alice".to_owned(), 2, 300).await.unwrap();
        let (bob, _) = app
            .join_lobby(&created.lobby_code, "Bob".to_owned())
            .await
            .unwrap();
        let (lobby, _) = app
            .watch_lobby(&created.lobby_id, &alice.player_id, &alice.session_token)
            .await
            .unwrap();
        let mut events = lobby.events.subscribe();
        let started = app
            .start_game(&created.lobby_id, &alice.player_id, &alice.session_token)
            .await
            .unwrap();
        let _ = events.recv().await.unwrap();

        app.submit_move(v1::SubmitMoveRequest {
            lobby_id: created.lobby_id.clone(),
            player_id: alice.player_id.clone(),
            session_token: alice.session_token.clone(),
            game_id: started.game_id,
            turn: 0,
            action: v1::Action::Stay as i32,
            request_id: "alice-turn-zero".to_owned(),
        })
        .await
        .unwrap();
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(
            app.get_private_snapshot(&created.lobby_id, &alice.player_id, &alice.session_token)
                .await
                .unwrap()
                .my_move_submitted
        );
        assert!(
            !app.get_private_snapshot(&created.lobby_id, &bob.player_id, &bob.session_token)
                .await
                .unwrap()
                .my_move_submitted
        );

        let recovered = Repository::open(path.to_str().unwrap())
            .await
            .unwrap()
            .load_lobbies()
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].pending.len(), 1);
        assert_eq!(recovered[0].pending[&alice.player_id].action, Action::Stay);

        app.submit_move(v1::SubmitMoveRequest {
            lobby_id: created.lobby_id.clone(),
            player_id: bob.player_id.clone(),
            session_token: bob.session_token.clone(),
            game_id: started.game_id,
            turn: 0,
            action: v1::Action::Stay as i32,
            request_id: "bob-turn-zero".to_owned(),
        })
        .await
        .unwrap();
        let resolved = events.recv().await.unwrap();
        assert_eq!(resolved.kind, v1::LobbyEventKind::TurnResolved as i32);
        assert_eq!(resolved.moves.len(), 2);
        assert_eq!(resolved.lobby.unwrap().game.unwrap().turn, 1);
    }

    async fn test_app() -> (Arc<App>, tempfile::TempDir) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("game.db");
        let repository = Repository::open(path.to_str().unwrap()).await.unwrap();
        let app = App::load(repository).await.unwrap();
        (app, directory)
    }

    #[tokio::test]
    async fn words_route_players_into_shared_lobbies() {
        let (app, _dir) = test_app().await;

        let (alice, first) = app
            .join_or_create_lobby("Wiggly Sheep!", "Alice".to_owned())
            .await
            .unwrap();
        assert_eq!(first.lobby_code, "wiggly-sheep");
        assert_eq!(first.host_player_id, alice.player_id);
        assert_eq!(first.players.len(), 1);

        // Spacing and case differences normalize to the same word, so Bob lands
        // in Alice's lobby rather than a new one.
        let (_bob, joined) = app
            .join_or_create_lobby("  WIGGLY   sheep ", "Bob".to_owned())
            .await
            .unwrap();
        assert_eq!(joined.lobby_id, first.lobby_id);
        assert_eq!(joined.players.len(), 2);

        let (_cara, other) = app
            .join_or_create_lobby("cosmic-collie", "Cara".to_owned())
            .await
            .unwrap();
        assert_ne!(other.lobby_id, first.lobby_id);
    }

    #[tokio::test]
    async fn leaving_reassigns_host_then_reclaims_the_word() {
        let (app, _dir) = test_app().await;
        let (alice, lobby) = app
            .join_or_create_lobby("wiggly-sheep", "Alice".to_owned())
            .await
            .unwrap();
        let (bob, _) = app
            .join_or_create_lobby("wiggly-sheep", "Bob".to_owned())
            .await
            .unwrap();

        // Host leaves: the room survives and Bob inherits the host role.
        assert!(app
            .leave_lobby(&lobby.lobby_id, &alice.player_id, &alice.session_token)
            .await
            .unwrap());
        let snapshot = app
            .get_private_snapshot(&lobby.lobby_id, &bob.player_id, &bob.session_token)
            .await
            .unwrap()
            .lobby
            .unwrap();
        assert_eq!(snapshot.host_player_id, bob.player_id);
        assert_eq!(snapshot.players.len(), 1);

        // Last human leaves: the lobby is torn down and the word is free again,
        // so re-entering it mints a brand new lobby.
        assert!(app
            .leave_lobby(&lobby.lobby_id, &bob.player_id, &bob.session_token)
            .await
            .unwrap());
        let (_dee, fresh) = app
            .join_or_create_lobby("wiggly-sheep", "Dee".to_owned())
            .await
            .unwrap();
        assert_ne!(fresh.lobby_id, lobby.lobby_id);
        assert_eq!(fresh.players.len(), 1);
    }

    #[tokio::test]
    async fn a_full_eight_dog_lobby_starts() {
        let (app, _dir) = test_app().await;
        let (host, lobby) = app
            .join_or_create_lobby("big-flock", "Dog 1".to_owned())
            .await
            .unwrap();
        for index in 2..=8 {
            app.join_or_create_lobby("big-flock", format!("Dog {index}"))
                .await
                .unwrap();
        }
        // The 8-seat lobby is now full; a 9th dog is turned away.
        assert!(app
            .join_or_create_lobby("big-flock", "Latecomer".to_owned())
            .await
            .is_err());

        let started = app
            .start_game(&lobby.lobby_id, &host.player_id, &host.session_token)
            .await
            .unwrap();
        assert_eq!(started.phase, v1::LobbyPhase::Playing as i32);
        assert_eq!(started.game.unwrap().players.len(), 8);
    }

    #[tokio::test]
    async fn games_are_recorded_in_lobby_history() {
        let (app, _dir) = test_app().await;
        let (host, lobby) = app
            .join_or_create_lobby("history-room", "Alice".to_owned())
            .await
            .unwrap();
        app.join_or_create_lobby("history-room", "Bob".to_owned())
            .await
            .unwrap();
        app.start_game(&lobby.lobby_id, &host.player_id, &host.session_token)
            .await
            .unwrap();

        let games = app
            .list_games(&lobby.lobby_id, &host.player_id, &host.session_token)
            .await
            .unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id, 1);
        assert_eq!(games[0].status, v1::LobbyPhase::Playing as i32);
    }

    #[tokio::test]
    async fn joining_a_running_game_starts_as_a_spectator() {
        let (app, _dir) = test_app().await;
        let (alice, lobby) = app
            .join_or_create_lobby("wiggly-sheep", "Alice".to_owned())
            .await
            .unwrap();
        app.join_or_create_lobby("wiggly-sheep", "Bob".to_owned())
            .await
            .unwrap();
        app.start_game(&lobby.lobby_id, &alice.player_id, &alice.session_token)
            .await
            .unwrap();

        let (cara, snapshot) = app
            .join_or_create_lobby("wiggly-sheep", "Cara".to_owned())
            .await
            .unwrap();
        assert_eq!(snapshot.phase, v1::LobbyPhase::Playing as i32);
        let cara_view = snapshot
            .players
            .iter()
            .find(|player| player.player_id == cara.player_id)
            .unwrap();
        assert!(cara_view.seat.is_none(), "late joiner should be unseated");
    }
}
