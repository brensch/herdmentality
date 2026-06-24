use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::{distance_to_pen, legal_actions, manhattan, step, Action, GameState, Pos, SeatId};

pub const GREEDY_BOT_ID: &str = "greedy-v1";
pub const LOOKAHEAD_BOT_ID: &str = "lookahead-v1";

/// How deep the lookahead beam will go when time allows.
const LOOKAHEAD_MAX_DEPTH: u8 = 7;
/// Budget used when a caller doesn't supply one. Generous so direct callers and
/// tests get the full, deterministic depth-7 search.
const LOOKAHEAD_DEFAULT_BUDGET: Duration = Duration::from_secs(3600);

/// Select the strategy advertised by a bot provider. Unknown ids retain the
/// original greedy behaviour so an older provider remains safe to run.
pub fn choose_action_for(state: &GameState, seat: SeatId, bot_type_id: &str) -> Action {
    match bot_type_id {
        LOOKAHEAD_BOT_ID => choose_lookahead_action(state, seat),
        _ => choose_action(state, seat),
    }
}

/// As [`choose_action_for`], but the lookahead strategy must return within
/// `budget`. Critical with short turns: an unbounded depth-7 search can take
/// many seconds for a full lobby and would miss the move deadline entirely,
/// leaving the bot looking frozen.
pub fn choose_action_for_within(
    state: &GameState,
    seat: SeatId,
    bot_type_id: &str,
    budget: Duration,
) -> Action {
    match bot_type_id {
        LOOKAHEAD_BOT_ID => choose_lookahead_action_within(state, seat, budget),
        _ => choose_action(state, seat),
    }
}

/// Greedy Greg's original one-turn strategy.
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

/// Lookahead Lucy searches possible herding sequences while predicting Greg's
/// simultaneous move. Uses the full depth; see [`choose_lookahead_action_within`]
/// for a time-bounded variant.
pub fn choose_lookahead_action(state: &GameState, seat: SeatId) -> Action {
    choose_lookahead_action_within(state, seat, LOOKAHEAD_DEFAULT_BUDGET)
}

/// Lookahead by iterative deepening under a wall-clock `budget`: it always has a
/// complete shallow answer ready, then deepens only while time remains. This
/// guarantees a move within the budget even on slow (debug) builds, instead of
/// blowing past a short turn deadline and submitting nothing.
pub fn choose_lookahead_action_within(
    state: &GameState,
    seat: SeatId,
    budget: Duration,
) -> Action {
    let deadline = Instant::now() + budget;
    let candidates: Vec<(GameState, Action)> = legal_actions(state, seat)
        .into_iter()
        .filter_map(|action| Some((predicted_step(state, seat, action)?, action)))
        .collect();
    let Some(&(_, fallback)) = candidates.first() else {
        return Action::Stay;
    };

    let mut best = fallback;
    for depth in 0..=LOOKAHEAD_MAX_DEPTH {
        if Instant::now() >= deadline {
            break;
        }
        // Score every first move at this depth. If any search runs out of time
        // the level is incomplete and discarded, keeping the last full depth.
        let level: Option<Vec<(i64, Action)>> = candidates
            .iter()
            .map(|(candidate, action)| {
                beam_score(candidate.clone(), seat, depth, deadline).map(|score| (score, *action))
            })
            .collect();
        match level {
            Some(scores) => {
                if let Some((_, action)) = scores
                    .into_iter()
                    .max_by_key(|(score, action)| (*score, std::cmp::Reverse(action_tie_break(*action))))
                {
                    best = action;
                }
            }
            None => break,
        }
    }
    best
}

fn predicted_step(state: &GameState, seat: SeatId, action: Action) -> Option<GameState> {
    let mut actions: BTreeMap<_, _> = state
        .players
        .iter()
        .filter(|opponent| opponent.seat != seat)
        .map(|opponent| (opponent.seat, choose_action(state, opponent.seat)))
        .collect();
    actions.insert(seat, action);
    step(state, &actions).ok()
}

/// Beam search to `depth`, returning `None` if `deadline` passes mid-search so
/// the caller can discard the incomplete level.
fn beam_score(initial: GameState, seat: SeatId, depth: u8, deadline: Instant) -> Option<i64> {
    const BEAM_WIDTH: usize = 8;
    let mut beam = vec![initial];
    for _ in 0..depth {
        if Instant::now() >= deadline {
            return None;
        }
        let mut next = Vec::with_capacity(BEAM_WIDTH * 5);
        for state in std::mem::take(&mut beam) {
            // Expanding one node (which predicts every opponent) is the costly
            // unit, so check here to keep overshoot to a single node.
            if Instant::now() >= deadline {
                return None;
            }
            if state.game_over {
                next.push(state);
                continue;
            }
            for action in legal_actions(&state, seat) {
                if let Some(candidate) = predicted_step(&state, seat, action) {
                    next.push(candidate);
                }
            }
        }
        if next.is_empty() {
            return Some(i64::MIN);
        }
        next.sort_by_key(|state| std::cmp::Reverse(planning_evaluate(state, seat)));
        next.truncate(BEAM_WIDTH);
        beam = next;
    }
    Some(
        beam.into_iter()
            .map(|state| planning_evaluate(&state, seat))
            .max()
            .unwrap_or(i64::MIN),
    )
}

fn planning_evaluate(state: &GameState, seat: SeatId) -> i64 {
    let Some(player) = state.players.iter().find(|player| player.seat == seat) else {
        return i64::MIN;
    };
    if state.game_over {
        return match state.winners.contains(&seat) {
            true if state.winners.len() == 1 => 100_000_000,
            true => 0,
            false => -100_000_000,
        };
    }
    let opponent_score = state
        .players
        .iter()
        .filter(|opponent| opponent.seat != seat)
        .map(|opponent| opponent.score)
        .max()
        .unwrap_or(0);
    let score_margin = 1_000_000 * (i64::from(player.score) - i64::from(opponent_score));
    let territory = state
        .sheep
        .iter()
        .map(|sheep| {
            let own = distance_to_pen(*sheep, &player.pen);
            let opponent = state
                .players
                .iter()
                .filter(|opponent| opponent.seat != seat)
                .map(|opponent| distance_to_pen(*sheep, &opponent.pen))
                .min()
                .unwrap_or(own);
            i64::from(opponent - own)
        })
        .sum::<i64>();
    let setup = state
        .sheep
        .iter()
        .flat_map(|sheep| {
            driving_positions(state, *sheep, &player.pen)
                .into_iter()
                .filter(|position| dog_can_stand(state, *position))
                .map(move |position| (*sheep, position))
        })
        .map(|(sheep, position)| {
            10 * distance_to_pen(sheep, &player.pen) + manhattan(player.dog, position)
        })
        .min()
        .unwrap_or(1_000);
    score_margin + 100 * territory - 1_000 * i64::from(setup)
}

fn action_tie_break(action: Action) -> u8 {
    match action {
        Action::Up => 0,
        Action::Right => 1,
        Action::Down => 2,
        Action::Left => 3,
        Action::Stay => 4,
    }
}

fn driving_positions(state: &GameState, sheep: Pos, pen: &[Pos]) -> Vec<Pos> {
    let Some(target) = pen.iter().min_by_key(|tile| manhattan(sheep, **tile)) else {
        return Vec::new();
    };
    let mut positions = Vec::with_capacity(2);
    if sheep.x != target.x {
        let behind = Pos::new(sheep.x + (sheep.x - target.x).signum(), sheep.y);
        add_driving_position(state, sheep, behind, &mut positions);
    }
    if sheep.y != target.y {
        let behind = Pos::new(sheep.x, sheep.y + (sheep.y - target.y).signum());
        add_driving_position(state, sheep, behind, &mut positions);
    }
    positions
}

fn add_driving_position(state: &GameState, sheep: Pos, behind: Pos, positions: &mut Vec<Pos>) {
    if in_arena(state, behind) {
        positions.push(behind);
        return;
    }

    // A sheep against a wall cannot be approached from directly behind. A dog
    // on either perpendicular side still drives it along the wall.
    let perpendicular = if behind.x != sheep.x {
        [
            Pos::new(sheep.x, sheep.y - 1),
            Pos::new(sheep.x, sheep.y + 1),
        ]
    } else {
        [
            Pos::new(sheep.x - 1, sheep.y),
            Pos::new(sheep.x + 1, sheep.y),
        ]
    };
    positions.extend(
        perpendicular
            .into_iter()
            .filter(|position| in_arena(state, *position)),
    );
}

fn in_arena(state: &GameState, position: Pos) -> bool {
    position.x >= 0 && position.x < state.width && position.y >= 0 && position.y < state.height
}

fn dog_can_stand(state: &GameState, position: Pos) -> bool {
    in_arena(state, position)
        && !state.sheep.contains(&position)
        && !state.rocks.contains(&position)
        && !state
            .players
            .iter()
            .any(|player| player.pen.contains(&position))
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

    #[test]
    fn strategy_dispatches_lookahead_bot() {
        let state = initial_state_for_players(2).unwrap();
        assert_eq!(
            choose_action_for(&state, 0, LOOKAHEAD_BOT_ID),
            choose_lookahead_action(&state, 0)
        );
        assert_eq!(
            choose_action_for(&state, 0, GREEDY_BOT_ID),
            choose_action(&state, 0)
        );
    }

    #[test]
    fn lookahead_respects_a_short_budget_on_a_full_lobby() {
        use crate::{initial_state_with_behavior, SheepBehavior};
        // A full eight-dog lobby is the worst case; an unbounded search takes
        // many seconds in debug. The budgeted call must return promptly so the
        // bot never misses a short turn deadline.
        let state = initial_state_with_behavior(MAX_PLAYERS, SheepBehavior::Skittish).unwrap();
        let budget = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let _ = choose_lookahead_action_within(&state, 0, budget);
        let elapsed = started.elapsed();
        assert!(
            elapsed < budget + Duration::from_millis(400),
            "lookahead overran its budget badly: {elapsed:?}"
        );
    }

    #[test]
    fn lookahead_is_deterministic() {
        let state = initial_state_for_players(2).unwrap();
        assert_eq!(
            choose_lookahead_action(&state, 0),
            choose_lookahead_action(&state, 0)
        );
    }
}
