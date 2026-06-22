//! Route view components: header, home, join panel, lobby page, and game page.
//! Actions send WebSocket command frames through the shared [`Connection`];
//! state arrives back through the reducer the connection drives.

use herdcore_core::{is_action_legal, Action as CoreAction};
use herdcore_protocol::v1::client_frame;
use herdcore_protocol::v1;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlInputElement, KeyboardEvent};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api::Session;
use crate::names;
use crate::render;
use crate::state::{AppAction, AppHandle};
use crate::ws::Connection;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn frame(body: client_frame::Body) -> v1::ClientFrame {
    v1::ClientFrame { body: Some(body) }
}

fn die_svg() -> Html {
    let svg = r##"<svg class="die" viewBox="0 0 32 32" width="26" height="26" shape-rendering="crispEdges" aria-hidden="true"><polygon points="16,4 28,11 16,18 4,11" fill="#cfe07a"/><polygon points="4,11 16,18 16,29 4,22" fill="#8bac0f"/><polygon points="16,18 28,11 28,22 16,29" fill="#306230"/><rect x="15" y="10" width="2" height="2" fill="#0f380f"/><rect x="8" y="15" width="2" height="2" fill="#0f380f"/><rect x="11" y="21" width="2" height="2" fill="#0f380f"/><rect x="20" y="18" width="2" height="2" fill="#cfe07a"/><rect x="23" y="15" width="2" height="2" fill="#cfe07a"/></svg>"##;
    Html::from_html_unchecked(AttrValue::from(svg))
}

fn input_value(node: &NodeRef) -> String {
    node.cast::<HtmlInputElement>()
        .map(|input| input.value())
        .unwrap_or_default()
}

fn prefill(node: &NodeRef, value: String) {
    if let Some(input) = node.cast::<HtmlInputElement>() {
        if input.value().trim().is_empty() {
            input.set_value(&value);
        }
    }
}

// ---------------------------------------------------------------------------
// Header (title doubles as home/leave) + status line
// ---------------------------------------------------------------------------

#[function_component(Header)]
pub fn header() -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let connection = use_context::<Connection>().expect("connection");
    let navigator = use_navigator().expect("navigator");

    let on_home = {
        let connection = connection.clone();
        Callback::from(move |_: MouseEvent| {
            connection.leave();
            navigator.push(&crate::app::Route::Home);
        })
    };

    html! {
        <>
            <h1 id="home" title="Leave and go home" onclick={on_home}>{ "HERDCORE" }</h1>
            <div id="status">{ state.status.clone() }</div>
        </>
    }
}

// ---------------------------------------------------------------------------
// Home: pick a name + lobby word
// ---------------------------------------------------------------------------

#[function_component(Home)]
pub fn home() -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let connection = use_context::<Connection>().expect("connection");
    let name_ref = use_node_ref();
    let lobby_ref = use_node_ref();

    {
        let name_ref = name_ref.clone();
        let lobby_ref = lobby_ref.clone();
        use_effect_with((), move |_| {
            prefill(&name_ref, names::random_player_name());
            prefill(&lobby_ref, names::random_lobby_slug());
            || ()
        });
    }

    let reroll_name = {
        let name_ref = name_ref.clone();
        Callback::from(move |_: MouseEvent| {
            if let Some(input) = name_ref.cast::<HtmlInputElement>() {
                input.set_value(&names::random_player_name());
            }
        })
    };
    let reroll_lobby = {
        let lobby_ref = lobby_ref.clone();
        Callback::from(move |_: MouseEvent| {
            if let Some(input) = lobby_ref.cast::<HtmlInputElement>() {
                input.set_value(&names::random_lobby_slug());
            }
        })
    };

    let on_start = {
        let state = state.clone();
        let connection = connection.clone();
        let name_ref = name_ref.clone();
        let lobby_ref = lobby_ref.clone();
        Callback::from(move |_: MouseEvent| {
            let name = input_value(&name_ref);
            let word = input_value(&lobby_ref);
            if name.trim().is_empty() {
                state.dispatch(AppAction::Status("Pick a name first".into()));
                return;
            }
            if word.trim().is_empty() {
                state.dispatch(AppAction::Status("Type or roll a lobby name".into()));
                return;
            }
            connection.join(word, name);
        })
    };

    html! {
        <section class="panel">
            <div class="field">
                <label class="cap" for="display-name">{ "YOUR NAME" }</label>
                <div class="row">
                    <input ref={name_ref} id="display-name" maxlength="24" autocomplete="off"
                        placeholder="who goes there" />
                    <button class="dice" onclick={reroll_name} title="Random name">{ die_svg() }</button>
                </div>
            </div>
            <button class="primary" onclick={on_start}>{ "START GAME" }</button>
            <div class="field">
                <label class="cap" for="lobby-name">{ "LOBBY NAME" }</label>
                <div class="row">
                    <input ref={lobby_ref} id="lobby-name" maxlength="32" autocomplete="off"
                        placeholder="lobby word" />
                    <button class="dice" onclick={reroll_lobby} title="Random lobby">{ die_svg() }</button>
                </div>
            </div>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Join panel: shown when a lobby/game URL is opened without a session
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct JoinProps {
    pub lobby: String,
}

#[function_component(JoinPanel)]
pub fn join_panel(props: &JoinProps) -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let connection = use_context::<Connection>().expect("connection");
    let word = props.lobby.clone();
    let name_ref = use_node_ref();

    {
        let name_ref = name_ref.clone();
        use_effect_with((), move |_| {
            prefill(&name_ref, names::random_player_name());
            || ()
        });
    }

    let on_join = {
        let state = state.clone();
        let connection = connection.clone();
        let name_ref = name_ref.clone();
        let word = word.clone();
        Callback::from(move |_: MouseEvent| {
            let name = input_value(&name_ref);
            if name.trim().is_empty() {
                state.dispatch(AppAction::Status("Pick a name first".into()));
                return;
            }
            connection.join(word.clone(), name);
        })
    };

    html! {
        <section class="panel">
            <div class="lobby-head">{ "JOIN LOBBY" }</div>
            <div class="lobby-code">{ &word }</div>
            <div class="field">
                <label class="cap">{ "YOUR NAME" }</label>
                <div class="row">
                    <input ref={name_ref} maxlength="24" autocomplete="off" placeholder="who goes there" />
                </div>
            </div>
            <button class="primary" onclick={on_join}>{ format!("JOIN {}", word.to_uppercase()) }</button>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Lobby: roster + game history + host controls
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct LobbyProps {
    pub lobby: String,
}

#[function_component(LobbyView)]
pub fn lobby_view(props: &LobbyProps) -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let connection = use_context::<Connection>().expect("connection");
    let word = props.lobby.clone();

    // Refresh the games list whenever a game starts or ends.
    {
        let connection = connection.clone();
        let key = state
            .lobby
            .as_ref()
            .map(|l| (l.game_id, l.phase))
            .unwrap_or((0, 0));
        use_effect_with(key, move |_| {
            connection.send(frame(client_frame::Body::ListGames(v1::ListGamesCommand {})));
            || ()
        });
    }

    if !state.is_member_of(&word) {
        return html! { <JoinPanel lobby={word} /> };
    }
    let lobby = match state.lobby.clone() {
        Some(lobby) => lobby,
        None => return html! { <section class="panel">{ "Loading lobby…" }</section> },
    };

    let is_host = state
        .session
        .as_ref()
        .is_some_and(|session| session.player_id == lobby.host_player_id);
    let waiting = lobby.phase == v1::LobbyPhase::Waiting as i32;
    let finished = lobby.phase == v1::LobbyPhase::Finished as i32;
    let can_manage = is_host && (waiting || finished);

    let on_start = {
        let connection = connection.clone();
        Callback::from(move |_: MouseEvent| {
            connection.send(frame(client_frame::Body::Start(v1::StartCommand {})));
        })
    };
    let on_remove_bot = {
        let connection = connection.clone();
        Callback::from(move |bot_player_id: String| {
            connection.send(frame(client_frame::Body::RemoveBot(v1::RemoveBotCommand {
                bot_player_id,
            })));
        })
    };

    let start_label = if finished { "PLAY AGAIN" } else { "START GAME" };

    html! {
        <section class="panel">
            <div class="lobby-head">{ "LOBBY" }</div>
            <div class="lobby-code">{ &word }</div>

            <div class="section-label">{ "PLAYERS" }</div>
            <div class="players">{ roster(&lobby, state.session.as_ref(), can_manage, on_remove_bot) }</div>

            <div class="section-label">{ "GAMES" }</div>
            <div class="games">{ games_list(&state.games, &lobby) }</div>

            if can_manage {
                <div class="bot-row">{ bot_buttons(&state.catalogue, &connection) }</div>
                <div class="start-row">
                    <button onclick={on_start}>{ start_label }</button>
                </div>
            } else if waiting {
                <div class="hint">{ "Waiting for the host to start…" }</div>
            }
        </section>
    }
}

/// One "+ Name" button per catalogue bot, on a single row.
fn bot_buttons(catalogue: &[v1::BotKind], connection: &Connection) -> Html {
    html! {
        { for catalogue.iter().map(|kind| {
            let connection = connection.clone();
            let bot_type_id = kind.id.clone();
            let display_name = kind.name.clone();
            let label = format!("+ {}", kind.name);
            let address: AttrValue = kind.address.clone().into();
            let onclick = Callback::from(move |_: MouseEvent| {
                connection.send(frame(client_frame::Body::AddBot(v1::AddBotCommand {
                    display_name: display_name.clone(),
                    bot_type_id: bot_type_id.clone(),
                    url: String::new(),
                })));
            });
            html! { <button class="bot-btn" title={address} {onclick}>{ label }</button> }
        }) }
    }
}

fn roster(
    lobby: &v1::LobbySnapshot,
    session: Option<&Session>,
    can_manage: bool,
    on_remove: Callback<String>,
) -> Html {
    html! {
        { for lobby.players.iter().map(|player| {
            let me = session.is_some_and(|s| s.player_id == player.player_id);
            let host = player.player_id == lobby.host_player_id;
            let is_bot = player.kind == v1::PlayerKind::Bot as i32;
            let mut tags = Vec::new();
            if host { tags.push("HOST"); }
            if me { tags.push("YOU"); }
            if is_bot { tags.push("CPU"); }
            let position = match player.seat {
                Some(seat) => format!("seat {}", seat + 1),
                None if lobby.phase == v1::LobbyPhase::Playing as i32 => "spectating".to_owned(),
                None => "ready".to_owned(),
            };
            let tag_text = if tags.is_empty() { String::new() } else { format!("[{}] ", tags.join(" ")) };
            let label = format!("{} {}{}", player.display_name, tag_text, position);
            let remove = if can_manage && is_bot {
                let on_remove = on_remove.clone();
                let pid = player.player_id.clone();
                let onclick = Callback::from(move |_: MouseEvent| on_remove.emit(pid.clone()));
                html! { <button class="remove-bot" title="Remove CPU" {onclick}>{ "×" }</button> }
            } else {
                html! {}
            };
            html! { <div class="player-row"><span>{ label }</span>{ remove }</div> }
        }) }
    }
}

fn games_list(games: &[v1::GameSummary], lobby: &v1::LobbySnapshot) -> Html {
    if games.is_empty() {
        return html! { <div class="hint">{ "No games yet — start one!" }</div> };
    }
    html! {
        { for games.iter().map(|game| {
            let finished = game.status == v1::LobbyPhase::Finished as i32;
            let detail = if !finished {
                "in progress".to_owned()
            } else if game.winners.is_empty() {
                "no winner".to_owned()
            } else {
                let names = game.winners.iter().map(|seat| {
                    lobby.players.iter()
                        .find(|p| p.seat == Some(*seat))
                        .map(|p| p.display_name.clone())
                        .unwrap_or_else(|| format!("Seat {}", seat + 1))
                }).collect::<Vec<_>>().join(", ");
                format!("won by {names}")
            };
            html! { <div class="game-row">{ format!("Game {} — {}", game.game_id, detail) }</div> }
        }) }
    }
}

// ---------------------------------------------------------------------------
// Game: the live board + D-pad
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct GameProps {
    pub lobby: String,
    pub game: u64,
}

#[function_component(GameView)]
pub fn game_view(props: &GameProps) -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let connection = use_context::<Connection>().expect("connection");
    let word = props.lobby.clone();

    let canvas_ref = use_node_ref();
    let version = state.lobby.as_ref().map(|l| l.public_version).unwrap_or(0);

    {
        let canvas_ref = canvas_ref.clone();
        let lobby = state.lobby.clone();
        let my_seat = state.my_seat();
        use_effect_with(version, move |_| {
            if let (Some(canvas), Some(lobby)) =
                (canvas_ref.cast::<HtmlCanvasElement>(), lobby.as_ref())
            {
                if let Some(game_proto) = &lobby.game {
                    if let Ok(game) = herdcore_protocol::game_from_proto(game_proto) {
                        let _ = render::render_game(&canvas, &game, my_seat);
                    }
                }
            }
            || ()
        });
    }

    // Keyboard controls; re-registered per snapshot so it sees fresh state.
    {
        let state = state.clone();
        let connection = connection.clone();
        use_effect_with(version, move |_| {
            let state = state.clone();
            let connection = connection.clone();
            let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
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
                    submit_action(&state, &connection, action);
                }
            });
            if let Some(window) = web_sys::window() {
                let _ = window
                    .add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
            }
            move || {
                if let Some(window) = web_sys::window() {
                    let _ = window.remove_event_listener_with_callback(
                        "keydown",
                        listener.as_ref().unchecked_ref(),
                    );
                }
                drop(listener);
            }
        });
    }

    if !state.is_member_of(&word) {
        return html! { <JoinPanel lobby={word} /> };
    }

    let lobby = state.lobby.clone();
    let playing = lobby
        .as_ref()
        .is_some_and(|l| l.phase == v1::LobbyPhase::Playing as i32);
    let have_seat = state.my_seat().is_some();
    let disabled = state.my_move_submitted || !playing || !have_seat;

    let hud = lobby
        .as_ref()
        .and_then(|l| l.game.as_ref())
        .and_then(|g| herdcore_protocol::game_from_proto(g).ok())
        .map(|game| render::hud_text(&game, state.my_seat()))
        .unwrap_or_default();

    html! {
        <>
            <div id="hud">{ hud }</div>
            <canvas id="game" ref={canvas_ref} aria-label="Herdcore game board"></canvas>
            <div id="controls">
                <button id="up" disabled={disabled} onclick={action_cb(&state, &connection, CoreAction::Up)} aria-label="Up"></button>
                <button id="left" disabled={disabled} onclick={action_cb(&state, &connection, CoreAction::Left)} aria-label="Left"></button>
                <button id="stay" disabled={disabled} onclick={action_cb(&state, &connection, CoreAction::Stay)}>{ "STAY" }</button>
                <button id="right" disabled={disabled} onclick={action_cb(&state, &connection, CoreAction::Right)} aria-label="Right"></button>
                <button id="down" disabled={disabled} onclick={action_cb(&state, &connection, CoreAction::Down)} aria-label="Down"></button>
            </div>
        </>
    }
}

fn action_cb(state: &AppHandle, connection: &Connection, action: CoreAction) -> Callback<MouseEvent> {
    let state = state.clone();
    let connection = connection.clone();
    Callback::from(move |_: MouseEvent| submit_action(&state, &connection, action))
}

fn submit_action(state: &AppHandle, connection: &Connection, action: CoreAction) {
    let (Some(session), Some(lobby)) = (state.session.clone(), state.lobby.clone()) else {
        return;
    };
    if state.my_move_submitted {
        return;
    }
    let Some(game_proto) = lobby.game.as_ref() else {
        return;
    };
    let Ok(game) = herdcore_protocol::game_from_proto(game_proto) else {
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
        state.dispatch(AppAction::Status("You're spectating—next game".into()));
        return;
    };
    if !is_action_legal(&game, seat, action) {
        state.dispatch(AppAction::Status("Blocked direction—try another move or stay".into()));
        return;
    }
    connection.send(frame(client_frame::Body::Move(v1::MoveCommand {
        game_id: lobby.game_id,
        turn: game.turn,
        action: herdcore_protocol::action_to_proto(action) as i32,
        request_id: uuid::Uuid::new_v4().to_string(),
    })));
    state.dispatch(AppAction::SetSubmitted(true));
    state.dispatch(AppAction::Status("Move committed—waiting for the herd".into()));
}
