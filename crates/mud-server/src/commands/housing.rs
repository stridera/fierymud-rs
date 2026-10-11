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
    AsyncCommand, Category, Command, Help, cmd_look, cmd_mail_stub, send_msg, send_to,
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
#[allow(clippy::too_many_lines)]
async fn cmd_house_expand(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
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
    // The copper and the room commit together, in the database, under the
    // character's save-order turn; the in-memory wealth follows through the
    // update channel while that turn is still held (see `charge_and_add_room`).
    // Nothing is deducted here, so a job that never reports back (watchdog,
    // dropped link) cannot lose copper or leave a room unpaid.
    let (paid, paid_rx) = tokio::sync::oneshot::channel();
    let charge = RoomCharge {
        pool: pool.clone(),
        coordinator: world
            .get_resource::<crate::autosave::SaveCoordinator>()
            .cloned()
            .unwrap_or_default(),
        update_tx: world
            .get_resource::<crate::commands::PlayerUpdateTx>()
            .map(|t| t.0.clone()),
        character_id,
        cost,
        wealth_after: on_hand - cost,
        house_id: summary.house_id,
        from_room_id: foyer.id,
        name: new_name.clone(),
        description: new_desc.clone(),
    };
    charge_and_add_room(charge, paid);
    let wait = async move {
        paid_rx
            .await
            .unwrap_or_else(|_| Err("the expansion was interrupted".to_string()))
    };
    off_tick::run(world, player, wait, move |world, player, result| {
        finish_house_expand(
            world, player, &summary, foyer.id, cost, &new_name, &new_desc, result,
        );
    })
    .await;
}

/// Everything the paid room insert needs, owned, for its spawned task.
struct RoomCharge {
    pool: mud_db::sqlx::PgPool,
    coordinator: crate::autosave::SaveCoordinator,
    update_tx: Option<tokio::sync::mpsc::Sender<crate::commands::PendingPlayerUpdate>>,
    character_id: String,
    cost: i64,
    wealth_after: i64,
    house_id: i32,
    from_room_id: i32,
    name: String,
    description: String,
}

/// What a finished room charge reports: the new room's `(id, local_index)` and
/// whether the charge already reached the in-memory wealth through the update
/// channel, or is left for the finish step to apply.
type RoomCharged = Result<((i32, i32), bool), String>;

/// Insert the room and take the copper in one transaction, from a task that
/// holds the character's write turn ([`SaveCoordinator::spawn_ordered`]). The
/// task is not part of the command's job, so the job's watchdog cannot cut it
/// off between the commit and what follows it.
///
/// Wealth is database-first money, like quest gold: after the commit, and
/// still holding the turn, the charge is queued on the player-update channel.
/// Every snapshot taken under the turn (save, quit, checkpoint) and every
/// background one drains that channel first, so none can write the old
/// balance back; snapshots taken earlier are made stale by
/// `supersede_earlier_snapshots`.
fn charge_and_add_room(c: RoomCharge, paid: tokio::sync::oneshot::Sender<RoomCharged>) {
    let coordinator = c.coordinator.clone();
    let character_id = c.character_id.clone();
    coordinator.spawn_ordered(&character_id, move |mut ordered| async move {
        let added = mud_db::housing::add_room_paid(
            &c.pool,
            &c.character_id,
            c.wealth_after,
            c.house_id,
            c.from_room_id,
            "North",
            "South",
            &c.name,
            &c.description,
        )
        .await;
        let reply = match added {
            Ok(room) => {
                let applied = match &c.update_tx {
                    Some(tx) => tx
                        .send(crate::commands::PendingPlayerUpdate::WealthDelta {
                            character_id: c.character_id.clone(),
                            amount: -c.cost,
                        })
                        .await
                        .is_ok(),
                    None => false,
                };
                ordered.supersede_earlier_snapshots();
                Ok((room, applied))
            }
            Err(e) => Err(e.to_string()),
        };
        drop(ordered);
        let _ = paid.send(reply);
    });
}

/// The tick-side half of `house expand`: record the new room in the owner's
/// `HouseSummary` and build it in the world. The copper was taken by the
/// database transaction; a failed insert took none.
#[allow(clippy::too_many_arguments)]
fn finish_house_expand(
    world: &mut World,
    player: Entity,
    summary: &HouseSummary,
    foyer_id: i32,
    cost: i64,
    new_name: &str,
    new_desc: &str,
    result: RoomCharged,
) {
    let ((row_id, local_index), applied) = match result {
        Ok(done) => done,
        Err(e) => {
            send_to(world, player, format!("Expansion failed: {e}\r\n"));
            return;
        }
    };
    if !applied && let Some(mut w) = world.get_mut::<mud_world::Wealth>(player) {
        // No update channel to carry the charge: apply it here.
        w.0 -= cost;
    }
    // Update the in-memory summary so subsequent `house rooms` /
    // `house info` show the new room without a relog.
    if let Some(mut s) = world.get_mut::<HouseSummary>(player) {
        s.rooms.push(mud_world::HouseRoomEntry {
            id: row_id,
            local_index,
            name: new_name.to_string(),
            description: new_desc.to_string(),
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
        .contains_key(&(summary.house_id, 0));
    if foyer_already_spawned {
        synthesize_single_room(world, summary.house_id, local_index, new_name, new_desc);
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
    use super::*;
    use crate::commands::test_support::player_in;
    use mud_world::{HouseRoomEntry, Wealth};

    fn world_with_owner() -> (World, Entity, HouseSummary) {
        let mut world = World::new();
        world.init_resource::<HousingIndex>();
        let room = world.spawn_empty().id();
        let (player, _rx) = player_in(&mut world, room);
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
        world
            .entity_mut(player)
            .insert((Wealth(10_000), summary.clone()));
        (world, player, summary)
    }

    fn finish(world: &mut World, player: Entity, summary: &HouseSummary, r: RoomCharged) {
        finish_house_expand(world, player, summary, 100, 4_000, "Room", "desc", r);
    }

    /// The database transaction already took the copper. When the charge went
    /// out on the update channel the finish step must not take it again.
    #[test]
    fn a_charge_already_on_the_update_channel_is_not_taken_twice() {
        let (mut world, player, summary) = world_with_owner();
        finish(&mut world, player, &summary, Ok(((101, 1), true)));
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 10_000);
        assert_eq!(world.get::<HouseSummary>(player).unwrap().rooms.len(), 2);
    }

    #[test]
    fn without_an_update_channel_the_finish_step_applies_the_charge() {
        let (mut world, player, summary) = world_with_owner();
        finish(&mut world, player, &summary, Ok(((101, 1), false)));
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 6_000);
    }

    /// A failed insert took nothing and so refunds nothing.
    #[test]
    fn a_failed_expansion_leaves_the_wealth_alone() {
        let (mut world, player, summary) = world_with_owner();
        finish(&mut world, player, &summary, Err("db down".into()));
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 10_000);
        assert_eq!(world.get::<HouseSummary>(player).unwrap().rooms.len(), 1);
    }

    /// The insert that fails charges nothing: no update reaches the player,
    /// and the write turn is given back.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_insert_queues_no_charge_and_frees_the_turn() {
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        let coordinator = crate::autosave::SaveCoordinator::default();
        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let (paid, paid_rx) = tokio::sync::oneshot::channel();
        charge_and_add_room(
            RoomCharge {
                pool,
                coordinator: coordinator.clone(),
                update_tx: Some(update_tx),
                character_id: "c-house".into(),
                cost: 4_000,
                wealth_after: 6_000,
                house_id: 9,
                from_room_id: 100,
                name: "Room".into(),
                description: String::new(),
            },
            paid,
        );
        assert!(paid_rx.await.unwrap().is_err());
        assert!(update_rx.try_recv().is_err(), "no charge was queued");
        assert!(coordinator.try_begin_ordered("c-house").is_some());
    }
}
