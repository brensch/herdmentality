//! Silly name and lobby-word generation.
//!
//! Crates such as `petname` / `random_word` exist for this, but they pull in
//! `rand`/`getrandom`, which need extra wasm wiring and bloat the bundle for a
//! handful of words. Embedding small, on-theme lists keeps the build trivial on
//! `wasm32-unknown-unknown` and lets the words stay sheep-and-dog flavored.

use js_sys::Math;

/// Player names are built from a first part + a last part so they read like a
/// proper (very silly) name, e.g. "Captain Fleecington". Kept short enough that
/// the longest pairing still fits the 24-character display-name limit, and big
/// enough (26 x 26 = 676 combos) to comfortably name a full 16-dog lobby.
const FIRST_NAMES: &[&str] = &[
    "Sir", "Captain", "Lord", "Lady", "Major", "Baron", "Duke", "Sergeant", "Professor", "Colonel",
    "Madam", "Chief", "Wooly", "Fluffy", "Scruffy", "Biscuit", "Bonnie", "Angus", "Clover", "Daisy",
    "Rufus", "Bella", "Maple", "Shadow", "Pepper", "Waffles",
];

/// Last part of a player name — dog- and sheep-flavoured surnames.
const LAST_NAMES: &[&str] = &[
    "Barksalot",
    "Fleecington",
    "Waggletail",
    "Muttonchops",
    "Pawsworth",
    "Woolworth",
    "Fluffbottom",
    "Chewington",
    "Snufflekins",
    "Lambchop",
    "McFluff",
    "Woofington",
    "Shepworth",
    "Curlyhorn",
    "Tailwagger",
    "Bleatley",
    "Goodboy",
    "Pawson",
    "Fleecewell",
    "Ramsbottom",
    "Borkowski",
    "Nibbles",
    "Drools",
    "Baaxter",
    "Cloudchaser",
    "Wigglesworth",
];

/// First half of a two-word lobby slug.
const LOBBY_ADJECTIVES: &[&str] = &[
    "wiggly", "cosmic", "soggy", "grumpy", "sneaky", "fluffy", "turbo", "sleepy", "spicy", "mighty",
    "wonky", "zippy", "cheeky", "noble", "feral", "jolly", "crispy", "mellow", "rowdy", "dizzy",
];

/// Second half of a two-word lobby slug.
const LOBBY_NOUNS: &[&str] = &[
    "sheep", "collie", "lamb", "ewe", "ram", "corgi", "shepherd", "pup", "flock", "mutton",
    "fleece", "woofer", "herder", "goodboy", "paddock", "pasture", "border", "heeler", "meadow",
    "kelpie",
];

fn pick<'a>(list: &'a [&'a str]) -> &'a str {
    let index = (Math::random() * list.len() as f64) as usize;
    list[index.min(list.len() - 1)]
}

/// A random two-part player name, e.g. "Captain Fleecington".
pub fn random_player_name() -> String {
    format!("{} {}", pick(FIRST_NAMES), pick(LAST_NAMES))
}

/// A random two-word lobby slug, e.g. `wiggly-sheep`.
pub fn random_lobby_slug() -> String {
    format!("{}-{}", pick(LOBBY_ADJECTIVES), pick(LOBBY_NOUNS))
}
