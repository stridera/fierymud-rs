//! Account-shared item chest. Distinct from the per-character
//! inventory: anything stored here is reachable from every character
//! on the same `Users` row via the `chest` / `chest_deposit` /
//! `chest_withdraw` family.
//!
//! Persistence model:
//! * Storage lives entirely in the `account_items` table — there is
//!   no long-lived in-memory snapshot. `chest` re-reads on every
//!   invocation, so a sibling character's deposit is visible without
//!   any sync ceremony.
//! * `chest_deposit` INSERTs one row (via `mud_db::account_items::deposit`)
//!   and despawns the item entity from the calling character's
//!   inventory. The runtime side of the per-instance state worth
//!   preserving (`charges`, `liquid_remaining`, `liquid_type`,
//!   `light_remaining`) is serialized into `custom_data` JSON;
//!   future fields can extend the shape without a schema change.
//! * `chest_withdraw` DELETEs the row and spawns a fresh item entity
//!   in the player's inventory, rehydrating the runtime components
//!   from the proto + `custom_data`.
//!
//! Refusals match the rest of the wave 2.B gating: SOULBOUND items
//! refuse to leave the original owner, and NO_DROP items refuse to
//! be transferred at all. No silent loss — the player gets a clear
//! "can't deposit" line and the item stays in their inventory.
//!
//! Dispatch shape: these are `AsyncCommand`s because the DB
//! roundtrips need `await`. The sync `Command` entries each register
//! a `cmd_mail_stub` placeholder — the AsyncCommand wins dispatch
//! order, mirroring `cmd_save` / `cmd_mail` etc.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    Account, AttachedTriggers, BoardLink, Charges, Description, Item, Keywords,
    LightFuel, LiquidContainer, Located, Named, ObjectFlags, ObjectPrototypes,
    ObjectRestrictions, TriggerCatalog, WorldKey,
};
use serde::{Deserialize, Serialize};

use crate::commands::{
    AsyncCommand, Category, Command, EquipFilter, Help, cmd_mail_stub, find_carried_by,
    has_object_flag, has_restriction, name_of, send_to,
};

inventory::submit! {
    Command {
        names: &["chest", "accountchest", "achest"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "chest",
            summary: "List items in your account-shared chest.",
            long: "Shows every item any character on this account has \
                   deposited via `chest_deposit`. Use the slot number \
                   shown to withdraw via `chest_withdraw <slot>`.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["chest_deposit", "chestdeposit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "chest_deposit <item>",
            summary: "Store an inventory item in the account chest.",
            long: "Moves the named item from your inventory into the \
                   account-shared chest, where any character on this \
                   account can take it back via `chest_withdraw`. \
                   Refuses SOULBOUND and NO_DROP items.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["chest_withdraw", "chestwithdraw"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "chest_withdraw <slot>",
            summary: "Take an item back from the account chest.",
            long: "Withdraws the item at the given slot (see `chest` \
                   for the listing). The item is spawned back into your \
                   inventory with its per-instance state (charges, \
                   liquid level) restored.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "chest" | "accountchest" | "achest" => {
                Some(Box::pin(cmd_account_chest(world, player, pool)))
            }
            "chest_deposit" | "chestdeposit" => {
                Some(Box::pin(cmd_chest_deposit(world, player, pool, args)))
            }
            "chest_withdraw" | "chestwithdraw" => {
                Some(Box::pin(cmd_chest_withdraw(world, player, pool, args)))
            }
            _ => None,
        },
    }
}

/// Per-instance state worth round-tripping through the chest.
/// Mirrors the runtime-owned columns in `CharacterItems` so a chest
/// stash → withdraw cycle preserves charges and liquid state.
/// Extending this struct is a non-breaking change: serde
/// deserialization tolerates missing fields, and the deposit path
/// only writes fields it knows about.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ChestItemState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charges: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liquid_remaining: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liquid_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_remaining: Option<i32>,
}

async fn cmd_account_chest(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    let Some(account) = world.get::<Account>(player).cloned() else {
        send_to(world, player, "You don't have an account chest.\r\n");
        return;
    };
    let rows = match mud_db::account_items::list_for_user(pool, &account.user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "account chest list failed");
            send_to(world, player, "Couldn't read your account chest.\r\n");
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "\r\nYour account chest is empty.\r\n");
        return;
    }
    let protos = world.resource::<ObjectPrototypes>();
    let mut out = String::from("\r\n<b:cyan>Account chest:</>\r\n");
    for row in &rows {
        let proto_name = protos
            .by_key
            .get(&(row.object_zone_id, row.object_id))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("[{}:{}]", row.object_zone_id, row.object_id));
        let qty = if row.quantity > 1 {
            format!(" x{}", row.quantity)
        } else {
            String::new()
        };
        let stored_by = row
            .stored_by_character_id
            .as_deref()
            .unwrap_or("unknown");
        let stamp = row.stored_at.format("%Y-%m-%d %H:%M");
        out.push_str(&format!(
            "  [{slot}] {name}{qty}  <dim>(by {stored_by}, {stamp})</>\r\n",
            slot = row.slot,
            name = proto_name,
        ));
    }
    send_to(world, player, out);
}

async fn cmd_chest_deposit(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let target = args.trim();
    if target.is_empty() {
        send_to(world, player, "Deposit what?\r\n");
        return;
    }
    let Some(account) = world.get::<Account>(player).cloned() else {
        send_to(world, player, "You don't have an account chest.\r\n");
        return;
    };
    let Some(item) = find_carried_by(world, target, player, EquipFilter::Inventory) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{target}'.\r\n"),
        );
        return;
    };
    // SOULBOUND / NO_DROP gates — same shape as drop/give.
    if has_object_flag(world, item, mud_db::enums::ObjectFlag::Soulbound) {
        let item_name = name_of(world, item);
        send_to(
            world,
            player,
            format!("{item_name} is soulbound — it stays with you.\r\n"),
        );
        return;
    }
    if has_restriction(world, item, mud_db::enums::ObjectRestriction::NoDrop) {
        let item_name = name_of(world, item);
        send_to(
            world,
            player,
            format!("You can't seem to let go of {item_name}.\r\n"),
        );
        return;
    }
    // Capture proto key + per-instance state for the DB row.
    let Some(key) = world.get::<WorldKey>(item).copied() else {
        send_to(world, player, "That item has no proto key.\r\n");
        return;
    };
    let state = ChestItemState {
        charges: world.get::<Charges>(item).map(|c| c.0),
        liquid_remaining: world.get::<LiquidContainer>(item).map(|l| l.remaining),
        liquid_type: world
            .get::<LiquidContainer>(item)
            .map(|l| l.liquid.clone()),
        light_remaining: world.get::<LightFuel>(item).map(|f| f.remaining),
    };
    let custom_data = serde_json::to_value(&state).ok();
    let item_name = name_of(world, item);
    let insert_result = mud_db::account_items::deposit(
        pool,
        &account.user_id,
        key.zone,
        key.id,
        1,
        custom_data.as_ref(),
        Some(&account.character_id),
    )
    .await;
    if let Err(e) = insert_result {
        tracing::warn!(error = %e, "account chest deposit failed");
        send_to(world, player, "Couldn't deposit that item right now.\r\n");
        return;
    }
    // Despawn the inventory entity. The row in account_items is the
    // sole representation of the item until someone withdraws it.
    world.despawn(item);
    send_to(
        world,
        player,
        format!("You stash {item_name} in the account chest.\r\n"),
    );
    crate::commands::refresh_player_items_gmcp(world, player);
}

async fn cmd_chest_withdraw(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let target = args.trim();
    if target.is_empty() {
        send_to(world, player, "Withdraw which slot?\r\n");
        return;
    }
    let Some(account) = world.get::<Account>(player).cloned() else {
        send_to(world, player, "You don't have an account chest.\r\n");
        return;
    };
    // Resolve the target — first try numeric slot, then proto-name
    // substring match against the listing. The substring path lets
    // players write `chest_withdraw sword` instead of looking up the
    // slot number first.
    let rows = match mud_db::account_items::list_for_user(pool, &account.user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "account chest list failed");
            send_to(world, player, "Couldn't read your account chest.\r\n");
            return;
        }
    };
    if rows.is_empty() {
        send_to(world, player, "Your account chest is empty.\r\n");
        return;
    }
    let chosen = match target.parse::<i32>() {
        Ok(slot) => rows.iter().find(|r| r.slot == slot).cloned(),
        Err(_) => {
            let needle = target.to_ascii_lowercase();
            let protos = world.resource::<ObjectPrototypes>();
            rows.iter()
                .find(|r| {
                    protos
                        .by_key
                        .get(&(r.object_zone_id, r.object_id))
                        .map(|p| p.name.to_ascii_lowercase().contains(&needle))
                        .unwrap_or(false)
                })
                .cloned()
        }
    };
    let Some(row) = chosen else {
        send_to(
            world,
            player,
            format!("Nothing in your chest matches '{target}'.\r\n"),
        );
        return;
    };
    // DELETE first, then spawn — failing this order would risk
    // duplicating the item on a partial-write failure. The row's
    // returned shape gives us everything we need to rehydrate.
    let withdrawn = match mud_db::account_items::withdraw(pool, row.id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            send_to(world, player, "That item is already gone.\r\n");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "account chest withdraw failed");
            send_to(world, player, "Couldn't withdraw that item right now.\r\n");
            return;
        }
    };
    spawn_withdrawn_item(world, player, &withdrawn);
}

/// Spawn a chest row back into the player's inventory. Mirrors
/// `respawn::spawn_item_into`'s bundle but takes per-instance state
/// from `custom_data`. Pulled out so tests can exercise the
/// rehydration without a live DB.
pub(crate) fn spawn_withdrawn_item(
    world: &mut World,
    player: Entity,
    withdrawn: &mud_db::account_items::AccountItemRow,
) {
    let state: ChestItemState = withdrawn
        .custom_data
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let proto_key = (withdrawn.object_zone_id, withdrawn.object_id);
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&proto_key)
        .cloned();
    let Some(proto) = proto else {
        send_to(
            world,
            player,
            "Item's prototype is missing. Logging the row for staff review.\r\n",
        );
        tracing::warn!(
            zone = withdrawn.object_zone_id,
            id = withdrawn.object_id,
            "account chest withdraw: proto missing"
        );
        return;
    };
    let trigger_keys = world
        .get_resource::<TriggerCatalog>()
        .and_then(|cat| cat.object_attachments.get(&proto_key).cloned());
    let primary_slot = mud_world::wear_flags_primary_slot(&proto.wear_flags);
    let item_entity = {
        let mut bundle = world.spawn((
            Item,
            Named { name: proto.name.clone() },
            Keywords(proto.keywords.clone()),
            WorldKey { zone: proto.zone_id, id: proto.id },
            Located(player),
        ));
        if let Some(desc) = proto.examine_description.clone() {
            bundle.insert(Description(desc));
        }
        if let Some(s) = primary_slot {
            bundle.insert(mud_world::WearableIn(s));
        }
        if let Some(board_id) = proto.board_id {
            bundle.insert(BoardLink(board_id));
        }
        if let Some(liq) = proto.liquid.clone() {
            // Per-instance override beats proto defaults — same
            // shape as the character_items loader.
            bundle.insert(LiquidContainer {
                liquid: state.liquid_type.clone().unwrap_or(liq.liquid),
                capacity: liq.capacity,
                remaining: state.liquid_remaining.unwrap_or(liq.remaining),
                poisoned: liq.poisoned,
            });
        }
        if let Some(fuel) = proto.light_fuel {
            bundle.insert(LightFuel {
                capacity: fuel.capacity,
                remaining: state.light_remaining.unwrap_or(fuel.remaining),
            });
        }
        if let Some(keys) = trigger_keys {
            bundle.insert(AttachedTriggers(keys));
        }
        if !proto.flags.is_empty() {
            bundle.insert(ObjectFlags(proto.flags.clone()));
        }
        if !proto.restrictions.is_empty() {
            bundle.insert(ObjectRestrictions(proto.restrictions.clone()));
        }
        bundle.id()
    };
    if let Some(c) = state.charges
        && let Ok(mut em) = world.get_entity_mut(item_entity)
    {
        em.insert(Charges(c));
    }
    send_to(
        world,
        player,
        format!("You retrieve {} from the account chest.\r\n", proto.name),
    );
    crate::commands::refresh_player_items_gmcp(world, player);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chest_state_round_trips() {
        // The shape of ChestItemState must survive JSON round-trip
        // so per-instance fields (charges, liquid level) come back
        // intact on withdraw. Tests the serialization contract
        // independent of the actual DB write.
        let state = ChestItemState {
            charges: Some(3),
            liquid_remaining: Some(2),
            liquid_type: Some("WATER".to_string()),
            light_remaining: None,
        };
        let json = serde_json::to_value(&state).unwrap();
        let back: ChestItemState = serde_json::from_value(json).unwrap();
        assert_eq!(back.charges, Some(3));
        assert_eq!(back.liquid_remaining, Some(2));
        assert_eq!(back.liquid_type.as_deref(), Some("WATER"));
        assert_eq!(back.light_remaining, None);
    }

    #[test]
    fn chest_state_tolerates_missing_fields() {
        // Forward-compat: an older row that only saved `charges`
        // shouldn't fail to load just because newer fields exist.
        let json: serde_json::Value = serde_json::from_str(r#"{"charges":5}"#).unwrap();
        let back: ChestItemState = serde_json::from_value(json).unwrap();
        assert_eq!(back.charges, Some(5));
        assert_eq!(back.liquid_remaining, None);
    }
}
