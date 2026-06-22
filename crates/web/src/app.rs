//! Root component: routing, shared state, the WebSocket connection, and the
//! navigation controller that keeps the URL in sync with the live game.

use herdcore_protocol::v1;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::state::{AppHandle, AppState};
use crate::views::{GameView, Header, Home, LobbyView, StatusFooter};
use crate::ws::Connection;

type GameKey = (String, String, u64);

fn is_new_game(last: &Option<GameKey>, next: &GameKey) -> bool {
    last.as_ref() != Some(next)
}

/// `/l/:lobby` is the lobby (roster + game history); a live game lives at
/// `/l/:lobby/g/:game`, so Back returns to the lobby.
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

    // One connection for the whole app lifetime; created once.
    let connection = {
        let state = state.clone();
        use_mut_ref(move || Connection::new(state))
    };
    let connection = connection.borrow().clone();

    // Re-attach a stored session on load; the server replies with a snapshot.
    {
        let connection = connection.clone();
        use_effect_with((), move |_| {
            if let Some(session) = api::load_session() {
                connection.resume(session);
            }
            || ()
        });
    }

    html! {
        <ContextProvider<AppHandle> context={state}>
            <ContextProvider<Connection> context={connection}>
                <BrowserRouter>
                    <Header />
                    <NavController />
                    <Switch<Route> render={switch} />
                    <StatusFooter />
                </BrowserRouter>
            </ContextProvider<Connection>>
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

/// Keeps the URL pointing at the right place: into the active game when one
/// newly starts (so loading a lobby with a live game lands you in it), back to
/// the lobby when it ends — without bouncing the Back button.
#[function_component(NavController)]
fn nav_controller() -> Html {
    let state = use_context::<AppHandle>().expect("app context");
    let navigator = use_navigator().expect("navigator");
    let route = use_route::<Route>();
    // A game number is only unique inside one lobby. Include the player session
    // too, so leaving and rejoining a still-running lobby is a new navigation
    // edge while using Back during the same session still stays in the lobby.
    let last_game = use_mut_ref(|| None::<GameKey>);

    use_effect(move || {
        if let (Some(session), Some(lobby)) = (state.session.as_ref(), state.lobby.as_ref()) {
            let word = session.word.clone();
            let playing = lobby.phase == v1::LobbyPhase::Playing as i32 && lobby.game.is_some();
            let game_id = lobby.game_id;
            let game_key = (session.player_id.clone(), lobby.lobby_id.clone(), game_id);

            let acceptable = match &route {
                Some(Route::Lobby { lobby }) => *lobby == word,
                Some(Route::Game { lobby, game }) => *lobby == word && playing && *game == game_id,
                _ => false,
            };

            if playing && is_new_game(&last_game.borrow(), &game_key) {
                *last_game.borrow_mut() = Some(game_key);
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

#[cfg(test)]
mod tests {
    use super::{is_new_game, GameKey};

    #[test]
    fn game_navigation_is_scoped_to_lobby_and_session() {
        let previous: Option<GameKey> = Some(("player-a".into(), "lobby-a".into(), 1));

        assert!(!is_new_game(
            &previous,
            &("player-a".into(), "lobby-a".into(), 1)
        ));
        assert!(is_new_game(
            &previous,
            &("player-b".into(), "lobby-b".into(), 1)
        ));
        assert!(is_new_game(
            &previous,
            &("player-b".into(), "lobby-a".into(), 1)
        ));
    }
}
