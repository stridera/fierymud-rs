//! Player-facing housing commands. `cmd_home` and the
//! `house place / take / rename / guest` cluster live in
//! `info.rs` for historical reasons (they share helpers with
//! several Info-section handlers); this file is the home for
//! the async, guest-list-gated `visit` command.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, HouseSummary, HousingIndex, WorldKey};

use crate::commands::off_tick;
use crate::commands::{
    AsyncCommand, Category, Command, Help, cmd_look, cmd_mail_stub, send_msg, send_to, try_insert,
};

inventory::submit! {
    Command {
        names: &["visit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "visit <player>",
            summary: "Step into another player's house — guest list required.",
            long: "Refuses unless the named owner has added you as a \
                   guest via 'house guest add <you>'. Synthesizes \
                   the house's ECS rooms on first call (cached in \
                   HousingIndex), then warps you to the foyer. \
                   Async: the multi-table guest-list + room fetch \
                   runs off the world tick.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "visit" => Some(Box::pin(cmd_visit(world, player, pool, args))),
            // `house expand` carves out a new room off the foyer's
            // north exit. The other `house` subcommands stay on
            // the sync dispatcher path inside cmd_house.
            "house" if args.trim().eq_ignore_ascii_case("expand") => {
                Some(Box::pin(cmd_house_expand(world, player, pool)))
            }
            _ => None,
        },
    }
}

/// Default per-room expansion costs in copper. Live values come
/// from `GameConfig` rows `housing.expand_base_cost` /
/// `housing.expand_per_room`; these constants are the call-site
/// fallback when the rows are missing or unparseable.
const DEFAULT_HOUSE_EXPAND_BASE_COST: i64 = 50_000;
const DEFAULT_HOUSE_EXPAND_PER_ROOM: i64 = 25_000;

/// The refusals a house visit shares with `recall`: a teleport, so the owner's
/// own house is subject to them as much as a guest's. Sends the reason and
/// returns true when the visit may not go ahead.
fn visit_blocked(world: &mut World, player: Entity) -> bool {
    if crate::commands::refuse_teleport(world, player, "visit a house") {
        return true;
    }
    if let Some(located) = world.get::<mud_world::Located>(player)
        && world.get::<mud_world::NoRecallRoom>(located.0).is_some()
    {
        send_to(world, player, "Some power blocks your passage.\r\n");
        return true;
    }
    false
}

/// `visit <player>`: the guest-list and house reads run off the tick
/// ([`fetch_visit`]); the teleport is finished on a later one
/// ([`finish_visit`]), which checks the refusals again.
async fn cmd_visit(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Usage: visit <player>\r\n");
        return;
    }
    if visit_blocked(world, player) {
        return;
    }
    // Visitor's own character_id — the guest-list check looks for
    // it in the owner's PlayerHouseGuests rows.
    let Some(visitor_cid) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        send_to(world, player, "You aren't logged in as a character.\r\n");
        return;
    };
    let fetch = fetch_visit(pool.clone(), name.to_string(), visitor_cid);
    off_tick::run(world, player, fetch, finish_visit).await;
}

/// What a visit needs from the database: the owner's name and house, or the
/// line to show the visitor instead.
#[allow(clippy::too_many_lines)]
async fn fetch_visit(
    pool: mud_db::sqlx::PgPool,
    name: String,
    visitor_cid: String,
) -> Result<(String, HouseSummary), String> {
    let pool = &pool;
    let name = name.as_str();
    let target = match mud_db::characters::find_by_name(pool, name).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(format!("No character named '{name}'.\r\n")),
        Err(e) => return Err(format!("DB error: {e}\r\n")),
    };
    // Self-visit is always allowed (covers the "lost your house key"
    // case where the owner's online HouseSummary was bumped).
    let is_self = target.id == visitor_cid;
    let house = match mud_db::housing::for_character(pool, &target.id).await {
        Ok(Some(h)) => h,
        Ok(None) => return Err(format!("{} doesn't own a house.\r\n", target.name)),
        Err(e) => return Err(format!("DB error: {e}\r\n")),
    };
    let guests = mud_db::housing::guests_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    if !is_self && !guests.iter().any(|g| g.character_id == visitor_cid) {
        return Err(format!(
            "{} hasn't added you to their guest list.\r\n",
            target.name
        ));
    }
    let rooms = mud_db::housing::rooms_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    if rooms.is_empty() {
        return Err(format!("{}'s house has no rooms.\r\n", target.name));
    }
    let exits = mud_db::housing::exits_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    let items = mud_db::housing::items_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    let summary = HouseSummary {
        house_id: house.id,
        entrance_room: WorldKey {
            zone: house.entrance_room_zone_id,
            id: house.entrance_room_id,
        },
        return_room: match (house.return_room_zone_id, house.return_room_id) {
            (Some(z), Some(i)) => Some(WorldKey { zone: z, id: i }),
            _ => None,
        },
        rooms: rooms
            .into_iter()
            .map(|r| mud_world::HouseRoomEntry {
                id: r.id,
                local_index: r.local_index,
                name: r.name,
                description: r.description,
                is_peaceful: r.is_peaceful,
                capacity: r.capacity,
            })
            .collect(),
        exits: exits
            .into_iter()
            .map(|e| mud_world::HouseExitEntry {
                from_room_id: e.from_room_id,
                to_room_id: e.to_room_id,
                direction: e.direction,
            })
            .collect(),
        items: items
            .into_iter()
            .map(|i| mud_world::HouseItemEntry {
                id: i.id,
                room_id: i.room_id,
                object_zone_id: i.object_zone_id,
                object_id: i.object_id,
                custom: i.custom(),
            })
            .collect(),
        guests: guests
            .into_iter()
            .map(|g| mud_world::HouseGuestEntry {
                character_id: g.character_id,
                can_place: g.can_place,
            })
            .collect(),
    };
    Ok((target.name, summary))
}

/// The tick-side half of `visit`: put the visitor in the foyer.
fn finish_visit(
    world: &mut World,
    player: Entity,
    fetched: Result<(String, HouseSummary), String>,
) {
    let (target_name, summary) = match fetched {
        Ok(found) => found,
        Err(line) => {
            send_msg(world, player, line);
            return;
        }
    };
    // Time has passed since the command was typed: it may have started a
    // fight or walked into a no-recall room.
    if visit_blocked(world, player) {
        return;
    }
    let foyer_local = summary
        .rooms
        .iter()
        .find(|r| r.local_index == 0)
        .map_or(summary.rooms[0].local_index, |r| r.local_index);
    let already_spawned = world
        .resource::<HousingIndex>()
        .by_key
        .contains_key(&(summary.house_id, foyer_local));
    if !already_spawned {
        crate::commands::synthesize_house_rooms(world, &summary);
    }
    let foyer = world
        .resource::<HousingIndex>()
        .by_key
        .get(&(summary.house_id, foyer_local))
        .copied();
    let Some(foyer_entity) = foyer else {
        send_to(world, player, "Couldn't resolve the house foyer.\r\n");
        return;
    };
    if crate::room_access::refuse_entry(world, player, foyer_entity) {
        return;
    }
    // Every refusal is behind us: only now does a sitter stand up.
    crate::commands::auto_stand(world, player);
    crate::combat::relocate(world, player, foyer_entity);
    send_to(
        world,
        player,
        format!("You step into {target_name}'s house.\r\n"),
    );
    cmd_look(world, player, "");
}

/// `house expand` — pay copper to attach a new room off the
/// foyer's north exit. Refused if the player doesn't own a
/// house, the foyer's north wall is taken, or wealth is short.
/// On success, the new room is inserted, the player's
/// `HouseSummary` is refreshed in-place, and the new ECS `Room`
/// entity is synthesized + the foyer's exit gets wired so the
/// owner can walk into it immediately.
#[allow(clippy::too_many_lines, clippy::unused_async)] // async: the async-command dispatch shape
async fn cmd_house_expand(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    // The last expansion has not settled: the summary below is stale and the
    // balance already carries its charge.
    if world.get::<PendingHouseExpand>(player).is_some() {
        send_to(
            world,
            player,
            "Construction on your house is still under way.\r\n",
        );
        return;
    }
    let summary = world.get::<HouseSummary>(player).cloned();
    let Some(summary) = summary else {
        send_to(
            world,
            player,
            "You don't own a house. Speak with a builder to claim one.\r\n",
        );
        return;
    };
    let Some(foyer) = summary.rooms.iter().find(|r| r.local_index == 0).cloned() else {
        send_to(
            world,
            player,
            "Your house has no foyer — that shouldn't happen.\r\n",
        );
        return;
    };
    let foyer_north_taken = summary
        .exits
        .iter()
        .any(|e| e.from_room_id == foyer.id && e.direction.eq_ignore_ascii_case("north"));
    if foyer_north_taken {
        send_to(
            world,
            player,
            "Your foyer's north wall already opens onto another room — \
             rename or remove that one before expanding further.\r\n",
        );
        return;
    }
    let cfg = world.resource::<mud_world::RuntimeConfig>();
    let per_room = cfg.get_i64("housing", "expand_per_room", DEFAULT_HOUSE_EXPAND_PER_ROOM);
    let base = cfg.get_i64(
        "housing",
        "expand_base_cost",
        DEFAULT_HOUSE_EXPAND_BASE_COST,
    );
    let cost = i64::try_from(summary.rooms.len())
        .unwrap_or(0)
        .saturating_sub(1)
        .max(0)
        .saturating_mul(per_room)
        .saturating_add(base);
    let on_hand = world.get::<mud_world::Wealth>(player).map_or(0, |w| w.0);
    if on_hand < cost {
        send_to(
            world,
            player,
            format!(
                "You need {} on hand to expand (you have {}).\r\n",
                crate::commands::format_amount(cost),
                crate::commands::format_amount(on_hand)
            ),
        );
        return;
    }
    let new_name = format!("Empty Room #{}", summary.rooms.len());
    let new_desc = "An unfurnished new room. Use 'house describe <#> <text>' \
                    or 'house rename <#> <name>' to make it your own."
        .to_string();
    let Some(character_id) = world.get::<Account>(player).map(|a| a.character_id.clone()) else {
        send_to(world, player, "You aren't logged in as a character.\r\n");
        return;
    };
    let Some(update_tx) = world
        .get_resource::<crate::commands::PlayerUpdateTx>()
        .map(|t| t.0.clone())
    else {
        send_to(
            world,
            player,
            "House expansion is unavailable right now.\r\n",
        );
        return;
    };
    let plan = ExpansionDone {
        house_id: summary.house_id,
        foyer_row_id: foyer.id,
        cost,
        name: new_name.clone(),
        description: new_desc.clone(),
        outcome: Err(String::new()),
    };
    let pool = pool.clone();
    let owner = character_id.clone();
    start_expansion(world, player, character_id, update_tx, plan, async move {
        match mud_db::housing::add_room_paid(
            &pool,
            &owner,
            cost,
            summary.house_id,
            foyer.id,
            "North",
            "South",
            &new_name,
            &new_desc,
        )
        .await
        {
            Ok(Some(room)) => Ok(room),
            Ok(None) => Err("your saved balance falls short of the cost; \
                             'save' and try again"
                .to_string()),
            Err(e) => Err(e.to_string()),
        }
    });
}

/// Take the copper now, on the tick, and mark the house as under
/// construction: nothing can spend it again, and a second `house expand` is
/// refused until the outcome arrives. The outcome (room added, or the copper
/// handed back) reaches the player through the ordered update channel, so no
/// timeout of the command can lose it.
fn start_expansion<F>(
    world: &mut World,
    player: Entity,
    character_id: String,
    update_tx: tokio::sync::mpsc::Sender<crate::commands::PendingPlayerUpdate>,
    plan: ExpansionDone,
    add: F,
) where
    F: std::future::Future<Output = Result<(i32, i32), String>> + Send + 'static,
{
    if let Some(mut w) = world.get_mut::<mud_world::Wealth>(player) {
        w.0 -= plan.cost;
    }
    try_insert(world, player, PendingHouseExpand);
    let charge = RoomCharge {
        coordinator: world
            .get_resource::<crate::autosave::SaveCoordinator>()
            .cloned()
            .unwrap_or_default(),
        update_tx,
        done: plan,
        character_id,
    };
    charge_and_add_room(charge, add);
}

/// Marks a player whose `house expand` is out: the copper is already taken
/// and the room not yet in the summary.
#[derive(Component)]
pub(crate) struct PendingHouseExpand;

/// What a settled `house expand` carries to the player.
#[derive(Debug, Clone)]
pub struct ExpansionDone {
    house_id: i32,
    foyer_row_id: i32,
    cost: i64,
    name: String,
    description: String,
    /// The new room's `(id, local_index)`, or why the insert failed.
    outcome: Result<(i32, i32), String>,
}

/// Everything the charge task needs, owned.
struct RoomCharge {
    coordinator: crate::autosave::SaveCoordinator,
    update_tx: tokio::sync::mpsc::Sender<crate::commands::PendingPlayerUpdate>,
    character_id: String,
    done: ExpansionDone,
}

/// Run `add` (the paid insert) from a task holding the character's write
/// turn ([`SaveCoordinator::spawn_ordered`]) and report the outcome through
/// the update channel. The task is independent of any command job, so no
/// watchdog can cut it off between the commit and the report.
///
/// The report is queued with `try_send` while the turn is held, so every
/// snapshot taken under the turn (save, quit) drains it first and the
/// refund of a failed insert cannot be written over. It must not wait for
/// queue room under the turn: the tick can itself be waiting for that turn
/// (a relog's `begin_ordered`) and is also what drains the queue, which
/// would be a deadlock. A full queue is rare; then the turn is released
/// first and the report awaits room, at the price of a short window in
/// which a snapshot still shows the charge.
fn charge_and_add_room<F>(c: RoomCharge, add: F)
where
    F: std::future::Future<Output = Result<(i32, i32), String>> + Send + 'static,
{
    use tokio::sync::mpsc::error::TrySendError;
    let coordinator = c.coordinator.clone();
    let character_id = c.character_id.clone();
    coordinator.spawn_ordered(&character_id, move |mut ordered| async move {
        let mut done = c.done;
        done.outcome = add.await;
        let msg = crate::commands::PendingPlayerUpdate::HouseExpandDone {
            character_id: c.character_id.clone(),
            done: Box::new(done),
        };
        // Snapshots taken before this report would write the charge (or its
        // absence) over the new balance.
        ordered.supersede_earlier_snapshots();
        match c.update_tx.try_send(msg) {
            Ok(()) => drop(ordered),
            Err(TrySendError::Full(msg)) => {
                drop(ordered);
                if c.update_tx.send(msg).await.is_err() {
                    tracing::error!(character_id = %c.character_id,
                        "house expansion outcome could not be delivered");
                }
            }
            Err(TrySendError::Closed(_)) => {
                tracing::error!(character_id = %c.character_id,
                    "house expansion outcome could not be delivered");
            }
        }
    });
}

/// The world-side half of `house expand`, run when the outcome drains: hand
/// the copper back if the insert failed, otherwise record the new room in the
/// owner's `HouseSummary` and build it. Runs whether or not the command that
/// started it is still around.
pub(crate) fn apply_expansion(world: &mut World, player: Entity, d: &ExpansionDone) {
    world.entity_mut(player).remove::<PendingHouseExpand>();
    let (row_id, local_index) = match &d.outcome {
        Ok(room) => *room,
        Err(e) => {
            if let Some(mut w) = world.get_mut::<mud_world::Wealth>(player) {
                w.0 = w.0.saturating_add(d.cost);
            }
            send_to(world, player, format!("Expansion failed: {e}\r\n"));
            return;
        }
    };
    let (house_id, foyer_id, new_name, new_desc, cost) =
        (d.house_id, d.foyer_row_id, &d.name, &d.description, d.cost);
    // Update the in-memory summary so subsequent `house rooms` /
    // `house info` show the new room without a relog.
    if let Some(mut s) = world.get_mut::<HouseSummary>(player) {
        s.rooms.push(mud_world::HouseRoomEntry {
            id: row_id,
            local_index,
            name: new_name.clone(),
            description: new_desc.clone(),
            is_peaceful: false,
            capacity: 20,
        });
        s.exits.push(mud_world::HouseExitEntry {
            from_room_id: foyer_id,
            to_room_id: row_id,
            direction: "North".to_string(),
        });
        s.exits.push(mud_world::HouseExitEntry {
            from_room_id: row_id,
            to_room_id: foyer_id,
            direction: "South".to_string(),
        });
    }
    // Synthesize the new ECS room + wire its exits. Rather than
    // re-spawning every existing room, we re-run synthesis only
    // when the foyer hasn't been spawned yet; otherwise spawn
    // just the new room and patch the foyer's Exits.
    let foyer_already_spawned = world
        .resource::<HousingIndex>()
        .by_key
        .contains_key(&(house_id, 0));
    if foyer_already_spawned {
        synthesize_single_room(world, house_id, local_index, new_name, new_desc);
    } else {
        // Cheap path — first call into this house since boot.
        // Re-read the summary (we just mutated it) and synth all.
        let fresh = world.get::<HouseSummary>(player).cloned();
        if let Some(fresh) = fresh {
            crate::commands::synthesize_house_rooms(world, &fresh);
        }
    }
    send_to(
        world,
        player,
        format!(
            "Construction crews carve out a new room (#{local_index}) to \
             your north. {cost} copper changes hands.\r\n"
        ),
    );
}

/// Spawn the ECS Room for one freshly-added house room and wire
/// the bidirectional north / south exit between it and the foyer.
/// The foyer `Room` entity is already in `HousingIndex` (caller
/// guarantees), so we look it up and patch its `Exits`.
fn synthesize_single_room(
    world: &mut World,
    house_id: i32,
    local_index: i32,
    name: &str,
    description: &str,
) {
    use mud_world::{Description, Exits, Named, Room, RoomSector};
    let new_entity = world
        .spawn((
            Room,
            mud_world::HouseRoom {
                house_id,
                local_index,
            },
            Named {
                name: name.to_string(),
            },
            Description(description.to_string()),
            RoomSector(mud_db::enums::Sector::Structure),
            Exits::default(),
        ))
        .id();
    world
        .resource_mut::<HousingIndex>()
        .by_key
        .insert((house_id, local_index), new_entity);
    let foyer_entity = world
        .resource::<HousingIndex>()
        .by_key
        .get(&(house_id, 0))
        .copied();
    if let Some(foyer_entity) = foyer_entity {
        if let Some(mut exits) = world.get_mut::<Exits>(foyer_entity) {
            exits.0.insert(
                mud_db::enums::Direction::North,
                mud_world::ExitData {
                    to: Some(new_entity),
                    state: mud_db::enums::ExitState::Open,
                    key: None,
                    description: None,
                    keywords: Vec::new(),
                    is_hidden: false,
                    is_pickproof: false,
                    is_bashable: false,
                    hit_points: None,
                },
            );
        }
        if let Some(mut exits) = world.get_mut::<Exits>(new_entity) {
            exits.0.insert(
                mud_db::enums::Direction::South,
                mud_world::ExitData {
                    to: Some(foyer_entity),
                    state: mud_db::enums::ExitState::Open,
                    key: None,
                    description: None,
                    keywords: Vec::new(),
                    is_hidden: false,
                    is_pickproof: false,
                    is_bashable: false,
                    hit_points: None,
                },
            );
        }
    }
}

#[cfg(test)]
mod expand_tests {
    use std::time::Duration;

    use super::*;
    use crate::commands::test_support::{Rx, drain, player_in};
    use crate::commands::{PlayerUpdateInbox, PlayerUpdateTx, drain_player_updates};
    use mud_world::{HouseRoomEntry, Wealth};

    const COST: i64 = 4_000;

    struct Fixture {
        world: World,
        player: Entity,
        rx: Rx,
        tx: tokio::sync::mpsc::Sender<crate::commands::PendingPlayerUpdate>,
    }

    fn fixture() -> Fixture {
        let mut world = World::new();
        world.init_resource::<HousingIndex>();
        world.init_resource::<mud_world::RuntimeConfig>();
        world.init_resource::<crate::autosave::SaveCoordinator>();
        let (tx, update_rx) = tokio::sync::mpsc::channel(16);
        world.insert_resource(PlayerUpdateTx(tx.clone()));
        world.insert_resource(PlayerUpdateInbox(std::sync::Mutex::new(update_rx)));
        let room = world.spawn_empty().id();
        let (player, rx) = player_in(&mut world, room);
        let summary = HouseSummary {
            house_id: 9,
            entrance_room: WorldKey { zone: 30, id: 1 },
            return_room: None,
            rooms: vec![HouseRoomEntry {
                id: 100,
                local_index: 0,
                name: "Foyer".into(),
                description: String::new(),
                is_peaceful: true,
                capacity: 20,
            }],
            exits: vec![],
            items: vec![],
            guests: vec![],
        };
        world.entity_mut(player).insert((
            Wealth(10_000),
            summary,
            Account {
                user_id: "u".into(),
                character_id: "c-house".into(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: vec![],
            },
        ));
        Fixture {
            world,
            player,
            rx,
            tx,
        }
    }

    fn plan() -> ExpansionDone {
        ExpansionDone {
            house_id: 9,
            foyer_row_id: 100,
            cost: COST,
            name: "Room".into(),
            description: "desc".into(),
            outcome: Err(String::new()),
        }
    }

    /// Start an expansion whose insert takes a moment and then `result`.
    fn start(f: &mut Fixture, result: Result<(i32, i32), String>) {
        start_expansion(
            &mut f.world,
            f.player,
            "c-house".into(),
            f.tx.clone(),
            plan(),
            async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                result
            },
        );
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    fn wealth(f: &Fixture) -> i64 {
        f.world.get::<Wealth>(f.player).unwrap().0
    }

    fn rooms(f: &Fixture) -> usize {
        f.world.get::<HouseSummary>(f.player).unwrap().rooms.len()
    }

    fn pool() -> mud_db::sqlx::PgPool {
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_retry_while_an_expansion_is_pending_is_refused() {
        let mut f = fixture();
        start(&mut f, Ok((101, 1)));
        // The copper is held from the start.
        assert_eq!(wealth(&f), 10_000 - COST);
        cmd_house_expand(&mut f.world, f.player, &pool()).await;
        assert!(drain(&mut f.rx).contains("still under way"));
        assert_eq!(wealth(&f), 10_000 - COST, "the retry charged nothing");
        // Once the outcome has landed, the marker is gone.
        settle().await;
        drain_player_updates(&mut f.world);
        assert!(f.world.get::<PendingHouseExpand>(f.player).is_none());
    }

    /// The outcome comes through the update channel, not through any command
    /// job, so nothing that cuts a job short (its watchdog, a dropped link)
    /// can lose the room the database now has.
    #[tokio::test(flavor = "current_thread")]
    async fn the_room_and_the_charge_land_once_with_no_job_around() {
        let mut f = fixture();
        start(&mut f, Ok((101, 1)));
        settle().await;
        drain_player_updates(&mut f.world);
        assert_eq!(rooms(&f), 2);
        assert_eq!(wealth(&f), 10_000 - COST, "charged once");
        let exits = &f.world.get::<HouseSummary>(f.player).unwrap().exits;
        assert_eq!(exits.iter().filter(|e| e.direction == "North").count(), 1);
        assert!(drain(&mut f.rx).contains("new room (#1)"));
        // A second drain finds nothing more to apply.
        drain_player_updates(&mut f.world);
        assert_eq!(rooms(&f), 2);
        assert_eq!(wealth(&f), 10_000 - COST);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_insert_refunds_exactly_once() {
        let mut f = fixture();
        start(&mut f, Err("db down".into()));
        assert_eq!(wealth(&f), 10_000 - COST);
        settle().await;
        drain_player_updates(&mut f.world);
        drain_player_updates(&mut f.world);
        assert_eq!(wealth(&f), 10_000, "handed back once");
        assert_eq!(rooms(&f), 1);
        assert!(f.world.get::<PendingHouseExpand>(f.player).is_none());
        assert!(drain(&mut f.rx).contains("Expansion failed: db down"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wealth_never_goes_below_zero() {
        let mut f = fixture();
        f.tx.try_send(crate::commands::PendingPlayerUpdate::WealthDelta {
            character_id: "c-house".into(),
            amount: -50_000,
        })
        .unwrap();
        drain_player_updates(&mut f.world);
        assert_eq!(wealth(&f), 0);
    }

    /// Holding the turn must not block on a full update queue: the outcome is
    /// queued without waiting while the turn is held, and still arrives when
    /// the queue is full (after the turn is released).
    #[tokio::test(flavor = "current_thread")]
    async fn a_full_update_queue_does_not_hold_the_turn() {
        let mut f = fixture();
        for _ in 0..16 {
            f.tx.try_send(crate::commands::PendingPlayerUpdate::WealthDelta {
                character_id: "c-house".into(),
                amount: 0,
            })
            .unwrap();
        }
        start(&mut f, Ok((101, 1)));
        settle().await;
        let turns = f
            .world
            .resource::<crate::autosave::SaveCoordinator>()
            .clone();
        assert!(
            turns.try_begin_ordered("c-house").is_some(),
            "the turn is free although the queue is full"
        );
        drain_player_updates(&mut f.world);
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain_player_updates(&mut f.world);
        assert_eq!(rooms(&f), 2, "the outcome arrived once there was room");
    }

    /// Live database: the charge is relative, so a quest reward added to the
    /// stored wealth by another writer is not erased, and a short balance
    /// charges nothing and adds no room.
    #[tokio::test(flavor = "current_thread")]
    async fn the_stored_charge_keeps_a_concurrent_quest_reward() {
        let _lock = crate::commands::test_support::db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let Ok(Ok(pool)) = tokio::time::timeout(
            Duration::from_secs(3),
            mud_db::connect_with(&url, crate::commands::test_support::db_test_pool_settings()),
        )
        .await
        else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = format!("zz-house-{suffix}");
        if mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, wealth, updated_at) VALUES ($1, $2, 10000, NOW())",
        )
        .bind(&id)
        .bind(format!("ZzHouse{}", suffix % 1_000_000_000_000))
        .execute(&pool)
        .await
        .is_err()
        {
            eprintln!("skipping: Characters not writable");
            return;
        }
        let stored = |pool: mud_db::sqlx::PgPool, id: String| async move {
            mud_db::sqlx::query_scalar::<_, i64>("SELECT wealth FROM \"Characters\" WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        // A quest reward lands (SQL `wealth = wealth + Q`, outside the turn)
        // before the charge.
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = wealth + 500 WHERE id = $1")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(
            mud_db::housing::debit_wealth(&mut tx, &id, COST)
                .await
                .unwrap()
        );
        tx.commit().await.unwrap();
        assert_eq!(stored(pool.clone(), id.clone()).await, 10_500 - COST);
        // A balance that does not cover the cost: nothing is written, and
        // the paid insert reports it without touching any house row.
        let added =
            mud_db::housing::add_room_paid(&pool, &id, 1_000_000, 0, 0, "North", "South", "x", "y")
                .await
                .unwrap();
        assert!(added.is_none());
        assert_eq!(stored(pool.clone(), id.clone()).await, 10_500 - COST);
        mud_db::sqlx::query("DELETE FROM \"Characters\" WHERE id = $1")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
