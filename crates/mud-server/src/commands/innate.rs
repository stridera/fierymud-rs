//! Racial active abilities (issue #14): `innate <ability> [target]` and the
//! racial commands such as `breathe`.
//!
//! Which abilities a race can use is data: the `RaceAbilities` table, loaded
//! at boot into [`mud_world::RaceAbilitiesData`]. A row grants the ability
//! (passive rows, e.g. weapon proficiencies, are never "used"); nothing here
//! names a race. `innate <name>` resolves the name among the abilities the
//! caller's race grants and runs it exactly like the matching `cast` /
//! `chant` / `perform` / skill command, so wind-up, slots, cooldowns and
//! targeting stay data-driven too.

use bevy_ecs::prelude::{Entity, World};
use mud_db::abilities::AbilityKind;
use mud_world::targeting::{NameRank, rank_ability_name};
use mud_world::{
    AbilityCatalog, AbilityDef, Cooldowns, CoreStats, Profile, RaceAbilitiesData,
    innate_cooldown_key,
};

use crate::commands::{invoke_ability_innate, send_to};

/// The active (non-passive) abilities the caller's race grants, sorted by
/// name. Empty without a race or without loaded race data.
pub(crate) fn racial_actives(world: &World, player: Entity) -> Vec<AbilityDef> {
    let Some(race) = world.get::<Profile>(player).map(|p| p.race.clone()) else {
        return Vec::new();
    };
    let (Some(data), Some(catalog)) = (
        world.get_resource::<RaceAbilitiesData>(),
        world.get_resource::<AbilityCatalog>(),
    ) else {
        return Vec::new();
    };
    let mut out: Vec<AbilityDef> = catalog
        .by_name
        .values()
        .filter(|d| !d.passive && data.grants(&race, d.id))
        .cloned()
        .collect();
    out.sort_by_key(|d| d.plain_name.to_ascii_lowercase());
    out
}

/// Real seconds in one MUD hour (legacy `SECS_PER_MUD_HOUR`).
const SECS_PER_MUD_HOUR: u64 = 75;

/// Legacy `LVL_IMMORT`: characters above it ignore innate cooldowns.
const LVL_IMMORT: i32 = 100;

/// Legacy `stat_bonus[x].skill_small` (`constants.cpp`): -7 at 0 rising to
/// -1 at 44, zero through 59, then 1 at 60 rising to 5 at 100, each value
/// truncated toward zero from single-precision arithmetic as the legacy
/// table is built. A fixed legacy curve, not builder-tunable content.
#[must_use]
pub(crate) fn skill_small_bonus(stat: i32) -> i32 {
    let stat = stat.clamp(0, 100);
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let x = stat as f32;
    #[allow(clippy::cast_possible_truncation)]
    match stat {
        0..=44 => (6.0_f32 / 44.0 * x - 7.0) as i32,
        45..=59 => 0,
        _ => (1.0_f32 / 10.0 * x - 5.0) as i32,
    }
}

/// The caller's `stat` (`STR`..`CHA`, any case) score, if `stat` names one.
fn stat_score(world: &World, player: Entity, stat: &str) -> Option<i32> {
    let core = world.get::<CoreStats>(player)?;
    match stat.to_ascii_uppercase().as_str() {
        "STR" => Some(core.strength),
        "DEX" => Some(core.dexterity),
        "CON" => Some(core.constitution),
        "INT" => Some(core.intelligence),
        "WIS" => Some(core.wisdom),
        "CHA" => Some(core.charisma),
        _ => None,
    }
}

/// Real seconds the caller must wait after using `def` again, from their
/// race's `RaceAbilities` cooldown (`hours` minus the stat's small skill
/// bonus, in MUD hours). `None` when the row has no cooldown.
fn cooldown_secs(world: &World, player: Entity, def: &AbilityDef) -> Option<u64> {
    let race = world.get::<Profile>(player)?.race.clone();
    let cd = world
        .get_resource::<RaceAbilitiesData>()?
        .cooldown_of(&race, def.id)?;
    let bonus = cd
        .stat
        .as_deref()
        .and_then(|stat| stat_score(world, player, stat))
        .map_or(0, skill_small_bonus);
    let hours = u64::try_from(cd.hours.saturating_sub(bonus)).ok()?;
    (hours > 0).then(|| hours * SECS_PER_MUD_HOUR)
}

/// Whether the racial cooldown governs this invocation of `def`: always for
/// `innate <name>`, and for plain `cast` / `chant` / `perform` only when the
/// caster's class does not grant the ability (a race-only grant). Legacy
/// checks `CD_INNATE` only in `do_innate`, so a class spell slot is never
/// locked out by a racial row for the same spell.
pub(crate) fn cooldown_applies(
    world: &World,
    player: Entity,
    def: &AbilityDef,
    via_innate: bool,
) -> bool {
    if via_innate {
        return true;
    }
    let Some(class_id) = world.get::<Profile>(player).and_then(|p| p.class_id) else {
        return true;
    };
    let in_slots = world
        .get_resource::<mud_world::SpellSlotData>()
        .is_some_and(|d| d.ability_circle.contains_key(&(class_id, def.id)));
    let in_skills = world
        .get_resource::<mud_world::ClassSkillsData>()
        .is_some_and(|d| d.min_level_for(class_id, def.id).is_some());
    !(in_slots || in_skills)
}

/// The refusal while `def` is still on cooldown for the caller (legacy
/// `do_innate`), or `None` when it is ready.
pub(crate) fn cooldown_refusal(world: &World, player: Entity, def: &AbilityDef) -> Option<String> {
    if mud_world::effective_level(world, player) > LVL_IMMORT {
        return None;
    }
    let ready_at = *world
        .get::<Cooldowns>(player)?
        .ready_at
        .get(&innate_cooldown_key(def.id))?;
    let left = ready_at.checked_duration_since(std::time::Instant::now())?;
    // Legacy truncates; a cooldown still running never reads "0 seconds".
    let secs = left.as_secs().max(1);
    let unit = if secs == 1 { "second" } else { "seconds" };
    let phrase = world
        .get_resource::<RaceAbilitiesData>()
        .and_then(|d| {
            let race = &world.get::<Profile>(player)?.race;
            d.cooldown_of(race, def.id)?.phrase.clone()
        })
        .unwrap_or_else(|| "use that".to_string());
    Some(format!(
        "You're too tired right now.\r\nYou can {phrase} again in {secs} {unit}.\r\n"
    ))
}

/// Start the caller's cooldown on `def` (it just landed), if their race
/// gives it one.
pub(crate) fn start_cooldown(world: &mut World, player: Entity, def: &AbilityDef) {
    let Some(secs) = cooldown_secs(world, player, def) else {
        return;
    };
    if mud_world::effective_level(world, player) > LVL_IMMORT {
        return;
    }
    let ready_at = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut cd = world
        .get_mut::<Cooldowns>(player)
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default();
    cd.ready_at.insert(innate_cooldown_key(def.id), ready_at);
    crate::commands::try_insert(world, player, cd);
}

/// The caller's racial breath weapon (`BREATHE_*`), if their race has one.
pub(crate) fn racial_breath(world: &World, player: Entity) -> Option<AbilityDef> {
    racial_actives(world, player).into_iter().find(is_breath)
}

fn is_breath(def: &AbilityDef) -> bool {
    def.kind == AbilityKind::Skill && def.plain_name.to_ascii_lowercase().starts_with("breathe_")
}

/// How well the typed `needle` names `def`: its schema name (`INN_SYLL` also
/// answers to `syll`) or its display name (`Innate Sylvan`).
fn rank(needle: &str, def: &AbilityDef) -> NameRank {
    let plain = def.plain_name.to_ascii_lowercase();
    let short = plain.strip_prefix("inn_").unwrap_or(&plain);
    let display = crate::commands::render_color_tags(&def.name, crate::commands::ColorMode::Strip);
    [plain.as_str(), short, display.as_str()]
        .into_iter()
        .map(|candidate| rank_ability_name(needle, candidate))
        .min_by_key(|r| match r {
            NameRank::Exact => 0,
            NameRank::Prefix => 1,
            NameRank::None => 2,
        })
        .unwrap_or(NameRank::None)
}

/// Resolve a typed name among `actives`: an exact name wins, then the first
/// prefix match alphabetically (`actives` is sorted).
fn pick<'a>(needle: &str, actives: &'a [AbilityDef]) -> Option<&'a AbilityDef> {
    let ranked: Vec<(NameRank, &AbilityDef)> = actives
        .iter()
        .map(|d| (rank(needle, d), d))
        .filter(|(r, _)| *r != NameRank::None)
        .collect();
    ranked
        .iter()
        .find(|(r, _)| *r == NameRank::Exact)
        .or_else(|| ranked.first())
        .map(|(_, d)| *d)
}

/// `innate <ability> [target]`. The ability name may be quoted or bare; for a
/// bare name the longest run of leading words that names an innate wins and
/// the rest is the target (`innate faerie step bob`).
pub(crate) fn use_innate(world: &mut World, player: Entity, args: &str) {
    // The bare `innate` list is allowed mid-cast; using one is not.
    if world.get::<mud_world::Casting>(player).is_some() {
        send_to(world, player, "You are busy spellcasting...\r\n");
        return;
    }
    let actives = racial_actives(world, player);
    let (quoted, tail) = crate::commands::parse_quoted_first_token(args);
    let args = args.trim();
    let found = if args.starts_with(['\'', '"']) && !quoted.is_empty() {
        pick(&quoted, &actives).map(|d| (d.clone(), tail.unwrap_or("").to_string()))
    } else {
        let words: Vec<&str> = args.split_whitespace().collect();
        (1..=words.len()).rev().find_map(|n| {
            pick(&words[..n].join(" "), &actives).map(|d| (d.clone(), words[n..].join(" ")))
        })
    };
    let Some((def, target)) = found else {
        send_to(
            world,
            player,
            format!(
                "You have no innate ability called '{}'. Type 'innate' to list them.\r\n",
                args.trim_matches(['\'', '"'])
            ),
        );
        return;
    };
    if is_breath(&def) {
        super::combat_commands::breathe_with(world, player, &def, &target);
        return;
    }
    let verb = match def.kind {
        AbilityKind::Spell => "cast",
        AbilityKind::Chant => "chant",
        AbilityKind::Song => "perform",
        AbilityKind::Skill => "use",
    };
    let name = def.plain_name.to_ascii_lowercase();
    let line = if target.is_empty() {
        format!("'{name}'")
    } else {
        format!("'{name}' {target}")
    };
    invoke_ability_innate(world, player, &line, def.kind, verb);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::invoke_ability;
    use crate::commands::test_support::{Rx, ability_def, drain, player_in};
    use mud_world::CoreStats;
    use mud_world::{Profile, Room};

    const SYLL: i32 = 1;
    const MISSILE: i32 = 2;
    const SLASH: i32 = 3;
    const BREATH: i32 = 4;
    const DOOR: i32 = 5;
    const BARK: i32 = 6;

    fn world_with_races() -> (World, Entity, Entity, Rx) {
        let mut world = World::new();
        let mut catalog = AbilityCatalog::default();
        let mut syll = ability_def(SYLL, "Innate Sylvan", AbilityKind::Spell);
        syll.plain_name = "INN_SYLL".into();
        let mut slash = ability_def(SLASH, "Slashing Weapons", AbilityKind::Skill);
        slash.plain_name = "SLASHING".into();
        slash.passive = true;
        let mut breath = ability_def(BREATH, "Breathe Fire", AbilityKind::Skill);
        breath.plain_name = "BREATHE_FIRE".into();
        let mut missile = ability_def(MISSILE, "Magic Missile", AbilityKind::Spell);
        missile.plain_name = "MAGIC_MISSILE".into();
        let mut door = ability_def(DOOR, "Dimension Door", AbilityKind::Spell);
        door.plain_name = "DIMENSION_DOOR".into();
        let mut bark = ability_def(BARK, "Barkskin", AbilityKind::Spell);
        bark.plain_name = "BARKSKIN".into();
        for d in [syll, missile, slash, breath, door, bark] {
            catalog.by_name.insert(d.plain_name.to_ascii_lowercase(), d);
        }
        world.insert_resource(catalog);
        world.insert_resource(mud_world::EffectCatalog::default());
        let mut race = RaceAbilitiesData::default();
        race.insert("ELF", SYLL, 100);
        race.insert("ELF", MISSILE, 100);
        race.insert("ELF", SLASH, 100);
        race.insert("DRAGONBORN_FIRE", BREATH, 100);
        race.insert("FAERIE_SEELIE", DOOR, 100);
        race.insert("ARBOREAN", BARK, 100);
        let cd = |hours, stat: Option<&str>, phrase: &str| mud_world::InnateCooldown {
            hours,
            stat: stat.map(str::to_string),
            phrase: Some(phrase.to_string()),
        };
        race.set_cooldown("ELF", SYLL, cd(7, None, "improve your grace"));
        race.set_cooldown("FAERIE_SEELIE", DOOR, cd(7, None, "traverse the Reverie"));
        race.set_cooldown("ARBOREAN", BARK, cd(20, Some("CON"), "armor yourself"));
        world.insert_resource(race);
        let room = world.spawn(Room).id();
        let (elf, rx) = player_in(&mut world, room);
        world.entity_mut(elf).insert(profile("ELF"));
        let (human, _rx) = player_in(&mut world, room);
        world.entity_mut(human).insert(profile("HUMAN"));
        (world, elf, human, rx)
    }

    fn profile(race: &str) -> Profile {
        Profile {
            level: 10,
            class_id: None,
            race: race.into(),
            experience: 0,
            gender: "neutral".into(),
        }
    }

    fn names(world: &World, e: Entity) -> Vec<String> {
        racial_actives(world, e)
            .into_iter()
            .map(|d| d.plain_name)
            .collect()
    }

    #[test]
    fn actives_come_from_the_race_rows_and_skip_passives() {
        let (world, elf, human, _rx) = world_with_races();
        assert_eq!(names(&world, elf), vec!["INN_SYLL", "MAGIC_MISSILE"]);
        assert!(names(&world, human).is_empty());
    }

    #[test]
    fn names_resolve_by_schema_name_display_name_and_prefix() {
        let (world, elf, _human, _rx) = world_with_races();
        let actives = racial_actives(&world, elf);
        for typed in ["syll", "inn_syll", "innate sylvan", "inn", "sylvan"] {
            let want = if typed == "sylvan" { None } else { Some(SYLL) };
            assert_eq!(pick(typed, &actives).map(|d| d.id), want, "{typed}");
        }
        assert_eq!(pick("magic missile", &actives).map(|d| d.id), Some(MISSILE));
        assert_eq!(pick("mag", &actives).map(|d| d.id), Some(MISSILE));
        assert!(
            pick("slashing", &actives).is_none(),
            "passives are not used"
        );
        assert!(pick("zzz", &actives).is_none());
    }

    #[test]
    fn innate_command_refuses_abilities_the_race_does_not_grant() {
        let (mut world, elf, _human, mut rx) = world_with_races();
        use_innate(&mut world, elf, "breathe fire");
        let out = drain(&mut rx);
        assert!(
            out.contains("no innate ability called 'breathe fire'"),
            "{out}"
        );
        use_innate(&mut world, elf, "'nonsense'");
        assert!(drain(&mut rx).contains("no innate ability called 'nonsense'"));
    }

    #[test]
    fn dragonborn_breath_is_found_by_data_not_by_race_name() {
        let (mut world, _elf, human, _rx) = world_with_races();
        assert!(racial_breath(&world, human).is_none());
        world.entity_mut(human).insert(profile("DRAGONBORN_FIRE"));
        assert_eq!(
            racial_breath(&world, human).map(|d| d.id),
            Some(BREATH),
            "race rows decide"
        );
    }

    #[test]
    fn innate_spell_starts_the_same_cast_as_the_cast_command() {
        let (mut world, elf, human, mut rx) = world_with_races();
        world.insert_resource(mud_world::SpellSlotData::default());
        world.insert_resource(mud_world::ClassSkillsData::default());
        world.entity_mut(elf).insert((
            mud_world::Health { hp: 50, max: 50 },
            mud_world::KnownAbilities {
                entries: vec![(SYLL, 1000, true)],
            },
        ));
        world
            .resource_mut::<AbilityCatalog>()
            .by_name
            .get_mut("inn_syll")
            .unwrap()
            .cast_time_rounds = 1;
        use_innate(&mut world, elf, "syll");
        assert!(
            world.get::<mud_world::Casting>(elf).is_some(),
            "winding up: {}",
            drain(&mut rx)
        );
        // A human who somehow knows the spell still cannot use it as an innate.
        world.entity_mut(human).insert(mud_world::KnownAbilities {
            entries: vec![(SYLL, 1000, true)],
        });
        use_innate(&mut world, human, "syll");
        assert!(world.get::<mud_world::Casting>(human).is_none());
    }

    #[test]
    fn breathe_follows_the_race_rows() {
        use crate::commands::combat_commands::cmd_breathe;
        let (mut world, elf, human, mut rx) = world_with_races();
        cmd_breathe(&mut world, elf, "");
        assert!(drain(&mut rx).contains("You have no breath weapon."));
        let _ = human;
        let (mut world2, _e, human2, mut rx2) = world_with_races();
        world2.entity_mut(human2).insert((
            profile("DRAGONBORN_FIRE"),
            mud_world::Stamina {
                current: 50,
                max: 50,
            },
        ));
        cmd_breathe(&mut world2, human2, "");
        let out = drain(&mut rx2);
        assert!(!out.contains("no breath weapon"), "{out}");
        assert!(
            world2.get::<mud_world::Stamina>(human2).unwrap().current < 50,
            "breathing costs stamina: {out}"
        );
    }

    fn ready_in(world: &mut World, e: Entity, ability: i32, secs: u64) {
        let mut cd = world
            .get_mut::<Cooldowns>(e)
            .map(|mut c| std::mem::take(&mut *c));
        let mut cd = cd.take().unwrap_or_default();
        cd.ready_at.insert(
            innate_cooldown_key(ability),
            // Half a second over, so the truncated readout is exactly `secs`.
            std::time::Instant::now() + std::time::Duration::from_millis(secs * 1000 + 500),
        );
        world.entity_mut(e).insert(cd);
    }

    fn knows(world: &mut World, e: Entity, ability: i32) {
        world.insert_resource(mud_world::SpellSlotData::default());
        world.insert_resource(mud_world::ClassSkillsData::default());
        world.entity_mut(e).insert((
            mud_world::Health { hp: 50, max: 50 },
            mud_world::KnownAbilities {
                entries: vec![(ability, 1000, true)],
            },
        ));
    }

    #[test]
    fn innate_on_cooldown_is_refused_with_the_legacy_message() {
        let (mut world, elf, _human, mut rx) = world_with_races();
        knows(&mut world, elf, SYLL);
        ready_in(&mut world, elf, SYLL, 100);
        use_innate(&mut world, elf, "syll");
        let out = drain(&mut rx);
        assert!(
            out.contains("You're too tired right now.\r\nYou can improve your grace again in 100 seconds.\r\n"),
            "{out}"
        );
        assert!(world.get::<mud_world::Casting>(elf).is_none());
    }

    #[test]
    fn innate_is_usable_again_after_the_cooldown_expires() {
        let (mut world, elf, _human, mut rx) = world_with_races();
        knows(&mut world, elf, SYLL);
        let past = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();
        world.entity_mut(elf).insert(Cooldowns {
            ready_at: [(innate_cooldown_key(SYLL), past)].into(),
        });
        use_innate(&mut world, elf, "syll");
        let out = drain(&mut rx);
        assert!(!out.contains("too tired"), "{out}");
        assert!(world.get::<mud_world::Casting>(elf).is_some(), "{out}");
    }

    #[test]
    fn plain_cast_of_a_race_granted_ability_obeys_the_same_cooldown() {
        let (mut world, elf, _human, mut rx) = world_with_races();
        let room = world.get::<mud_world::Located>(elf).unwrap().0;
        let (faerie, mut frx) = player_in(&mut world, room);
        world.entity_mut(faerie).insert(profile("FAERIE_SEELIE"));
        knows(&mut world, faerie, DOOR);
        ready_in(&mut world, faerie, DOOR, 300);
        invoke_ability(
            &mut world,
            faerie,
            "'dimension door'",
            AbilityKind::Spell,
            "cast",
        );
        let out = drain(&mut frx);
        assert!(
            out.contains("You can traverse the Reverie again in 300 seconds."),
            "{out}"
        );
        assert!(world.get::<mud_world::Casting>(faerie).is_none());
        // Off cooldown the very same cast begins.
        world.entity_mut(faerie).insert(Cooldowns::default());
        invoke_ability(
            &mut world,
            faerie,
            "'dimension door'",
            AbilityKind::Spell,
            "cast",
        );
        let out = drain(&mut frx);
        assert!(!out.contains("too tired"), "{out}");
        assert!(world.get::<mud_world::Casting>(faerie).is_some(), "{out}");
        let _ = drain(&mut rx);
    }

    #[test]
    fn the_cooldown_belongs_to_the_race_grant_not_to_the_ability() {
        // A human casting the same spell is not limited by the elf's row.
        let (mut world, _elf, human, mut rx) = world_with_races();
        knows(&mut world, human, SYLL);
        ready_in(&mut world, human, SYLL, 100);
        invoke_ability(&mut world, human, "'inn_syll'", AbilityKind::Spell, "cast");
        assert!(!drain(&mut rx).contains("too tired"));
    }

    #[test]
    fn cooldown_length_is_mud_hours_minus_the_stat_bonus() {
        let (mut world, elf, human, _rx) = world_with_races();
        let def = |w: &World, name: &str| w.resource::<AbilityCatalog>().by_name[name].clone();
        let sylldef = def(&world, "inn_syll");
        assert_eq!(cooldown_secs(&world, elf, &sylldef), Some(7 * 75));
        assert_eq!(
            cooldown_secs(&world, human, &sylldef),
            None,
            "no row, no cooldown"
        );
        let bark = def(&world, "barkskin");
        world.entity_mut(human).insert(profile("ARBOREAN"));
        for (con, hours) in [(50, 20), (60, 19), (80, 17), (100, 15), (20, 24)] {
            world.entity_mut(human).insert(CoreStats {
                strength: 50,
                dexterity: 50,
                constitution: con,
                intelligence: 50,
                wisdom: 50,
                charisma: 50,
            });
            assert_eq!(
                cooldown_secs(&world, human, &bark),
                Some(hours * 75),
                "CON {con}"
            );
        }
    }

    #[test]
    fn landing_starts_the_cooldown_and_gods_ignore_it() {
        let (mut world, elf, _human, _rx) = world_with_races();
        let def = world.resource::<AbilityCatalog>().by_name["inn_syll"].clone();
        start_cooldown(&mut world, elf, &def);
        let ready = world.get::<Cooldowns>(elf).unwrap().ready_at[&innate_cooldown_key(SYLL)];
        let left = ready
            .saturating_duration_since(std::time::Instant::now())
            .as_secs();
        assert!((7 * 75 - 2..=7 * 75).contains(&left), "{left}");
        assert!(
            !world
                .get::<Cooldowns>(elf)
                .unwrap()
                .ready_at
                .contains_key(&SYLL),
            "the spell's own cooldown entry is untouched"
        );
        assert!(cooldown_refusal(&world, elf, &def).is_some());
        world.entity_mut(elf).get_mut::<Profile>().unwrap().level = 101;
        assert!(cooldown_refusal(&world, elf, &def).is_none());
    }

    #[test]
    fn skill_small_bonus_follows_the_legacy_table() {
        assert_eq!(skill_small_bonus(0), -7);
        assert_eq!(skill_small_bonus(44), -1);
        for x in 45..=59 {
            assert_eq!(skill_small_bonus(x), 0, "{x}");
        }
        assert_eq!(skill_small_bonus(60), 1);
        assert_eq!(skill_small_bonus(80), 3);
        assert_eq!(skill_small_bonus(100), 5);
        assert_eq!(skill_small_bonus(250), 5, "clamped");
    }

    const INVIS: i32 = 7;
    const INVIS_EFFECT: i32 = 70;
    const SORCERER: i32 = 11;
    const WARRIOR: i32 = 12;

    /// `Invisible` (a no-wind-up status self spell) that DUERGAR grants as
    /// a racial with a 9 MUD hour cooldown, and that the sorcerer class
    /// also has in its own spell slots.
    fn invisible_fixture(world: &mut World) {
        let mut def = ability_def(INVIS, "Invisible", AbilityKind::Spell);
        def.cast_time_rounds = 0;
        let mut catalog = world.resource_mut::<AbilityCatalog>();
        catalog.by_name.insert("invisible".into(), def);
        catalog.effects_for.insert(
            INVIS,
            vec![(
                INVIS_EFFECT,
                Some(serde_json::json!({"flag": "invisible", "duration": "60"})),
            )],
        );
        world
            .resource_mut::<mud_world::EffectCatalog>()
            .by_id
            .insert(
                INVIS_EFFECT,
                mud_world::EffectDef {
                    id: INVIS_EFFECT,
                    name: "status".into(),
                    description: None,
                    effect_type: "status".into(),
                    tags: vec![],
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
        let mut race = world.resource_mut::<RaceAbilitiesData>();
        race.insert("DUERGAR", INVIS, 100);
        race.set_cooldown(
            "DUERGAR",
            INVIS,
            mud_world::InnateCooldown {
                hours: 9,
                stat: None,
                phrase: Some("turn invisible".to_string()),
            },
        );
        let mut slots = mud_world::SpellSlotData::default();
        slots.ability_circle.insert((SORCERER, INVIS), 1);
        slots.progression.insert((10, 1), 9);
        world.insert_resource(slots);
        world.insert_resource(mud_world::ClassSkillsData::default());
    }

    fn caster(world: &mut World, room: Entity, race: &str, class: i32) -> (Entity, Rx) {
        let (e, rx) = player_in(world, room);
        let mut p = profile(race);
        p.class_id = Some(class);
        world.entity_mut(e).insert((
            p,
            mud_world::Health { hp: 50, max: 50 },
            mud_world::KnownAbilities {
                entries: vec![(INVIS, 1000, true)],
            },
        ));
        (e, rx)
    }

    fn has_cd(world: &World, e: Entity) -> bool {
        world
            .get::<Cooldowns>(e)
            .is_some_and(|c| c.ready_at.contains_key(&innate_cooldown_key(INVIS)))
    }

    #[test]
    fn a_class_granted_spell_never_touches_the_racial_cooldown() {
        let (mut world, _elf, _human, _rx) = world_with_races();
        invisible_fixture(&mut world);
        let room = world.spawn(Room).id();
        let (duergar, mut rx) = caster(&mut world, room, "DUERGAR", SORCERER);
        for _ in 0..2 {
            invoke_ability(&mut world, duergar, "invisible", AbilityKind::Spell, "cast");
            let out = drain(&mut rx);
            assert!(!out.contains("too tired"), "{out}");
        }
        assert!(
            !has_cd(&world, duergar),
            "class cast starts no racial cooldown"
        );
    }

    #[test]
    fn the_same_spell_through_innate_starts_the_cooldown() {
        let (mut world, _elf, _human, _rx) = world_with_races();
        invisible_fixture(&mut world);
        let room = world.spawn(Room).id();
        let (duergar, mut rx) = caster(&mut world, room, "DUERGAR", SORCERER);
        use_innate(&mut world, duergar, "invisible");
        let out = drain(&mut rx);
        assert!(has_cd(&world, duergar), "{out}");
        use_innate(&mut world, duergar, "invisible");
        let out = drain(&mut rx);
        assert!(out.contains("You can turn invisible again in"), "{out}");
    }

    #[test]
    fn a_race_only_grant_is_gated_through_plain_cast_too() {
        let (mut world, _elf, _human, _rx) = world_with_races();
        invisible_fixture(&mut world);
        let room = world.spawn(Room).id();
        // A Duergar warrior: the class has no row for the spell.
        let (warrior, mut rx) = caster(&mut world, room, "DUERGAR", WARRIOR);
        invoke_ability(&mut world, warrior, "invisible", AbilityKind::Spell, "cast");
        let out = drain(&mut rx);
        assert!(has_cd(&world, warrior), "{out}");
        invoke_ability(&mut world, warrior, "invisible", AbilityKind::Spell, "cast");
        let out = drain(&mut rx);
        assert!(out.contains("You can turn invisible again in"), "{out}");
    }

    #[test]
    fn the_refusal_truncates_seconds_but_never_reads_zero() {
        let (mut world, elf, _human, _rx) = world_with_races();
        let def = world.resource::<AbilityCatalog>().by_name["inn_syll"].clone();
        for (millis, shown) in [
            (100_400, "100 seconds"),
            (1_900, "1 second."),
            (300, "1 second."),
        ] {
            world.entity_mut(elf).insert(Cooldowns {
                ready_at: [(
                    innate_cooldown_key(SYLL),
                    std::time::Instant::now() + std::time::Duration::from_millis(millis),
                )]
                .into(),
            });
            let out = cooldown_refusal(&world, elf, &def).unwrap();
            assert!(out.contains(shown), "{millis}ms: {out}");
        }
    }
}
