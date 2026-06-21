use std::collections::{BTreeMap, BTreeSet};

pub type SeatId = u8;

pub const MIN_PLAYERS: u8 = 2;
pub const MAX_PLAYERS: u8 = 16;
pub const LAYOUT_VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub x: i8,
    pub y: i8,
}

impl Pos {
    pub const fn new(x: i8, y: i8) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Stay,
}

impl Action {
    pub const ALL: [Action; 5] = [
        Action::Up,
        Action::Down,
        Action::Left,
        Action::Right,
        Action::Stay,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerState {
    pub seat: SeatId,
    pub dog: Pos,
    pub pen: Vec<Pos>,
    pub score: u16,
    pub last_action: Option<Action>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameState {
    pub layout_version: u16,
    pub width: i8,
    pub height: i8,
    pub turn: u64,
    pub players: Vec<PlayerState>,
    pub sheep: Vec<Pos>,
    pub rocks: Vec<Pos>,
    pub game_over: bool,
    pub winners: Vec<SeatId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleError {
    InvalidPlayerCount,
    UnknownSeat(SeatId),
    IllegalAction(SeatId, Action),
}

pub fn initial_state() -> GameState {
    initial_state_for_players(2).expect("two players is a valid arena size")
}

pub fn initial_state_for_players(player_count: u8) -> Result<GameState, RuleError> {
    if !(MIN_PLAYERS..=MAX_PLAYERS).contains(&player_count) {
        return Err(RuleError::InvalidPlayerCount);
    }

    let slots = arena_slots();
    let selected_slots = balanced_slot_indices(player_count);
    let players = selected_slots
        .into_iter()
        .enumerate()
        .map(|(seat, slot)| {
            let slot = &slots[slot];
            PlayerState {
                seat: seat as SeatId,
                dog: slot.dog,
                pen: slot.pen.clone(),
                score: 0,
                last_action: None,
            }
        })
        .collect();

    Ok(GameState {
        layout_version: LAYOUT_VERSION,
        width: 41,
        height: 41,
        turn: 0,
        players,
        sheep: central_sheep(player_count),
        rocks: Vec::new(),
        game_over: false,
        winners: Vec::new(),
    })
}

pub fn step(state: &GameState, actions: &BTreeMap<SeatId, Action>) -> Result<GameState, RuleError> {
    if is_terminal(state) {
        let mut terminal = state.clone();
        update_terminal_status(&mut terminal);
        return Ok(terminal);
    }

    for (&seat, &action) in actions {
        if player_index(state, seat).is_none() {
            return Err(RuleError::UnknownSeat(seat));
        }
        if !is_action_legal(state, seat, action) {
            return Err(RuleError::IllegalAction(seat, action));
        }
    }

    let resolved_actions: Vec<Action> = state
        .players
        .iter()
        .map(|player| actions.get(&player.seat).copied().unwrap_or(Action::Stay))
        .collect();
    let dog_positions = resolve_dogs(state, &resolved_actions);

    let mut next = state.clone();
    for (index, player) in next.players.iter_mut().enumerate() {
        player.dog = dog_positions[index];
        player.last_action = Some(resolved_actions[index]);
    }
    next.sheep = resolve_sheep(state, &dog_positions);
    score_and_remove_sheep(&mut next);
    next.turn = next.turn.saturating_add(1);
    update_terminal_status(&mut next);
    Ok(next)
}

pub fn legal_actions(state: &GameState, seat: SeatId) -> Vec<Action> {
    if is_terminal(state) || player_index(state, seat).is_none() {
        return Vec::new();
    }

    Action::ALL
        .into_iter()
        .filter(|action| is_action_legal(state, seat, *action))
        .collect()
}

pub fn is_action_legal(state: &GameState, seat: SeatId, action: Action) -> bool {
    let Some(index) = player_index(state, seat) else {
        return false;
    };
    action == Action::Stay || !is_dog_blocked(state, moved(state.players[index].dog, action))
}

pub fn is_terminal(state: &GameState) -> bool {
    state.game_over || state.sheep.is_empty() || clinched_winner(state).is_some()
}

pub fn manhattan(a: Pos, b: Pos) -> i16 {
    i16::from((a.x - b.x).abs()) + i16::from((a.y - b.y).abs())
}

pub fn distance_to_pen(pos: Pos, pen: &[Pos]) -> i16 {
    pen.iter()
        .map(|tile| manhattan(pos, *tile))
        .min()
        .unwrap_or(0)
}

fn player_index(state: &GameState, seat: SeatId) -> Option<usize> {
    state.players.iter().position(|player| player.seat == seat)
}

fn moved(pos: Pos, action: Action) -> Pos {
    match action {
        Action::Up => Pos::new(pos.x, pos.y - 1),
        Action::Down => Pos::new(pos.x, pos.y + 1),
        Action::Left => Pos::new(pos.x - 1, pos.y),
        Action::Right => Pos::new(pos.x + 1, pos.y),
        Action::Stay => pos,
    }
}

fn in_bounds(state: &GameState, pos: Pos) -> bool {
    pos.x >= 0 && pos.x < state.width && pos.y >= 0 && pos.y < state.height
}

fn all_pen_tiles(state: &GameState, pos: Pos) -> bool {
    state.players.iter().any(|player| player.pen.contains(&pos))
}

fn is_dog_blocked(state: &GameState, pos: Pos) -> bool {
    !in_bounds(state, pos)
        || state.rocks.contains(&pos)
        || all_pen_tiles(state, pos)
        || state.sheep.contains(&pos)
}

fn resolve_dogs(state: &GameState, actions: &[Action]) -> Vec<Pos> {
    let origins: Vec<Pos> = state.players.iter().map(|player| player.dog).collect();
    let destinations: Vec<Pos> = origins
        .iter()
        .zip(actions)
        .map(|(origin, action)| moved(*origin, *action))
        .collect();
    let mut blocked: Vec<bool> = actions
        .iter()
        .map(|action| *action == Action::Stay)
        .collect();

    let mut counts = BTreeMap::new();
    for (index, destination) in destinations.iter().enumerate() {
        if !blocked[index] {
            *counts.entry(*destination).or_insert(0usize) += 1;
        }
    }
    for (index, destination) in destinations.iter().enumerate() {
        if counts.get(destination).copied().unwrap_or(0) > 1 {
            blocked[index] = true;
        }
    }

    for left in 0..origins.len() {
        for right in (left + 1)..origins.len() {
            if destinations[left] == origins[right] && destinations[right] == origins[left] {
                blocked[left] = true;
                blocked[right] = true;
            }
        }
    }

    loop {
        let mut changed = false;
        for mover in 0..origins.len() {
            if blocked[mover] {
                continue;
            }
            for occupant in 0..origins.len() {
                if destinations[mover] == origins[occupant] && blocked[occupant] {
                    blocked[mover] = true;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }

    origins
        .into_iter()
        .enumerate()
        .map(|(index, origin)| {
            if blocked[index] {
                origin
            } else {
                destinations[index]
            }
        })
        .collect()
}

fn resolve_sheep(state: &GameState, dogs: &[Pos]) -> Vec<Pos> {
    let tie_break_order = sheep_action_order(state.turn);
    let desired: Vec<Pos> = state
        .sheep
        .iter()
        .map(|sheep| {
            let mut best_position = *sheep;
            let mut best_distance = i16::MIN;

            for action in tie_break_order {
                let candidate = moved(*sheep, action);
                if !is_legal_sheep_candidate(state, *sheep, candidate, dogs) {
                    continue;
                }
                let distance = dogs
                    .iter()
                    .map(|dog| manhattan(candidate, *dog))
                    .min()
                    .unwrap_or(i16::MAX);
                if distance > best_distance {
                    best_distance = distance;
                    best_position = candidate;
                }
            }
            best_position
        })
        .collect();

    let mut counts = BTreeMap::new();
    for destination in &desired {
        *counts.entry(*destination).or_insert(0usize) += 1;
    }
    desired
        .into_iter()
        .enumerate()
        .map(|(index, destination)| {
            if counts[&destination] > 1 {
                state.sheep[index]
            } else {
                destination
            }
        })
        .collect()
}

fn is_legal_sheep_candidate(state: &GameState, sheep: Pos, candidate: Pos, dogs: &[Pos]) -> bool {
    in_bounds(state, candidate)
        && !state.rocks.contains(&candidate)
        && !dogs.contains(&candidate)
        && (candidate == sheep || !state.sheep.contains(&candidate))
}

fn sheep_action_order(turn: u64) -> [Action; 5] {
    match turn % 4 {
        0 => [
            Action::Up,
            Action::Right,
            Action::Down,
            Action::Left,
            Action::Stay,
        ],
        1 => [
            Action::Right,
            Action::Down,
            Action::Left,
            Action::Up,
            Action::Stay,
        ],
        2 => [
            Action::Down,
            Action::Left,
            Action::Up,
            Action::Right,
            Action::Stay,
        ],
        _ => [
            Action::Left,
            Action::Up,
            Action::Right,
            Action::Down,
            Action::Stay,
        ],
    }
}

fn score_and_remove_sheep(state: &mut GameState) {
    let mut remaining = Vec::with_capacity(state.sheep.len());
    for sheep in state.sheep.drain(..) {
        if let Some(player) = state
            .players
            .iter_mut()
            .find(|player| player.pen.contains(&sheep))
        {
            player.score = player.score.saturating_add(1);
        } else {
            remaining.push(sheep);
        }
    }
    state.sheep = remaining;
}

fn update_terminal_status(state: &mut GameState) {
    let clinched = clinched_winner(state);
    state.game_over = state.sheep.is_empty() || clinched.is_some();
    state.winners.clear();
    if state.game_over {
        if let Some(winner) = clinched {
            state.winners.push(winner);
            return;
        }
        let high_score = state
            .players
            .iter()
            .map(|player| player.score)
            .max()
            .unwrap_or(0);
        state.winners.extend(
            state
                .players
                .iter()
                .filter(|player| player.score == high_score)
                .map(|player| player.seat),
        );
    }
}

/// Returns a winner only when even awarding every remaining sheep to one
/// opponent could not produce a tie. A tie is considered a changed outcome.
fn clinched_winner(state: &GameState) -> Option<SeatId> {
    let remaining = state.sheep.len() as u64;
    state.players.iter().find_map(|leader| {
        let leader_score = u64::from(leader.score);
        let opponent_ceiling = state
            .players
            .iter()
            .filter(|player| player.seat != leader.seat)
            .map(|player| u64::from(player.score) + remaining)
            .max()?;
        (leader_score > opponent_ceiling).then_some(leader.seat)
    })
}

#[derive(Clone)]
struct ArenaSlot {
    dog: Pos,
    pen: Vec<Pos>,
}

fn arena_slots() -> Vec<ArenaSlot> {
    let offsets = [9, 16, 24, 31];
    let mut slots = Vec::with_capacity(16);

    for x in offsets {
        slots.push(ArenaSlot {
            dog: Pos::new(x, 5),
            pen: rectangle(x - 1, 0, x + 1, 2),
        });
    }
    for y in offsets {
        slots.push(ArenaSlot {
            dog: Pos::new(35, y),
            pen: rectangle(38, y - 1, 40, y + 1),
        });
    }
    for x in offsets.into_iter().rev() {
        slots.push(ArenaSlot {
            dog: Pos::new(x, 35),
            pen: rectangle(x - 1, 38, x + 1, 40),
        });
    }
    for y in offsets.into_iter().rev() {
        slots.push(ArenaSlot {
            dog: Pos::new(5, y),
            pen: rectangle(0, y - 1, 2, y + 1),
        });
    }
    slots
}

fn rectangle(min_x: i8, min_y: i8, max_x: i8, max_y: i8) -> Vec<Pos> {
    let mut positions = Vec::new();
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            positions.push(Pos::new(x, y));
        }
    }
    positions
}

fn balanced_slot_indices(player_count: u8) -> Vec<usize> {
    (0..usize::from(player_count))
        .map(|index| index * usize::from(MAX_PLAYERS) / usize::from(player_count))
        .collect()
}

fn central_sheep(player_count: u8) -> Vec<Pos> {
    let target = usize::from(player_count.max(4)) * 8;
    let center = Pos::new(20, 20);
    let mut bases = Vec::new();
    for dx in 2i8..=12 {
        for dy in 1i8..dx {
            bases.push((dx, dy));
        }
    }
    bases.sort_by_key(|(dx, dy)| (i16::from(*dx + *dy), *dx, *dy));

    let mut sheep = Vec::with_capacity(target);
    for (dx, dy) in bases {
        if sheep.len() >= target {
            break;
        }
        let orbit: BTreeSet<Pos> = [
            Pos::new(center.x + dx, center.y + dy),
            Pos::new(center.x + dx, center.y - dy),
            Pos::new(center.x - dx, center.y + dy),
            Pos::new(center.x - dx, center.y - dy),
            Pos::new(center.x + dy, center.y + dx),
            Pos::new(center.x + dy, center.y - dx),
            Pos::new(center.x - dy, center.y + dx),
            Pos::new(center.x - dy, center.y - dx),
        ]
        .into_iter()
        .collect();
        sheep.extend(orbit);
    }
    sheep.sort();
    sheep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_support_two_through_sixteen_players() {
        for count in MIN_PLAYERS..=MAX_PLAYERS {
            let state = initial_state_for_players(count).unwrap();
            assert_eq!(state.players.len(), usize::from(count));
            assert_eq!(state.sheep.len(), usize::from(count.max(4)) * 8);
            assert_eq!(
                state
                    .players
                    .iter()
                    .map(|p| p.dog)
                    .collect::<BTreeSet<_>>()
                    .len(),
                usize::from(count)
            );
        }
    }

    #[test]
    fn layout_generation_is_deterministic() {
        assert_eq!(initial_state_for_players(16), initial_state_for_players(16));
    }

    #[test]
    fn two_players_start_opposite() {
        let state = initial_state_for_players(2).unwrap();
        assert_eq!(state.players[0].dog, Pos::new(9, 5));
        assert_eq!(state.players[1].dog, Pos::new(31, 35));
    }

    #[test]
    fn stay_is_an_explicit_legal_action() {
        let state = initial_state();
        assert!(legal_actions(&state, 0).contains(&Action::Stay));
    }

    #[test]
    fn illegal_direction_is_rejected() {
        let mut state = initial_state();
        state.players[0].dog = Pos::new(10, 10);
        state.sheep = vec![Pos::new(11, 10)];
        let actions = BTreeMap::from([(0, Action::Right), (1, Action::Stay)]);
        assert_eq!(
            step(&state, &actions),
            Err(RuleError::IllegalAction(0, Action::Right))
        );
    }

    #[test]
    fn dogs_contesting_a_destination_all_bounce() {
        let mut state = initial_state_for_players(3).unwrap();
        state.sheep = vec![Pos::new(20, 20)];
        state.players[0].dog = Pos::new(10, 10);
        state.players[1].dog = Pos::new(12, 10);
        state.players[2].dog = Pos::new(20, 30);
        let actions = BTreeMap::from([(0, Action::Right), (1, Action::Left), (2, Action::Stay)]);
        let next = step(&state, &actions).unwrap();
        assert_eq!(next.players[0].dog, Pos::new(10, 10));
        assert_eq!(next.players[1].dog, Pos::new(12, 10));
    }

    #[test]
    fn blocked_dog_dependency_cascades() {
        let mut state = initial_state_for_players(3).unwrap();
        state.sheep = vec![Pos::new(20, 20)];
        state.players[0].dog = Pos::new(10, 10);
        state.players[1].dog = Pos::new(11, 10);
        state.players[2].dog = Pos::new(12, 10);
        let actions = BTreeMap::from([(0, Action::Right), (1, Action::Right), (2, Action::Stay)]);
        let next = step(&state, &actions).unwrap();
        assert_eq!(
            next.players.iter().map(|p| p.dog).collect::<Vec<_>>(),
            vec![Pos::new(10, 10), Pos::new(11, 10), Pos::new(12, 10),]
        );
    }

    #[test]
    fn direct_swap_bounces() {
        let mut state = initial_state();
        state.sheep = vec![Pos::new(20, 20)];
        state.players[0].dog = Pos::new(10, 10);
        state.players[1].dog = Pos::new(11, 10);
        let actions = BTreeMap::from([(0, Action::Right), (1, Action::Left)]);
        let next = step(&state, &actions).unwrap();
        assert_eq!(next.players[0].dog, Pos::new(10, 10));
        assert_eq!(next.players[1].dog, Pos::new(11, 10));
    }

    #[test]
    fn sheep_move_away_from_nearest_dog() {
        let mut state = initial_state();
        state.turn = 1;
        state.players[0].dog = Pos::new(2, 3);
        state.players[1].dog = Pos::new(30, 30);
        state.sheep = vec![Pos::new(3, 3)];
        let actions = BTreeMap::from([(0, Action::Up), (1, Action::Up)]);
        let next = step(&state, &actions).unwrap();
        assert_eq!(next.sheep, vec![Pos::new(4, 3)]);
    }

    #[test]
    fn sheep_entering_a_pen_scores_and_is_removed() {
        let mut state = initial_state();
        state.players[0].dog = Pos::new(9, 4);
        state.sheep = vec![Pos::new(9, 3)];
        let actions = BTreeMap::from([(0, Action::Stay), (1, Action::Stay)]);

        let next = step(&state, &actions).unwrap();

        assert_eq!(next.players[0].score, 1);
        assert!(next.sheep.is_empty());
    }

    #[test]
    fn scoring_is_symmetric_for_an_opposite_pen() {
        let mut state = initial_state();
        state.turn = 2;
        state.players[1].dog = Pos::new(31, 36);
        state.sheep = vec![Pos::new(31, 37)];
        let actions = BTreeMap::from([(0, Action::Stay), (1, Action::Stay)]);

        let next = step(&state, &actions).unwrap();

        assert_eq!(next.players[1].score, 1);
        assert!(next.sheep.is_empty());
    }

    #[test]
    fn an_uncatchable_lead_ends_the_game() {
        let mut state = initial_state();
        state.players[0].score = 5;
        state.players[1].score = 1;
        state.sheep = vec![Pos::new(20, 20), Pos::new(21, 20), Pos::new(22, 20)];

        assert!(is_terminal(&state));
        let terminal = step(&state, &BTreeMap::new()).unwrap();
        assert!(terminal.game_over);
        assert_eq!(terminal.winners, vec![0]);
    }

    #[test]
    fn a_lead_that_can_be_tied_does_not_end_the_game() {
        let mut state = initial_state();
        state.players[0].score = 5;
        state.players[1].score = 1;
        state.sheep = vec![
            Pos::new(20, 20),
            Pos::new(21, 20),
            Pos::new(22, 20),
            Pos::new(23, 20),
        ];

        assert!(!is_terminal(&state));
    }

    #[test]
    fn high_turn_numbers_do_not_end_the_game() {
        let mut state = initial_state();
        state.turn = 1_000_000;
        state.sheep = vec![Pos::new(20, 20)];
        state.players[0].score = 0;
        state.players[1].score = 0;

        assert!(!is_terminal(&state));
    }

    #[test]
    fn all_sheep_scored_can_end_in_a_tie() {
        let mut state = initial_state();
        state.players[0].score = 3;
        state.players[1].score = 3;
        state.sheep.clear();

        let terminal = step(&state, &BTreeMap::new()).unwrap();

        assert!(terminal.game_over);
        assert_eq!(terminal.winners, vec![0, 1]);
    }

    #[test]
    fn missing_submission_becomes_stay() {
        let state = initial_state();
        let next = step(&state, &BTreeMap::new()).unwrap();
        assert_eq!(next.players[0].dog, state.players[0].dog);
        assert_eq!(next.players[0].last_action, Some(Action::Stay));
        assert_eq!(next.turn, 1);
    }

    #[test]
    fn step_does_not_mutate_input() {
        let state = initial_state();
        let snapshot = state.clone();
        let _ = step(&state, &BTreeMap::new());
        assert_eq!(state, snapshot);
    }
}
