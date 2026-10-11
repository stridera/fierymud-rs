//! A violent spell starts the fight even when it does no damage (curses,
//! dispels, holds), judged on the victim's state after the spell ran.

use super::*;
use crate::commands::invoke_ability_with;
use crate::commands::test_support::{Rx, ability_def, drain, player_in};
use mud_db::abilities::AbilityKind;
use mud_world::{
    AbilityCatalog, AppliedTo, EffectCatalog, EffectDef, EffectInstance, EffectSource, Keywords,
    KnownAbilities, Mob, Named, SpellSlotData, Stunned,
};

const RAY: i32 = 1;
const DISPEL: i32 = 2;
const HOLD: i32 = 3;
const BUFF: i32 = 4;
const MODIFY: i32 = 7;
const DISPEL_FX: i32 = 8;

fn effect_def(id: i32, name: &str) -> EffectDef {
    EffectDef {
        id,
        name: name.to_string(),
        description: None,
        effect_type: name.to_string(),
        tags: if name == "dispel" {
            vec!["magic".to_string()]
        } else {
            Vec::new()
        },
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn spell(catalog: &mut AbilityCatalog, id: i32, plain: &str, violent: bool, fx: i32) {
    let mut def = ability_def(id, plain, AbilityKind::Spell);
    def.violent = violent;
    def.cast_time_rounds = 0;
    catalog.by_name.insert(plain.to_ascii_lowercase(), def);
    catalog.effects_for.insert(
        id,
        vec![(
            fx,
            Some(serde_json::json!({
                "target": "str", "amount": "-5", "duration": 10, "filter": "magic", "scope": "all"
            })),
        )],
    );
}

fn world_with_spells() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    let room = world.spawn_empty().id();
    let mut catalog = AbilityCatalog::default();
    spell(&mut catalog, RAY, "ray", true, MODIFY);
    spell(&mut catalog, DISPEL, "dispel", true, DISPEL_FX);
    spell(&mut catalog, HOLD, "hold", true, MODIFY);
    spell(&mut catalog, BUFF, "buff", false, MODIFY);
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(MODIFY, effect_def(MODIFY, "modify"));
    effects
        .by_id
        .insert(DISPEL_FX, effect_def(DISPEL_FX, "dispel"));
    world.insert_resource(effects);
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert((
        Health { hp: 100, max: 100 },
        KnownAbilities {
            entries: [RAY, DISPEL, HOLD, BUFF]
                .iter()
                .map(|id| (*id, 500, true))
                .collect(),
        },
    ));
    (world, room, caster, rx)
}

fn goblin(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Keywords(vec!["goblin".into()]),
            Located(room),
            Health { hp: 50, max: 50 },
        ))
        .id()
}

fn cast(world: &mut World, caster: Entity, spell: &str) {
    invoke_ability_with(
        world,
        caster,
        &format!("'{spell}' goblin"),
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
}

fn fighting(world: &World, e: Entity) -> Option<Entity> {
    world.get::<Fighting>(e).map(|f| f.0)
}

#[test]
fn a_non_damaging_violent_spell_starts_combat_both_ways() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    cast(&mut world, caster, "ray");
    assert_eq!(
        fighting(&world, mob),
        Some(caster),
        "the goblin turns on you"
    );
    assert_eq!(fighting(&world, caster), Some(mob));
}

#[test]
fn a_non_violent_spell_leaves_the_target_calm() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    cast(&mut world, caster, "buff");
    assert_eq!(fighting(&world, mob), None);
    assert_eq!(fighting(&world, caster), None);
}

#[test]
fn a_victim_already_held_stays_out_of_the_fight() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    world.entity_mut(mob).insert(Stunned);
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, mob), None);
}

#[test]
fn dispel_that_lifts_a_paralysis_makes_the_victim_engage_at_once() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    world.entity_mut(mob).insert(Stunned);
    world.spawn((
        EffectInstance {
            kind: MODIFY,
            name: "paralyzed".to_string(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: Some(HOLD),
        },
        AppliedTo(mob),
    ));
    cast(&mut world, caster, "dispel");
    assert!(world.get::<Stunned>(mob).is_none(), "the hold is lifted");
    assert_eq!(fighting(&world, mob), Some(caster));
}

#[test]
fn a_caster_already_fighting_keeps_their_opponent() {
    let (mut world, room, caster, _rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    let other = world
        .spawn((
            Mob,
            Named {
                name: "an orc".into(),
            },
            Located(room),
            Health { hp: 50, max: 50 },
        ))
        .id();
    world.entity_mut(caster).insert(Fighting(other));
    world.entity_mut(other).insert(Fighting(caster));
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, caster), Some(other), "opponent unchanged");
    assert_eq!(fighting(&world, mob), Some(caster), "the goblin joins in");
}

#[test]
fn a_charmed_or_sleeping_victim_is_not_dragged_into_a_fight() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    let mob = goblin(&mut world, room);
    world
        .entity_mut(mob)
        .insert(mud_world::Posture(mud_world::PostureKind::Sleeping));
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, mob), None);
    drain(&mut rx);
    world
        .entity_mut(mob)
        .insert(mud_world::Posture(mud_world::PostureKind::Standing));
    world.spawn((
        EffectInstance {
            kind: MODIFY,
            name: "charmed".to_string(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(mob),
    ));
    cast(&mut world, caster, "ray");
    assert_eq!(fighting(&world, mob), None);
}

/// Fly cast on someone else read as if the caster were the one rising:
/// the caster line and the room line must name the target.
fn fly_messages() -> mud_world::AbilityMessageSet {
    mud_world::AbilityMessageSet {
        success_to_caster: Some("{target.name} rises into the air and begins to fly.".into()),
        success_to_victim: Some("You rise into the air and begin to fly.".into()),
        success_to_room: Some("{target.name} rises into the air and begins to fly.".into()),
        success_to_self: Some("You rise into the air and begin to fly.".into()),
        success_self_room: Some("{actor.name} rises into the air and begins to fly.".into()),
        ..Default::default()
    }
}

#[test]
fn a_buff_cast_on_another_names_the_target_to_the_caster_and_the_room() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    world
        .resource_mut::<AbilityCatalog>()
        .messages
        .insert(BUFF, fly_messages());
    let (watcher, mut wrx) = player_in(&mut world, room);
    world.entity_mut(watcher).insert(Named {
        name: "Watcher".into(),
    });
    goblin(&mut world, room);
    drain(&mut rx);
    cast(&mut world, caster, "buff");
    let caster_out = drain(&mut rx);
    assert!(
        caster_out.contains("A goblin rises into the air and begins to fly."),
        "{caster_out}"
    );
    assert!(
        !caster_out.contains("You rise into the air"),
        "{caster_out}"
    );
    let room_out = drain(&mut wrx);
    assert!(
        room_out.contains("A goblin rises into the air and begins to fly."),
        "{room_out}"
    );
    assert!(!room_out.contains("Tester rises"), "{room_out}");
}

#[test]
fn a_buff_cast_on_yourself_keeps_the_first_person_lines() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    world
        .resource_mut::<AbilityCatalog>()
        .messages
        .insert(BUFF, fly_messages());
    let (_watcher, mut wrx) = player_in(&mut world, room);
    drain(&mut rx);
    invoke_ability_with(
        &mut world,
        caster,
        "'buff' me",
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
    assert!(drain(&mut rx).contains("You rise into the air and begin to fly."));
    assert!(drain(&mut wrx).contains("Tester rises into the air and begins to fly."));
}

const WORD_OF_COMMAND: i32 = 5;
const WORD_OF_RECALL: i32 = 6;

/// `word` prefixes both Word of Command (violent, sorts first) and Word of
/// Recall; the caster knows both, as an immortal does.
fn world_with_word_spells() -> (World, Entity, Entity, Rx) {
    let (mut world, room, caster, rx) = world_with_spells();
    {
        let mut catalog = world.resource_mut::<AbilityCatalog>();
        spell(
            &mut catalog,
            WORD_OF_COMMAND,
            "word_of_command",
            true,
            MODIFY,
        );
        spell(
            &mut catalog,
            WORD_OF_RECALL,
            "word_of_recall",
            false,
            MODIFY,
        );
    }
    world
        .get_mut::<KnownAbilities>(caster)
        .unwrap()
        .entries
        .extend([(WORD_OF_COMMAND, 500, true), (WORD_OF_RECALL, 500, true)]);
    world.entity_mut(room).insert(mud_world::PeacefulRoom);
    (world, room, caster, rx)
}

fn cast_args(world: &mut World, caster: Entity, args: &str) {
    crate::commands::reset_cast_landed();
    invoke_ability_with(
        world,
        caster,
        args,
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
}

#[test]
fn unquoted_word_of_recall_works_in_a_peaceful_room() {
    let (mut world, _room, caster, mut rx) = world_with_word_spells();
    drain(&mut rx);
    cast_args(&mut world, caster, "word of recall");
    let out = drain(&mut rx);
    assert!(!out.contains("peaceful aura"), "{out}");
    assert!(crate::commands::cast_landed(), "recall landed: {out}");
}

#[test]
fn quoted_and_abbreviated_word_of_recall_work_in_a_peaceful_room() {
    let (mut world, _room, caster, mut rx) = world_with_word_spells();
    for args in ["'word of recall'", "word of rec", "'word of recall' me"] {
        drain(&mut rx);
        cast_args(&mut world, caster, args);
        let out = drain(&mut rx);
        assert!(!out.contains("peaceful aura"), "{args}: {out}");
        assert!(crate::commands::cast_landed(), "{args}: {out}");
    }
}

#[test]
fn word_of_command_is_still_refused_in_a_peaceful_room() {
    let (mut world, room, caster, mut rx) = world_with_word_spells();
    goblin(&mut world, room);
    drain(&mut rx);
    cast_args(&mut world, caster, "word of command goblin");
    let out = drain(&mut rx);
    assert!(out.contains("peaceful aura"), "{out}");
    assert!(!crate::commands::cast_landed());
}

/// The real catalog names the unquoted-cast parser has to tell apart: a
/// short spell that is also the first word of longer ones.
fn parse_catalog() -> AbilityCatalog {
    let mut catalog = AbilityCatalog::default();
    for (id, name) in [
        (20, "summon"),
        (21, "summon_demon"),
        (22, "summon_elemental"),
        (23, "armor"),
        (24, "armor_of_gaia"),
        (25, "heal"),
        (26, "word_of_recall"),
        (27, "word_of_command"),
        (28, "magic_missile"),
        (29, "color_spray"),
        (30, "cone_of_cold"),
    ] {
        catalog
            .by_name
            .insert(name.to_string(), ability_def(id, name, AbilityKind::Spell));
    }
    catalog
}

/// `(ability plain name, target words)` for unquoted `args`, as the cast
/// parser resolves them; a miss falls back to the first word alone.
fn parse(
    catalog: &AbilityCatalog,
    known: Option<&KnownAbilities>,
    args: &str,
) -> (String, Option<String>) {
    let (first, rest) = parse_quoted_first_token(args);
    if let Some((def, tail, _)) =
        super::resolve_unquoted_ability(catalog, known, AbilityKind::Spell, args)
    {
        return (def.plain_name, tail.map(str::to_string));
    }
    let def = catalog
        .find_by_prefix(&first, Some(AbilityKind::Spell), known)
        .map_or_else(|| "NONE".to_string(), |d| d.plain_name.clone());
    (def, rest.map(str::to_string))
}

fn t(s: &str) -> String {
    s.to_string()
}

#[test]
fn an_exact_first_word_beats_a_longer_prefix_that_swallows_the_target() {
    let catalog = parse_catalog();
    // `summon d` must not become Summon Demon; `armor o` not Armor of Gaia.
    assert_eq!(
        parse(&catalog, None, "summon d"),
        ("SUMMON".into(), Some(t("d")))
    );
    assert_eq!(
        parse(&catalog, None, "summon e"),
        ("SUMMON".into(), Some(t("e")))
    );
    assert_eq!(
        parse(&catalog, None, "summon co"),
        ("SUMMON".into(), Some(t("co")))
    );
    assert_eq!(
        parse(&catalog, None, "armor o"),
        ("ARMOR".into(), Some(t("o")))
    );
    assert_eq!(
        parse(&catalog, None, "summon dracos extra"),
        ("SUMMON".into(), Some(t("dracos extra")))
    );
    // The same word as ability and target.
    assert_eq!(
        parse(&catalog, None, "heal heal"),
        ("HEAL".into(), Some(t("heal")))
    );
    assert_eq!(
        parse(&catalog, None, "HEAL Heal"),
        ("HEAL".into(), Some(t("Heal")))
    );
}

#[test]
fn an_exact_multiword_name_wins_over_its_first_word() {
    let catalog = parse_catalog();
    assert_eq!(
        parse(&catalog, None, "word of recall"),
        ("WORD_OF_RECALL".into(), None)
    );
    assert_eq!(
        parse(&catalog, None, "word of command goblin"),
        ("WORD_OF_COMMAND".into(), Some(t("goblin")))
    );
    assert_eq!(
        parse(&catalog, None, "summon demon bob"),
        ("SUMMON_DEMON".into(), Some(t("bob")))
    );
    assert_eq!(
        parse(&catalog, None, "armor of gaia me"),
        ("ARMOR_OF_GAIA".into(), Some(t("me")))
    );
    assert_eq!(
        parse(&catalog, None, "Magic Missile goblin"),
        ("MAGIC_MISSILE".into(), Some(t("goblin")))
    );
}

#[test]
fn abbreviated_multiword_names_still_resolve_by_prefix() {
    let catalog = parse_catalog();
    // `word` is not an exact ability, so the prefix run applies.
    assert_eq!(
        parse(&catalog, None, "word of rec"),
        ("WORD_OF_RECALL".into(), None)
    );
    assert_eq!(
        parse(&catalog, None, "c s goblin"),
        ("COLOR_SPRAY".into(), Some(t("goblin")))
    );
}

/// `cast word recall`: "word recall" is not a name, `word` alone is not
/// exact, and `recall` is not a word of "word of recall", so the words are
/// not joined. The first word resolves by prefix (the caster's known spell
/// first) and `recall` is the target - the spell is not guessed from the
/// second word.
#[test]
fn word_recall_without_of_resolves_the_first_word_with_known_first() {
    let catalog = parse_catalog();
    let known = KnownAbilities {
        entries: vec![(26, 500, true)],
    };
    assert_eq!(
        parse(&catalog, Some(&known), "word recall"),
        ("WORD_OF_RECALL".into(), Some(t("recall")))
    );
    // Without knowing either, alphabetical order decides.
    assert_eq!(
        parse(&catalog, None, "word recall"),
        ("WORD_OF_COMMAND".into(), Some(t("recall")))
    );
}

#[test]
fn cast_summon_d_targets_dracos_not_summon_demon() {
    let (mut world, room, caster, mut rx) = world_with_spells();
    {
        let mut catalog = world.resource_mut::<AbilityCatalog>();
        spell(&mut catalog, 20, "summon", false, MODIFY);
        spell(&mut catalog, 21, "summon_demon", false, MODIFY);
    }
    world
        .get_mut::<KnownAbilities>(caster)
        .unwrap()
        .entries
        .extend([(20, 500, true), (21, 500, true)]);
    world.spawn((
        Mob,
        Named {
            name: "Dracos".into(),
        },
        Keywords(vec!["dracos".into()]),
        Located(room),
        Health { hp: 50, max: 50 },
    ));
    drain(&mut rx);
    cast_args(&mut world, caster, "summon d");
    let out = drain(&mut rx);
    // Summon itself resolved (it looks up an online player named `d`); a
    // swallowed target would have cast Summon Demon on nobody instead.
    assert!(out.contains("no one named 'd' online"), "{out}");
    assert!(!out.contains("summon_demon"), "{out}");
}

#[test]
fn unquoted_multiword_name_keeps_the_rest_as_the_target() {
    let catalog = parse_catalog();
    let found =
        super::resolve_unquoted_ability(&catalog, None, AbilityKind::Spell, "magic missile goblin")
            .map(|(d, tail, needle)| (d.id, tail, needle));
    assert_eq!(
        found,
        Some((28, Some("goblin"), "magic_missile".to_string()))
    );
    // A lone word never takes the multi-word path.
    assert!(super::resolve_unquoted_ability(&catalog, None, AbilityKind::Spell, "armor").is_none());
}
