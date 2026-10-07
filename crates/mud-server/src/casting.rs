//! Cast queue + per-tick wind-up resolution.
//!
//! `invoke_ability_with` is the resolution path. The `start_cast`
//! entry inspects `Ability.cast_time_rounds`; when > 0 it installs a
//! `Casting` component instead of running the cast immediately. Once
//! per `casting_tick`, every active `Casting` component decrements
//! `ticks_remaining`; on reaching 0 the cast is resolved through
//! `invoke_ability_with` with `skip_queue = true`.
//!
//! Targeting: the target is resolved once when the cast starts and
//! stored in `Casting.target` as an entity (never re-resolved by name).
//! Every tick the lock is re-validated; a target that died, left the
//! room, or stopped being the caster's opponent aborts the cast
//! ("You stop chanting abruptly!", legacy `casting_handler`).
//!
//! While a cast winds up the dispatcher refuses every command that
//! isn't on the legacy `CMD_CAST` allow-list ("You are busy
//! spellcasting..."), so walking / fighting can't break it by
//! accident; `abort`, `disengage` and `flee` are the deliberate exits.
//!
//! Interruption:
//! - Posture drop / sleep / stun / silence → cancel.
//! - Damage taken during cast → Concentration check (damage as % of
//!   max HP, > 30% breaks).
//! - `abort`, `disengage`, `flee` → explicit interrupt.
//! - Being moved by something else (follow, summon) → cancel.
//! - A bash knocks the caster flat (see `commands/combat.rs`).
//!
//! Quick Chant: when the spell starts, a caster who knows the skill
//! rolls the legacy formula; success halves the wind-up and trims it
//! further by how far the spell sits below the caster's top circle
//! (`wind_up_ticks`).
//!
//! `cast_time_rounds = 0` is treated as instant — call site routes
//! straight into `invoke_ability_with`. Item-driven casts (scrolls /
//! wands / potions) also skip the queue: the item itself is the
//! delay-bearer, the resulting cast lands instantly.

use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, AbilityDef, Account, CastTarget, Casting, CoreStats, Fighting, Health,
    KnownAbilities, Located, Player, Posture, PostureKind, Profile, SpellSlotData, SpellSlots,
};

use crate::commands::{Prevent, can_see_player, effect_prevents, name_of, name_or, send_to};

/// One combat round in ticks. Combat round = 4s (per
/// `GameConfig.combat.round_seconds`), `TICK_HZ` = 10, so 40 ticks.
/// Cast wind-up = `cast_time_rounds * COMBAT_ROUND_TICKS`.
///
/// Kept in code as the conversion is a runtime invariant — the
/// `GameConfig` row tunes the *seconds* per round, not the tick rate.
pub(crate) const COMBAT_ROUND_TICKS: i32 = 40;

/// Legacy cast times are counted in "stars" of one second each.
const TICKS_PER_STAR: i32 = 10;

/// Ticks between the `Casting: Fireball **` countdown lines (legacy:
/// one every two seconds).
const COUNTDOWN_TICKS: i32 = 20;

/// Catalog key (lowercased `plain_name`) of the Quick Chant skill.
const QUICK_CHANT_KEY: &str = "quick_chant";

/// Catalog key of the skill bystanders use to recognise a chant.
const KNOW_SPELL_KEY: &str = "know_spell";

/// Commands (canonical registry names) a player may still use while a
/// cast winds up: the deliberate exits plus the passive info and
/// channel commands legacy flagged `CMD_CAST`. Staff commands are
/// always allowed (legacy `CMD_ANY`).
const CASTING_ALLOWED: &[&str] = &[
    "abort",
    "disengage",
    "flee",
    "look",
    "consent",
    "gossip",
    "tell",
    "reply",
    "gsay",
    "ctell",
    "qsay",
    "music",
    "ignore",
    "score",
    "inventory",
    "equipment",
    "experience",
    "level",
    "trophy",
    "who",
    "whoami",
    "where",
    "time",
    "date",
    "weather",
    "uptime",
    "version",
    "help",
    "commands",
    "socials",
    "skills",
    "spells",
    "songs",
    "chants",
    "innate",
    "quests",
    "prompt",
    "color",
    "title",
    "news",
    "motd",
    "credits",
    "policies",
    "lasttells",
    "lastgossips",
    "greport",
    "bug",
    "idea",
    "typo",
    "petition",
];

/// Whether `command` (canonical name) may run while the caster winds
/// up a spell. Staff checks happen at the call site.
#[must_use]
pub(crate) fn allowed_while_casting(command: &str) -> bool {
    CASTING_ALLOWED.contains(&command)
}

/// Legacy quick-chant roll: `random(1..=110) < skill + int + wis`
/// (stat bonuses never subtract).
#[must_use]
pub(crate) fn quick_chant_hit(skill: i32, int_bonus: i32, wis_bonus: i32, roll: i32) -> bool {
    roll < skill + int_bonus.max(0) + wis_bonus.max(0)
}

/// Wind-up length in stars after a successful quick chant: halved,
/// then trimmed by how far below the caster's top circle the spell
/// sits (every 3 circles for damage / healing / violent spells, with
/// a floor; every 2 for the rest). Never below one star.
#[must_use]
pub(crate) fn quick_chant_stars(
    base_stars: i32,
    max_circle: i32,
    spell_circle: i32,
    offensive: bool,
) -> i32 {
    let gap = (max_circle - spell_circle).max(0);
    let mut stars = base_stars / 2;
    if offensive {
        if stars > 1 {
            stars -= gap / 3;
            // Long casts never drop below (half - 2) stars.
            if base_stars >= 10 && stars < base_stars / 2 - 2 {
                stars = base_stars / 2 - 2;
            }
        }
    } else {
        stars -= gap / 2;
    }
    stars.max(1)
}

/// Wind-up length in ticks for `caster` starting `def`, and whether
/// quick chant fired. `roll` is the `1..=110` die, passed in so tests
/// can force either outcome. Only spells quick-chant, and only for a
/// caster who has the Quick Chant skill.
#[must_use]
pub(crate) fn wind_up_ticks(
    world: &World,
    caster: Entity,
    def: &AbilityDef,
    roll: i32,
) -> (i32, bool) {
    let base = def.cast_time_rounds * COMBAT_ROUND_TICKS;
    if !matches!(def.kind, mud_db::abilities::AbilityKind::Spell) {
        return (base, false);
    }
    let catalog = world.resource::<AbilityCatalog>();
    let Some(quick) = catalog.by_name.get(QUICK_CHANT_KEY) else {
        return (base, false);
    };
    let Some(skill) = world.get::<KnownAbilities>(caster).and_then(|k| {
        k.entries
            .iter()
            .find(|(id, _, known)| *id == quick.id && *known)
            .map(|(_, prof, _)| (prof / 10).clamp(0, 100))
    }) else {
        return (base, false);
    };
    let core = world.get::<CoreStats>(caster).copied().unwrap_or_default();
    if !quick_chant_hit(
        skill,
        CoreStats::bonus(core.intelligence),
        CoreStats::bonus(core.wisdom),
        roll,
    ) {
        return (base, false);
    }
    let slots = world.resource::<SpellSlotData>();
    let (max_circle, spell_circle) = world.get::<Profile>(caster).map_or((0, 0), |p| {
        let circle = p
            .class_id
            .and_then(|c| slots.ability_circle.get(&(c, def.id)).copied())
            .unwrap_or(0);
        (slots.max_circle_at_level(p.level), circle)
    });
    // A spell with no class circle can't be measured against the
    // caster's top circle: it just gets the halving.
    let max_circle = if spell_circle == 0 { 0 } else { max_circle };
    let offensive = def.damage_type.is_some()
        || def.violent
        || def.sphere.as_deref() == Some("healing")
        || def.plain_name.eq_ignore_ascii_case("STONE_SKIN");
    let stars = quick_chant_stars(base / TICKS_PER_STAR, max_circle, spell_circle, offensive);
    (stars * TICKS_PER_STAR, true)
}

fn actor_alive(world: &World, e: Entity) -> bool {
    world.get_entity(e).is_ok() && world.get::<Health>(e).is_none_or(|h| h.hp > 0)
}

/// Whether the entity a wind-up locked onto is still somewhere the
/// spell can reach (legacy `casting_handler` target validity).
#[must_use]
pub(crate) fn target_still_valid(world: &World, caster: Entity, target: CastTarget) -> bool {
    match target {
        CastTarget::Area | CastTarget::Caster => true,
        CastTarget::InRoom(t) => {
            actor_alive(world, t)
                && world.get::<Located>(t).map(|l| l.0) == world.get::<Located>(caster).map(|l| l.0)
        }
        CastTarget::World(t) => actor_alive(world, t),
        CastTarget::Fighting(t) => {
            actor_alive(world, t) && world.get::<Fighting>(caster).is_some_and(|f| f.0 == t)
        }
        CastTarget::Carried(item) => {
            world.get_entity(item).is_ok()
                && world.get::<Located>(item).is_some_and(|l| l.0 == caster)
        }
    }
}

/// Awake players sharing the caster's room (the audience for chant
/// lines).
fn observers(world: &mut World, caster: Entity) -> Vec<Entity> {
    let Some(room) = world.get::<Located>(caster).map(|l| l.0) else {
        return Vec::new();
    };
    let mut q = world.query_filtered::<(Entity, &Located, Option<&Posture>), With<Player>>();
    q.iter(world)
        .filter(|(e, l, posture)| {
            *e != caster
                && l.0 == room
                && !posture.is_some_and(|p| matches!(p.0, PostureKind::Sleeping))
        })
        .map(|(e, _, _)| e)
        .collect()
}

fn is_staff(world: &World, e: Entity) -> bool {
    world
        .get::<Account>(e)
        .is_some_and(|a| a.role.at_least(mud_db::enums::UserRole::Builder))
}

/// Legacy syllable table: a spell's name as heard by someone who
/// doesn't recognise it. Whole-syllable rewrites first, then a
/// letter-for-letter cipher. Flavour text, not tuning.
const SYLLABLES: &[(&str, &str)] = &[
    (" ", " "),
    ("ar", "abra"),
    ("ate", "i"),
    ("cau", "kada"),
    ("blind", "nose"),
    ("bur", "mosa"),
    ("cu", "judi"),
    ("de", "oculo"),
    ("dis", "mar"),
    ("ect", "kamina"),
    ("en", "uns"),
    ("gro", "cra"),
    ("light", "dies"),
    ("lo", "hi"),
    ("magi", "kari"),
    ("mon", "bar"),
    ("mor", "zak"),
    ("move", "sido"),
    ("ness", "lacri"),
    ("ning", "illa"),
    ("per", "duda"),
    ("ra", "gru"),
    ("re", "candus"),
    ("son", "sabru"),
    ("tect", "infra"),
    ("tri", "cula"),
    ("ven", "nofo"),
    ("word of", "inset"),
    ("a", "i"),
    ("b", "v"),
    ("c", "q"),
    ("d", "m"),
    ("e", "o"),
    ("f", "y"),
    ("g", "t"),
    ("h", "p"),
    ("i", "u"),
    ("j", "y"),
    ("k", "t"),
    ("l", "r"),
    ("m", "w"),
    ("n", "b"),
    ("o", "a"),
    ("p", "s"),
    ("q", "d"),
    ("r", "f"),
    ("s", "g"),
    ("t", "h"),
    ("u", "e"),
    ("v", "z"),
    ("w", "x"),
    ("x", "n"),
    ("y", "l"),
    ("z", "k"),
];

/// Spell name as a bystander who can't place it hears it.
fn syllabize(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut out = String::new();
    let mut rest = lower.as_str();
    while !rest.is_empty() {
        if let Some((org, new)) = SYLLABLES.iter().find(|(org, _)| rest.starts_with(org)) {
            out.push_str(new);
            rest = &rest[org.len()..];
        } else {
            let ch = rest.chars().next().unwrap_or(' ');
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

/// Legacy `garble_text(-1)`: half the letters are swapped for random
/// ones before the syllable table is applied, so repeat casts don't
/// sound identical.
fn garble(name: &str) -> String {
    const LETTERS: &[u8] = b"aeiousthpwxyz";
    name.chars()
        .map(|c| {
            if c.is_ascii_alphabetic() && rand::random_range(0..=1) == 1 {
                char::from(LETTERS[rand::random_range(0..LETTERS.len())])
            } else {
                c
            }
        })
        .collect()
}

/// What the spell is called to the room: lowercase plain name with
/// spaces (`MAGIC_MISSILE` -> `magic missile`).
fn spoken_name(def: &AbilityDef) -> String {
    def.plain_name.to_ascii_lowercase().replace('_', " ")
}

fn caster_label(world: &World, observer: Entity, caster: Entity) -> String {
    if can_see_player(world, observer, caster) {
        name_of(world, caster)
    } else {
        "Someone".to_string()
    }
}

/// Legacy `start_chant`: tell everyone in the room the caster has
/// begun. Each awake bystander rolls `Know Spell` to recognise the
/// spell, otherwise hears garbled syllables; an Intelligence roll (or
/// recognising the spell) shows who it's aimed at. Returns the
/// bystanders who recognised it, for the completion line.
pub(crate) fn announce_cast_start(
    world: &mut World,
    caster: Entity,
    def: &AbilityDef,
    verb: &str,
    target: CastTarget,
) -> Vec<Entity> {
    let know_spell_id = world
        .resource::<AbilityCatalog>()
        .by_name
        .get(KNOW_SPELL_KEY)
        .map(|d| d.id);
    let plain = spoken_name(def);
    let garbled = syllabize(&garble(&plain));
    let target_actor = match target {
        CastTarget::InRoom(t) | CastTarget::Fighting(t) => Some(t),
        _ => None,
    };
    let mut recognized = Vec::new();
    for observer in observers(world, caster) {
        let staff = is_staff(world, observer);
        let skill = know_spell_id
            .and_then(|id| {
                world.get::<KnownAbilities>(observer).and_then(|k| {
                    k.entries
                        .iter()
                        .find(|(aid, _, _)| *aid == id)
                        .map(|(_, p, _)| (p / 10).clamp(0, 100))
                })
            })
            .unwrap_or(0);
        let knows = staff || rand::random_range(0..=101) <= skill;
        if knows {
            recognized.push(observer);
        }
        let spell = if knows { &plain } else { &garbled };
        let int = world
            .get::<CoreStats>(observer)
            .map_or(0, |s| s.intelligence);
        let at = match target_actor {
            Some(t) if knows || rand::random_range(0..=101) < int => {
                if t == observer {
                    " at <b><red>You</></>!!!".to_string()
                } else {
                    format!(" at <b>{}</>", name_or(world, t, "someone"))
                }
            }
            _ => String::new(),
        };
        let who = caster_label(world, observer, caster);
        send_to(
            world,
            observer,
            format!("{who} starts {verb}ing <b><yellow>'{spell}'</></>{at}...\r\n"),
        );
    }
    recognized
}

/// Legacy `complete_spell` + `end_chant`: "You complete your spell",
/// the room's "completes their spell", then the words spoken (true
/// name for those who recognised it).
fn announce_cast_complete(world: &mut World, caster: Entity, snap: &Casting) {
    let noun = match snap.kind_label.as_str() {
        "chant" => "chant",
        "song" => "song",
        _ => "spell",
    };
    send_to(world, caster, format!("You complete your {noun}.\r\n"));
    let plain = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .find(|d| d.id == snap.ability_id)
        .map_or_else(|| snap.ability_name.clone(), spoken_name);
    let garbled = syllabize(&plain);
    for observer in observers(world, caster) {
        let who = caster_label(world, observer, caster);
        send_to(
            world,
            observer,
            format!("{who} completes their {noun}...\r\n"),
        );
        let spell = if snap.recognized_by.contains(&observer) || is_staff(world, observer) {
            &plain
        } else {
            &garbled
        };
        let line = match snap.target {
            CastTarget::Caster => format!("closes their eyes and utters the words, '{spell}'."),
            CastTarget::InRoom(t) | CastTarget::Fighting(t) if t == observer => {
                format!("stares at you and utters the words, '{spell}'.")
            }
            CastTarget::InRoom(t) | CastTarget::Fighting(t) => {
                if can_see_player(world, observer, t) {
                    format!(
                        "stares at {} and utters the words, '{spell}'.",
                        name_or(world, t, "someone"),
                    )
                } else {
                    format!("stares off at nothing and utters the words, '{spell}'.")
                }
            }
            CastTarget::Carried(item) => format!(
                "stares at {} and utters the words, '{spell}'.",
                name_or(world, item, "something"),
            ),
            CastTarget::Area | CastTarget::World(_) => {
                format!("utters the words, '{spell}'.")
            }
        };
        send_to(world, observer, format!("{who} {line}\r\n"));
    }
}

/// Remove the wind-up and hand the spent slot back (legacy only
/// charges a spell that completes). `None` when nothing was casting.
fn end_cast(world: &mut World, caster: Entity) -> Option<Casting> {
    let snap = world.get::<Casting>(caster).cloned()?;
    if let Ok(mut em) = world.get_entity_mut(caster) {
        em.remove::<Casting>();
    }
    if let Some(circle) = snap.slot_circle
        && let Some(mut slots) = world.get_mut::<SpellSlots>(caster)
        && let Some(pos) = slots.in_flight.iter().rposition(|c| c.circle == circle)
    {
        slots.in_flight.remove(pos);
    }
    Some(snap)
}

/// Tell the room the caster's chant just broke off.
fn announce_stop(world: &mut World, caster: Entity) {
    let name = name_of(world, caster);
    for observer in observers(world, caster) {
        let who = if can_see_player(world, observer, caster) {
            name.clone()
        } else {
            "Someone".to_string()
        };
        send_to(
            world,
            observer,
            format!("{who} stops chanting abruptly!\r\n"),
        );
    }
}

/// Legacy `abort_casting`: the wind-up ends with "You stop chanting
/// abruptly!" (unless the caster was knocked out).
pub(crate) fn abort_casting(world: &mut World, caster: Entity) -> bool {
    if end_cast(world, caster).is_none() {
        return false;
    }
    send_to(world, caster, "You stop chanting abruptly!\r\n");
    announce_stop(world, caster);
    true
}

/// Advance every wind-up one tick: drop casts whose target is gone
/// or whose caster can no longer cast, print the countdown, and
/// resolve casts that reach 0 against the target locked at the start.
/// Runs once per server tick.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn casting_tick(world: &mut World) {
    let casters: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<Casting>>();
        q.iter(world).collect()
    };
    for caster in casters {
        let Some(snap) = world.get::<Casting>(caster).cloned() else {
            continue;
        };
        if world.get::<Health>(caster).is_none_or(|h| h.hp <= 0) {
            // Dead casters just lose the wind-up.
            end_cast(world, caster);
            continue;
        }
        if world
            .get::<Posture>(caster)
            .is_some_and(|p| matches!(p.0, PostureKind::Sleeping))
        {
            interrupt_cast(world, caster, "you fall asleep");
            continue;
        }
        if !target_still_valid(world, caster, snap.target) {
            abort_casting(world, caster);
            continue;
        }
        if effect_prevents(world, caster, Prevent::Casting) {
            interrupt_cast(world, caster, "your magic is suppressed");
            continue;
        }
        let remaining = snap.ticks_remaining - 1;
        if let Some(mut c) = world.get_mut::<Casting>(caster) {
            c.ticks_remaining = remaining;
        }
        if remaining > 0 {
            let elapsed = snap.ticks_total - remaining;
            if elapsed > 0 && elapsed % COUNTDOWN_TICKS == 0 {
                let stars = "*".repeat(
                    usize::try_from((remaining + COUNTDOWN_TICKS - 1) / COUNTDOWN_TICKS)
                        .unwrap_or(1),
                );
                send_to(
                    world,
                    caster,
                    format!("Casting: {} {stars}\r\n", snap.ability_name),
                );
            }
            continue;
        }
        // The wind-up is over: the slot stays spent. Take the
        // component off *without* refunding, then land the spell.
        if let Ok(mut em) = world.get_entity_mut(caster) {
            em.remove::<Casting>();
        }
        announce_cast_complete(world, caster, &snap);
        crate::commands::resolve_queued_cast(
            world,
            caster,
            &snap.args,
            mud_db::abilities::AbilityKind::from_label(&snap.kind_label.to_ascii_uppercase()),
            &snap.verb,
            snap.target,
        );
    }
}

/// Break the caster's concentration: the wind-up ends with `reason`
/// and the room hears the chant stop.
pub(crate) fn interrupt_cast(world: &mut World, caster: Entity, reason: &str) -> bool {
    let Some(snap) = end_cast(world, caster) else {
        return false;
    };
    send_to(
        world,
        caster,
        format!(
            "Your concentration on {} shatters — {reason}.\r\n",
            snap.ability_name,
        ),
    );
    announce_stop(world, caster);
    true
}

/// Concentration break on incoming damage. Damage > 30% of caster
/// max HP forces a save; for now any spike that big simply
/// interrupts. Lower-impact hits leave the cast intact.
pub(crate) fn check_concentration_on_damage(world: &mut World, caster: Entity, damage_taken: i32) {
    if world.get::<Casting>(caster).is_none() {
        return;
    }
    let max_hp = world.get::<Health>(caster).map_or(1, |h| h.max).max(1);
    let frac = (damage_taken * 100) / max_hp;
    if frac >= 30 {
        interrupt_cast(world, caster, "the blow rattles you");
    }
}

/// Used by `abort` / `disengage` / `flee`: player-initiated stop.
pub(crate) fn cancel_own_cast(world: &mut World, caster: Entity) -> bool {
    abort_casting(world, caster)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, ability_def, drain, player_in};
    use crate::commands::{dispatch, invoke_ability};
    use mud_db::abilities::AbilityKind;
    use mud_world::{EffectCatalog, EffectDef, Named};

    const MEND: i32 = 1;
    const QUICK_CHANT: i32 = 2;
    const EFFECT: i32 = 10;

    fn world_with_spell(rounds: i32) -> (World, Entity, Entity) {
        let mut world = World::new();
        let mut catalog = AbilityCatalog::default();
        let mut mend = ability_def(MEND, "Mend", AbilityKind::Spell);
        mend.cast_time_rounds = rounds;
        catalog.by_name.insert("mend".to_string(), mend);
        catalog.by_name.insert(
            "quick_chant".to_string(),
            ability_def(QUICK_CHANT, "Quick Chant", AbilityKind::Skill),
        );
        catalog.effects_for.insert(
            MEND,
            vec![(EFFECT, Some(serde_json::json!({ "amount": 4 })))],
        );
        world.insert_resource(catalog);
        let mut effects = EffectCatalog::default();
        effects.by_id.insert(
            EFFECT,
            EffectDef {
                id: EFFECT,
                name: "mend".to_string(),
                description: None,
                effect_type: "heal".to_string(),
                tags: Vec::new(),
                presence_override: None,
                default_params: serde_json::json!({}),
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
        world.insert_resource(effects);
        world.insert_resource(SpellSlotData::default());
        world.insert_resource(mud_world::ClassSkillsData::default());
        let room_a = world.spawn_empty().id();
        let room_b = world.spawn_empty().id();
        (world, room_a, room_b)
    }

    fn caster_in(world: &mut World, room: Entity) -> (Entity, Rx) {
        let (caster, rx) = player_in(world, room);
        world.entity_mut(caster).insert((
            Health { hp: 50, max: 50 },
            KnownAbilities {
                entries: vec![(MEND, 500, true)],
            },
        ));
        (caster, rx)
    }

    /// A wounded player called "Bob" standing in `room`.
    fn bob_in(world: &mut World, room: Entity) -> (Entity, Rx) {
        let (bob, rx) = player_in(world, room);
        world.entity_mut(bob).insert((
            Named {
                name: "Bob".to_string(),
            },
            Health { hp: 5, max: 50 },
        ));
        (bob, rx)
    }

    fn hp(world: &World, e: Entity) -> i32 {
        world.get::<Health>(e).unwrap().hp
    }

    fn run_ticks(world: &mut World, n: i32) {
        for _ in 0..n {
            casting_tick(world);
        }
    }

    fn start_mend(world: &mut World, caster: Entity) {
        invoke_ability(world, caster, "'mend' bob", AbilityKind::Spell, "cast");
    }

    #[test]
    fn target_is_locked_when_the_cast_starts() {
        let (mut world, room, _) = world_with_spell(2);
        let (caster, _rx) = caster_in(&mut world, room);
        let (bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        let c = world.get::<Casting>(caster).expect("cast queued");
        assert_eq!(c.target, CastTarget::InRoom(bob));
        assert_eq!(c.ticks_total, 80);
    }

    #[test]
    fn unknown_target_is_refused_before_the_chant_not_after() {
        let (mut world, room, _) = world_with_spell(2);
        let (caster, mut rx) = caster_in(&mut world, room);
        // Hostile spells refuse an unresolvable name outright.
        world
            .resource_mut::<AbilityCatalog>()
            .by_name
            .get_mut("mend")
            .unwrap()
            .violent = true;
        invoke_ability(
            &mut world,
            caster,
            "'mend' nobody",
            AbilityKind::Spell,
            "cast",
        );
        assert!(world.get::<Casting>(caster).is_none());
        assert!(drain(&mut rx).contains("You don't see 'nobody' here"));
    }

    #[test]
    fn landing_heals_the_locked_target() {
        let (mut world, room, _) = world_with_spell(1);
        let (caster, _rx) = caster_in(&mut world, room);
        let (bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        run_ticks(&mut world, 40);
        assert!(world.get::<Casting>(caster).is_none());
        assert!(hp(&world, bob) > 5, "the spell landed on Bob");
    }

    #[test]
    fn target_leaving_the_room_fizzles_the_cast() {
        let (mut world, room, elsewhere) = world_with_spell(1);
        let (caster, mut rx) = caster_in(&mut world, room);
        let (bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        drain(&mut rx);
        world.entity_mut(bob).insert(Located(elsewhere));
        run_ticks(&mut world, 1);
        assert!(world.get::<Casting>(caster).is_none());
        assert!(drain(&mut rx).contains("You stop chanting abruptly!"));
        run_ticks(&mut world, 60);
        assert_eq!(hp(&world, bob), 5, "nothing landed");
    }

    #[test]
    fn same_named_newcomer_never_receives_the_spell() {
        let (mut world, room, elsewhere) = world_with_spell(1);
        let (caster, _rx) = caster_in(&mut world, room);
        let (bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        // A second "Bob" walks in while the first is still there.
        let (other, _o) = bob_in(&mut world, room);
        run_ticks(&mut world, 40);
        assert!(hp(&world, bob) > 5, "the original was healed");
        assert_eq!(hp(&world, other), 5, "the newcomer was not");

        // And when the original leaves, the newcomer still isn't hit.
        world.entity_mut(bob).insert(Health { hp: 5, max: 50 });
        start_mend(&mut world, caster);
        assert_eq!(
            world.get::<Casting>(caster).unwrap().target,
            CastTarget::InRoom(bob),
            "first match is the original"
        );
        world.entity_mut(bob).insert(Located(elsewhere));
        run_ticks(&mut world, 40);
        assert_eq!(hp(&world, other), 5, "never re-resolved onto the newcomer");
        assert_eq!(hp(&world, bob), 5);
    }

    #[test]
    fn dead_target_aborts_and_the_slot_is_handed_back() {
        let (mut world, room, _) = world_with_spell(1);
        let (caster, mut rx) = caster_in(&mut world, room);
        let (bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        {
            let mut c = world.get_mut::<Casting>(caster).unwrap();
            c.slot_circle = Some(1);
        }
        world.entity_mut(caster).insert(SpellSlots {
            in_flight: vec![mud_world::SpellCooldown {
                circle: 1,
                secs_remaining: 9,
                total_secs: 9,
            }],
        });
        world.entity_mut(bob).insert(Health { hp: 0, max: 50 });
        drain(&mut rx);
        run_ticks(&mut world, 1);
        assert!(drain(&mut rx).contains("You stop chanting abruptly!"));
        assert!(
            world
                .get::<SpellSlots>(caster)
                .unwrap()
                .in_flight
                .is_empty()
        );
    }

    #[test]
    fn commands_are_refused_while_casting_except_the_allow_list() {
        let (mut world, room, _) = world_with_spell(2);
        let (caster, mut rx) = caster_in(&mut world, room);
        world.entity_mut(caster).insert(Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: mud_db::enums::UserRole::Player,
            account_role: mud_db::enums::UserRole::Player,
            perms: vec![],
        });
        let (_bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        drain(&mut rx);
        for line in ["north", "kill bob", "sit", "get all", "cast 'mend' bob"] {
            dispatch(&mut world, caster, line);
            let out = drain(&mut rx);
            assert!(
                out.contains("You are busy spellcasting..."),
                "{line:?} should be refused, got {out:?}"
            );
        }
        assert!(world.get::<Casting>(caster).is_some(), "still casting");
        for line in ["inventory", "equipment"] {
            dispatch(&mut world, caster, line);
            assert!(
                !drain(&mut rx).contains("busy spellcasting"),
                "{line:?} is allowed mid-cast"
            );
        }
        assert!(world.get::<Casting>(caster).is_some());
        dispatch(&mut world, caster, "abort");
        let out = drain(&mut rx);
        assert!(out.contains("You abort your spell!"), "{out:?}");
        assert!(world.get::<Casting>(caster).is_none());
    }

    #[test]
    fn allow_list_matches_legacy_exits() {
        for name in ["abort", "flee", "disengage", "look", "gossip", "tell"] {
            assert!(allowed_while_casting(name), "{name}");
        }
        for name in [
            "north", "kill", "cast", "chant", "get", "sit", "say", "quit",
        ] {
            assert!(!allowed_while_casting(name), "{name}");
        }
    }

    #[test]
    fn observers_hear_the_chant_and_the_finish() {
        let (mut world, room, _) = world_with_spell(1);
        let (caster, _rx) = caster_in(&mut world, room);
        let (_bob, _b) = bob_in(&mut world, room);
        let (watcher, mut wrx) = player_in(&mut world, room);
        world.entity_mut(watcher).insert(Named {
            name: "Watcher".to_string(),
        });
        start_mend(&mut world, caster);
        let out = drain(&mut wrx);
        assert!(out.contains("Tester starts casting"), "{out:?}");
        run_ticks(&mut world, 40);
        let out = drain(&mut wrx);
        assert!(out.contains("Tester completes their spell..."), "{out:?}");
        assert!(out.contains("utters the words"), "{out:?}");
    }

    #[test]
    fn sleeping_bystanders_hear_nothing() {
        let (mut world, room, _) = world_with_spell(1);
        let (caster, _rx) = caster_in(&mut world, room);
        let (_bob, _b) = bob_in(&mut world, room);
        let (sleeper, mut srx) = player_in(&mut world, room);
        world
            .entity_mut(sleeper)
            .insert(Posture(PostureKind::Sleeping));
        start_mend(&mut world, caster);
        assert!(drain(&mut srx).is_empty());
    }

    #[test]
    fn caster_sees_a_countdown() {
        let (mut world, room, _) = world_with_spell(2);
        let (caster, mut rx) = caster_in(&mut world, room);
        let (_bob, _b) = bob_in(&mut world, room);
        start_mend(&mut world, caster);
        drain(&mut rx);
        run_ticks(&mut world, 20);
        assert!(drain(&mut rx).contains("Casting: Mend"));
    }

    fn quick_chant_world(rounds: i32) -> (World, Entity) {
        let (mut world, room, _) = world_with_spell(rounds);
        let (caster, _rx) = caster_in(&mut world, room);
        world.entity_mut(caster).insert(KnownAbilities {
            entries: vec![(MEND, 500, true), (QUICK_CHANT, 800, true)],
        });
        (world, caster)
    }

    #[test]
    fn quick_chant_success_halves_the_wind_up() {
        let (world, caster) = quick_chant_world(4);
        let def = world.resource::<AbilityCatalog>().by_name["mend"].clone();
        // Roll 1 always beats skill 80 + stat bonuses.
        let (ticks, quick) = wind_up_ticks(&world, caster, &def, 1);
        assert!(quick);
        assert_eq!(ticks, 80, "4 rounds (160 ticks) halved");
    }

    #[test]
    fn quick_chant_failure_keeps_the_full_wind_up() {
        let (world, caster) = quick_chant_world(4);
        let def = world.resource::<AbilityCatalog>().by_name["mend"].clone();
        let (ticks, quick) = wind_up_ticks(&world, caster, &def, 110);
        assert!(!quick);
        assert_eq!(ticks, 160);
    }

    #[test]
    fn quick_chant_needs_the_skill() {
        let (mut world, room, _) = world_with_spell(4);
        let (caster, _rx) = caster_in(&mut world, room);
        let def = world.resource::<AbilityCatalog>().by_name["mend"].clone();
        let (ticks, quick) = wind_up_ticks(&world, caster, &def, 1);
        assert!(!quick);
        assert_eq!(ticks, 160);
    }

    #[test]
    fn quick_chant_trims_low_circle_spells_further() {
        // Utility spell, circle 1 for a caster whose top circle is 9:
        // 8 stars halved = 4, minus (9 - 1) / 2 = 0 -> floored at 1.
        assert_eq!(quick_chant_stars(8, 9, 1, false), 1);
        // Offensive spells lose one star per three circles instead.
        assert_eq!(quick_chant_stars(8, 9, 1, true), 2);
        // Same circle as the caster's top: only the halving.
        assert_eq!(quick_chant_stars(8, 9, 9, false), 4);
        // Long casts keep at least (half - 2) stars.
        assert_eq!(quick_chant_stars(12, 12, 1, true), 4);
    }

    #[test]
    fn quick_chant_roll_uses_skill_and_stats() {
        assert!(quick_chant_hit(50, 3, 2, 54));
        assert!(!quick_chant_hit(50, 3, 2, 55));
        // Negative stat bonuses never hurt.
        assert!(!quick_chant_hit(50, -4, -4, 50));
    }

    #[test]
    fn quick_chant_shortens_a_started_cast() {
        let (mut world, caster) = quick_chant_world(4);
        let room = world.get::<Located>(caster).unwrap().0;
        let (_bob, _b) = bob_in(&mut world, room);
        // Force the roll by maxing the skill: with prof 1000 (skill
        // 100) the 1..=110 die only misses on 101+.
        world.entity_mut(caster).insert(KnownAbilities {
            entries: vec![(MEND, 500, true), (QUICK_CHANT, 1000, true)],
        });
        let mut halved = 0;
        for _ in 0..40 {
            start_mend(&mut world, caster);
            let c = world.get::<Casting>(caster).unwrap();
            if c.ticks_total == 80 {
                halved += 1;
            } else {
                assert_eq!(c.ticks_total, 160);
            }
            world.entity_mut(caster).remove::<Casting>();
        }
        assert!(halved > 20, "quick chant fires most of the time: {halved}");
    }
}
