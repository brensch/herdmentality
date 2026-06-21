# Herdcore

Deterministic simultaneous-move sheep herding for 2–16 players. The browser client uses
gRPC-Web; the authoritative Rust server persists hidden move submissions and resolved turns
to the pure-Rust Turso database before acknowledging or broadcasting them.

## Development

Install the browser tooling once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk
```

Run the external bot provider:

```bash
cargo run -p herdcore-bot-provider
```

Run the game server:

```bash
HERDCORE_BOT_PROVIDER_URL=http://127.0.0.1:55052 \
HERDCORE_PUBLIC_URL=http://127.0.0.1:55051 \
cargo run -p herdcore-server
```

Run the browser client:

```bash
trunk serve
```

Then open Trunk's URL. The development UI defaults to `http://127.0.0.1:55051` for the
game server.

## Game ending

There is no turn limit or fixed score target. A game ends after all sheep are scored, or
when one player's lead is greater than every opponent's score plus the number of sheep still
on the board. A possible tie is enough to keep the game running.

## Persistence ordering

The server follows two fail-closed durability barriers:

```text
receive move -> commit hidden submission -> acknowledge only its sender
resolve turn -> commit state + revealed actions + outbox -> broadcast TurnResolved
```

Turso uses WAL mode with `synchronous = FULL`. Its pure-Rust crypto feature is enabled, and
the upstream native-only vector accelerator is replaced by the safe scalar compatibility
crate in `crates/simsimd-pure`, so backend builds do not invoke a C compiler. The server
recovers active lobbies, pending moves, game snapshots, and deadlines from the database after
restart. The local database defaults to `target/herdcore.db`; set `HERDCORE_DB` to override it.
