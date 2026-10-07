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
//! * `chest_withdraw` DELETEs the row, INSERTs the matching
//!   `CharacterItems` row in the same transaction, and spawns a fresh
//!   item entity in the player's inventory (stamped with that row id),
//!   rehydrating the runtime components from the proto + `custom_data`.
//! * Both take the character's save-order turn first, so an in-flight
//!   background save can't re-insert a deposited row or delete a
//!   withdrawn one (see `autosave.rs`).
//!
//! Refusals match the rest of the wave 2.B gating: SOULBOUND items
//! refuse to leave the original owner, and `NO_DROP` items refuse to
//! be transferred at all. No silent loss — the player gets a clear
//! "can't deposit" line and the item stays in their inventory.
//!
//! Dispatch shape: these are `AsyncCommand`s because the DB
//! roundtrips need `await`. The sync `Command` entries each register
//! a `cmd_mail_stub` placeholder — the `AsyncCommand` wins dispatch
//! order, mirroring `cmd_save` / `cmd_mail` etc.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    AttachedTriggers, BoardLink, Charges, Description, Item, Keywords, LightFuel, LiquidContainer,
    Located, Named, ObjectFlags, ObjectPrototypes, ObjectRestrictions, TriggerCatalog, WorldKey,
};
use serde::{Deserialize, Serialize};

use crate::autosave::SaveCoordinator;
use crate::commands::{
    AsyncCommand, Category, Command, EquipFilter, Help, cmd_mail_stub, find_carried_by,
    has_object_flag, has_restriction, name_of, send_to,
};

inventory::submit! {
    Command {
        names: &["chest", "accountchest", "achest", "storage", "vault"],
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
            "chest" | "accountchest" | "achest" | "storage" | "vault" => {
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

async fn cmd_account_chest(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let Some(account) = crate::commands::require_linked_account(world, player, "the account chest")
    else {
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
            .map_or_else(
                || format!("[{}:{}]", row.object_zone_id, row.object_id),
                |p| p.name.clone(),
            );
        let qty = if row.quantity > 1 {
            format!(" x{}", row.quantity)
        } else {
            String::new()
        };
        let stored_by = row.stored_by_character_id.as_deref().unwrap_or("unknown");
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
    let Some(account) = crate::commands::require_linked_account(world, player, "the account chest")
    else {
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
    // A container's contents live as separate item entities; despawning the
    // container would silently destroy them, and the chest row has no way
    // to carry them. Make the player empty it first.
    if world
        .get::<mud_world::Contents>(item)
        .is_some_and(|c| c.iter().next().is_some())
    {
        let item_name = name_of(world, item);
        send_to(
            world,
            player,
            format!("{item_name} still has things in it. Empty it first.\r\n"),
        );
        return;
    }
    // Capture proto key + per-instance state for the DB row.
    let Some(key) = world.get::<WorldKey>(item).copied() else {
        send_to(world, player, "That item has no proto key.\r\n");
        return;
    };
    // Take the character's save-order turn BEFORE reading the item's row id:
    // an in-flight background save may be about to stamp a fresh id on it,
    // and must have finished (and been folded in) so the id we delete is
    // current and the save can't re-INSERT the row afterwards.
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let mut ordered = coordinator.begin_ordered(&account.character_id).await;
    coordinator.apply_completions(world);
    let state = ChestItemState {
        charges: world.get::<Charges>(item).map(|c| c.0),
        liquid_remaining: world.get::<LiquidContainer>(item).map(|l| l.remaining),
        liquid_type: world.get::<LiquidContainer>(item).map(|l| l.liquid.clone()),
        light_remaining: world.get::<LightFuel>(item).map(|f| f.remaining),
    };
    let custom_data = serde_json::to_value(&state).ok();
    let item_name = name_of(world, item);
    // The item's inventory row is removed in the same transaction as the
    // chest INSERT (see `deposit`), so a crash can't leave it in both.
    let inventory_row_id = world.get::<mud_world::PersistedItemId>(item).map(|p| p.0);
    let insert_result = mud_db::account_items::deposit(
        pool,
        &account.user_id,
        key.zone,
        key.id,
        1,
        custom_data.as_ref(),
        Some(&account.character_id),
        inventory_row_id,
    )
    .await;
    if let Err(e) = insert_result {
        tracing::warn!(error = %e, "account chest deposit failed");
        send_to(world, player, "Couldn't deposit that item right now.\r\n");
        return;
    }
    // Snapshots taken before this point still list the item; make sure none
    // of them lands after the row is gone.
    ordered.supersede_earlier_snapshots();
    drop(ordered);
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
    let Some(account) = crate::commands::require_linked_account(world, player, "the account chest")
    else {
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
    let chosen = if let Ok(slot) = target.parse::<i32>() {
        rows.iter().find(|r| r.slot == slot).cloned()
    } else {
        let needle = target.to_ascii_lowercase();
        let protos = world.resource::<ObjectPrototypes>();
        rows.iter()
            .find(|r| {
                protos
                    .by_key
                    .get(&(r.object_zone_id, r.object_id))
                    .is_some_and(|p| {
                        mud_world::targeting::entity_matches(&needle, &p.name, Some(&p.keywords))
                    })
            })
            .cloned()
    };
    let Some(row) = chosen else {
        send_to(
            world,
            player,
            format!("Nothing in your chest matches '{target}'.\r\n"),
        );
        return;
    };
    // Refuse before touching the DB if the prototype is gone: the row stays
    // in the chest instead of being consumed for nothing.
    if !world
        .resource::<ObjectPrototypes>()
        .by_key
        .contains_key(&(row.object_zone_id, row.object_id))
    {
        send_to(
            world,
            player,
            "Item's prototype is missing. Logging the row for staff review.\r\n",
        );
        tracing::warn!(
            zone = row.object_zone_id,
            id = row.object_id,
            "account chest withdraw: proto missing"
        );
        return;
    }
    // Same save-order turn as deposit: no background save may be mid-write
    // (or queued with an older item list that would delete the new row).
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let mut ordered = coordinator.begin_ordered(&account.character_id).await;
    coordinator.apply_completions(world);
    // Chest DELETE and inventory INSERT happen in ONE transaction, so a
    // crash can neither lose the item nor duplicate it; the new row's id is
    // stamped on the spawned entity so the next save updates it in place.
    let (withdrawn, inventory_row_id) =
        match mud_db::account_items::withdraw_to_inventory(pool, row.id, &account.character_id)
            .await
        {
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
    spawn_withdrawn_item(world, player, &withdrawn, inventory_row_id);
    ordered.supersede_earlier_snapshots();
}

/// Spawn a chest row back into the player's inventory. Mirrors
/// `respawn::spawn_item_into`'s bundle but takes per-instance state
/// from `custom_data`. Pulled out so tests can exercise the
/// rehydration without a live DB.
pub(crate) fn spawn_withdrawn_item(
    world: &mut World,
    player: Entity,
    withdrawn: &mud_db::account_items::AccountItemRow,
    inventory_row_id: i32,
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
            Named {
                name: proto.name.clone(),
            },
            Keywords(proto.keywords.clone()),
            WorldKey {
                zone: proto.zone_id,
                id: proto.id,
            },
            mud_world::PersistedItemId(inventory_row_id),
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
    crate::item_decay::attach_timer_if_decaying(world, item_entity, &proto);
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
    use mud_world::Account;

    #[tokio::test(flavor = "current_thread")]
    async fn unlinked_character_gets_link_hint_instead_of_account_chest() {
        let mut world = World::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let e = world
            .spawn((
                Account {
                    user_id: String::new(),
                    character_id: "char-l".to_string(),
                    role: UserRole::Player,
                    account_role: UserRole::Player,
                    perms: Vec::new(),
                },
                crate::commands::Connection(tx),
            ))
            .id();
        // Never connects: the link check must short-circuit before any DB use.
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        cmd_account_chest(&mut world, e, &pool).await;
        cmd_chest_deposit(&mut world, e, &pool, "sword").await;
        cmd_chest_withdraw(&mut world, e, &pool, "1").await;
        let mut out = String::new();
        while let Ok(b) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&b));
        }
        assert_eq!(out.matches("to use the account chest.").count(), 3, "{out}");
        assert!(out.contains("https://muditor.fierymud.org"), "{out}");
    }

    /// Live-DB scaffolding: a temp user + character, torn down by `end`.
    struct Fx {
        /// Serialises live-DB tests; released when the fixture drops.
        _db_lock: tokio::sync::MutexGuard<'static, ()>,
        pool: mud_db::sqlx::PgPool,
        user_id: String,
        char_id: String,
        oz: i32,
        oid: i32,
    }

    async fn fixture() -> Option<Fx> {
        let db_lock = crate::commands::test_support::db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let Ok(Ok(pool)) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            mud_db::connect_with(&url, crate::commands::test_support::db_test_pool_settings()),
        )
        .await
        else {
            eprintln!("skipping: dev database unavailable");
            return None;
        };
        let Ok(Some((oz, oid))) =
            mud_db::sqlx::query_as::<_, (i32, i32)>("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
        else {
            eprintln!("skipping: no Objects rows");
            return None;
        };
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let user_id = format!("zz-chest-u-{tag}");
        let char_id = format!("zz-chest-c-{tag}");
        mud_db::sqlx::query(
            "INSERT INTO \"Users\" (id, email, display_name, updated_at) \
             VALUES ($1, $2, $1, NOW())",
        )
        .bind(&user_id)
        .bind(format!("{user_id}@test.invalid"))
        .execute(&pool)
        .await
        .unwrap();
        mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())",
        )
        .bind(&char_id)
        .bind(format!("Zzch{}", tag % 1_000_000_000_000))
        .execute(&pool)
        .await
        .unwrap();
        Some(Fx {
            _db_lock: db_lock,
            pool,
            user_id,
            char_id,
            oz,
            oid,
        })
    }

    impl Fx {
        /// A linked player plus a world carrying the save coordinator and
        /// a prototype for the fixture's object.
        fn world(&self) -> (World, Entity, crate::commands::test_support::Rx) {
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            let mut protos = ObjectPrototypes::default();
            protos.by_key.insert(
                (self.oz, self.oid),
                crate::commands::test_support::object_proto(
                    self.oz,
                    self.oid,
                    mud_db::enums::ObjectType::Other,
                ),
            );
            world.insert_resource(protos);
            let (tx, rx) = tokio::sync::mpsc::channel(64);
            let room = world.spawn_empty().id();
            let player = world
                .spawn((
                    Account {
                        user_id: self.user_id.clone(),
                        character_id: self.char_id.clone(),
                        role: UserRole::Player,
                        account_role: UserRole::Player,
                        perms: Vec::new(),
                    },
                    mud_world::Player,
                    mud_world::Health { hp: 10, max: 10 },
                    crate::commands::Connection(tx),
                    Located(room),
                ))
                .id();
            (world, player, rx)
        }

        fn item(&self, world: &mut World, holder: Entity, name: &str, kw: &str) -> Entity {
            world
                .spawn((
                    Item,
                    Named {
                        name: name.to_string(),
                    },
                    Keywords(vec![kw.to_string()]),
                    WorldKey {
                        zone: self.oz,
                        id: self.oid,
                    },
                    Located(holder),
                ))
                .id()
        }

        async fn inventory_ids(&self) -> Vec<i32> {
            mud_db::sqlx::query_scalar(
                "SELECT id FROM \"CharacterItems\" WHERE character_id = $1 ORDER BY id",
            )
            .bind(&self.char_id)
            .fetch_all(&self.pool)
            .await
            .unwrap()
        }

        async fn chest_count(&self) -> i64 {
            mud_db::sqlx::query_scalar("SELECT COUNT(*) FROM account_items WHERE user_id = $1")
                .bind(&self.user_id)
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }

        async fn stock_chest(&self, custom: Option<serde_json::Value>) {
            mud_db::account_items::deposit(
                &self.pool,
                &self.user_id,
                self.oz,
                self.oid,
                1,
                custom.as_ref(),
                Some(&self.char_id),
                None,
            )
            .await
            .unwrap();
        }

        async fn end(self) {
            for sql in [
                "DELETE FROM account_items WHERE user_id = $1",
                "DELETE FROM \"Users\" WHERE id = $1",
            ] {
                mud_db::sqlx::query(sql)
                    .bind(&self.user_id)
                    .execute(&self.pool)
                    .await
                    .unwrap();
            }
            for sql in [
                "DELETE FROM \"CharacterItems\" WHERE character_id = $1",
                "DELETE FROM \"Characters\" WHERE id = $1",
            ] {
                mud_db::sqlx::query(sql)
                    .bind(&self.char_id)
                    .execute(&self.pool)
                    .await
                    .unwrap();
            }
        }
    }

    fn out(rx: &mut crate::commands::test_support::Rx) -> String {
        crate::commands::test_support::drain(rx)
    }

    /// Depositing removes the item's `CharacterItems` row in the same
    /// transaction as the chest INSERT, so a crash before the next save
    /// can't leave the item in both places.
    #[tokio::test(flavor = "current_thread")]
    async fn deposit_removes_the_inventory_row() {
        let Some(fx) = fixture().await else { return };
        let (mut world, player, _rx) = fx.world();
        let row_id: i32 = mud_db::sqlx::query_scalar(
            "INSERT INTO \"CharacterItems\" (character_id, object_zone_id, object_id, updated_at) \
             VALUES ($1, $2, $3, NOW()) RETURNING id",
        )
        .bind(&fx.char_id)
        .bind(fx.oz)
        .bind(fx.oid)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        let item = fx.item(&mut world, player, "a test blade", "blade");
        world
            .entity_mut(item)
            .insert(mud_world::PersistedItemId(row_id));
        cmd_chest_deposit(&mut world, player, &fx.pool, "blade").await;
        let (chest, inv) = (fx.chest_count().await, fx.inventory_ids().await);
        fx.end().await;
        assert_eq!(chest, 1, "item is in the chest");
        assert!(inv.is_empty(), "and no longer has an inventory row");
    }

    /// The deposit's delete is not scoped to the depositor: an item handed
    /// over since its last save still sits in the previous holder's row.
    #[tokio::test(flavor = "current_thread")]
    async fn deposit_deletes_the_row_even_if_another_character_owns_it() {
        let Some(fx) = fixture().await else { return };
        let other = format!("{}-other", fx.char_id);
        mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())",
        )
        .bind(&other)
        .bind(format!("Zzo{}", &fx.char_id[fx.char_id.len() - 9..]))
        .execute(&fx.pool)
        .await
        .unwrap();
        let row_id: i32 = mud_db::sqlx::query_scalar(
            "INSERT INTO \"CharacterItems\" (character_id, object_zone_id, object_id, updated_at) \
             VALUES ($1, $2, $3, NOW()) RETURNING id",
        )
        .bind(&other)
        .bind(fx.oz)
        .bind(fx.oid)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        mud_db::account_items::deposit(
            &fx.pool,
            &fx.user_id,
            fx.oz,
            fx.oid,
            1,
            None,
            Some(&fx.char_id),
            Some(row_id),
        )
        .await
        .unwrap();
        let left: i64 =
            mud_db::sqlx::query_scalar("SELECT COUNT(*) FROM \"CharacterItems\" WHERE id = $1")
                .bind(row_id)
                .fetch_one(&fx.pool)
                .await
                .unwrap();
        mud_db::sqlx::query("DELETE FROM \"Characters\" WHERE id = $1")
            .bind(&other)
            .execute(&fx.pool)
            .await
            .unwrap();
        fx.end().await;
        assert_eq!(left, 0);
    }

    /// A container with contents is refused: depositing it would despawn
    /// the contents with it. Empty, it deposits fine.
    #[tokio::test(flavor = "current_thread")]
    async fn non_empty_container_is_refused_and_nothing_is_lost() {
        let Some(fx) = fixture().await else { return };
        let (mut world, player, mut rx) = fx.world();
        let bag = fx.item(&mut world, player, "a leather bag", "bag");
        let gem = fx.item(&mut world, bag, "a gem", "gem");
        cmd_chest_deposit(&mut world, player, &fx.pool, "bag").await;
        let text = out(&mut rx);
        assert!(text.contains("Empty it first."), "{text}");
        assert_eq!(fx.chest_count().await, 0);
        assert!(world.get_entity(bag).is_ok(), "bag untouched");
        assert_eq!(world.get::<Located>(gem).unwrap().0, bag, "contents kept");
        // Emptied, the bag deposits.
        world.entity_mut(gem).insert(Located(player));
        cmd_chest_deposit(&mut world, player, &fx.pool, "bag").await;
        let chest = fx.chest_count().await;
        fx.end().await;
        assert_eq!(chest, 1);
    }

    /// Withdraw is one transaction: after it (and before any save, i.e.
    /// a crash right now) the item already exists in `CharacterItems`,
    /// the entity carries that row id, and a later save updates in place.
    #[tokio::test(flavor = "current_thread")]
    async fn withdraw_is_atomic_and_survives_a_crash_before_the_next_save() {
        let Some(fx) = fixture().await else { return };
        let (mut world, player, mut rx) = fx.world();
        fx.stock_chest(Some(serde_json::json!({"charges": 4})))
            .await;
        cmd_chest_withdraw(&mut world, player, &fx.pool, "0").await;
        let text = out(&mut rx);
        assert!(text.contains("You retrieve"), "{text}");
        assert_eq!(fx.chest_count().await, 0);
        let rows = fx.inventory_ids().await;
        assert_eq!(rows.len(), 1, "row exists without any save: {rows:?}");
        let charges: i32 =
            mud_db::sqlx::query_scalar("SELECT charges FROM \"CharacterItems\" WHERE id = $1")
                .bind(rows[0])
                .fetch_one(&fx.pool)
                .await
                .unwrap();
        assert_eq!(charges, 4, "per-instance state carried over");
        let spawned = world
            .query_filtered::<&mud_world::PersistedItemId, With<Item>>()
            .single(&world)
            .unwrap()
            .0;
        assert_eq!(spawned, rows[0]);
        // The next save updates that row instead of inserting a second.
        let saved = crate::login::save_player(&mut world, player, &fx.pool).await;
        assert!(saved.committed, "{:?}", saved.error);
        let after = fx.inventory_ids().await;
        fx.end().await;
        assert_eq!(after, rows);
    }

    /// A chest row whose prototype is gone stays in the chest.
    #[tokio::test(flavor = "current_thread")]
    async fn withdraw_with_missing_proto_leaves_the_row_in_the_chest() {
        let Some(fx) = fixture().await else { return };
        let (mut world, player, mut rx) = fx.world();
        world.insert_resource(ObjectPrototypes::default());
        fx.stock_chest(None).await;
        cmd_chest_withdraw(&mut world, player, &fx.pool, "0").await;
        let text = out(&mut rx);
        let (chest, inv) = (fx.chest_count().await, fx.inventory_ids().await);
        fx.end().await;
        assert!(text.contains("prototype is missing"), "{text}");
        assert_eq!(chest, 1);
        assert!(inv.is_empty());
    }

    /// A background save that snapshotted before a deposit must not
    /// re-insert the deposited row (nor, for a withdraw, delete the new
    /// one): either order ends consistent.
    #[tokio::test(flavor = "current_thread")]
    async fn chest_moves_are_ordered_against_in_flight_background_saves() {
        let Some(fx) = fixture().await else { return };
        let (mut world, player, _rx) = fx.world();
        let coordinator = world.resource::<SaveCoordinator>().clone();

        // Deposit while a background save carrying the item is in flight.
        let item = fx.item(&mut world, player, "a test blade", "blade");
        assert!(crate::login::spawn_background_save(
            &mut world, player, &fx.pool
        ));
        cmd_chest_deposit(&mut world, player, &fx.pool, "blade").await;
        assert!(
            coordinator
                .flush(&mut world, std::time::Duration::from_secs(10))
                .await
        );
        assert!(world.get_entity(item).is_err());
        assert_eq!(fx.chest_count().await, 1);
        assert!(
            fx.inventory_ids().await.is_empty(),
            "the background save must not resurrect the deposited row"
        );

        // Withdraw while a background save (taken without the item) is
        // queued: it must not delete the freshly inserted row.
        assert!(crate::login::spawn_background_save(
            &mut world, player, &fx.pool
        ));
        cmd_chest_withdraw(&mut world, player, &fx.pool, "0").await;
        assert!(
            coordinator
                .flush(&mut world, std::time::Duration::from_secs(10))
                .await
        );
        let inv = fx.inventory_ids().await;
        let chest = fx.chest_count().await;
        fx.end().await;
        assert_eq!(chest, 0);
        assert_eq!(inv.len(), 1, "withdrawn item survives the queued save");
    }

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
