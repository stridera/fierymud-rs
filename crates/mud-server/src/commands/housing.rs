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
            _ => None,
        },
    }
}

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
