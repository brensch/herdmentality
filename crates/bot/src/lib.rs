//! Herdcore bot service. A bot is just a participant that speaks the same
//! WebSocket protocol as a human: [`provider`] registers with the game server
//! and is handed seats to play; [`play`] runs the gameplay client per seat.

pub mod play;
pub mod provider;
