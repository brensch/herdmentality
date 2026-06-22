//! Root component: routing, shared-state wiring, the watch loop, and the
//! navigation controller that keeps the URL and the live game in sync.

use std::cell::Cell;
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use herdcore_protocol::v1;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api::{self, Session};
use crate::state::{AppAction, AppHandle, AppState};
use crate::views::{GameView, Header, Home, LobbyView};

/// The app's routes. `/l/:lobby` is the lobby (roster + game history); a live
/// game lives at `/l/:lobby/g/:game`, so Back naturally returns to the lobby.
#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/l/:lobby")]
    Lobby { lobby: String },
    #[at("/l/:lobby/g/:game")]
    Game { lobby: String, game: u64 },
    #[not_found]
    #[at("/404")]
    NotFound,
}

#[function_component(App)]
pub fn app() -> Html {
    let state = use_reducer(AppState::default);

    // Restore a stored session once on mount; the watcher fetches its lobby.
    {
        let state = state.clone();
        use_effect_with((), move |_| {
            if let Some(session) = api::load_session() {
                state.dispatch(AppAction::Restore(session));
            }
            || ()
        });
    }

    // (Re)start the watch loop whenever the session changes.
    {
        let state = state.clone();
        let session = state.session.clone();
        use_effect_with(session, move |session| {
            let cancel = Rc::new(Cell::new(false));
            if let Some(session) = session.clone() {
                let state = state.clone();
                let cancel = cancel.clone();
                spawn_local(async move { watch_loop(state, session, cancel).await });
            }
            let cancel = cancel.clone();
            move || cancel.set(true)
        });
    }

    html! {
        <ContextProvider<AppHandle> context={state}>
            <BrowserRouter>
                <Header />
                <NavController />
                <Switch<Route> render={switch} />
            </BrowserRouter>
        </ContextProvider<AppHandle>>
    }
}

fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <Home /> },
        Route::Lobby { lobby } => html! { <LobbyView lobby={lobby} /> },
        Route::Game { lobby, game } => html! { <GameView lobby={lobby} game={game} /> },
        Route::NotFound => html! { <Home /> },
    }
}

/// Keeps the URL pointing at the right place: into the active game when one is
/// running, back to the lobby when it ends. Guards on route equality so it
/// never loops, and never auto-navigates a non-member (the view shows a join
/// panel instead).
#[function_component(NavController)]
fn nav_controller() -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let navigator = use_navigator().expect("navigator");
    let route = use_route::<Route>();
    // The last game id we auto-navigated into, so we only pull players into a
    // game when it newly starts — not every time they sit on the lobby route
    // (which would defeat the Back button).
    let last_game = use_mut_ref(|| 0u64);

    use_effect(move || {
        if let (Some(session), Some(lobby)) = (state.session.as_ref(), state.lobby.as_ref()) {
            let word = session.word.clone();
            let playing = lobby.phase == v1::LobbyPhase::Playing as i32 && lobby.game.is_some();
            let game_id = lobby.game_id;

            // Routes a member may legitimately sit on for this lobby.
            let acceptable = match &route {
                Some(Route::Lobby { lobby }) => *lobby == word,
                Some(Route::Game { lobby, game }) => *lobby == word && playing && *game == game_id,
                _ => false,
            };

            if playing && game_id > *last_game.borrow() {
                // A new game just started: pull everyone into it (once).
                *last_game.borrow_mut() = game_id;
                let desired = Route::Game {
                    lobby: word.clone(),
                    game: game_id,
                };
                if route.as_ref() != Some(&desired) {
                    if matches!(route, Some(Route::Game { .. })) {
                        navigator.replace(&desired);
                    } else {
                        navigator.push(&desired);
                    }
                }
            } else if !acceptable {
                // On home, another lobby, or a game that has ended: fall back to
                // the lobby page (which lists the games played). This does NOT
                // fire when sitting on the lobby during a live game, so Back from
                // a game returns here instead of bouncing into it.
                let desired = Route::Lobby { lobby: word.clone() };
                if matches!(route, Some(Route::Game { .. })) {
                    navigator.replace(&desired);
                } else {
                    navigator.push(&desired);
                }
            }
        }
        || ()
    });

    Html::default()
}

async fn watch_loop(state: AppHandle, session: Session, cancel: Rc<Cell<bool>>) {
    loop {
        if cancel.get() {
            return;
        }
        let mut client = api::rpc_client();
        let version = match client
            .get_lobby(v1::GetLobbyRequest {
                lobby_id: session.lobby_id.clone(),
                player_id: session.player_id.clone(),
                session_token: session.token.clone(),
            })
            .await
        {
            Ok(response) => {
                let private = response.into_inner();
                private.lobby.map(|lobby| {
                    let version = lobby.public_version;
                    state.dispatch(AppAction::SetLobby {
                        lobby,
                        my_move_submitted: private.my_move_submitted,
                    });
                    version
                })
            }
            Err(_) => None,
        };
        if let Some(version) = version {
            watch_stream(&state, &session, &cancel, version).await;
        }
        if cancel.get() {
            return;
        }
        TimeoutFuture::new(1000).await;
    }
}

async fn watch_stream(
    state: &AppHandle,
    session: &Session,
    cancel: &Rc<Cell<bool>>,
    after_version: u64,
) {
    let mut client = api::rpc_client();
    let Ok(response) = client
        .watch_lobby(v1::WatchLobbyRequest {
            lobby_id: session.lobby_id.clone(),
            player_id: session.player_id.clone(),
            session_token: session.token.clone(),
            after_version,
        })
        .await
    else {
        return;
    };
    let mut stream = response.into_inner();
    loop {
        if cancel.get() {
            return;
        }
        match stream.message().await {
            Ok(Some(event)) => {
                if event.kind == v1::LobbyEventKind::Heartbeat as i32 {
                    continue;
                }
                if let Some(lobby) = event.lobby {
                    let reset = event.kind == v1::LobbyEventKind::GameStarted as i32
                        || event.kind == v1::LobbyEventKind::TurnResolved as i32;
                    state.dispatch(AppAction::ApplyEvent {
                        lobby,
                        reset_submitted: reset,
                    });
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}
