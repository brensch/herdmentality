use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use herdcore_core::{bot, Action as CoreAction, GameState, SeatId};
use herdcore_protocol::v1;
use herdcore_protocol::v1::bot_provider_server::{BotProvider, BotProviderServer};
use herdcore_protocol::v1::herdcore_client::HerdcoreClient;
use tokio::sync::Mutex;
use tonic::transport::{Endpoint, Server};
use tonic::{Request, Response, Status};
use uuid::Uuid;

trait BotStrategy: Send + Sync {
    fn choose(&self, state: &GameState, seat: SeatId) -> CoreAction;
}

struct GreedyBot;

impl BotStrategy for GreedyBot {
    fn choose(&self, state: &GameState, seat: SeatId) -> CoreAction {
        bot::choose_action(state, seat)
    }
}

struct ProviderService {
    strategies: HashMap<String, Arc<dyn BotStrategy>>,
    active: Arc<Mutex<HashMap<String, String>>>,
}

impl ProviderService {
    fn new() -> Self {
        let mut strategies: HashMap<String, Arc<dyn BotStrategy>> = HashMap::new();
        strategies.insert("greedy-v1".to_owned(), Arc::new(GreedyBot));
        Self {
            strategies,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[tonic::async_trait]
impl BotProvider for ProviderService {
    async fn start_bot(
        &self,
        request: Request<v1::StartBotRequest>,
    ) -> Result<Response<v1::StartBotResponse>, Status> {
        let request = request.into_inner();
        let strategy = self
            .strategies
            .get(&request.bot_type_id)
            .cloned()
            .ok_or_else(|| Status::not_found("unknown bot type"))?;
        if let Some(instance_id) = self.active.lock().await.get(&request.player_id).cloned() {
            return Ok(Response::new(v1::StartBotResponse {
                bot_instance_id: instance_id,
            }));
        }
        let instance_id = Uuid::new_v4().to_string();
        println!(
            "starting bot {instance_id} type={} lobby={} player={}",
            request.bot_type_id, request.lobby_id, request.player_id
        );
        self.active
            .lock()
            .await
            .insert(request.player_id.clone(), instance_id.clone());
        let active = Arc::clone(&self.active);
        let player_id = request.player_id.clone();
        tokio::spawn(async move {
            if let Err(error) = run_bot(request, strategy).await {
                eprintln!("bot stopped: {error:#}");
            }
            active.lock().await.remove(&player_id);
        });
        Ok(Response::new(v1::StartBotResponse {
            bot_instance_id: instance_id,
        }))
    }
}

async fn run_bot(request: v1::StartBotRequest, strategy: Arc<dyn BotStrategy>) -> Result<()> {
    loop {
        match run_bot_connection(&request, Arc::clone(&strategy)).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                eprintln!("bot reconnecting after error: {error:#}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn run_bot_connection(
    request: &v1::StartBotRequest,
    strategy: Arc<dyn BotStrategy>,
) -> Result<()> {
    let channel = Endpoint::from_shared(request.game_server_url.clone())?
        .connect()
        .await?;
    let mut client = HerdcoreClient::new(channel);
    let mut stream = client
        .watch_lobby(v1::WatchLobbyRequest {
            lobby_id: request.lobby_id.clone(),
            player_id: request.player_id.clone(),
            session_token: request.session_token.clone(),
            after_version: 0,
        })
        .await?
        .into_inner();

    while let Some(event) = stream.message().await? {
        if event.kind == v1::LobbyEventKind::Heartbeat as i32 {
            continue;
        }
        let Some(lobby) = event.lobby else {
            continue;
        };
        if lobby.phase == v1::LobbyPhase::Finished as i32 {
            return Ok(());
        }
        if lobby.phase != v1::LobbyPhase::Playing as i32 {
            continue;
        }
        let private = client
            .get_lobby(v1::GetLobbyRequest {
                lobby_id: request.lobby_id.clone(),
                player_id: request.player_id.clone(),
                session_token: request.session_token.clone(),
            })
            .await?
            .into_inner();
        if private.my_move_submitted {
            continue;
        }
        let lobby = private.lobby.context("lobby snapshot missing")?;
        let player = lobby
            .players
            .iter()
            .find(|player| player.player_id == request.player_id)
            .context("bot player missing")?;
        let seat = u8::try_from(player.seat.context("bot seat missing")?)?;
        let game_proto = lobby.game.as_ref().context("game missing")?;
        let game = herdcore_protocol::game_from_proto(game_proto).map_err(anyhow::Error::msg)?;
        let action = strategy.choose(&game, seat);
        println!(
            "bot {} submitting {:?} for game {} turn {}",
            request.player_id, action, lobby.game_id, game.turn
        );
        client
            .submit_move(v1::SubmitMoveRequest {
                lobby_id: request.lobby_id.clone(),
                player_id: request.player_id.clone(),
                session_token: request.session_token.clone(),
                game_id: lobby.game_id,
                turn: game.turn,
                action: herdcore_protocol::action_to_proto(action) as i32,
                request_id: Uuid::new_v4().to_string(),
            })
            .await?;
    }
    anyhow::bail!("event stream ended")
}

#[tokio::main]
async fn main() -> Result<()> {
    let address: SocketAddr = env::var("HERDCORE_BOT_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:55052".to_owned())
        .parse()?;
    println!("Herdcore bot provider listening on {address}");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = Server::builder()
        .add_service(BotProviderServer::new(ProviderService::new()))
        .serve_with_shutdown(address, async {
            let _ = shutdown_rx.await;
        });
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            println!("Shutting down Herdcore bot provider…");
            let _ = shutdown_tx.send(());
            match tokio::time::timeout(Duration::from_secs(1), &mut server).await {
                Ok(result) => result?,
                Err(_) => eprintln!("Closing remaining bot tasks"),
            }
        }
    }
    Ok(())
}
