use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use herdcore_core::{is_action_legal, Action as CoreAction, GameState, Pos};
use herdcore_protocol::v1;
use herdcore_protocol::v1::herdcore_client::HerdcoreClient;
use tonic_web_wasm_client::Client as GrpcWebClient;
use uuid::Uuid;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    CanvasRenderingContext2d, Document, HtmlButtonElement, HtmlCanvasElement, HtmlInputElement,
    KeyboardEvent,
};

const SESSION_KEY: &str = "herdcore.session.v1";

#[derive(Clone)]
struct Session {
    server_url: String,
    lobby_id: String,
    player_id: String,
    token: String,
}

#[derive(Default)]
struct ClientState {
    session: Option<Session>,
    lobby: Option<v1::LobbySnapshot>,
    my_move_submitted: bool,
    status: String,
}

struct WebApp {
    document: Document,
    canvas: HtmlCanvasElement,
    state: RefCell<ClientState>,
    watch_generation: Cell<u32>,
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("document unavailable"))?;
    let canvas = element::<HtmlCanvasElement>(&document, "game")?;
    let app = Rc::new(WebApp {
        document,
        canvas,
        state: RefCell::new(ClientState::default()),
        watch_generation: Cell::new(0),
    });
    install_handlers(&app)?;
    app.restore_session();
    app.render_ui();
    if app.state.borrow().session.is_some() {
        let reconnecting = Rc::clone(&app);
        spawn_local(async move { reconnecting.reconnect().await });
    }
    Ok(())
}

impl WebApp {
    fn restore_session(&self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let Ok(Some(value)) = storage.get_item(SESSION_KEY) else {
            return;
        };
        let parts: Vec<&str> = value.split('|').collect();
        if parts.len() == 4 {
            self.state.borrow_mut().session = Some(Session {
                server_url: parts[0].to_owned(),
                lobby_id: parts[1].to_owned(),
                player_id: parts[2].to_owned(),
                token: parts[3].to_owned(),
            });
        }
    }

    fn persist_session(&self) {
        let Some(session) = self.state.borrow().session.clone() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let _ = storage.set_item(
            SESSION_KEY,
            &format!(
                "{}|{}|{}|{}",
                session.server_url, session.lobby_id, session.player_id, session.token
            ),
        );
    }

    fn forget_session(&self) {
        self.watch_generation
            .set(self.watch_generation.get().wrapping_add(1));
        *self.state.borrow_mut() = ClientState::default();
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                let _ = storage.remove_item(SESSION_KEY);
            }
        }
        self.render_ui();
    }

    async fn create_lobby(self: Rc<Self>) {
        let server_url = self.input("server-url");
        let request = v1::CreateLobbyRequest {
            display_name: self.input("display-name"),
            max_players: self.input("max-players").parse().unwrap_or(4),
            turn_seconds: self.input("turn-seconds").parse().unwrap_or(20),
        };
        self.set_status("Creating lobby…");
        let mut client = rpc_client(&server_url);
        match client.create_lobby(request).await {
            Ok(response) => self.accept_join(server_url, response.into_inner()),
            Err(error) => self.set_status(&format!("Create failed: {}", error.message())),
        }
    }

    async fn join_lobby(self: Rc<Self>) {
        let server_url = self.input("server-url");
        let request = v1::JoinLobbyRequest {
            lobby_code: self.input("lobby-code-input").to_ascii_uppercase(),
            display_name: self.input("display-name"),
        };
        self.set_status("Joining lobby…");
        let mut client = rpc_client(&server_url);
        match client.join_lobby(request).await {
            Ok(response) => self.accept_join(server_url, response.into_inner()),
            Err(error) => self.set_status(&format!("Join failed: {}", error.message())),
        }
    }

    fn accept_join(self: &Rc<Self>, server_url: String, response: v1::JoinLobbyResponse) {
        let Some(lobby) = response.lobby else {
            self.set_status("Server returned no lobby snapshot");
            return;
        };
        self.state.replace(ClientState {
            session: Some(Session {
                server_url,
                lobby_id: lobby.lobby_id.clone(),
                player_id: response.player_id,
                token: response.session_token,
            }),
            lobby: Some(lobby),
            my_move_submitted: false,
            status: "Connected".to_owned(),
        });
        self.persist_session();
        self.render_ui();
        self.begin_watch();
    }

    async fn reconnect(self: Rc<Self>) {
        let Some(session) = self.state.borrow().session.clone() else {
            return;
        };
        self.set_status("Reconnecting…");
        let mut client = rpc_client(&session.server_url);
        match client
            .get_lobby(v1::GetLobbyRequest {
                lobby_id: session.lobby_id.clone(),
                player_id: session.player_id.clone(),
                session_token: session.token.clone(),
            })
            .await
        {
            Ok(response) => {
                let private = response.into_inner();
                let mut state = self.state.borrow_mut();
                state.lobby = private.lobby;
                state.my_move_submitted = private.my_move_submitted;
                state.status = "Reconnected".to_owned();
                drop(state);
                self.render_ui();
                self.begin_watch();
            }
            Err(error) => self.set_status(&format!("Reconnect failed: {}", error.message())),
        }
    }

    fn begin_watch(self: &Rc<Self>) {
        let generation = self.watch_generation.get().wrapping_add(1);
        self.watch_generation.set(generation);
        let app = Rc::clone(self);
        spawn_local(async move { app.watch_loop(generation).await });
    }

    async fn watch_loop(self: Rc<Self>, generation: u32) {
        loop {
            if self.watch_generation.get() != generation {
                return;
            }
            let Some(session) = self.state.borrow().session.clone() else {
                return;
            };
            let mut client = rpc_client(&session.server_url);
            if let Ok(response) = client
                .get_lobby(v1::GetLobbyRequest {
                    lobby_id: session.lobby_id.clone(),
                    player_id: session.player_id.clone(),
                    session_token: session.token.clone(),
                })
                .await
            {
                let private = response.into_inner();
                let mut state = self.state.borrow_mut();
                state.lobby = private.lobby;
                state.my_move_submitted = private.my_move_submitted;
                drop(state);
                self.render_ui();
            }
            let after_version = self
                .state
                .borrow()
                .lobby
                .as_ref()
                .map(|lobby| lobby.public_version)
                .unwrap_or(0);
            match client
                .watch_lobby(v1::WatchLobbyRequest {
                    lobby_id: session.lobby_id,
                    player_id: session.player_id,
                    session_token: session.token,
                    after_version,
                })
                .await
            {
                Ok(response) => {
                    self.set_status("Connected");
                    let mut stream = response.into_inner();
                    loop {
                        if self.watch_generation.get() != generation {
                            return;
                        }
                        match stream.message().await {
                            Ok(Some(event)) => self.handle_event(event),
                            Ok(None) | Err(_) => break,
                        }
                    }
                }
                Err(error) => {
                    self.set_status(&format!("Stream disconnected: {}", error.message()));
                }
            }
            TimeoutFuture::new(1000).await;
        }
    }

    fn handle_event(&self, event: v1::LobbyEvent) {
        if event.kind == v1::LobbyEventKind::Heartbeat as i32 {
            return;
        }
        if let Some(lobby) = event.lobby {
            let mut state = self.state.borrow_mut();
            let current_version = state
                .lobby
                .as_ref()
                .map(|current| current.public_version)
                .unwrap_or(0);
            if lobby.public_version >= current_version {
                state.lobby = Some(lobby);
            }
            if event.kind == v1::LobbyEventKind::GameStarted as i32
                || event.kind == v1::LobbyEventKind::TurnResolved as i32
            {
                state.my_move_submitted = false;
                state.status = if event.kind == v1::LobbyEventKind::TurnResolved as i32 {
                    format!("Turn resolved: {} moves revealed", event.moves.len())
                } else {
                    "Game started—submit your move".to_owned()
                };
            }
        }
        self.render_ui();
    }

    async fn start_game(self: Rc<Self>) {
        let Some(session) = self.state.borrow().session.clone() else {
            return;
        };
        let mut client = rpc_client(&session.server_url);
        match client
            .start_game(v1::StartGameRequest {
                lobby_id: session.lobby_id,
                player_id: session.player_id,
                session_token: session.token,
            })
            .await
        {
            Ok(response) => {
                self.state.borrow_mut().lobby = Some(response.into_inner());
                self.set_status("Game started");
            }
            Err(error) => self.set_status(&format!("Start failed: {}", error.message())),
        }
    }

    async fn add_bot(self: Rc<Self>) {
        let Some(session) = self.state.borrow().session.clone() else {
            return;
        };
        self.set_status("Requesting CPU player…");
        let mut client = rpc_client(&session.server_url);
        match client
            .add_bot(v1::AddBotRequest {
                lobby_id: session.lobby_id,
                player_id: session.player_id,
                session_token: session.token,
                bot_type_id: "greedy-v1".to_owned(),
                display_name: "CPU".to_owned(),
            })
            .await
        {
            Ok(response) => {
                self.state.borrow_mut().lobby = response.into_inner().lobby;
                self.set_status("CPU added");
            }
            Err(error) => self.set_status(&format!("Add CPU failed: {}", error.message())),
        }
    }

    async fn submit_action(self: Rc<Self>, action: CoreAction) {
        let (session, lobby, already_submitted) = {
            let state = self.state.borrow();
            let (Some(session), Some(lobby)) = (state.session.clone(), state.lobby.clone()) else {
                return;
            };
            (session, lobby, state.my_move_submitted)
        };
        if already_submitted {
            self.set_status("Your move is already locked for this turn");
            return;
        }
        let Some(game_proto) = lobby.game.as_ref() else {
            return;
        };
        let Ok(game) = herdcore_protocol::game_from_proto(game_proto) else {
            self.set_status("Invalid game state from server");
            return;
        };
        let Some(player) = lobby
            .players
            .iter()
            .find(|player| player.player_id == session.player_id)
        else {
            return;
        };
        let Some(seat) = player.seat.and_then(|seat| u8::try_from(seat).ok()) else {
            return;
        };
        if !is_action_legal(&game, seat, action) {
            self.set_status("Blocked direction—choose another move or Stay");
            return;
        }
        self.set_status("Submitting move…");
        let mut client = rpc_client(&session.server_url);
        match client
            .submit_move(v1::SubmitMoveRequest {
                lobby_id: session.lobby_id,
                player_id: session.player_id,
                session_token: session.token,
                game_id: lobby.game_id,
                turn: game.turn,
                action: herdcore_protocol::action_to_proto(action) as i32,
                request_id: Uuid::new_v4().to_string(),
            })
            .await
        {
            Ok(_) => {
                let still_same_turn = self
                    .state
                    .borrow()
                    .lobby
                    .as_ref()
                    .and_then(|current| current.game.as_ref())
                    .is_some_and(|current| current.turn == game.turn);
                if still_same_turn {
                    self.state.borrow_mut().my_move_submitted = true;
                    self.set_status("Move committed—waiting for the other players");
                }
                self.render_ui();
            }
            Err(error) => self.set_status(&format!("Move rejected: {}", error.message())),
        }
    }

    fn render_ui(&self) {
        let state = self.state.borrow();
        let connected = state.session.is_some();
        self.set_hidden("connect-panel", connected);
        self.set_hidden("lobby-panel", !connected);
        self.text("status", &state.status);

        if let Some(lobby) = &state.lobby {
            self.text("lobby-code", &lobby.lobby_code);
            let players = lobby
                .players
                .iter()
                .map(|player| {
                    let kind = if player.kind == v1::PlayerKind::Bot as i32 {
                        "CPU"
                    } else {
                        "Human"
                    };
                    let seat = player
                        .seat
                        .map(|seat| format!("seat {}", seat + 1))
                        .unwrap_or_else(|| "unseated".to_owned());
                    format!("{} — {} — {}", player.display_name, kind, seat)
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.text("players", &players);
            let is_host = state
                .session
                .as_ref()
                .is_some_and(|session| session.player_id == lobby.host_player_id);
            let waiting = lobby.phase == v1::LobbyPhase::Waiting as i32;
            self.set_hidden("start", !(is_host && waiting));
            self.set_hidden("add-bot", !(is_host && waiting));
            let playing = lobby.game.is_some();
            self.set_hidden("game", !playing);
            self.set_hidden("controls", !playing);
            for id in ["up", "down", "left", "right", "stay"] {
                if let Ok(button) = element::<HtmlButtonElement>(&self.document, id) {
                    button.set_disabled(
                        state.my_move_submitted || lobby.phase != v1::LobbyPhase::Playing as i32,
                    );
                }
            }
            if let Some(game_proto) = &lobby.game {
                if let Ok(game) = herdcore_protocol::game_from_proto(game_proto) {
                    let _ = render_game(&self.canvas, &game, lobby, state.session.as_ref());
                }
            }
        }
    }

    fn set_status(&self, message: &str) {
        self.state.borrow_mut().status = message.to_owned();
        self.text("status", message);
    }

    fn input(&self, id: &str) -> String {
        element::<HtmlInputElement>(&self.document, id)
            .map(|input| input.value())
            .unwrap_or_default()
    }

    fn text(&self, id: &str, value: &str) {
        if let Some(element) = self.document.get_element_by_id(id) {
            element.set_text_content(Some(value));
        }
    }

    fn set_hidden(&self, id: &str, hidden: bool) {
        if let Some(element) = self.document.get_element_by_id(id) {
            let base = if id == "connect-panel" || id == "lobby-panel" {
                "panel"
            } else {
                ""
            };
            let class = if hidden {
                format!("{base} hidden")
            } else {
                base.to_owned()
            };
            element.set_class_name(class.trim());
        }
    }
}

fn install_handlers(app: &Rc<WebApp>) -> Result<(), JsValue> {
    install_async_button(app, "create", |app| async move { app.create_lobby().await })?;
    install_async_button(app, "join", |app| async move { app.join_lobby().await })?;
    install_async_button(app, "start", |app| async move { app.start_game().await })?;
    install_async_button(app, "add-bot", |app| async move { app.add_bot().await })?;
    install_async_button(app, "reconnect", |app| async move { app.reconnect().await })?;

    let forget_app = Rc::clone(app);
    let forget = Closure::<dyn FnMut()>::new(move || forget_app.forget_session());
    element::<HtmlButtonElement>(&app.document, "forget")?
        .add_event_listener_with_callback("click", forget.as_ref().unchecked_ref())?;
    forget.forget();

    for (id, action) in [
        ("up", CoreAction::Up),
        ("down", CoreAction::Down),
        ("left", CoreAction::Left),
        ("right", CoreAction::Right),
        ("stay", CoreAction::Stay),
    ] {
        let action_app = Rc::clone(app);
        let closure = Closure::<dyn FnMut()>::new(move || {
            let app = Rc::clone(&action_app);
            spawn_local(async move { app.submit_action(action).await });
        });
        element::<HtmlButtonElement>(&app.document, id)?
            .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    let keyboard_app = Rc::clone(app);
    let keyboard = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if let Some(active) = keyboard_app.document.active_element() {
            if active.tag_name() == "INPUT" {
                return;
            }
        }
        let action = match event.key().to_ascii_lowercase().as_str() {
            "arrowup" | "w" => Some(CoreAction::Up),
            "arrowdown" | "s" => Some(CoreAction::Down),
            "arrowleft" | "a" => Some(CoreAction::Left),
            "arrowright" | "d" => Some(CoreAction::Right),
            " " | "spacebar" => Some(CoreAction::Stay),
            _ => None,
        };
        if let Some(action) = action {
            event.prevent_default();
            let app = Rc::clone(&keyboard_app);
            spawn_local(async move { app.submit_action(action).await });
        }
    });
    app.document
        .add_event_listener_with_callback("keydown", keyboard.as_ref().unchecked_ref())?;
    keyboard.forget();

    let resize_app = Rc::clone(app);
    let resize = Closure::<dyn FnMut()>::new(move || resize_app.render_ui());
    web_sys::window()
        .unwrap()
        .add_event_listener_with_callback("resize", resize.as_ref().unchecked_ref())?;
    resize.forget();
    Ok(())
}

fn install_async_button<F, Fut>(app: &Rc<WebApp>, id: &str, callback: F) -> Result<(), JsValue>
where
    F: Fn(Rc<WebApp>) -> Fut + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let button_app = Rc::clone(app);
    let closure = Closure::<dyn FnMut()>::new(move || {
        spawn_local(callback(Rc::clone(&button_app)));
    });
    element::<HtmlButtonElement>(&app.document, id)?
        .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn rpc_client(base_url: &str) -> HerdcoreClient<GrpcWebClient> {
    HerdcoreClient::new(GrpcWebClient::new(base_url.to_owned()))
}

fn element<T: JsCast>(document: &Document, id: &str) -> Result<T, JsValue> {
    let element = document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing #{id}")))?;
    element
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("#{id} has the wrong element type")))
}

const COLORS: [&str; 16] = [
    "#4b91f1", "#ed5c5c", "#e6b84b", "#a879e8", "#35bfa4", "#ee8548", "#e56cae", "#8bc34a",
    "#42c7dd", "#c78452", "#778eea", "#e07878", "#bdd45a", "#876ad1", "#48b77a", "#d86b45",
];

fn render_game(
    canvas: &HtmlCanvasElement,
    game: &GameState,
    lobby: &v1::LobbySnapshot,
    session: Option<&Session>,
) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
    let viewport_width = window.inner_width()?.as_f64().unwrap_or(900.0);
    let logical_width = (viewport_width - 16.0).clamp(320.0, 980.0);
    let board_size = logical_width - 20.0;
    let logical_height = board_size + 155.0;
    let ratio = window.device_pixel_ratio().max(1.0);
    canvas.set_width((logical_width * ratio) as u32);
    canvas.set_height((logical_height * ratio) as u32);
    canvas.set_attribute(
        "style",
        &format!(
            "width:{logical_width}px;height:{logical_height}px;max-width:100vw;max-height:82vh"
        ),
    )?;
    let context: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("2d context unavailable"))?
        .dyn_into()?;
    context.set_transform(ratio, 0.0, 0.0, ratio, 0.0, 0.0)?;
    fill(&context, "#171a20");
    context.fill_rect(0.0, 0.0, logical_width, logical_height);
    context.set_text_align("center");
    context.set_text_baseline("middle");
    context.set_font("700 25px system-ui, sans-serif");
    fill(&context, "#f4f6f8");
    context.fill_text("HERDCORE", logical_width / 2.0, 23.0)?;
    context.set_font("14px system-ui, sans-serif");
    fill(&context, "#bbc1cb");
    context.fill_text(
        &format!("Turn {}  •  Sheep {}", game.turn, game.sheep.len()),
        logical_width / 2.0,
        50.0,
    )?;

    let board_x = 10.0;
    let board_y = 68.0;
    let cell = board_size / f64::from(game.width);
    for y in 0..game.height {
        for x in 0..game.width {
            let pos = Pos::new(x, y);
            let pen_owner = game
                .players
                .iter()
                .position(|player| player.pen.contains(&pos));
            fill(&context, pen_owner.map(dark_color).unwrap_or("#3e5d3c"));
            context.fill_rect(
                board_x + f64::from(x) * cell,
                board_y + f64::from(y) * cell,
                cell,
                cell,
            );
            stroke(&context, "#253725");
            context.set_line_width(0.7);
            context.stroke_rect(
                board_x + f64::from(x) * cell + 0.35,
                board_y + f64::from(y) * cell + 0.35,
                cell - 0.7,
                cell - 0.7,
            );
        }
    }
    for sheep in &game.sheep {
        let (x, y) = center(*sheep, board_x, board_y, cell);
        circle(&context, x, y, cell * 0.31, "#f3f0e7", "#aaa9a3");
    }
    for (index, player) in game.players.iter().enumerate() {
        let (x, y) = center(player.dog, board_x, board_y, cell);
        circle(
            &context,
            x,
            y,
            cell * 0.38,
            COLORS[index % COLORS.len()],
            "#ffffff",
        );
        if cell >= 15.0 {
            context.set_font(&format!("700 {}px system-ui, sans-serif", cell * 0.38));
            fill(&context, "#ffffff");
            let _ = context.fill_text(&(index + 1).to_string(), x, y + 0.5);
        }
    }

    let own_seat = session.and_then(|session| {
        lobby
            .players
            .iter()
            .find(|player| player.player_id == session.player_id)
            .and_then(|player| player.seat)
    });
    let scores = game
        .players
        .iter()
        .map(|player| {
            let marker = if Some(u32::from(player.seat)) == own_seat {
                "You"
            } else {
                "P"
            };
            format!("{marker}{}:{}", u32::from(player.seat) + 1, player.score)
        })
        .collect::<Vec<_>>()
        .join("  ");
    context.set_font("14px system-ui, sans-serif");
    fill(&context, "#dce1e9");
    context.fill_text(&scores, logical_width / 2.0, board_y + board_size + 24.0)?;
    Ok(())
}

fn center(pos: Pos, board_x: f64, board_y: f64, cell: f64) -> (f64, f64) {
    (
        board_x + (f64::from(pos.x) + 0.5) * cell,
        board_y + (f64::from(pos.y) + 0.5) * cell,
    )
}

fn circle(
    context: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    radius: f64,
    color: &str,
    edge: &str,
) {
    context.begin_path();
    let _ = context.arc(x, y, radius, 0.0, PI * 2.0);
    fill(context, color);
    context.fill();
    stroke(context, edge);
    context.set_line_width(1.2);
    context.stroke();
}

fn dark_color(index: usize) -> &'static str {
    const DARK: [&str; 16] = [
        "#294766", "#663535", "#66592d", "#4e3b67", "#285c52", "#66432e", "#633750", "#425d2d",
        "#285966", "#5e4633", "#3b4566", "#663d3d", "#56632f", "#453962", "#315c44", "#633d2e",
    ];
    DARK[index % DARK.len()]
}

#[allow(deprecated)]
fn fill(context: &CanvasRenderingContext2d, color: &str) {
    context.set_fill_style_str(color);
}

#[allow(deprecated)]
fn stroke(context: &CanvasRenderingContext2d, color: &str) {
    context.set_stroke_style_str(color);
}
