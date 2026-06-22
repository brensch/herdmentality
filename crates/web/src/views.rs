//! Route view components: header, home, join panel, lobby page, and game page.

use herdcore_core::{is_action_legal, Action as CoreAction};
use herdcore_protocol::v1;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlCanvasElement, HtmlInputElement, KeyboardEvent};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api::{self, Session};
use crate::names;
use crate::render;
use crate::state::{AppAction, AppHandle};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The dot-matrix isometric die icon used on the reroll buttons.
fn die_svg() -> Html {
    let svg = r##"<svg class="die" viewBox="0 0 32 32" width="26" height="26" shape-rendering="crispEdges" aria-hidden="true"><polygon points="16,4 28,11 16,18 4,11" fill="#cfe07a"/><polygon points="4,11 16,18 16,29 4,22" fill="#8bac0f"/><polygon points="16,18 28,11 28,22 16,29" fill="#306230"/><rect x="15" y="10" width="2" height="2" fill="#0f380f"/><rect x="8" y="15" width="2" height="2" fill="#0f380f"/><rect x="11" y="21" width="2" height="2" fill="#0f380f"/><rect x="20" y="18" width="2" height="2" fill="#cfe07a"/><rect x="23" y="15" width="2" height="2" fill="#cfe07a"/></svg>"##;
    Html::from_html_unchecked(AttrValue::from(svg))
}

/// Join or create `word` as `name`, store the session, and update state. The
/// navigation controller then routes us into the lobby or the live game.
async fn join_lobby(word: String, name: String, state: AppHandle) {
    let mut client = api::rpc_client();
    match client
        .join_or_create_lobby(v1::JoinOrCreateLobbyRequest {
            lobby_name: word,
            display_name: name,
        })
        .await
    {
        Ok(response) => {
            let response = response.into_inner();
            if let Some(lobby) = response.lobby {
                let session = Session {
                    lobby_id: lobby.lobby_id.clone(),
                    player_id: response.player_id,
                    token: response.session_token,
                    word: lobby.lobby_code.clone(),
                };
                api::save_session(&session);
                state.dispatch(AppAction::Joined { session, lobby });
            } else {
                state.dispatch(AppAction::Status("Server returned no lobby".into()));
            }
        }
        Err(error) => {
            state.dispatch(AppAction::Status(format!("Could not enter: {}", error.message())))
        }
    }
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
    let navigator = use_navigator().expect("navigator");

    let on_home = {
        let state = state.clone();
        Callback::from(move |_: MouseEvent| {
            if let Some(session) = state.session.clone() {
                spawn_local(async move {
                    let mut client = api::rpc_client();
                    let _ = client
                        .leave_lobby(v1::LeaveLobbyRequest {
                            lobby_id: session.lobby_id,
                            player_id: session.player_id,
                            session_token: session.token,
                        })
                        .await;
                });
            }
            api::clear_session();
            state.dispatch(AppAction::Cleared);
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
            let state = state.clone();
            spawn_local(async move { join_lobby(word, name, state).await });
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
        let name_ref = name_ref.clone();
        let word = word.clone();
        Callback::from(move |_: MouseEvent| {
            let name = input_value(&name_ref);
            if name.trim().is_empty() {
                state.dispatch(AppAction::Status("Pick a name first".into()));
                return;
            }
            let state = state.clone();
            let word = word.clone();
            spawn_local(async move { join_lobby(word, name, state).await });
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
// Lobby: roster + game history + host controls (the waiting/results room)
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct LobbyProps {
    pub lobby: String,
}

#[function_component(LobbyView)]
pub fn lobby_view(props: &LobbyProps) -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let word = props.lobby.clone();

    let games = use_state(Vec::<v1::GameSummary>::new);
    {
        let games = games.clone();
        let session = state.session.clone();
        let key = state
            .lobby
            .as_ref()
            .map(|l| (l.game_id, l.phase))
            .unwrap_or((0, 0));
        use_effect_with(key, move |_| {
            if let Some(session) = session {
                let games = games.clone();
                spawn_local(async move {
                    let mut client = api::rpc_client();
                    if let Ok(response) = client
                        .list_games(v1::GetLobbyRequest {
                            lobby_id: session.lobby_id,
                            player_id: session.player_id,
                            session_token: session.token,
                        })
                        .await
                    {
                        games.set(response.into_inner().games);
                    }
                });
            }
            || ()
        });
    }

    // After all hooks: non-members get the join panel.
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
        let state = state.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(session) = state.session.clone() else {
                return;
            };
            let state = state.clone();
            spawn_local(async move {
                let mut client = api::rpc_client();
                match client
                    .start_game(v1::StartGameRequest {
                        lobby_id: session.lobby_id,
                        player_id: session.player_id,
                        session_token: session.token,
                    })
                    .await
                {
                    Ok(response) => state.dispatch(AppAction::SetLobby {
                        lobby: response.into_inner(),
                        my_move_submitted: false,
                    }),
                    Err(error) => {
                        state.dispatch(AppAction::Status(format!("Start failed: {}", error.message())))
                    }
                }
            });
        })
    };

    let on_add_bot = {
        let state = state.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(session) = state.session.clone() else {
                return;
            };
            let state = state.clone();
            spawn_local(async move {
                let mut client = api::rpc_client();
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
                        if let Some(lobby) = response.into_inner().lobby {
                            state.dispatch(AppAction::SetLobby {
                                lobby,
                                my_move_submitted: false,
                            });
                        }
                    }
                    Err(error) => state
                        .dispatch(AppAction::Status(format!("Add CPU failed: {}", error.message()))),
                }
            });
        })
    };

    let start_label = if finished { "PLAY AGAIN" } else { "START GAME" };

    html! {
        <section class="panel">
            <div class="lobby-head">{ "LOBBY" }</div>
            <div class="lobby-code">{ &word }</div>

            <div class="section-label">{ "PLAYERS" }</div>
            <div class="players">{ roster(&lobby, state.session.as_ref()) }</div>

            <div class="section-label">{ "GAMES" }</div>
            <div class="games">{ games_list(&games, &lobby) }</div>

            if can_manage {
                <div class="actions">
                    <button onclick={on_start}>{ start_label }</button>
                    <button onclick={on_add_bot}>{ "ADD CPU" }</button>
                </div>
            } else if waiting {
                <div class="hint">{ "Waiting for the host to start…" }</div>
            }
        </section>
    }
}

fn roster(lobby: &v1::LobbySnapshot, session: Option<&Session>) -> Html {
    html! {
        { for lobby.players.iter().map(|player| {
            let me = session.is_some_and(|s| s.player_id == player.player_id);
            let host = player.player_id == lobby.host_player_id;
            let mut tags = Vec::new();
            if host { tags.push("HOST"); }
            if me { tags.push("YOU"); }
            if player.kind == v1::PlayerKind::Bot as i32 { tags.push("CPU"); }
            let position = match player.seat {
                Some(seat) => format!("seat {}", seat + 1),
                None if lobby.phase == v1::LobbyPhase::Playing as i32 => "spectating".to_owned(),
                None => "ready".to_owned(),
            };
            let tag_text = if tags.is_empty() { String::new() } else { format!("[{}] ", tags.join(" ")) };
            html! { <div class="player-row">{ format!("{} {}{}", player.display_name, tag_text, position) }</div> }
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
    let word = props.lobby.clone();

    let canvas_ref = use_node_ref();
    let version = state.lobby.as_ref().map(|l| l.public_version).unwrap_or(0);

    // Redraw the canvas whenever the lobby snapshot changes.
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

    // Keyboard controls; re-registered per snapshot so it always sees fresh state.
    {
        let state = state.clone();
        use_effect_with(version, move |_| {
            let key_state = state.clone();
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
                    let state = key_state.clone();
                    spawn_local(async move { submit_action(&state, action).await });
                }
            });
            let window = web_sys::window().unwrap();
            let _ = window
                .add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
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

    // Membership check comes after all hooks so the hook order never changes.
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
                <button id="up" disabled={disabled} onclick={action_cb(&state, CoreAction::Up)} aria-label="Up"></button>
                <button id="left" disabled={disabled} onclick={action_cb(&state, CoreAction::Left)} aria-label="Left"></button>
                <button id="stay" disabled={disabled} onclick={action_cb(&state, CoreAction::Stay)}>{ "STAY" }</button>
                <button id="right" disabled={disabled} onclick={action_cb(&state, CoreAction::Right)} aria-label="Right"></button>
                <button id="down" disabled={disabled} onclick={action_cb(&state, CoreAction::Down)} aria-label="Down"></button>
            </div>
        </>
    }
}

fn action_cb(state: &AppHandle, action: CoreAction) -> Callback<MouseEvent> {
    let state = state.clone();
    Callback::from(move |_: MouseEvent| {
        let state = state.clone();
        spawn_local(async move { submit_action(&state, action).await });
    })
}

async fn submit_action(state: &AppHandle, action: CoreAction) {
    let Some(session) = state.session.clone() else {
        return;
    };
    let Some(lobby) = state.lobby.clone() else {
        return;
    };
    if state.my_move_submitted {
        return;
    }
    let Some(game_proto) = lobby.game.as_ref() else {
        return;
    };
    let Ok(game) = herdcore_protocol::game_from_proto(game_proto) else {
        state.dispatch(AppAction::Status("Invalid game state from server".into()));
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
    let mut client = api::rpc_client();
    match client
        .submit_move(v1::SubmitMoveRequest {
            lobby_id: session.lobby_id,
            player_id: session.player_id,
            session_token: session.token,
            game_id: lobby.game_id,
            turn: game.turn,
            action: herdcore_protocol::action_to_proto(action) as i32,
            request_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
    {
        Ok(_) => {
            state.dispatch(AppAction::SetSubmitted(true));
            state.dispatch(AppAction::Status("Move committed—waiting for the herd".into()));
        }
        Err(error) => {
            state.dispatch(AppAction::Status(format!("Move rejected: {}", error.message())))
        }
    }
}
