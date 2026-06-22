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
    pub games: Vec<v1::GameSummary>,
    pub catalogue: Vec<v1::BotKind>,
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
    /// A Welcome frame: we joined/reconnected and hold a session + snapshot.
    Joined {
        session: Session,
        lobby: v1::LobbySnapshot,
    },
    /// An Update frame; `reset_submitted` clears our pending move at turn edges.
    ApplyEvent {
        lobby: v1::LobbySnapshot,
        reset_submitted: bool,
    },
    SetGames(Vec<v1::GameSummary>),
    SetCatalogue(Vec<v1::BotKind>),
    Status(String),
    SetSubmitted(bool),
    Cleared,
}

impl Reducible for AppState {
    type Action = AppAction;

    fn reduce(self: Rc<Self>, action: AppAction) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            AppAction::Joined { session, lobby } => {
                next.session = Some(session);
                next.lobby = Some(lobby);
                next.my_move_submitted = false;
                next.status = "Connected".into();
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
            AppAction::SetGames(games) => next.games = games,
            AppAction::SetCatalogue(catalogue) => next.catalogue = catalogue,
            AppAction::Status(message) => next.status = message,
            AppAction::SetSubmitted(value) => next.my_move_submitted = value,
            AppAction::Cleared => next = AppState::default(),
        }
        Rc::new(next)
    }
}

pub type AppHandle = UseReducerHandle<AppState>;
