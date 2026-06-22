use std::collections::BTreeMap;

use crate::{distance_to_pen, legal_actions, manhattan, step, Action, GameState, Pos, SeatId};

pub fn choose_action(state: &GameState, seat: SeatId) -> Action {
    let Some(player) = state.players.iter().find(|player| player.seat == seat) else {
        return Action::Stay;
    };
    let actions = legal_actions(state, seat);
    let mut best = Action::Stay;
    let mut best_score = i64::MIN;

    for action in actions {
        let candidate = step(state, &BTreeMap::from([(seat, action)]));
        let Ok(candidate) = candidate else {
            continue;
        };
        let score = evaluate(&candidate, player.seat);
        if score > best_score {
            best_score = score;
            best = action;
        }
    }
    best
}

fn evaluate(state: &GameState, seat: SeatId) -> i64 {
    let Some(player) = state.players.iter().find(|player| player.seat == seat) else {
        return i64::MIN;
    };
    if state.game_over {
        return if state.winners.contains(&seat) {
            1_000_000
        } else {
            -1_000_000
        };
    }

    let opponent_high = state
        .players
        .iter()
        .filter(|other| other.seat != seat)
        .map(|other| other.score)
        .max()
        .unwrap_or(0);
    let score = 1000 * i64::from(player.score) - 1000 * i64::from(opponent_high);
    let sheep_position = state
        .sheep
        .iter()
        .map(|sheep| {
            let own_distance = distance_to_pen(*sheep, &player.pen);
            let opponent_distance = state
                .players
                .iter()
                .filter(|other| other.seat != seat)
                .map(|other| distance_to_pen(*sheep, &other.pen))
                .min()
                .unwrap_or(own_distance);
            20 * i64::from(opponent_distance - own_distance)
        })
        .sum::<i64>();
    let center = Pos::new((state.width - 1) / 2, (state.height - 1) / 2);
    let centrality =
        5 * i64::from(i16::from(state.width + state.height) - manhattan(player.dog, center));
    score + sheep_position + centrality
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{initial_state_for_players, MAX_PLAYERS};

    #[test]
    fn bot_is_deterministic_for_every_seat() {
        let state = initial_state_for_players(MAX_PLAYERS).unwrap();
        for seat in 0..MAX_PLAYERS {
            assert_eq!(choose_action(&state, seat), choose_action(&state, seat));
        }
    }
}
