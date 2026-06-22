//! Shared application state, provided through context and driven by a reducer.

use std::rc::Rc;

use herdcore_protocol::v1;
use yew::prelude::*;

use crate::api::Session;

#[derive(Clone, PartialEq, Default)]
pub struct AppState {
    pub session: Option<Session>,
    pub lobby: Option<v1::LobbySnapshot>,
    pub my_move_submitted: bool,
    pub status: String,
}

impl AppState {
    /// The word of the lobby we currently hold a session for.
    pub fn session_word(&self) -> Option<&str> {
        self.session.as_ref().map(|session| session.word.as_str())
    }

    /// Whether we are a joined member of the lobby identified by `word`.
    pub fn is_member_of(&self, word: &str) -> bool {
        self.session_word() == Some(word)
    }

    /// Our seat in the live game, if we have one.
    pub fn my_seat(&self) -> Option<u32> {
        let session = self.session.as_ref()?;
        let lobby = self.lobby.as_ref()?;
        lobby
            .players
            .iter()
            .find(|player| player.player_id == session.player_id)
            .and_then(|player| player.seat)
    }
}

pub enum AppAction {
    /// Restore a session loaded from storage (lobby fetched by the watcher).
    Restore(Session),
    /// We just joined or created a lobby.
    Joined {
        session: Session,
        lobby: v1::LobbySnapshot,
    },
    /// Authoritative snapshot from `get_lobby`.
    SetLobby {
        lobby: v1::LobbySnapshot,
        my_move_submitted: bool,
    },
    /// Streamed update; `reset_submitted` clears our pending move at turn edges.
    ApplyEvent {
        lobby: v1::LobbySnapshot,
        reset_submitted: bool,
    },
    Status(String),
    SetSubmitted(bool),
    Cleared,
}

impl Reducible for AppState {
    type Action = AppAction;

    fn reduce(self: Rc<Self>, action: AppAction) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            AppAction::Restore(session) => {
                next.session = Some(session);
                next.status = "Reconnecting…".into();
            }
            AppAction::Joined { session, lobby } => {
                next.session = Some(session);
                next.lobby = Some(lobby);
                next.my_move_submitted = false;
                next.status = "Connected".into();
            }
            AppAction::SetLobby {
                lobby,
                my_move_submitted,
            } => {
                next.lobby = Some(lobby);
                next.my_move_submitted = my_move_submitted;
            }
            AppAction::ApplyEvent {
                lobby,
                reset_submitted,
            } => {
                let current = next.lobby.as_ref().map(|l| l.public_version).unwrap_or(0);
                if lobby.public_version >= current {
                    next.lobby = Some(lobby);
                }
                if reset_submitted {
                    next.my_move_submitted = false;
                }
            }
            AppAction::Status(message) => next.status = message,
            AppAction::SetSubmitted(value) => next.my_move_submitted = value,
            AppAction::Cleared => next = AppState::default(),
        }
        Rc::new(next)
    }
}

pub type AppHandle = UseReducerHandle<AppState>;
