//! Shared application state, provided through context and driven by a reducer.

use std::rc::Rc;

use herdcore_protocol::v1;
use yew::prelude::*;

use crate::api::Session;

#[derive(Clone, PartialEq, Default)]
pub struct AppState {
    pub session: Option<Session>,
    pub lobby: Option<v1::LobbySnapshot>,
    /// Seats that have submitted for the current turn (drives the scoreboard).
    pub moved_seats: Vec<u32>,
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

    /// Whether we have already submitted a move this turn.
    pub fn has_moved(&self) -> bool {
        self.my_seat()
            .is_some_and(|seat| self.moved_seats.contains(&seat))
    }

    fn current_turn(&self) -> Option<(u64, u64)> {
        let lobby = self.lobby.as_ref()?;
        Some((lobby.game_id, lobby.game.as_ref()?.turn))
    }
}

pub enum AppAction {
    /// A Welcome frame: we joined/reconnected and hold a session + snapshot.
    Joined {
        session: Session,
        lobby: v1::LobbySnapshot,
    },
    /// An Update frame; `reset_submitted` marks a turn edge.
    ApplyEvent {
        lobby: v1::LobbySnapshot,
        reset_submitted: bool,
    },
    /// A Moved frame: a seat submitted for `(game_id, turn)`.
    Moved {
        game_id: u64,
        turn: u64,
        seat: u32,
    },
    SetGames(Vec<v1::GameSummary>),
    SetCatalogue(Vec<v1::BotKind>),
    Status(String),
    Cleared,
}

impl Reducible for AppState {
    type Action = AppAction;

    fn reduce(self: Rc<Self>, action: AppAction) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            AppAction::Joined { session, lobby } => {
                next.moved_seats = lobby.submitted_seats.clone();
                next.session = Some(session);
                next.lobby = Some(lobby);
                next.status = String::new();
            }
            AppAction::ApplyEvent {
                lobby,
                reset_submitted,
            } => {
                let current = next.lobby.as_ref().map(|l| l.public_version).unwrap_or(0);
                if lobby.public_version >= current {
                    // The snapshot is authoritative for who has moved this turn.
                    next.moved_seats = lobby.submitted_seats.clone();
                    next.lobby = Some(lobby);
                }
                if reset_submitted {
                    // New turn: drop any stale transient message.
                    next.status = String::new();
                }
            }
            AppAction::Moved {
                game_id,
                turn,
                seat,
            } => {
                if next.current_turn() == Some((game_id, turn)) && !next.moved_seats.contains(&seat)
                {
                    next.moved_seats.push(seat);
                }
            }
            AppAction::SetGames(games) => next.games = games,
            AppAction::SetCatalogue(catalogue) => next.catalogue = catalogue,
            AppAction::Status(message) => next.status = message,
            AppAction::Cleared => next = AppState::default(),
        }
        Rc::new(next)
    }
}

pub type AppHandle = UseReducerHandle<AppState>;
