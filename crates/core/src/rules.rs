use std::collections::{BTreeMap, BTreeSet};

pub type SeatId = u8;

pub const MIN_PLAYERS: u8 = 2;
pub const MAX_PLAYERS: u8 = 8;
pub const LAYOUT_VERSION: u16 = 3;

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

/// How the flock reacts to dogs each turn. Chosen per game and stored in
/// [`GameState`], so every simulation — the server's authoritative step and the
/// bots' lookahead clones alike — applies the same rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum SheepBehavior {
    /// The original flock: every sheep flees whichever dog is nearest, from
    /// anywhere on the board, every single turn.
    #[default]
    Classic,
    /// Calm and local: a sheep only reacts to a dog within close range, fleeing
    /// that one dog; with no dog near it simply grazes in place.
    Skittish,
    /// Herd animals: sheep flee nearby dogs but also pull toward one another, so
    /// the flock clumps and drifts as a single mass.
    Flocking,
    /// Hard to spook: a sheep holds its ground until a dog is right beside it.
    Lazy,
}

impl SheepBehavior {
    pub const ALL: [SheepBehavior; 4] =
        [Self::Classic, Self::Skittish, Self::Flocking, Self::Lazy];

    /// Stable identifier used on the wire and to round-trip the UI selection.
    pub fn id(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Skittish => "skittish",
            Self::Flocking => "flocking",
            Self::Lazy => "lazy",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|behavior| behavior.id() == id)
    }

    /// Short display name for the picker and HUD.
    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Skittish => "Skittish",
            Self::Flocking => "Flocking",
            Self::Lazy => "Lazy",
        }
    }

    /// One-line explanation shown alongside the picker.
    pub fn description(self) -> &'static str {
        match self {
            Self::Classic => "Every sheep flees the nearest dog from anywhere. The original chaos.",
            Self::Skittish => "Sheep only react to close dogs, grazing otherwise. Calm and easy to herd.",
            Self::Flocking => "Sheep stick together and move as one. Push the flock, not individuals.",
            Self::Lazy => "Sheep barely budge until a dog is right beside them. Slow and deliberate.",
        }
    }
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
    pub sheep_behavior: SheepBehavior,
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
    initial_state_with_behavior(player_count, SheepBehavior::default())
}

pub fn initial_state_with_behavior(
    player_count: u8,
    sheep_behavior: SheepBehavior,
) -> Result<GameState, RuleError> {
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
        sheep_behavior,
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

/// A dog within this many tiles spooks a Skittish sheep; beyond it, the sheep
/// grazes. Small enough that distant dogs no longer creep the whole flock into
/// the corners — the chief complaint with the Classic rule.
const SKITTISH_RADIUS: i16 = 6;
/// A Lazy sheep only bolts when a dog is essentially on top of it.
const LAZY_RADIUS: i16 = 2;
/// Flocking: how far a sheep looks for neighbours to cohere with, and the dog
/// range that still triggers a flee.
const FLOCK_NEIGHBOUR_RADIUS: i16 = 5;
const FLOCK_DOG_RADIUS: i16 = 6;

/// Resolve every sheep's next position: the chosen behaviour decides each
/// sheep's desired cell, then shared collision handling stops two sheep from
/// landing on the same tile.
fn resolve_sheep(state: &GameState, dogs: &[Pos]) -> Vec<Pos> {
    let desired: Vec<Pos> = match state.sheep_behavior {
        SheepBehavior::Classic => desired_classic(state, dogs),
        SheepBehavior::Skittish => desired_flee_nearest(state, dogs, SKITTISH_RADIUS),
        SheepBehavior::Lazy => desired_flee_nearest(state, dogs, LAZY_RADIUS),
        SheepBehavior::Flocking => desired_flocking(state, dogs),
    };
    resolve_sheep_collisions(state, desired)
}

/// Two sheep that pick the same tile both stay put.
fn resolve_sheep_collisions(state: &GameState, desired: Vec<Pos>) -> Vec<Pos> {
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

/// Classic: maximise distance to the nearest dog anywhere on the board, with a
/// tie-break order that rotates by turn. The original, board-wide reaction.
fn desired_classic(state: &GameState, dogs: &[Pos]) -> Vec<Pos> {
    let tie_break_order = sheep_action_order(state.turn);
    state
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
        .collect()
}

/// Skittish / Lazy: react only to the single nearest dog, and only when it is
/// within `radius`. Otherwise the sheep grazes in place, so distant dogs exert
/// no pull and the flock no longer funnels into the corners.
fn desired_flee_nearest(state: &GameState, dogs: &[Pos], radius: i16) -> Vec<Pos> {
    state
        .sheep
        .iter()
        .map(|sheep| {
            let Some((threat, distance)) = nearest_dog(*sheep, dogs) else {
                return *sheep;
            };
            if distance > radius {
                return *sheep;
            }
            // Squared Euclidean, not Manhattan: a dog directly to one side should
            // push the sheep straight away from it. Under Manhattan, fleeing
            // straight back ties with sliding sideways, so cardinal pushes failed.
            best_sheep_cell(state, *sheep, dogs, |candidate| dist_sq(candidate, threat))
        })
        .collect()
}

/// Flocking: flee a nearby dog while also pulling toward the centroid of nearby
/// sheep, so the flock holds together and shoves as a single body.
fn desired_flocking(state: &GameState, dogs: &[Pos]) -> Vec<Pos> {
    const FLEE_WEIGHT: i32 = 10;
    const COHESION_WEIGHT: i32 = 3;
    state
        .sheep
        .iter()
        .map(|sheep| {
            let threat = nearest_dog(*sheep, dogs)
                .filter(|(_, distance)| *distance <= FLOCK_DOG_RADIUS)
                .map(|(dog, _)| dog);
            let centroid = flock_centroid(state, *sheep);
            // With nothing pressuring or attracting it, a lone sheep grazes.
            if threat.is_none() && centroid.is_none() {
                return *sheep;
            }
            best_sheep_cell(state, *sheep, dogs, |candidate| {
                let flee = threat.map_or(0, |dog| FLEE_WEIGHT * dist_sq(candidate, dog));
                let cohere = centroid.map_or(0, |c| -COHESION_WEIGHT * dist_sq(candidate, c));
                flee + cohere
            })
        })
        .collect()
}

/// Average position of the other sheep within [`FLOCK_NEIGHBOUR_RADIUS`].
fn flock_centroid(state: &GameState, sheep: Pos) -> Option<Pos> {
    let mut count = 0i32;
    let (mut sx, mut sy) = (0i32, 0i32);
    for other in &state.sheep {
        if *other != sheep && manhattan(sheep, *other) <= FLOCK_NEIGHBOUR_RADIUS {
            count += 1;
            sx += i32::from(other.x);
            sy += i32::from(other.y);
        }
    }
    (count > 0).then(|| Pos::new((sx / count) as i8, (sy / count) as i8))
}

fn nearest_dog(sheep: Pos, dogs: &[Pos]) -> Option<(Pos, i16)> {
    dogs.iter()
        .map(|dog| (*dog, manhattan(sheep, *dog)))
        .min_by_key(|(_, distance)| *distance)
}

/// Squared Euclidean distance. Used for fleeing so a sheep moves *directly* away
/// from a threat rather than treating a perpendicular step as just as good.
fn dist_sq(a: Pos, b: Pos) -> i32 {
    let dx = i32::from(a.x) - i32::from(b.x);
    let dy = i32::from(a.y) - i32::from(b.y);
    dx * dx + dy * dy
}

/// Pick the legal cell that maximises `score`, breaking ties deterministically
/// with a preference for holding still (no turn-dependent jitter). Staying is
/// always legal, so this always returns a valid cell.
fn best_sheep_cell<F: Fn(Pos) -> i32>(
    state: &GameState,
    sheep: Pos,
    dogs: &[Pos],
    score: F,
) -> Pos {
    let mut best = sheep;
    let mut best_key = (i32::MIN, 0u8);
    for action in Action::ALL {
        let candidate = moved(sheep, action);
        if !is_legal_sheep_candidate(state, sheep, candidate, dogs) {
            continue;
        }
        let key = (score(candidate), sheep_tie_rank(action));
        if key > best_key {
            best_key = key;
            best = candidate;
        }
    }
    best
}

/// Higher wins ties; staying is preferred so a sheep only moves when it strictly
/// helps, which keeps the flock from twitching between equal options.
fn sheep_tie_rank(action: Action) -> u8 {
    match action {
        Action::Stay => 5,
        Action::Up => 4,
        Action::Right => 3,
        Action::Down => 2,
        Action::Left => 1,
    }
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
    // Two near-corner slots per edge (8 total). The mid-edge positions are left
    // out so no dog is stranded in the middle of a wall without a corner to herd
    // sheep against.
    let offsets = [9, 31];
    let mut slots = Vec::with_capacity(8);

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
    fn layouts_support_two_through_eight_players() {
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
        assert_eq!(initial_state_for_players(8), initial_state_for_players(8));
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
    fn skittish_sheep_ignore_a_distant_dog() {
        let mut state = initial_state_with_behavior(2, SheepBehavior::Skittish).unwrap();
        state.players[0].dog = Pos::new(0, 0);
        state.players[1].dog = Pos::new(40, 40);
        // A sheep far from every dog should graze rather than creep away.
        state.sheep = vec![Pos::new(20, 20)];
        let next = step(&state, &BTreeMap::new()).unwrap();
        assert_eq!(next.sheep, vec![Pos::new(20, 20)]);
    }

    #[test]
    fn skittish_sheep_flee_a_close_dog() {
        let mut state = initial_state_with_behavior(2, SheepBehavior::Skittish).unwrap();
        state.players[0].dog = Pos::new(20, 21);
        state.players[1].dog = Pos::new(40, 40);
        state.sheep = vec![Pos::new(20, 20)];
        let next = step(&state, &BTreeMap::from([(0, Action::Stay), (1, Action::Stay)])).unwrap();
        // The dog is directly below, so the sheep steps up, away from it.
        assert_eq!(next.sheep, vec![Pos::new(20, 19)]);
    }

    #[test]
    fn skittish_sheep_can_be_pushed_in_every_cardinal_direction() {
        // Dog adjacent on one side must push the sheep straight to the opposite
        // side — the bug the squared-distance flee fixes.
        let cases = [
            (Pos::new(19, 20), Pos::new(21, 20)), // dog left  -> sheep right
            (Pos::new(21, 20), Pos::new(19, 20)), // dog right -> sheep left
            (Pos::new(20, 19), Pos::new(20, 21)), // dog above -> sheep down
            (Pos::new(20, 21), Pos::new(20, 19)), // dog below -> sheep up
        ];
        for (dog, expected) in cases {
            let mut state = initial_state_with_behavior(2, SheepBehavior::Skittish).unwrap();
            state.players[0].dog = dog;
            state.players[1].dog = Pos::new(0, 0);
            state.sheep = vec![Pos::new(20, 20)];
            let next =
                step(&state, &BTreeMap::from([(0, Action::Stay), (1, Action::Stay)])).unwrap();
            assert_eq!(next.sheep, vec![expected], "dog at {dog:?}");
        }
    }

    #[test]
    fn lazy_sheep_ignore_a_dog_a_few_tiles_away() {
        let mut state = initial_state_with_behavior(2, SheepBehavior::Lazy).unwrap();
        state.players[0].dog = Pos::new(20, 24); // 4 tiles away: outside LAZY_RADIUS
        state.players[1].dog = Pos::new(40, 40);
        state.sheep = vec![Pos::new(20, 20)];
        let next = step(&state, &BTreeMap::new()).unwrap();
        assert_eq!(next.sheep, vec![Pos::new(20, 20)]);
    }

    #[test]
    fn behavior_round_trips_through_id() {
        for behavior in SheepBehavior::ALL {
            assert_eq!(SheepBehavior::from_id(behavior.id()), Some(behavior));
        }
        assert_eq!(SheepBehavior::from_id("nope"), None);
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
