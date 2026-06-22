//! Server connection details, the gRPC-web client, and session persistence.

use herdcore_protocol::v1::herdcore_client::HerdcoreClient;
use tonic_web_wasm_client::Client as GrpcWebClient;

/// Address of the authoritative game server, baked into the wasm bundle so
/// players never see or type it. Override per deployment by building with
/// `HERDCORE_WEB_SERVER_URL=https://…`.
pub const SERVER_URL: &str = match option_env!("HERDCORE_WEB_SERVER_URL") {
    Some(url) => url,
    None => "http://127.0.0.1:55051",
};

const SESSION_KEY: &str = "herdcore.session.v3";

/// Everything needed to act as a player in a lobby across reloads.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    pub lobby_id: String,
    pub player_id: String,
    pub token: String,
    /// The lobby word, used for routing and membership checks.
    pub word: String,
}

pub fn rpc_client() -> HerdcoreClient<GrpcWebClient> {
    HerdcoreClient::new(GrpcWebClient::new(SERVER_URL.to_owned()))
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
