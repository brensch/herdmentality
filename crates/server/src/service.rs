use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::Stream;
use herdcore_protocol::v1;
use herdcore_protocol::v1::bot_provider_client::BotProviderClient;
use herdcore_protocol::v1::herdcore_server::Herdcore;
use tokio::sync::broadcast;
use tonic::transport::Endpoint;
use tonic::{Request, Response, Status};

use crate::app::{App, BotAssignment};

pub struct HerdcoreService {
    app: Arc<App>,
    bot_provider_url: Option<String>,
    public_server_url: String,
}

impl HerdcoreService {
    pub fn new(app: Arc<App>, bot_provider_url: Option<String>, public_server_url: String) -> Self {
        Self {
            app,
            bot_provider_url,
            public_server_url,
        }
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<v1::LobbyEvent, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Herdcore for HerdcoreService {
    type WatchLobbyStream = EventStream;

    async fn create_lobby(
        &self,
        request: Request<v1::CreateLobbyRequest>,
    ) -> Result<Response<v1::JoinLobbyResponse>, Status> {
        let request = request.into_inner();
        let max_players = u8::try_from(request.max_players).map_err(invalid)?;
        let turn_seconds = u16::try_from(request.turn_seconds).map_err(invalid)?;
        let (player, lobby) = self
            .app
            .create_lobby(request.display_name, max_players, turn_seconds)
            .await
            .map_err(status)?;
        Ok(Response::new(v1::JoinLobbyResponse {
            player_id: player.player_id,
            session_token: player.session_token,
            lobby: Some(lobby),
        }))
    }

    async fn join_lobby(
        &self,
        request: Request<v1::JoinLobbyRequest>,
    ) -> Result<Response<v1::JoinLobbyResponse>, Status> {
        let request = request.into_inner();
        let (player, lobby) = self
            .app
            .join_lobby(&request.lobby_code, request.display_name)
            .await
            .map_err(status)?;
        Ok(Response::new(v1::JoinLobbyResponse {
            player_id: player.player_id,
            session_token: player.session_token,
            lobby: Some(lobby),
        }))
    }

    async fn join_or_create_lobby(
        &self,
        request: Request<v1::JoinOrCreateLobbyRequest>,
    ) -> Result<Response<v1::JoinLobbyResponse>, Status> {
        let request = request.into_inner();
        let (player, lobby) = self
            .app
            .join_or_create_lobby(&request.lobby_name, request.display_name)
            .await
            .map_err(status)?;
        Ok(Response::new(v1::JoinLobbyResponse {
            player_id: player.player_id,
            session_token: player.session_token,
            lobby: Some(lobby),
        }))
    }

    async fn leave_lobby(
        &self,
        request: Request<v1::LeaveLobbyRequest>,
    ) -> Result<Response<v1::LeaveLobbyResponse>, Status> {
        let request = request.into_inner();
        let left = self
            .app
            .leave_lobby(
                &request.lobby_id,
                &request.player_id,
                &request.session_token,
            )
            .await
            .map_err(status)?;
        Ok(Response::new(v1::LeaveLobbyResponse { left }))
    }

    async fn get_lobby(
        &self,
        request: Request<v1::GetLobbyRequest>,
    ) -> Result<Response<v1::PrivateLobbySnapshot>, Status> {
        let request = request.into_inner();
        self.app
            .get_private_snapshot(
                &request.lobby_id,
                &request.player_id,
                &request.session_token,
            )
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn list_games(
        &self,
        request: Request<v1::GetLobbyRequest>,
    ) -> Result<Response<v1::ListGamesResponse>, Status> {
        let request = request.into_inner();
        let games = self
            .app
            .list_games(
                &request.lobby_id,
                &request.player_id,
                &request.session_token,
            )
            .await
            .map_err(status)?;
        Ok(Response::new(v1::ListGamesResponse { games }))
    }

    async fn watch_lobby(
        &self,
        request: Request<v1::WatchLobbyRequest>,
    ) -> Result<Response<Self::WatchLobbyStream>, Status> {
        let request = request.into_inner();
        let (lobby, initial) = self
            .app
            .watch_lobby(
                &request.lobby_id,
                &request.player_id,
                &request.session_token,
            )
            .await
            .map_err(status)?;
        let mut receiver = lobby.events.subscribe();
        let stream = async_stream::stream! {
            yield Ok(initial);
            let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
            heartbeat.tick().await;
            loop {
                tokio::select! {
                    event = receiver.recv() => match event {
                        Ok(event) => yield Ok(event),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            yield Err(Status::aborted("event stream lagged; reconnect for a snapshot"));
                            break;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = heartbeat.tick() => {
                        yield Ok(v1::LobbyEvent {
                            version: 0,
                            kind: v1::LobbyEventKind::Heartbeat as i32,
                            lobby: None,
                            moves: Vec::new(),
                        });
                    }
                }
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }

    async fn start_game(
        &self,
        request: Request<v1::StartGameRequest>,
    ) -> Result<Response<v1::LobbySnapshot>, Status> {
        let request = request.into_inner();
        self.app
            .start_game(
                &request.lobby_id,
                &request.player_id,
                &request.session_token,
            )
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn submit_move(
        &self,
        request: Request<v1::SubmitMoveRequest>,
    ) -> Result<Response<v1::SubmitMoveResponse>, Status> {
        self.app
            .submit_move(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn add_bot(
        &self,
        request: Request<v1::AddBotRequest>,
    ) -> Result<Response<v1::AddBotResponse>, Status> {
        let provider_url = self
            .bot_provider_url
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("bot provider is not configured"))?;
        let request = request.into_inner();
        let assignment = self.app.add_bot(&request).await.map_err(status)?;
        if let Err(error) =
            start_external_bot(provider_url, &self.public_server_url, &assignment).await
        {
            if let Err(rollback) = self
                .app
                .remove_failed_bot(&assignment.lobby_id, &assignment.player_id)
                .await
            {
                eprintln!("failed to roll back bot seat: {rollback:#}");
            }
            return Err(error);
        }
        Ok(Response::new(v1::AddBotResponse {
            lobby: Some(assignment.snapshot),
        }))
    }
}

pub async fn start_external_bot(
    provider_url: &str,
    public_server_url: &str,
    assignment: &BotAssignment,
) -> Result<(), Status> {
    let channel = Endpoint::from_shared(provider_url.to_owned())
        .map_err(|error| Status::invalid_argument(error.to_string()))?
        .connect()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    let mut client = BotProviderClient::new(channel);
    client
        .start_bot(v1::StartBotRequest {
            game_server_url: public_server_url.to_owned(),
            lobby_id: assignment.lobby_id.clone(),
            player_id: assignment.player_id.clone(),
            session_token: assignment.session_token.clone(),
            bot_type_id: assignment.bot_type_id.clone(),
            display_name: assignment.display_name.clone(),
        })
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    Ok(())
}

fn status(error: anyhow::Error) -> Status {
    Status::failed_precondition(error.to_string())
}

fn invalid(error: impl std::fmt::Display) -> Status {
    Status::invalid_argument(error.to_string())
}
