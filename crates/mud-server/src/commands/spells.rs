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
    flip_door_both_sides, invoke_ability, name_of, parse_direction, resolve_spell_for_class,
    send_to,
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
            usage: "memorize <spell>",
            summary: "Prepare a spell into one of your circle slots.",
            long: "Looks up the spell by name in your class's circle \
                   list (via `ClassAbilities`), checks slot availability \
                   for that circle (via `SpellSlotProgression`), and \
                   appends the spell to your `MemorizedSpells` list. \
                   Refuses unknown spells, off-class spells, or full \
                   circles. Session-only — re-memorize on reconnect.",
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
            usage: "forget <spell>",
            summary: "Drop a memorized spell from your prepared list.",
            long: "Removes the first matching memorized spell, freeing \
                   that circle slot for a new memorize. No-op if the \
                   spell isn't currently memorized.",
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

inventory::submit! {
    Command {
        names: &["skill", "use"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "skill <name> [target]",
            summary: "Invoke a SKILL-type ability from the catalog.",
            long: "Sibling to cast/chant/perform: looks up a SKILL \
                   row by name and runs it through the same effect \
                   application pipeline. New combat skills should be \
                   added as Muditor `Ability` rows (kind=SKILL) with \
                   `AbilityEffect` mappings — no Rust change needed. \
                   Hardcoded skills (bandage, gouge, etc.) coexist \
                   for now; they'll migrate as Phase B effect-type \
                   consumers land.",
        },
        run: cmd_skill,
    }
}

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
    send_to(
        world,
        player,
        "You aren't casting anything. (Use `cancel <effect>` to drop an active buff.)\r\n",
    );
}
pub(crate) fn cmd_cancel(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim().to_ascii_lowercase();
    let cancellable: Vec<(Entity, String, i32)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, a)| a.0 == player && inst.remaining_secs >= 0)
            .map(|(e, inst, _)| (e, inst.name.clone(), inst.remaining_secs))
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
        for (_, name, remaining) in &cancellable {
            out.push_str(&format!("  {name} ({remaining}s)\r\n"));
        }
        out.push_str("\r\nUse `cancel <name>` to drop one.\r\n");
        send_to(world, player, out);
        return;
    }
    let target = cancellable
        .iter()
        .find(|(_, name, _)| name.to_ascii_lowercase().contains(&needle))
        .map(|(e, _, _)| *e);
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
pub(crate) fn cmd_memorize(world: &mut World, player: Entity, args: &str) {
    use mud_world::{MemorizedSpells, SpellSlotData};
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let (ability_id, circle) = match resolve_spell_for_class(world, class_id, args) {
        Ok(t) => t,
        Err(e) => {
            send_to(world, player, format!("{e}\r\n"));
            return;
        }
    };
    let max = world
        .resource::<SpellSlotData>()
        .progression
        .get(&(profile.level, circle))
        .copied()
        .unwrap_or(0);
    if max <= 0 {
        send_to(
            world,
            player,
            format!("You can't memorize circle {circle} spells yet.\r\n"),
        );
        return;
    }
    let used = world
        .get::<MemorizedSpells>(player)
        .map_or(0, |m| m.used_in_circle(circle));
    if used >= max {
        send_to(
            world,
            player,
            format!("All circle {circle} slots ({used}/{max}) are already prepared.\r\n"),
        );
        return;
    }
    let display_name = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .find(|d| d.id == ability_id)
        .map_or_else(String::new, |d| d.name.clone());
    let prep_secs = (circle * 5).max(5); // default 5s/circle until Ability.memorization_time is seeded
    let entry = mud_world::MemEntry {
        ability_id,
        circle,
        ready: false,
        prep_secs_remaining: prep_secs,
    };
    if let Some(mut mem) = world.get_mut::<MemorizedSpells>(player) {
        mem.entries.push(entry);
    } else {
        world
            .entity_mut(player)
            .insert(MemorizedSpells { entries: vec![entry] });
    }
    send_to(
        world,
        player,
        format!(
            "You begin memorizing {display_name} (circle {circle}, ~{prep_secs}s while resting).\r\n"
        ),
    );
}
pub(crate) fn cmd_forget(world: &mut World, player: Entity, args: &str) {
    use mud_world::MemorizedSpells;
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    let (ability_id, _) = match resolve_spell_for_class(world, class_id, args) {
        Ok(t) => t,
        Err(e) => {
            send_to(world, player, format!("{e}\r\n"));
            return;
        }
    };
    let display_name = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .find(|d| d.id == ability_id)
        .map_or_else(String::new, |d| d.name.clone());
    let removed = if let Some(mut mem) = world.get_mut::<MemorizedSpells>(player) {
        // Prefer dropping a not-yet-ready entry (cheaper to lose).
        let idx = mem
            .entries
            .iter()
            .position(|e| e.ability_id == ability_id && !e.ready)
            .or_else(|| mem.entries.iter().position(|e| e.ability_id == ability_id));
        if let Some(idx) = idx {
            mem.entries.remove(idx);
            true
        } else {
            false
        }
    } else {
        false
    };
    if removed {
        send_to(world, player, format!("You forget {display_name}.\r\n"));
    } else {
        send_to(
            world,
            player,
            format!("{display_name} isn't currently memorized.\r\n"),
        );
    }
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
pub(crate) fn cmd_skill(world: &mut World, player: Entity, args: &str) {
    invoke_ability(world, player, args, mud_db::abilities::AbilityKind::Skill, "use");
}
