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
use mud_world::{AbilityCatalog, AbilityDef, Profile, RaceAbilitiesData};

use crate::commands::{invoke_ability, send_to};

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
    invoke_ability(world, player, &line, def.kind, verb);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, ability_def, drain, player_in};
    use mud_world::{Profile, Room};

    const SYLL: i32 = 1;
    const MISSILE: i32 = 2;
    const SLASH: i32 = 3;
    const BREATH: i32 = 4;

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
        for d in [syll, missile, slash, breath] {
            catalog.by_name.insert(d.plain_name.to_ascii_lowercase(), d);
        }
        world.insert_resource(catalog);
        world.insert_resource(mud_world::EffectCatalog::default());
        let mut race = RaceAbilitiesData::default();
        race.insert("ELF", SYLL, 100);
        race.insert("ELF", MISSILE, 100);
        race.insert("ELF", SLASH, 100);
        race.insert("DRAGONBORN_FIRE", BREATH, 100);
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
}
