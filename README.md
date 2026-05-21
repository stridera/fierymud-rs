# fierymud-rs

A clean-slate Rust rewrite of the [FieryMUD](https://fierymud.org) game server,
built on an ECS architecture. Players connect over telnet (with optional TLS and
GMCP); world content lives in PostgreSQL and is authored through the
[Muditor](https://muditor.utaboshi.com) web editor and imported by FieryLib.

> **Status:** in-progress. The combat, magic, movement, shop, quest, housing,
> and scripting systems are wired end-to-end, but this is not yet a drop-in
> replacement for the production CircleMUD server.

## Architecture

The server is a Cargo workspace built around [`bevy_ecs`](https://docs.rs/bevy_ecs)
for the world model, [`tokio`](https://tokio.rs) for networking, `sqlx` for the
database, and [`mlua`](https://docs.rs/mlua) for trigger scripting.

| Crate        | Responsibility                                                        |
|--------------|-----------------------------------------------------------------------|
| `mud-server` | Game loop, command dispatch, combat, magic, login, admin HTTP API.    |
| `mud-world`  | ECS components, resources, and the DB → world loader.                 |
| `mud-db`     | `sqlx` row structs and query helpers (PostgreSQL).                    |
| `mud-net`    | Telnet / TLS / GMCP protocol layer.                                   |
| `mud-script` | Lua host for mob/object/room triggers.                               |

World entities use composite keys `(zone_id, id)`; the schema's source of truth
is `muditor/packages/db/prisma/schema.prisma`.

## Building

```bash
# Build everything
cargo build --release

# Run the test suite
cargo test
```

The build forbids `unsafe_code` and runs `clippy::pedantic` at deny level across
the workspace.

## Running

The server expects a populated PostgreSQL database (imported via FieryLib) and a
`DATABASE_URL` in the environment (a `.env` file is read on startup).

```bash
DATABASE_URL=postgres://user@localhost/fierydev ./target/release/mud-server
```

| Listener        | Default            | Env override            |
|-----------------|--------------------|-------------------------|
| Telnet          | `0.0.0.0:4003`     | `MUD_LISTEN_ADDR`       |
| TLS (optional)  | `0.0.0.0:4443`     | `MUD_TLS_LISTEN_ADDR`   |
| Admin HTTP API  | `127.0.0.1:8080`   | `ADMIN_LISTEN_ADDR`     |

The admin HTTP API (localhost-only by default; bearer auth via `ADMIN_TOKEN`)
exposes `/api/admin/*` endpoints for world inspection and manipulation —
status, room/actor/mob rendering, virtual sessions, command execution,
teleport/spawn, sim pause/tick, and trigger reload/fire.

## Related projects

This server is one of several integrated projects sharing the FieryMUD
PostgreSQL database:

- **Muditor** — TypeScript/Next.js web editor for world building.
- **FieryLib** — Python one-time importer for legacy CircleMUD `lib/` data.
- **FieryMUD (legacy)** — the original C++23 CircleMUD-based production server.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
