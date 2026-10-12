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

use crate::commands::info::{door_phrase, door_sentence};
use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, direction_name,
    flip_door_both_sides, invoke_ability, name_of, refuse_hidden_exit, resolve_exit_arg, send_msg,
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
            long: "Skill check against your 'PICK_LOCK' proficiency. \
                   Refuses on unlocked exits, exits without a keyhole, \
                   and players who haven't trained pick lock. Costs \
                   5 stamina whether you succeed or fail. On success \
                   the door flips Locked → Closed (same as 'unlock'); \
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
            long: "Adds the spell to your 'KnownAbilities' at the \
                   minimum proficiency tier ('known=true', \
                   proficiency=1). Refuses unknown abilities, \
                   already-known spells, or off-class spells. \
                   Persists across reconnect via 'CharacterAbilities'.",
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
                   per circle (see 'slots' for your current capacity), and \
                   any spell you've trained ('study <spell>') can be cast \
                   while a slot of its circle is free. This command \
                   redirects to 'slots'.",
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
            summary: "(legacy alias) Slots aren't pre-prepared — see 'slots'.",
            long: "There's no prepared-spell list to forget from. Spell \
                   slots are a circle-pool you draw from when casting; a \
                   slot you've spent regenerates on its own under \
                   resting / sleeping / meditating postures. Redirects to \
                   'slots'.",
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
                   SPELL; for chants and songs use 'chant' / 'perform'.",
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
            long: "Same shape as 'cast' but filters to abilityType = \
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
            long: "Same shape as 'cast' but filters to abilityType = \
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
                   'Unknown command'. Use 'cancel' to drop a \
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
            summary: "Drop one of your own timed beneficial spells.",
            long: "With no arg, lists the effects you can cancel. With an \
                   effect name (a prefix works; quote multi-word names), \
                   ends that beneficial spell on you right away, exactly \
                   as if it had worn off. Permanent effects (racial or \
                   from gear) and harmful ones cannot be cancelled, and \
                   you cannot cancel while fighting.",
        },
        run: cmd_cancel,
    }
}

// ---- handler bodies ----

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_pick(world: &mut World, player: Entity, args: &str) {
    const STAMINA_COST: i32 = 5;
    let arg = args.trim();
    if crate::commands::containers::try_container_verb(
        world,
        player,
        crate::commands::containers::Verb::Pick,
        arg,
    ) {
        return;
    }
    let Some(dir) = resolve_exit_arg(world, player, arg) else {
        send_to(world, player, "Pick which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    if refuse_hidden_exit(world, player, room, dir) {
        return;
    }
    let Some((state, key_req, is_pickproof)) = world
        .get::<Exits>(room)
        .and_then(|e| e.0.get(&dir).map(|ed| (ed.state, ed.key, ed.is_pickproof)))
    else {
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
        return;
    };
    if state != ExitState::Locked {
        let line = door_sentence(world, room, dir, "not locked");
        send_to(world, player, format!("{line}.\r\n"));
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
        let door = door_phrase(world, room, dir);
        send_to(world, player, format!("There's no keyhole on {door}.\r\n"));
        return;
    }

    // Pick Lock's id is resolved by name into `CoreAbilities`; a catalog
    // without it leaves the skill unusable.
    let proficiency = world
        .get_resource::<mud_world::CoreAbilities>()
        .and_then(|c| c.pick_lock)
        .and_then(|pick| mud_world::proficiency_of(world, player, pick));
    let Some(proficiency) = proficiency else {
        send_to(world, player, "You don't know how to pick locks.\r\n");
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
    let door = door_phrase(world, room, dir);
    if roll <= chance {
        flip_door_both_sides(world, room, dir, ExitState::Closed);
        send_to(
            world,
            player,
            format!("*click* You pick the lock on {door}.\r\n"),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} picks the lock on {door}.\r\n"),
        );
    } else {
        send_to(
            world,
            player,
            format!("Your tools slip -- the lock on {door} stays shut.\r\n"),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} fumbles with the lock on {door}.\r\n"),
        );
    }
}
pub(crate) fn cmd_abort(world: &mut World, player: Entity, _args: &str) {
    if world.get::<mud_world::Casting>(player).is_some() {
        send_to(world, player, "You abort your spell!\r\n");
        crate::casting::cancel_own_cast(world, player);
        return;
    }
    send_to(
        world,
        player,
        "You're not even casting a spell right now. (Use 'cancel <effect>' to drop an active buff.)\r\n",
    );
}
/// Why an effect on the player can or cannot be dropped with `cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelStatus {
    /// A timed, beneficial spell effect (or a wall the player may drop).
    Ok,
    /// Race / worn-item innate, or an effect with no end.
    Permanent,
    /// A debuff: hostile or violent source ability, or a `debuff`-tagged
    /// effect definition.
    Harmful,
    /// No source ability to vouch that it is beneficial (bleed, fear's
    /// stun, rest's Refreshed, admin effects).
    NotASpell,
}

/// One effect `cancel` considers: its listing label and the names it
/// answers to.
struct CancelRow {
    entity: Entity,
    name: String,
    label: String,
    /// Source ability's plain name, so `cancel enhance` works next to the
    /// stat label.
    src: Option<String>,
    ability_id: Option<i32>,
    remaining: i32,
    status: CancelStatus,
}

/// Classify one effect for `cancel`. Beneficial is read from existing
/// data: an effect is harmful when its source ability is hostile or
/// violent (the same split the mob AI buff / debuff tally uses) or its
/// `Effect` row carries the `debuff` tag. There is no explicit
/// beneficial flag in the schema. A god may drop anything but a race or
/// item innate, whose lifetime belongs to its source.
fn cancel_status(
    world: &World,
    inst: &EffectInstance,
    harmful_abilities: &std::collections::HashMap<i32, bool>,
    god: bool,
) -> CancelStatus {
    if mud_world::mob_effects::is_innate_effect(&inst.source) {
        return CancelStatus::Permanent;
    }
    if god {
        return CancelStatus::Ok;
    }
    if inst.remaining_secs < 0 {
        return CancelStatus::Permanent;
    }
    let Some(&ability_harmful) = inst.ability_id.and_then(|id| harmful_abilities.get(&id)) else {
        return CancelStatus::NotASpell;
    };
    let debuff_tagged = world
        .get_resource::<mud_world::EffectCatalog>()
        .and_then(|c| c.by_id.get(&inst.kind))
        .is_some_and(|def| def.tags.iter().any(|t| t.eq_ignore_ascii_case("debuff")));
    if ability_harmful || debuff_tagged {
        CancelStatus::Harmful
    } else {
        CancelStatus::Ok
    }
}

/// How well `needle` names a row: 0 exact, 1 in-order word prefix (`ench`,
/// `stone s`), 2 each typed word begins some word of a name (`skin`).
fn cancel_match_tier(needle: &str, row: &CancelRow) -> Option<u8> {
    use mud_world::targeting::{NameRank, names_match, rank_ability_name};
    let words = |s: &str| s.replace(['_', '-'], " ");
    let names: Vec<String> = std::iter::once(row.label.as_str())
        .chain(std::iter::once(row.name.as_str()))
        .chain(row.src.as_deref())
        .map(words)
        .collect();
    let best = names
        .iter()
        .map(|n| rank_ability_name(needle, n))
        .min_by_key(|r| match r {
            NameRank::Exact => 0,
            NameRank::Prefix => 1,
            NameRank::None => 2,
        });
    match best {
        Some(NameRank::Exact) => Some(0),
        Some(NameRank::Prefix) => Some(1),
        _ => names_match(needle, names.iter().map(String::as_str)).then_some(2),
    }
}

/// The first row at the best tier: the first exact match, else the first
/// prefix match, else the first loose word match.
fn pick_cancel_row<'a>(needle: &str, rows: &[&'a CancelRow]) -> Option<&'a CancelRow> {
    rows.iter()
        .filter_map(|r| cancel_match_tier(needle, r).map(|t| (t, *r)))
        .min_by_key(|(t, _)| *t)
        .map(|(_, r)| r)
}

/// `cancel [<effect>]`: drop one of your own timed, beneficial spell
/// effects before it wears off. Out of combat only, instant. The effect
/// ends through the expiry path ([`crate::effects::remove_effect_instance`]
/// after the normal wear-off text), so stat and flag reversals, hooks and
/// the GMCP effect list behave exactly as on a natural wear-off.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_cancel(world: &mut World, player: Entity, args: &str) {
    let god = mud_world::bypasses_restrictions(world, player);
    let needle = if args.trim_start().starts_with(['\'', '"']) {
        crate::commands::parse_quoted_first_token(args).0
    } else {
        args.trim().to_string()
    }
    .to_ascii_lowercase();
    // id -> (plain name, hostile or violent). The plain name is what the
    // user sees on `effects` ("from Enhance Ability"), so `cancel enhance`
    // works alongside `cancel cha`.
    let abilities: std::collections::HashMap<i32, (String, bool)> = world
        .resource::<crate::commands::AbilityCatalog>()
        .by_name
        .values()
        .map(|d| {
            (
                d.id,
                (
                    d.plain_name.replace('_', " ").to_ascii_lowercase(),
                    d.violent || crate::commands::ability_is_hostile(world, d),
                ),
            )
        })
        .collect();
    let harmful: std::collections::HashMap<i32, bool> =
        abilities.iter().map(|(id, (_, h))| (*id, *h)).collect();
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
    let rows: Vec<CancelRow> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        let mut rows = Vec::new();
        for (e, inst, a) in q.iter(world) {
            let is_wall = a.0 != player
                && player_room.is_some_and(|r| a.0 == r)
                && inst.name.starts_with("wall-");
            if a.0 != player && !is_wall {
                continue;
            }
            let status = if is_wall {
                if inst.remaining_secs < 0 {
                    CancelStatus::Permanent
                } else {
                    CancelStatus::Ok
                }
            } else {
                cancel_status(world, inst, &harmful, god)
            };
            rows.push(CancelRow {
                entity: e,
                name: inst.name.clone(),
                label: super::info::effect_label(world, inst),
                src: inst
                    .ability_id
                    .and_then(|id| abilities.get(&id).map(|(n, _)| n.clone())),
                ability_id: inst.ability_id,
                remaining: inst.remaining_secs,
                status,
            });
        }
        rows
    };
    let cancellable: Vec<&CancelRow> = rows
        .iter()
        .filter(|r| r.status == CancelStatus::Ok)
        .collect();
    if needle.is_empty() {
        if cancellable.is_empty() {
            send_to(world, player, "You have no effects you can cancel.\r\n");
            return;
        }
        // An ability that applies several effects lists as one row.
        let mut seen = std::collections::HashSet::new();
        let listed: Vec<&&CancelRow> = cancellable
            .iter()
            .filter(|r| r.ability_id.is_none_or(|id| seen.insert(id)))
            .collect();
        let mut out = format!("\r\n{} cancellable effect(s):\r\n", listed.len());
        for r in listed {
            let hours = super::info::format_effect_hours(r.remaining);
            out.push_str(&format!("  {} ({hours})\r\n", r.label));
        }
        out.push_str("\r\nUse 'cancel <name>' to drop one.\r\n");
        send_to(world, player, out);
        return;
    }
    if !god && crate::commands::in_combat(world, player) {
        send_to(
            world,
            player,
            "You can't cancel an effect in the middle of a fight!\r\n",
        );
        return;
    }
    let Some(chosen) = pick_cancel_row(&needle, &cancellable) else {
        // Name the reason when the match is an effect that cannot go.
        let others: Vec<&CancelRow> = rows
            .iter()
            .filter(|r| r.status != CancelStatus::Ok)
            .collect();
        let msg = match pick_cancel_row(&needle, &others) {
            Some(r) => {
                let why = match r.status {
                    CancelStatus::Permanent => "it is permanent",
                    CancelStatus::Harmful => "it is harmful, not a blessing",
                    _ => "it is not a spell you can drop",
                };
                format!("You can't cancel {}: {why}.\r\n", r.label)
            }
            None => format!("No cancellable effect matching '{needle}' on you.\r\n"),
        };
        send_to(world, player, msg);
        return;
    };
    let label = chosen.label.clone();
    let removed_name = chosen.name.clone();
    let chosen_ability = chosen.ability_id;
    // Every effect the same ability applied goes together (Statue: statue
    // plus familiarity), so the spell is truly dropped.
    let doomed: Vec<Entity> = match chosen_ability {
        Some(id) => cancellable
            .iter()
            .filter(|r| r.ability_id == Some(id))
            .map(|r| r.entity)
            .collect(),
        None => vec![chosen.entity],
    };
    let target_effect = chosen.entity;
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
    let is_wall = removed_name.starts_with("wall-");
    if is_wall {
        let target_room = world.get::<AppliedTo>(target_effect).map(|a| a.0);
        let mut dissolved: Option<(mud_db::enums::Direction, String, bevy_ecs::entity::Entity)> =
            None;
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
                if empty && let Ok(mut em) = world.get_entity_mut(room) {
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
    send_to(world, player, format!("You cancel {label}.\r\n"));
    // The normal wear-off text first (a wall's dissolve was broadcast
    // above), then the same early-removal path dispel uses.
    if !is_wall {
        crate::effects::send_wearoff_messages(world, player, &removed_name, chosen_ability);
    }
    for e in doomed {
        // A hook or sibling removal may already have taken it.
        if world.get::<EffectInstance>(e).is_some() {
            let bearer = world.get::<AppliedTo>(e).map_or(player, |a| a.0);
            crate::effects::remove_effect_instance(world, bearer, e);
        }
    }
}
/// Resolve what `study` was given with the cast matcher: quoted or unquoted
/// multi-word names, an exact name first, then a name prefix.
pub(crate) fn resolve_study_ability(
    catalog: &AbilityCatalog,
    args: &str,
) -> Option<mud_world::AbilityDef> {
    let needle = if args.trim_start().starts_with(['\'', '"']) {
        crate::commands::parse_quoted_first_token(args)
            .0
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_")
    } else {
        args.split_whitespace().collect::<Vec<_>>().join("_")
    }
    .to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    catalog
        .find_exact(&needle, None)
        .or_else(|| catalog.find_by_prefix(&needle, None, None))
        .cloned()
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_study(world: &mut World, player: Entity, args: &str) {
    use mud_world::SpellSlotData;
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let god = mud_world::bypasses_restrictions(world, player);
    let Some(class_id) = profile.class_id.or_else(|| god.then_some(0)) else {
        send_to(world, player, "You have no class.\r\n");
        return;
    };
    if args.trim().is_empty() && !god {
        list_learnable(world, player);
        return;
    }
    let Some(def) = resolve_study_ability(world.resource::<AbilityCatalog>(), args) else {
        let typed = args.trim();
        if typed.is_empty() {
            send_to(world, player, "Study what?\r\n");
        } else {
            send_to(
                world,
                player,
                format!("'{typed}' isn't a known ability.\r\n"),
            );
        }
        return;
    };
    // A class's list is its spell circles plus its skill table. A skill also
    // has a level to reach before it can be learned.
    let on_spell_list = world
        .resource::<SpellSlotData>()
        .ability_circle
        .contains_key(&(class_id, def.id));
    let skill_min_level = world
        .get_resource::<mud_world::ClassSkillsData>()
        .and_then(|d| d.min_level_for(class_id, def.id));
    if !god && !on_spell_list && skill_min_level.is_none() {
        send_msg(
            world,
            player,
            format!("{} isn't on your class's list.\r\n", def.name),
        );
        return;
    }
    // A spell is filed under a circle the class unlocks at a level; a
    // character below it cannot learn the spell yet.
    let spell_min_level = world
        .resource::<SpellSlotData>()
        .spell_min_level(class_id, def.id);
    if !god
        && on_spell_list
        && let Some(min) = spell_min_level
        && profile.level < min
    {
        send_to(
            world,
            player,
            format!(
                "You must reach level {min} before you can learn {}.\r\n",
                def.name
            ),
        );
        return;
    }
    if !god
        && !on_spell_list
        && let Some(min) = skill_min_level
        && profile.level < min
    {
        send_to(
            world,
            player,
            format!(
                "You must reach level {min} before you can learn {}.\r\n",
                def.name
            ),
        );
        return;
    }
    // A god already knows every ability; nothing is written for them.
    if god {
        send_to(world, player, format!("You already know {}.\r\n", def.name));
        return;
    }
    if let Some(known) = world.get::<KnownAbilities>(player)
        && known.has_any(def.id)
    {
        send_to(world, player, format!("You already know {}.\r\n", def.name));
        return;
    }
    // A skill starts at the low proficiency every class skill starts at;
    // a spell is simply known.
    let start = if on_spell_list {
        1
    } else {
        mud_world::STARTING_SKILL_PROFICIENCY
    };
    if let Some(mut known) = world.get_mut::<KnownAbilities>(player) {
        known.entries.push((def.id, start, true));
        known.entries.sort_by_key(|(id, _, _)| *id);
    } else {
        world.entity_mut(player).insert(KnownAbilities {
            entries: vec![(def.id, start, true)],
        });
    }
    // A skill starts at a set proficiency worth showing; a spell is just
    // known, and `practice` is where its proficiency shows.
    let message = if on_spell_list {
        format!("You commit {} to memory.\r\n", def.name)
    } else {
        format!(
            "You commit {} to memory. (proficiency {start})\r\n",
            def.name
        )
    };
    send_to(world, player, message);
}

/// A spell, chant or song the character's class offers at their level that
/// they have not yet learned.
pub(crate) struct Learnable {
    pub circle: i32,
    pub name: String,
}

/// Everything `study <name>` would accept right now: abilities on the class's
/// circle lists whose circle the character has reached, minus what they
/// already know and minus quest-only abilities (a quest hands those out).
/// Sorted by circle, then name. Empty for classless characters and staff.
pub(crate) fn learnable_abilities(world: &World, player: Entity) -> Vec<Learnable> {
    use mud_world::SpellSlotData;
    let Some(profile) = world.get::<Profile>(player) else {
        return Vec::new();
    };
    let Some(class_id) = profile.class_id else {
        return Vec::new();
    };
    if mud_world::bypasses_restrictions(world, player) {
        return Vec::new();
    }
    let (Some(slot_data), Some(catalog)) = (
        world.get_resource::<SpellSlotData>(),
        world.get_resource::<AbilityCatalog>(),
    ) else {
        return Vec::new();
    };
    let level = profile.level;
    let known = world.get::<KnownAbilities>(player);
    let mut out: Vec<Learnable> = catalog
        .by_name
        .values()
        .filter(|d| d.kind != mud_db::abilities::AbilityKind::Skill && !d.quest_only)
        .filter_map(|d| {
            let circle = *slot_data.ability_circle.get(&(class_id, d.id))?;
            let unlocked = slot_data
                .circle_min_level(class_id, circle)
                .is_none_or(|min| level >= min);
            (unlocked && !known.is_some_and(|k| k.has_any(d.id))).then(|| Learnable {
                circle,
                name: d.name.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.circle.cmp(&b.circle).then_with(|| a.name.cmp(&b.name)));
    out.dedup_by(|a, b| a.circle == b.circle && a.name == b.name);
    out
}

/// `study` with no argument: what the character can learn right now.
fn list_learnable(world: &mut World, player: Entity) {
    let learnable = learnable_abilities(world, player);
    if learnable.is_empty() {
        send_to(
            world,
            player,
            "There is nothing new for you to study at your level.\r\n",
        );
        return;
    }
    let width = crate::layout::wrap_width(world, player);
    let mut out = String::from("You can study:\r\n");
    let mut circles: Vec<i32> = learnable.iter().map(|l| l.circle).collect();
    circles.dedup();
    for circle in circles {
        let names: Vec<&str> = learnable
            .iter()
            .filter(|l| l.circle == circle)
            .map(|l| l.name.as_str())
            .collect();
        let head = format!("  Circle {circle}: ");
        let hang = head.len();
        for (n, line) in crate::layout::wrap_paragraph(&names.join(", "), width, hang, hang)
            .iter()
            .enumerate()
        {
            // The first line carries the circle label in place of its indent.
            if n == 0 {
                out.push_str(&head);
                out.push_str(line.trim_start());
            } else {
                out.push_str(line);
            }
            out.push_str("\r\n");
        }
    }
    out.push_str("Type 'study <name>' to learn one.\r\n");
    send_to(world, player, out);
}
/// Pooled slots recover on their own, so there is nothing to prepare:
/// `memorize` / `forget` with no argument show the slot pool, and with
/// an argument explain why nothing happened.
fn slots_recover_notice(world: &mut World, player: Entity, args: &str) {
    if args.trim().is_empty() {
        crate::commands::info::cmd_slots(world, player, "");
    } else {
        send_to(
            world,
            player,
            "Spell slots recover on their own; there's nothing to memorize.\r\n",
        );
    }
}
pub(crate) fn cmd_memorize(world: &mut World, player: Entity, args: &str) {
    slots_recover_notice(world, player, args);
}
pub(crate) fn cmd_forget(world: &mut World, player: Entity, args: &str) {
    slots_recover_notice(world, player, args);
}
pub(crate) fn cmd_cast(world: &mut World, player: Entity, args: &str) {
    invoke_ability(
        world,
        player,
        args,
        mud_db::abilities::AbilityKind::Spell,
        "cast",
    );
}
pub(crate) fn cmd_chant(world: &mut World, player: Entity, args: &str) {
    invoke_ability(
        world,
        player,
        args,
        mud_db::abilities::AbilityKind::Chant,
        "chant",
    );
}
pub(crate) fn cmd_perform(world: &mut World, player: Entity, args: &str) {
    invoke_ability(
        world,
        player,
        args,
        mud_db::abilities::AbilityKind::Song,
        "perform",
    );
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use crate::commands::test_support::{drain, install_core_abilities, player_in};
    use mud_db::enums::Direction;
    use mud_world::{ExitData, Stamina};

    fn locked_north_door(world: &mut World) -> (Entity, Entity) {
        let far = world.spawn(Exits::default()).id();
        let mut exits = Exits::default();
        exits.0.insert(
            Direction::North,
            ExitData {
                to: Some(far),
                state: ExitState::Locked,
                key: Some((1, 1)),
                description: None,
                keywords: Vec::new(),
                is_hidden: false,
                is_pickproof: false,
                is_bashable: false,
                hit_points: None,
            },
        );
        let room = world.spawn(exits).id();
        (room, far)
    }

    #[test]
    fn pick_lock_resolved_by_name_opens_the_door() {
        let mut world = World::new();
        let core = install_core_abilities(&mut world);
        let (room, _far) = locked_north_door(&mut world);
        let (p, mut rx) = player_in(&mut world, room);
        let pick = core.pick_lock.expect("Pick Lock resolves");
        // Proficiency 1000 is a 100% chance.
        world.entity_mut(p).insert((
            KnownAbilities {
                entries: vec![(pick, 1000, true)],
            },
            Stamina {
                current: 50,
                max: 50,
            },
        ));
        cmd_pick(&mut world, p, "north");
        let out = drain(&mut rx);
        assert!(!out.contains("don't know how"), "{out}");
        let state = world.get::<Exits>(room).unwrap().0[&Direction::North].state;
        assert_eq!(state, ExitState::Closed, "{out}");
    }

    fn name_the_door(world: &mut World, room: Entity, keyword: &str) {
        world
            .get_mut::<Exits>(room)
            .unwrap()
            .0
            .get_mut(&Direction::North)
            .unwrap()
            .keywords = vec![keyword.to_string()];
    }

    #[test]
    fn pick_messages_name_the_door_and_put_no_direction_last() {
        let mut world = World::new();
        let core = install_core_abilities(&mut world);
        let (room, _far) = locked_north_door(&mut world);
        name_the_door(&mut world, room, "grate");
        let (p, mut rx) = player_in(&mut world, room);
        world.entity_mut(p).insert((
            KnownAbilities {
                entries: vec![(core.pick_lock.unwrap(), 1000, true)],
            },
            Stamina {
                current: 50,
                max: 50,
            },
        ));
        cmd_pick(&mut world, p, "north");
        let out = drain(&mut rx);
        assert!(
            out.contains("*click* You pick the lock on the grate."),
            "{out}"
        );
        // Now closed, not locked.
        cmd_pick(&mut world, p, "north");
        let out = drain(&mut rx);
        assert!(out.contains("The grate isn't locked."), "{out}");
        assert!(!out.contains("north"), "{out}");
    }

    #[test]
    fn pick_without_a_keyhole_names_the_door() {
        let mut world = World::new();
        install_core_abilities(&mut world);
        let (room, _far) = locked_north_door(&mut world);
        name_the_door(&mut world, room, "slab");
        world
            .get_mut::<Exits>(room)
            .unwrap()
            .0
            .get_mut(&Direction::North)
            .unwrap()
            .key = None;
        let (p, mut rx) = player_in(&mut world, room);
        cmd_pick(&mut world, p, "north");
        let out = drain(&mut rx);
        assert!(out.contains("There's no keyhole on the slab."), "{out}");
    }

    #[test]
    fn the_retired_pick_lock_id_does_not_count() {
        let mut world = World::new();
        install_core_abilities(&mut world);
        let (room, _far) = locked_north_door(&mut world);
        let (p, mut rx) = player_in(&mut world, room);
        world.entity_mut(p).insert((
            KnownAbilities {
                entries: vec![(272, 1000, true)],
            },
            Stamina {
                current: 50,
                max: 50,
            },
        ));
        cmd_pick(&mut world, p, "north");
        assert!(drain(&mut rx).contains("don't know how"));
    }
}

#[cfg(test)]
mod study_resolver_tests {
    use super::*;
    use crate::commands::test_support::ability_def;
    use mud_db::abilities::AbilityKind;

    fn catalog() -> AbilityCatalog {
        let mut catalog = AbilityCatalog::default();
        for (id, name) in [
            (1, "Magic_Missile"),
            (2, "Fire"),
            (3, "Fireball"),
            (4, "Word_Of_Recall"),
        ] {
            catalog.by_name.insert(
                name.to_ascii_lowercase(),
                ability_def(id, name, AbilityKind::Spell),
            );
        }
        catalog
    }

    fn id_of(args: &str) -> Option<i32> {
        resolve_study_ability(&catalog(), args).map(|d| d.id)
    }

    #[test]
    fn unquoted_multi_word_names_resolve() {
        assert_eq!(id_of("magic missile"), Some(1));
        assert_eq!(id_of("Magic   Missile"), Some(1));
        assert_eq!(id_of("word of recall"), Some(4));
    }

    #[test]
    fn quoted_names_resolve() {
        assert_eq!(id_of("'magic missile'"), Some(1));
        assert_eq!(id_of("\"magic missile\""), Some(1));
    }

    #[test]
    fn a_prefix_resolves_but_an_exact_name_wins() {
        assert_eq!(id_of("magic m"), Some(1));
        assert_eq!(id_of("fireb"), Some(3));
        assert_eq!(id_of("fire"), Some(2), "exact name beats the longer prefix");
    }

    #[test]
    fn unknown_or_empty_input_resolves_to_nothing() {
        assert_eq!(id_of(""), None);
        assert_eq!(id_of("   "), None);
        assert_eq!(id_of("nonsense"), None);
    }
}

#[cfg(test)]
mod memorize_tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};

    #[test]
    fn memorize_and_forget_with_args_explain_instead_of_listing_slots() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (p, mut rx) = player_in(&mut world, room);
        for cmd in [cmd_memorize, cmd_forget] {
            cmd(&mut world, p, "magic missile");
            let out = drain(&mut rx);
            assert!(
                out.contains("Spell slots recover on their own; there's nothing to memorize."),
                "{out}"
            );
        }
    }
}
