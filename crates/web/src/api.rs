//! Server connection details and session persistence.

const SESSION_KEY: &str = "herdcore.session.v4";

/// WebSocket URL of the game server.
///
/// By default the app connects **same-origin**: the page's own host with a
/// `ws`/`wss` scheme matching `http`/`https`, at `/ws`. That makes a single
/// build work behind any domain or reverse proxy that serves the app and proxies
/// `/ws` to the server (the deployment's Caddy does exactly this), with no
/// per-domain rebuild.
///
/// A build-time `HERDCORE_WS_URL=wss://…/ws` override still wins when you need to
/// point a bundle at a different host (e.g. a local `trunk serve` talking to a
/// remote server).
pub fn ws_url() -> String {
    if let Some(url) = option_env!("HERDCORE_WS_URL") {
        return url.to_owned();
    }
    if let Some(window) = web_sys::window() {
        let location = window.location();
        if let (Ok(protocol), Ok(host)) = (location.protocol(), location.host()) {
            let scheme = if protocol == "https:" { "wss" } else { "ws" };
            return format!("{scheme}://{host}/ws");
        }
    }
    // Non-browser fallback (tests / SSR), matches the server's default listener.
    "ws://127.0.0.1:55051/ws".to_owned()
}

/// Everything needed to re-attach to a lobby across reloads.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    pub lobby_id: String,
    pub player_id: String,
    pub token: String,
    /// The lobby word, used for routing and membership checks.
    pub word: String,
}

pub fn load_session() -> Option<Session> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let value = storage.get_item(SESSION_KEY).ok()??;
    let parts: Vec<&str> = value.split('|').collect();
    if parts.len() == 4 {
        Some(Session {
            lobby_id: parts[0].to_owned(),
            player_id: parts[1].to_owned(),
            token: parts[2].to_owned(),
            word: parts[3].to_owned(),
        })
    } else {
        None
    }
}

pub fn save_session(session: &Session) {
    if let Some(Ok(Some(storage))) = web_sys::window().map(|window| window.local_storage()) {
        let _ = storage.set_item(
            SESSION_KEY,
            &format!(
                "{}|{}|{}|{}",
                session.lobby_id, session.player_id, session.token, session.word
            ),
        );
    }
}

pub fn clear_session() {
    if let Some(Ok(Some(storage))) = web_sys::window().map(|window| window.local_storage()) {
        let _ = storage.remove_item(SESSION_KEY);
    }
}
