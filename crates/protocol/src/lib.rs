use std::io::{Read, Write};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use herdcore_core as core;
use prost::Message;

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/herdcore.v1.rs"));
}

/// Frames are protobuf-encoded, then opportunistically deflated. The first byte
/// flags the payload: `0` = raw protobuf, `1` = deflated protobuf. Deflate is
/// only kept when it actually shrinks the frame, so tiny frames stay tiny.
const RAW: u8 = 0;
const DEFLATED: u8 = 1;

pub fn encode_frame<M: Message>(message: &M) -> Vec<u8> {
    let raw = message.encode_to_vec();
    if let Some(deflated) = deflate(&raw) {
        if deflated.len() + 1 < raw.len() + 1 {
            let mut out = Vec::with_capacity(deflated.len() + 1);
            out.push(DEFLATED);
            out.extend_from_slice(&deflated);
            return out;
        }
    }
    let mut out = Vec::with_capacity(raw.len() + 1);
    out.push(RAW);
    out.extend_from_slice(&raw);
    out
}

pub fn decode_frame<M: Message + Default>(bytes: &[u8]) -> Result<M, &'static str> {
    let (&flag, rest) = bytes.split_first().ok_or("empty frame")?;
    match flag {
        RAW => M::decode(rest).map_err(|_| "malformed frame"),
        DEFLATED => {
            let inflated = inflate(rest).ok_or("bad compression")?;
            M::decode(inflated.as_slice()).map_err(|_| "malformed frame")
        }
        _ => Err("unknown frame flag"),
    }
}

fn deflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = DeflateDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

pub fn game_to_proto(state: &core::GameState) -> v1::GameState {
    v1::GameState {
        layout_version: u32::from(state.layout_version),
        width: i32::from(state.width),
        height: i32::from(state.height),
        turn: state.turn,
        players: state
            .players
            .iter()
            .map(|player| v1::GamePlayerState {
                seat: u32::from(player.seat),
                dog: Some(pos_to_proto(player.dog)),
                pen: player.pen.iter().copied().map(pos_to_proto).collect(),
                score: u32::from(player.score),
                last_action: player
                    .last_action
                    .map(action_to_proto)
                    .unwrap_or(v1::Action::Unspecified) as i32,
            })
            .collect(),
        sheep: state.sheep.iter().copied().map(pos_to_proto).collect(),
        rocks: state.rocks.iter().copied().map(pos_to_proto).collect(),
        game_over: state.game_over,
        winners: state.winners.iter().map(|seat| u32::from(*seat)).collect(),
    }
}

pub fn game_from_proto(state: &v1::GameState) -> Result<core::GameState, &'static str> {
    Ok(core::GameState {
        layout_version: u16::try_from(state.layout_version).map_err(|_| "layout version")?,
        width: i8::try_from(state.width).map_err(|_| "width")?,
        height: i8::try_from(state.height).map_err(|_| "height")?,
        turn: state.turn,
        players: state
            .players
            .iter()
            .map(|player| {
                Ok(core::PlayerState {
                    seat: u8::try_from(player.seat).map_err(|_| "seat")?,
                    dog: pos_from_proto(player.dog.as_ref().ok_or("dog")?)?,
                    pen: player
                        .pen
                        .iter()
                        .map(pos_from_proto)
                        .collect::<Result<_, _>>()?,
                    score: u16::try_from(player.score).map_err(|_| "player score")?,
                    last_action: v1::Action::try_from(player.last_action)
                        .ok()
                        .and_then(action_from_proto),
                })
            })
            .collect::<Result<_, &'static str>>()?,
        sheep: state
            .sheep
            .iter()
            .map(pos_from_proto)
            .collect::<Result<_, _>>()?,
        rocks: state
            .rocks
            .iter()
            .map(pos_from_proto)
            .collect::<Result<_, _>>()?,
        game_over: state.game_over,
        winners: state
            .winners
            .iter()
            .map(|seat| u8::try_from(*seat).map_err(|_| "winner"))
            .collect::<Result<_, _>>()?,
    })
}

pub fn action_to_proto(action: core::Action) -> v1::Action {
    match action {
        core::Action::Up => v1::Action::Up,
        core::Action::Down => v1::Action::Down,
        core::Action::Left => v1::Action::Left,
        core::Action::Right => v1::Action::Right,
        core::Action::Stay => v1::Action::Stay,
    }
}

pub fn action_from_proto(action: v1::Action) -> Option<core::Action> {
    match action {
        v1::Action::Up => Some(core::Action::Up),
        v1::Action::Down => Some(core::Action::Down),
        v1::Action::Left => Some(core::Action::Left),
        v1::Action::Right => Some(core::Action::Right),
        v1::Action::Stay => Some(core::Action::Stay),
        v1::Action::Unspecified => None,
    }
}

fn pos_to_proto(pos: core::Pos) -> v1::Position {
    v1::Position {
        x: i32::from(pos.x),
        y: i32::from(pos.y),
    }
}

fn pos_from_proto(pos: &v1::Position) -> Result<core::Pos, &'static str> {
    Ok(core::Pos::new(
        i8::try_from(pos.x).map_err(|_| "x")?,
        i8::try_from(pos.y).map_err(|_| "y")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_state_round_trips() {
        let original = core::initial_state_for_players(8).unwrap();
        assert_eq!(game_from_proto(&game_to_proto(&original)), Ok(original));
    }
}
