//! WebSocket transport. Every participant — human browser or bot — speaks the
//! same protocol: connect, authenticate once (Join/Resume), then exchange bare
//! command/event frames. The connection is the session.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use herdcore_protocol::v1::{client_frame, server_frame};
use herdcore_protocol::{decode_frame, encode_frame, v1};
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::app::{App, BotCredentials};

#[derive(Clone)]
pub struct WsState {
    pub app: Arc<App>,
    /// The WebSocket URL the bot service should dial to play a seat.
    pub ws_url: String,
    /// The bots a player can add, offered as buttons on the client.
    pub catalogue: Arc<Vec<v1::BotKind>>,
    /// Connected bot-service providers that play CPU seats over their own
    /// gameplay connections.
    pub providers: ProviderRegistry,
}

/// Tracks the bot-service provider connections so the server can hand them CPU
/// seats to play.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    inner: Arc<Mutex<ProviderInner>>,
}

struct Provider {
    id: u64,
    bot_type_id: String,
    sender: mpsc::UnboundedSender<v1::ServerFrame>,
}

#[derive(Default)]
struct ProviderInner {
    next_id: u64,
    providers: Vec<Provider>,
}

impl ProviderRegistry {
    async fn register(
        &self,
        bot_type_id: String,
        sender: mpsc::UnboundedSender<v1::ServerFrame>,
    ) -> u64 {
        let mut inner = self.inner.lock().await;
        inner.next_id += 1;
        let id = inner.next_id;
        inner.providers.push(Provider {
            id,
            bot_type_id,
            sender,
        });
        id
    }

    async fn unregister(&self, id: u64) {
        let mut inner = self.inner.lock().await;
        inner.providers.retain(|provider| provider.id != id);
    }

    /// Hand an assignment to a provider serving `bot_type_id` (any provider if
    /// none matches); false if none are connected.
    async fn assign(&self, bot_type_id: &str, frame: v1::ServerFrame) -> bool {
        let mut inner = self.inner.lock().await;
        inner.providers.retain(|provider| !provider.sender.is_closed());
        let provider = inner
            .providers
            .iter()
            .find(|provider| provider.bot_type_id == bot_type_id)
            .or_else(|| inner.providers.first());
        match provider {
            Some(provider) => provider.sender.send(frame).is_ok(),
            None => false,
        }
    }
}

struct Session {
    lobby_id: String,
    player_id: String,
    token: String,
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WsState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: WsState) {
    match establish(&mut socket, &state).await {
        Some(Established::Provider(bot_type_id)) => {
            run_provider(socket, state, bot_type_id).await
        }
        Some(Established::Session(session)) => run_session(socket, state, session).await,
        None => {}
    }
}

/// Relay loop for a connected bot-service provider: forward Assign frames to its
/// socket and keep it alive until it disconnects.
async fn run_provider(mut socket: WebSocket, state: WsState, bot_type_id: String) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<v1::ServerFrame>();
    let id = state.providers.register(bot_type_id, sender).await;

    // Hand the provider every CPU seat that already exists (covers recovery and
    // a provider (re)connecting after games are underway).
    for credentials in state.app.recoverable_bots().await {
        let _ = socket
            .send(Message::Binary(encode_frame(&assign_frame(&state.ws_url, &credentials))))
            .await;
    }

    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            assignment = receiver.recv() => match assignment {
                Some(frame) => {
                    if send(&mut socket, frame).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(_))) | Some(Ok(Message::Ping(_))) => {}
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            _ = heartbeat.tick() => {
                if send(&mut socket, pong_frame()).await.is_err() {
                    break;
                }
            }
        }
    }
    state.providers.unregister(id).await;
}

async fn run_session(mut socket: WebSocket, state: WsState, session: Session) {
    // Subscribe before relaying so we don't miss events; the snapshot already
    // sent in Welcome is authoritative for catch-up.
    let Ok((lobby, _)) = state
        .app
        .watch_lobby(&session.lobby_id, &session.player_id, &session.token)
        .await
    else {
        return;
    };
    let mut events = lobby.events.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.tick().await;

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(bytes))) => {
                    if let Ok(frame) = decode_frame::<v1::ClientFrame>(&bytes) {
                        if !handle_command(&mut socket, &state, &session, frame).await {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            event = events.recv() => match event {
                Ok(event) => {
                    if send(&mut socket, update_frame(event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Fell behind: push a fresh authoritative snapshot to resync.
                    if let Ok((lobby, submitted)) = state
                        .app
                        .get_private_snapshot(&session.lobby_id, &session.player_id, &session.token)
                        .await
                    {
                        if send(&mut socket, welcome_frame(&session, lobby, submitted, &state.catalogue)).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => {
                if send(&mut socket, pong_frame()).await.is_err() {
                    break;
                }
            }
        }
    }
}

enum Established {
    Provider(String),
    Session(Session),
}

/// Phase 1: read the opening frame — a provider registration, or a Join/Resume
/// that establishes a player session.
async fn establish(socket: &mut WebSocket, state: &WsState) -> Option<Established> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(bytes))) => {
                let Ok(frame) = decode_frame::<v1::ClientFrame>(&bytes) else {
                    continue;
                };
                match frame.body {
                    Some(client_frame::Body::RegisterProvider(register)) => {
                        return Some(Established::Provider(register.bot_type_id));
                    }
                    Some(client_frame::Body::Join(join)) => {
                        match state
                            .app
                            .join_or_create_lobby(&join.lobby_name, join.display_name)
                            .await
                        {
                            Ok((player, snapshot)) => {
                                let session = Session {
                                    lobby_id: snapshot.lobby_id.clone(),
                                    player_id: player.player_id.clone(),
                                    token: player.session_token.clone(),
                                };
                                let _ = send(socket, welcome_frame(&session, snapshot, false, &state.catalogue)).await;
                                return Some(Established::Session(session));
                            }
                            Err(error) => {
                                let _ = send(socket, error_frame(&error.to_string(), false)).await;
                            }
                        }
                    }
                    Some(client_frame::Body::Resume(resume)) => {
                        match state
                            .app
                            .get_private_snapshot(
                                &resume.lobby_id,
                                &resume.player_id,
                                &resume.session_token,
                            )
                            .await
                        {
                            Ok((snapshot, submitted)) => {
                                let session = Session {
                                    lobby_id: resume.lobby_id,
                                    player_id: resume.player_id,
                                    token: resume.session_token,
                                };
                                let _ = send(socket, welcome_frame(&session, snapshot, submitted, &state.catalogue)).await;
                                return Some(Established::Session(session));
                            }
                            Err(_) => {
                                let _ = send(socket, error_frame("session expired", true)).await;
                                return None;
                            }
                        }
                    }
                    Some(client_frame::Body::Ping(_)) => {
                        let _ = send(socket, pong_frame()).await;
                    }
                    _ => {
                        let _ = send(socket, error_frame("join the lobby first", false)).await;
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
            _ => {}
        }
    }
}

/// Phase 2: handle a command on an established session. Returns false to close.
async fn handle_command(
    socket: &mut WebSocket,
    state: &WsState,
    session: &Session,
    frame: v1::ClientFrame,
) -> bool {
    match frame.body {
        Some(client_frame::Body::Move(command)) => {
            if let Err(error) = state
                .app
                .submit_move(
                    &session.player_id,
                    &session.token,
                    &session.lobby_id,
                    command.game_id,
                    command.turn,
                    command.action,
                    &command.request_id,
                )
                .await
            {
                let _ = send(socket, error_frame(&error.to_string(), false)).await;
            }
        }
        Some(client_frame::Body::Start(_)) => {
            if let Err(error) = state
                .app
                .start_game(&session.lobby_id, &session.player_id, &session.token)
                .await
            {
                let _ = send(socket, error_frame(&error.to_string(), false)).await;
            }
        }
        Some(client_frame::Body::AddBot(command)) => {
            match state
                .app
                .add_bot(
                    &session.lobby_id,
                    &session.player_id,
                    &session.token,
                    &command.display_name,
                    &command.bot_type_id,
                )
                .await
            {
                // Hand the seat to the bot service; it connects back to play it.
                Ok(credentials) => {
                    let assigned = state
                        .providers
                        .assign(
                            &credentials.bot_type_id,
                            assign_frame(&state.ws_url, &credentials),
                        )
                        .await;
                    if !assigned {
                        // No bot service connected: undo the seat and report it.
                        let _ = state
                            .app
                            .remove_bot(
                                &session.lobby_id,
                                &session.player_id,
                                &session.token,
                                &credentials.player_id,
                            )
                            .await;
                        let _ = send(
                            socket,
                            error_frame("no bot service connected — run herdcore-bot", false),
                        )
                        .await;
                    }
                }
                Err(error) => {
                    let _ = send(socket, error_frame(&error.to_string(), false)).await;
                }
            }
        }
        Some(client_frame::Body::RemoveBot(command)) => {
            if let Err(error) = state
                .app
                .remove_bot(
                    &session.lobby_id,
                    &session.player_id,
                    &session.token,
                    &command.bot_player_id,
                )
                .await
            {
                let _ = send(socket, error_frame(&error.to_string(), false)).await;
            }
        }
        Some(client_frame::Body::ListGames(_)) => {
            if let Ok(games) = state
                .app
                .list_games(&session.lobby_id, &session.player_id, &session.token)
                .await
            {
                let _ = send(socket, games_frame(games)).await;
            }
        }
        Some(client_frame::Body::Leave(_)) => {
            let _ = state
                .app
                .leave_lobby(&session.lobby_id, &session.player_id, &session.token)
                .await;
            let _ = send(socket, bye_frame()).await;
            return false;
        }
        Some(client_frame::Body::Ping(_)) => {
            let _ = send(socket, pong_frame()).await;
        }
        Some(client_frame::Body::Join(_))
        | Some(client_frame::Body::Resume(_))
        | Some(client_frame::Body::RegisterProvider(_))
        | None => {}
    }
    true
}

fn assign_frame(game_server_url: &str, credentials: &BotCredentials) -> v1::ServerFrame {
    server(server_frame::Body::Assign(v1::Assign {
        game_server_url: game_server_url.to_owned(),
        lobby_id: credentials.lobby_id.clone(),
        player_id: credentials.player_id.clone(),
        session_token: credentials.session_token.clone(),
        bot_type_id: credentials.bot_type_id.clone(),
    }))
}

async fn send(socket: &mut WebSocket, frame: v1::ServerFrame) -> Result<(), ()> {
    socket
        .send(Message::Binary(encode_frame(&frame)))
        .await
        .map_err(|_| ())
}

fn welcome_frame(
    session: &Session,
    lobby: v1::LobbySnapshot,
    submitted: bool,
    catalogue: &[v1::BotKind],
) -> v1::ServerFrame {
    server(server_frame::Body::Welcome(v1::Welcome {
        player_id: session.player_id.clone(),
        session_token: session.token.clone(),
        lobby: Some(lobby),
        my_move_submitted: submitted,
        catalogue: catalogue.to_vec(),
    }))
}

fn update_frame(event: v1::LobbyEvent) -> v1::ServerFrame {
    server(server_frame::Body::Update(v1::Update {
        lobby: event.lobby,
        my_move_submitted: false,
        kind: event.kind,
        moves: event.moves,
        result: event.result,
    }))
}

fn games_frame(games: Vec<v1::GameSummary>) -> v1::ServerFrame {
    server(server_frame::Body::Games(v1::GamesList { games }))
}

fn error_frame(message: &str, fatal: bool) -> v1::ServerFrame {
    server(server_frame::Body::Error(v1::ServerError {
        message: message.to_owned(),
        fatal,
    }))
}

fn pong_frame() -> v1::ServerFrame {
    server(server_frame::Body::Pong(v1::Pong {}))
}

fn bye_frame() -> v1::ServerFrame {
    server(server_frame::Body::Bye(v1::Bye {}))
}

fn server(body: server_frame::Body) -> v1::ServerFrame {
    v1::ServerFrame { body: Some(body) }
}
