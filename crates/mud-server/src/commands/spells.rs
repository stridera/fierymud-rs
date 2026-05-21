//! Spell / skill / ability commands and the cancel-cast control
//! verbs (abort/cancel). All Combat-category in the static array;
//! moved here as a separate file from commands/combat.rs since
//! they cluster around the casting pipeline rather than the
//! attack/defend verbs.

use bevy_ecs::prelude::*;
use mud_db::enums::{ExitState, UserRole};
use mud_world::{
    AbilityCatalog, AppliedTo, EffectInstance, Exits, KnownAbilities, Located, Profile, Stamina,
};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, direction_name,
    flip_door_both_sides, invoke_ability, name_of, parse_direction, send_to,
};

inventory::submit! {
    Command {
        names: &["pick"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "pick <direction>",
            summary: "Pick a locked door open with rogue tools.",
            long: "Skill check against your `PICK_LOCK` proficiency. \
                   Refuses on unlocked exits, exits without a keyhole, \
                   and players who haven't trained pick lock. Costs \
                   5 stamina whether you succeed or fail. On success \
                   the door flips Locked → Closed (same as `unlock`); \
                   on failure you get a fumble line.",
        },
        run: cmd_pick,
    }
}

inventory::submit! {
    Command {
        names: &["study"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "study <spell>",
            summary: "Permanently learn a spell from your class list.",
            long: "Adds the spell to your `KnownAbilities` at the \
                   minimum proficiency tier (`known=true`, \
                   proficiency=1). Refuses unknown abilities, \
                   already-known spells, or off-class spells. \
                   Persists across reconnect via `CharacterAbilities`.",
        },
        run: cmd_study,
    }
}

inventory::submit! {
    Command {
        names: &["memorize", "mem", "pray"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "memorize",
            summary: "(legacy alias) Show your spell-slot pool.",
            long: "FieryMUD uses a slot-pool model, not a Vance-style \
                   prepared-spell list. There's nothing to memorize ahead \
                   of time: at each level you have a fixed number of slots \
                   per circle (see `slots` for your current capacity), and \
                   any spell you've trained (`study <spell>`) can be cast \
                   while a slot of its circle is free. This command \
                   redirects to `slots`.",
        },
        run: cmd_memorize,
    }
}

inventory::submit! {
    Command {
        names: &["forget"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "forget",
            summary: "(legacy alias) Slots aren't pre-prepared — see `slots`.",
            long: "There's no prepared-spell list to forget from. Spell \
                   slots are a circle-pool you draw from when casting; a \
                   slot you've spent regenerates on its own under \
                   resting / sleeping / meditating postures. Redirects to \
                   `slots`.",
        },
        run: cmd_forget,
    }
}

inventory::submit! {
    Command {
        names: &["cast", "c"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "cast <spell> [target]",
            summary: "Cast a spell from the loaded catalog.",
            long: "Looks up a SPELL by case-insensitive name (partial \
                   match accepted). For now this is a stub: prints the \
                   ability's metadata so you can see what's in the \
                   catalog. Real effect application — slot consumption, \
                   restriction checks, damage/heal/buff resolution — \
                   lands when CharacterAbilities and the effect \
                   pipeline are wired. Only matches abilityType = \
                   SPELL; for chants and songs use `chant` / `perform`.",
        },
        run: cmd_cast,
    }
}

inventory::submit! {
    Command {
        names: &["chant"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "chant <chant> [target]",
            summary: "Invoke a chant from the catalog (cleric-side spells).",
            long: "Same shape as `cast` but filters to abilityType = \
                   CHANT. Stub: prints metadata and gates on \
                   KnownAbilities, no effect application yet.",
        },
        run: cmd_chant,
    }
}

inventory::submit! {
    Command {
        names: &["perform"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "perform <song> [target]",
            summary: "Perform a song from the catalog (bard).",
            long: "Same shape as `cast` but filters to abilityType = \
                   SONG. Stub: prints metadata and gates on \
                   KnownAbilities, no effect application yet.",
        },
        run: cmd_perform,
    }
}

// `skill` / `use` as a generic ability-dispatch command was removed
// 2026-05-17 — it was a backdoor that let players invoke passive
// defensives (DODGE, PARRY, RIPOSTE), weapon proficiencies
// (BLUDGEONING, PIERCING, …), and sphere masteries
// (SPHERE_FIRE, …) as if they were active commands. Every active
// skill already has a dedicated command in `commands/combat.rs`
// (`bash`, `kick`, `backstab`, `disarm`, `rescue`, `bandage`, …)
// with the right targeting + flavor; passive/proficiency skills
// fire automatically from the combat pipeline and aren't meant to
// be triggered manually. The `skills` listing command (in
// `commands/info.rs`) is untouched — that's the right shape for
// "what do I know?".

inventory::submit! {
    Command {
        names: &["abort"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "abort",
            summary: "Cancel an in-progress cast or queued spell.",
            long: "FieryMUD legacy: aborts the spell you're currently \
                   casting and clears any spell queued behind it. \
                   Today's runtime resolves casts immediately and \
                   has no queue, so abort has nothing to do — kept \
                   as a registered command name for muscle memory \
                   and to provide a clear message instead of \
                   'Unknown command'. Use `cancel` to drop a \
                   non-permanent buff already on you.",
        },
        run: cmd_abort,
    }
}

inventory::submit! {
    Command {
        names: &["cancel"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "cancel [<effect>]",
            summary: "Drop a non-permanent buff from yourself.",
            long: "With no arg, lists effects you can cancel \
                   (anything not flagged permanent). With an effect \
                   name, finds the matching `EffectInstance` on you \
                   and despawns it. Permanent effects (e.g. innate \
                   resistances) refuse to cancel.",
        },
        run: cmd_cancel,
    }
}



// ---- handler bodies ----

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_pick(world: &mut World, player: Entity, args: &str) {
    const STAMINA_COST: i32 = 5;
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Pick which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let Some((state, key_req, is_pickproof)) = world
        .get::<Exits>(room)
        .and_then(|e| {
            e.0.get(&dir)
                .map(|ed| (ed.state, ed.key, ed.is_pickproof))
        })
    else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    if state != ExitState::Locked {
        send_to(
            world,
            player,
            format!("It's not locked {}.\r\n", direction_name(dir)),
        );
        return;
    }
    // Pickproof doors refuse regardless of proficiency — keyed-only
    // or magically sealed. Mirrors the schema's `ExitFlag::PICKPROOF`.
    if is_pickproof {
        send_to(
            world,
            player,
            "The lock resists your tools — there's no tumbler to feel for.\r\n",
        );
        return;
    }
    if key_req.is_none() {
        send_to(
            world,
            player,
            format!("There's no keyhole {}.\r\n", direction_name(dir)),
        );
        return;
    }

    // PICK_LOCK ability id 272.
    let proficiency = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == 272).copied())
        .map(|(_, p, _)| p);
    let Some(proficiency) = proficiency else {
        send_to(
            world,
            player,
            "You don't know how to pick locks.\r\n",
        );
        return;
    };

    let stamina_ok = world
        .get::<Stamina>(player)
        .is_some_and(|s| s.current >= STAMINA_COST);
    if !stamina_ok {
        send_to(
            world,
            player,
            "You don't have the stamina for a steady hand.\r\n",
        );
        return;
    }
    if let Some(mut s) = world.get_mut::<Stamina>(player) {
        s.current = (s.current - STAMINA_COST).max(0);
    }

    // d100 roll vs proficiency. Proficiency is 0–1000 in the schema;
    // divide by 10 to get a 0–100 chance.
    let roll = rand::random_range(1..=100);
    let chance = (proficiency / 10).clamp(0, 100);
    let player_name = name_of(world, player);
    if roll <= chance {
        flip_door_both_sides(world, room, dir, ExitState::Closed);
        send_to(
            world,
            player,
            format!(
                "*click* The lock {} yields to your tools.\r\n",
                direction_name(dir)
            ),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} picks the lock {}.\r\n", direction_name(dir)),
        );
    } else {
        send_to(
            world,
            player,
            format!(
                "Your tools slip — the lock {} stays shut.\r\n",
                direction_name(dir)
            ),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} fumbles with the lock {}.\r\n", direction_name(dir)),
        );
    }
}
pub(crate) fn cmd_abort(world: &mut World, player: Entity, _args: &str) {
    if crate::casting::cancel_own_cast(world, player) {
        return;
    }
    send_to(
        world,
        player,
        "You aren't casting anything. (Use `cancel <effect>` to drop an active buff.)\r\n",
    );
}
pub(crate) fn cmd_cancel(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim().to_ascii_lowercase();
    // Snapshot every effect on the player + its source-ability
    // plain_name (when the originating ability is known). The
    // ability name is what the user sees on `effects` ("from
    // Enhance Ability"), so matching against it lets `cancel
    // enhance` work alongside `cancel cha`.
    let ability_names: std::collections::HashMap<i32, String> = world
        .resource::<crate::commands::AbilityCatalog>()
        .by_name
        .values()
        .map(|d| (d.id, d.plain_name.replace('_', " ").to_ascii_lowercase()))
        .collect();
    // Also pull effects in the player's current room when the name
    // starts with "wall-" — that's the caster (or any room
    // occupant) cleaning up a barrier they no longer want. Other
    // room-applied effects (magical darkness, room burning) are
    // intentionally excluded: those are environmental hazards
    // whose caster expected them to last, and a random passerby
    // cancelling them would surprise the original caster. Walls
    // are scoped narrowly enough that "anyone in the room can
    // drop one" matches the immediate, blocking-the-only-path UX.
    let player_room = world.get::<mud_world::Located>(player).map(|l| l.0);
    let cancellable: Vec<(Entity, String, Option<String>, i32)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, a)| {
                if inst.remaining_secs < 0 {
                    return false;
                }
                if a.0 == player {
                    return true;
                }
                player_room.is_some_and(|r| a.0 == r) && inst.name.starts_with("wall-")
            })
            .map(|(e, inst, _)| {
                let src = inst
                    .ability_id
                    .and_then(|id| ability_names.get(&id).cloned());
                (e, inst.name.clone(), src, inst.remaining_secs)
            })
            .collect()
    };
    if cancellable.is_empty() {
        send_to(
            world,
            player,
            "You have no effects you can cancel.\r\n",
        );
        return;
    }
    if needle.is_empty() {
        let mut out = format!("\r\n{} cancellable effect(s):\r\n", cancellable.len());
        for (_, name, src, remaining) in &cancellable {
            if let Some(src) = src {
                out.push_str(&format!("  {name} ({remaining}s) — from {src}\r\n"));
            } else {
                out.push_str(&format!("  {name} ({remaining}s)\r\n"));
            }
        }
        out.push_str("\r\nUse `cancel <name>` to drop one.\r\n");
        send_to(world, player, out);
        return;
    }
    let target = cancellable
        .iter()
        .find(|(_, name, src, _)| {
            name.to_ascii_lowercase().contains(&needle)
                || src.as_deref().is_some_and(|s| s.contains(&needle))
        })
        .map(|(e, _, _, _)| *e);
    let Some(target_effect) = target else {
        send_to(
            world,
            player,
            format!("No cancellable effect matching '{needle}' on you.\r\n"),
        );
        return;
    };
    let removed_name = world
        .get::<EffectInstance>(target_effect)
        .map_or_else(|| "?".to_string(), |i| i.name.clone());
    // If this is a wall, the room carries a RoomBlockedExits map
    // keyed on Direction that effect-expiry normally cleans up via
    // `effects_tick`. Despawning the EffectInstance directly skips
    // that branch and would leave a phantom wall entry in the map
    // — exits, look, and movement would all keep refusing the
    // blocked direction even though the backing effect was gone.
    // Walk the map up front, drop the matching entry inline, and
    // broadcast the dissolve to other players in the room so they
    // get the same "wall is gone" signal `effects_tick` would
    // emit on a natural expiry.
    if removed_name.starts_with("wall-") {
        let target_room = world
            .get::<AppliedTo>(target_effect)
            .map(|a| a.0);
        let mut dissolved: Option<(mud_db::enums::Direction, String, bevy_ecs::entity::Entity)> = None;
        if let Some(room) = target_room {
            // Capture (dir, kind_label) for the broadcast before
            // the retain mutates the map.
            let captured = world
                .get::<mud_world::RoomBlockedExits>(room)
                .and_then(|b| {
                    b.by_direction
                        .iter()
                        .find(|(_, e)| e.backed_by == target_effect)
                        .map(|(d, e)| (*d, e.kind_label.clone()))
                });
            if let Some(mut blocked) = world.get_mut::<mud_world::RoomBlockedExits>(room) {
                blocked
                    .by_direction
                    .retain(|_, entry| entry.backed_by != target_effect);
                let empty = blocked.by_direction.is_empty();
                drop(blocked);
                if empty
                    && let Ok(mut em) = world.get_entity_mut(room)
                {
                    em.remove::<mud_world::RoomBlockedExits>();
                }
            }
            if let Some((dir, kind_label)) = captured {
                dissolved = Some((dir, kind_label, room));
            }
        }
        if let Some((dir, kind_label, room)) = dissolved {
            let player_name = crate::commands::name_of(world, player);
            let dir_name = crate::commands::direction_name(dir);
            let players: Vec<bevy_ecs::entity::Entity> = {
                let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                q.iter(world)
                    .filter(|(e, l)| *e != player && l.0 == room)
                    .map(|(e, _)| e)
                    .collect()
            };
            let msg = format!(
                "{player_name} gestures and the {kind_label} {dir_name} crumbles into nothing.\r\n"
            );
            for p in players {
                crate::commands::send_to(world, p, msg.clone());
            }
        }
    }
    if let Ok(e) = world.get_entity_mut(target_effect) {
        e.despawn();
    }
    send_to(
        world,
        player,
        format!("You cancel {removed_name}.\r\n"),
    );
}
pub(crate) fn cmd_study(world: &mut World, player: Entity, args: &str) {
    use mud_world::SpellSlotData;
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let key = args.trim().to_ascii_lowercase();
    if key.is_empty() {
        send_to(world, player, "Study what?\r\n");
        return;
    }
    let Some(def) = world.resource::<AbilityCatalog>().by_name.get(&key).cloned() else {
        send_to(world, player, format!("'{key}' isn't a known ability.\r\n"));
        return;
    };
    if !world
        .resource::<SpellSlotData>()
        .ability_circle
        .contains_key(&(class_id, def.id))
    {
        send_to(
            world,
            player,
            format!("{} isn't on your class's list.\r\n", def.name),
        );
        return;
    }
    if let Some(known) = world.get::<KnownAbilities>(player)
        && known.has_any(def.id)
    {
        send_to(
            world,
            player,
            format!("You already know {}.\r\n", def.name),
        );
        return;
    }
    if let Some(mut known) = world.get_mut::<KnownAbilities>(player) {
        known.entries.push((def.id, 1, true));
        known.entries.sort_by_key(|(id, _, _)| *id);
    } else {
        world.entity_mut(player).insert(KnownAbilities {
            entries: vec![(def.id, 1, true)],
        });
    }
    send_to(
        world,
        player,
        format!(
            "You commit {} to memory. (proficiency 1)\r\n",
            def.name
        ),
    );
}
pub(crate) fn cmd_memorize(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "FieryMUD uses pooled spell slots — there's nothing to memorize \
         ahead of time. Spent slots recover on their own under rest / \
         sleep / meditate. Showing your slot pool:\r\n",
    );
    crate::commands::info::cmd_slots(world, player, "");
}
pub(crate) fn cmd_forget(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "Spell slots aren't pre-prepared, so there's nothing to forget. \
         Showing your slot pool:\r\n",
    );
    crate::commands::info::cmd_slots(world, player, "");
}
pub(crate) fn cmd_cast(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Spell, "cast");
}
pub(crate) fn cmd_chant(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Chant, "chant");
}
pub(crate) fn cmd_perform(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Song, "perform");
}
