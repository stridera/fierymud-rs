//! Player-facing housing commands. `cmd_home` and the
//! `house place / take / rename / guest` cluster live in
//! `info.rs` for historical reasons (they share helpers with
//! several Info-section handlers); this file is the home for
//! the async, guest-list-gated `visit` command.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, HouseSummary, HousingIndex, Located, WorldKey};

use crate::commands::{AsyncCommand, Category, Command, Help, cmd_look, cmd_mail_stub, send_to};

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
                   guest via `house guest add <you>`. Synthesizes \
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

#[allow(clippy::too_many_lines)]
async fn cmd_visit(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let name = args.trim();
    if name.is_empty() {
        send_to(world, player, "Usage: visit <player>\r\n");
        return;
    }
    // Visitor's own character_id — the guest-list check looks for
    // it in the owner's PlayerHouseGuests rows.
    let Some(visitor_cid) = world
        .get::<Account>(player)
        .map(|a| a.character_id.clone())
    else {
        send_to(world, player, "You aren't logged in as a character.\r\n");
        return;
    };
    let target = match mud_db::characters::find_by_name(pool, name).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            send_to(world, player, format!("No character named '{name}'.\r\n"));
            return;
        }
        Err(e) => {
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    // Self-visit is always allowed (covers the "lost your house key"
    // case where the owner's online HouseSummary was bumped).
    let is_self = target.id == visitor_cid;
    let house = match mud_db::housing::for_character(pool, &target.id).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            send_to(
                world,
                player,
                format!("{} doesn't own a house.\r\n", target.name),
            );
            return;
        }
        Err(e) => {
            send_to(world, player, format!("DB error: {e}\r\n"));
            return;
        }
    };
    let guests = mud_db::housing::guests_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    if !is_self && !guests.iter().any(|g| g.character_id == visitor_cid) {
        send_to(
            world,
            player,
            format!(
                "{} hasn't added you to their guest list.\r\n",
                target.name,
            ),
        );
        return;
    }
    let rooms = mud_db::housing::rooms_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    if rooms.is_empty() {
        send_to(
            world,
            player,
            format!("{}'s house has no rooms.\r\n", target.name),
        );
        return;
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
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = foyer_entity;
    }
    send_to(
        world,
        player,
        format!("You step into {}'s house.\r\n", target.name),
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
async fn cmd_house_expand(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
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
        send_to(world, player, "Your house has no foyer — that shouldn't happen.\r\n");
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
    let per_room = cfg.get_i64(
        "housing",
        "expand_per_room",
        DEFAULT_HOUSE_EXPAND_PER_ROOM,
    );
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
                "You need {cost} copper on hand to expand (you have {on_hand}).\r\n"
            ),
        );
        return;
    }
    // Deduct sync — async DB error path will roll back if needed.
    if let Some(mut w) = world.get_mut::<mud_world::Wealth>(player) {
        w.0 -= cost;
    }
    let new_name = format!("Empty Room #{}", summary.rooms.len());
    let new_desc = "An unfurnished new room. Use `house describe <#> <text>` \
                    or `house rename <#> <name>` to make it your own."
        .to_string();
    let result = mud_db::housing::add_room(
        pool,
        summary.house_id,
        foyer.id,
        "North",
        "South",
        &new_name,
        &new_desc,
    )
    .await;
    let (row_id, local_index) = match result {
        Ok(pair) => pair,
        Err(e) => {
            // Refund: the DB write didn't take, so the player keeps
            // their copper.
            if let Some(mut w) = world.get_mut::<mud_world::Wealth>(player) {
                w.0 += cost;
            }
            send_to(world, player, format!("Expansion failed: {e}\r\n"));
            return;
        }
    };
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
            from_room_id: foyer.id,
            to_room_id: row_id,
            direction: "North".to_string(),
        });
        s.exits.push(mud_world::HouseExitEntry {
            from_room_id: row_id,
            to_room_id: foyer.id,
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
        synthesize_single_room(
            world,
            summary.house_id,
            local_index,
            &new_name,
            &new_desc,
        );
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
            mud_world::HouseRoom { house_id, local_index },
            Named { name: name.to_string() },
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
                },
            );
        }
    }
}
