//! Content that used to be hard-coded by database id or name and is now
//! read from the data: class-gated skills (`ClassSkills` /
//! `KnownAbilities`), class roles resolved by `core:<role>` tag (`CoreClasses`) and
//! the `Effect.prevents_*` flags. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, ClassCatalog, ClassDef, ClassSkillsData, CoreAbilities, CoreClasses, EffectCatalog,
    EffectDef, EffectInstance, EffectSource, Fighting, KnownAbilities, Profile, Room, Stunned,
};
use mud_world::{
    CombatStats, Cooldowns, Follower, Located, Mob, MobPrototypes, RoomSector, SummonedMount,
};

use super::combat_commands::{cmd_claw, cmd_electrify, cmd_steal};
use super::info::cmd_summonmount;
use super::test_support::{Rx, drain, install_core_abilities, mob_proto, player_in};
use super::{Prevent, SkillAccess, check_ability_restrictions, effect_prevents, skill_access};

// Class ids differ per database; these mimic a prod-like numbering that
// disagrees with dev (Thief 3, Assassin 10, Bard 20, Paladin 5, Monk 14).
const THIEF: i32 = 31;
const ASSASSIN: i32 = 7;
const BARD: i32 = 44;
const PALADIN: i32 = 52;
const ANTI_PALADIN: i32 = 53;
const MONK: i32 = 9;

// Ability ids from `core_ability_catalog`.
const STEAL: i32 = 345;
const CLAW: i32 = 54;
const ELECTRIFY: i32 = 119;
const SUMMON_MOUNT: i32 = 355;

fn class(id: i32, plain_name: &str) -> ClassDef {
    ClassDef {
        id,
        name: plain_name.to_string(),
        plain_name: plain_name.to_string(),
        is_subclass: false,
        parent_class_id: None,
        description: None,
        hit_dice: "1d8".to_string(),
        primary_stat: None,
        hp_per_level: 0,
        exp_gain_factor: 1.0,
        alignment_bias: 0,
        campcraft_bonus: false,
        tags: Vec::new(),
        combat_rates: mud_db::characters::ClassCombatRates::default(),
        resistances: std::collections::HashMap::new(),
    }
}

fn class_catalog() -> ClassCatalog {
    let mut catalog = ClassCatalog::default();
    for (id, name) in [
        (THIEF, "Thief"),
        (ASSASSIN, "Assassin"),
        (BARD, "Bard"),
        (PALADIN, "Paladin"),
        (ANTI_PALADIN, "Anti-Paladin"),
        (MONK, "Monk"),
    ] {
        catalog.by_id.insert(id, class(id, name));
    }
    catalog
}

/// Legacy `class.cpp` rows: Thief and Bard steal at 10, Paladin summons a
/// mount at 15. Assassin has no Steal row.
fn class_skills() -> ClassSkillsData {
    let mut data = ClassSkillsData::default();
    for (class_id, ability, level) in [
        (THIEF, STEAL, 10),
        (BARD, STEAL, 10),
        (PALADIN, SUMMON_MOUNT, 15),
        (ANTI_PALADIN, SUMMON_MOUNT, 15),
        // Rows so the Assassin has a kit, just not Steal.
        (ASSASSIN, 999, 1),
    ] {
        data.min_level.insert((class_id, ability), level);
        data.proficiency_cap.insert((class_id, ability), 100);
    }
    data
}

fn world() -> (World, Entity, Rx) {
    let mut world = World::new();
    install_core_abilities(&mut world);
    world.insert_resource(class_skills());
    world.insert_resource(class_catalog());
    world.insert_resource(EffectCatalog::default());
    let room = world.spawn(Room).id();
    let (p, rx) = player_in(&mut world, room);
    (world, p, rx)
}

fn set_class(world: &mut World, p: Entity, class_id: i32, level: i32) {
    world.entity_mut(p).insert(Profile {
        level,
        class_id: Some(class_id),
        race: "HUMAN".into(),
        experience: 0,
        gender: "male".into(),
    });
}

fn learn(world: &mut World, p: Entity, ability: i32) {
    world.entity_mut(p).insert(KnownAbilities {
        entries: vec![(ability, 500, true)],
    });
}

// ---- steal ----

#[test]
fn bard_can_steal_and_assassin_cannot() {
    let (mut world, p, mut rx) = world();
    // Bard teaches Steal at level 10 in the legacy data.
    set_class(&mut world, p, BARD, 20);
    cmd_steal(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(out.contains("Usage: steal"), "bard passes the gate: {out}");

    // Assassin has no Steal row in the legacy data.
    set_class(&mut world, p, ASSASSIN, 40);
    cmd_steal(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(out.contains("You don't know how to steal."), "{out}");
}

#[test]
fn steal_waits_for_the_class_level() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 9);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("You don't know how to steal."));
    set_class(&mut world, p, THIEF, 10);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("Usage: steal"));
}

#[test]
fn steal_needs_practice_once_the_character_practices_anything() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 30);
    // Practiced something else only.
    learn(&mut world, p, 12345);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("You don't know how to steal."));
    learn(&mut world, p, STEAL);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("Usage: steal"));
}

#[test]
fn steal_is_off_when_the_catalog_has_no_steal_ability() {
    let (mut world, p, mut rx) = world();
    world.insert_resource(CoreAbilities::default());
    set_class(&mut world, p, THIEF, 50);
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("You don't know how to steal."));
}

#[test]
fn steal_still_refused_mid_fight() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 50);
    let foe = world.spawn_empty().id();
    world.entity_mut(p).insert(Fighting(foe));
    cmd_steal(&mut world, p, "");
    assert!(drain(&mut rx).contains("can't steal while fighting"));
}

// ---- claw / electrify ----

#[test]
fn claw_and_electrify_open_only_with_the_ability() {
    let (mut world, p, mut rx) = world();
    // No class used to be enough (Druid, Sorcerer, ...); now none is.
    set_class(&mut world, p, PALADIN, 50);
    cmd_claw(&mut world, p, "");
    assert!(drain(&mut rx).contains("Grow some longer fingernails first."));
    cmd_electrify(&mut world, p, "");
    assert!(drain(&mut rx).contains("You haven't the arcane training"));

    learn(&mut world, p, CLAW);
    cmd_claw(&mut world, p, "");
    assert!(drain(&mut rx).contains("Claw whom?"));
    // Knowing Claw does not open Electrify.
    cmd_electrify(&mut world, p, "");
    assert!(drain(&mut rx).contains("You haven't the arcane training"));

    learn(&mut world, p, ELECTRIFY);
    cmd_electrify(&mut world, p, "");
    assert!(drain(&mut rx).contains("Lightning whom?"));
}

// ---- summon mount ----

#[test]
fn skill_access_reports_level_and_refusal() {
    let (mut world, p, _rx) = world();
    set_class(&mut world, p, PALADIN, 14);
    assert_eq!(
        skill_access(&world, p, Some(SUMMON_MOUNT)),
        SkillAccess::NeedsLevel(15)
    );
    set_class(&mut world, p, PALADIN, 15);
    assert_eq!(
        skill_access(&world, p, Some(SUMMON_MOUNT)),
        SkillAccess::Allowed
    );
    // A class with no row and no grant.
    set_class(&mut world, p, THIEF, 50);
    assert_eq!(
        skill_access(&world, p, Some(SUMMON_MOUNT)),
        SkillAccess::Refused
    );
    // Ability missing from the catalog.
    assert_eq!(skill_access(&world, p, None), SkillAccess::Refused);
}

#[test]
fn summon_mount_follows_class_skills() {
    let (mut world, p, mut rx) = world();
    set_class(&mut world, p, THIEF, 50);
    cmd_summonmount(&mut world, p, "");
    assert!(drain(&mut rx).contains("You have no idea what you're trying to accomplish."));

    set_class(&mut world, p, PALADIN, 14);
    cmd_summonmount(&mut world, p, "");
    assert!(drain(&mut rx).contains("aren't yet deemed worthy"));

    // Level reached: the gate passes and the next refusal is the room's
    // sector (the test room has none).
    set_class(&mut world, p, PALADIN, 15);
    cmd_summonmount(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(
        !out.contains("no idea") && !out.contains("worthy"),
        "paladin 15 passes the skill gate: {out}"
    );
}

// ---- summon mount: alignment, cooldown, ownership ----

fn set_alignment(world: &mut World, p: Entity, alignment: i32) {
    let mut stats = world.get::<CombatStats>(p).copied().unwrap_or_default();
    stats.alignment = alignment;
    world.entity_mut(p).insert(stats);
}

/// A summoning-ready world: outdoor room, the mount protos for both
/// factions, a level 15 `class` with `alignment`.
fn mount_world(class_id: i32, alignment: i32) -> (World, Entity, Rx) {
    let (mut world, p, rx) = world();
    let room = world.get::<Located>(p).map(|l| l.0);
    let room = room.unwrap_or_else(|| world.spawn(Room).id());
    world
        .entity_mut(room)
        .insert(RoomSector(mud_db::enums::Sector::Field));
    world.entity_mut(p).insert(Located(room));
    let mut protos = MobPrototypes::default();
    for key in [(0, 48), (0, 47)] {
        let mut proto = mob_proto(key.0, key.1, mud_db::enums::MobProfession::Trainer);
        proto.professions.clear();
        protos.by_key.insert(key, proto);
    }
    world.insert_resource(protos);
    world.insert_resource(mud_world::WorldKeyIndex::default());
    set_class(&mut world, p, class_id, 15);
    set_alignment(&mut world, p, alignment);
    (world, p, rx)
}

fn mounts_of(world: &mut World, owner: Entity) -> Vec<Entity> {
    let mut q = world.query_filtered::<(Entity, &Follower), (With<Mob>, With<SummonedMount>)>();
    q.iter(world)
        .filter(|(_, f)| f.0 == owner)
        .map(|(e, _)| e)
        .collect()
}

#[test]
fn a_paladin_must_be_good_to_summon_a_mount() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 349);
    cmd_summonmount(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(
        out.contains("Not even horses can stand your offensive presence!"),
        "{out}"
    );
    assert!(mounts_of(&mut world, p).is_empty());

    set_alignment(&mut world, p, 350);
    cmd_summonmount(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(!out.contains("offensive presence"), "{out}");
    assert_eq!(mounts_of(&mut world, p).len(), 1, "{out}");
}

#[test]
fn an_anti_paladin_has_no_goodness_gate() {
    let (mut world, p, mut rx) = mount_world(ANTI_PALADIN, -900);
    cmd_summonmount(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(!out.contains("offensive presence"), "{out}");
    assert_eq!(mounts_of(&mut world, p).len(), 1, "{out}");
}

#[test]
fn losing_a_summoned_mount_starts_the_summon_cooldown() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 500);
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount = mounts_of(&mut world, p)[0];
    cmd_summonmount(&mut world, p, "");
    assert!(drain(&mut rx).contains("You already have a mount."));

    // The mount stops following (legacy `stop_follower`): it leaves the
    // game and the owner must wait 12 MUD hours.
    super::release_from(&mut world, mount, p);
    assert!(world.get_entity(mount).is_err(), "released mount is gone");
    let ready = world.get::<Cooldowns>(p).expect("cooldown").ready_at[&SUMMON_MOUNT];
    let left = ready.saturating_duration_since(std::time::Instant::now());
    assert!(left.as_secs() > 890 && left.as_secs() <= 900, "{left:?}");

    cmd_summonmount(&mut world, p, "");
    let out = drain(&mut rx);
    assert!(
        out.contains("You must wait another 900 seconds before you can summon a mount again."),
        "{out}"
    );
    assert!(mounts_of(&mut world, p).is_empty());

    // The cooldown runs out.
    world
        .get_mut::<Cooldowns>(p)
        .unwrap()
        .ready_at
        .insert(SUMMON_MOUNT, std::time::Instant::now());
    cmd_summonmount(&mut world, p, "");
    assert_eq!(mounts_of(&mut world, p).len(), 1);
}

#[test]
fn a_summoned_mount_that_dies_starts_the_cooldown() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 500);
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount = mounts_of(&mut world, p)[0];
    super::die_follower(&mut world, mount, false);
    assert!(
        world
            .get::<Cooldowns>(p)
            .is_some_and(|c| c.ready_at.contains_key(&SUMMON_MOUNT)),
        "mount lost, cooldown on"
    );
}

fn summon_cooldown_on(world: &World, owner: Entity) -> bool {
    world
        .get::<Cooldowns>(owner)
        .and_then(|c| c.ready_at.get(&SUMMON_MOUNT))
        .is_some_and(|r| *r > std::time::Instant::now())
}

#[test]
fn a_mount_told_to_follow_self_leaves_and_starts_the_cooldown() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 500);
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount = mounts_of(&mut world, p)[0];
    super::info::cmd_follow(&mut world, mount, "self");
    assert!(world.get_entity(mount).is_err(), "mount extracted");
    assert!(summon_cooldown_on(&world, p), "cooldown started");
    cmd_summonmount(&mut world, p, "");
    assert!(drain(&mut rx).contains("You must wait another"));
    assert!(mounts_of(&mut world, p).is_empty());
}

#[test]
fn a_mount_told_to_follow_someone_else_leaves_and_starts_the_cooldown() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 500);
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount = mounts_of(&mut world, p)[0];
    let room = world.get::<Located>(p).unwrap().0;
    let bob = world
        .spawn((Mob, Located(room), mud_world::Named { name: "Bob".into() }))
        .id();
    super::info::cmd_follow(&mut world, mount, "bob");
    assert!(world.get_entity(mount).is_err(), "mount extracted");
    assert!(world.get_entity(bob).is_ok());
    assert!(summon_cooldown_on(&world, p), "cooldown on the owner");
    assert!(
        world.query::<&Follower>().iter(&world).next().is_none(),
        "nothing re-pointed at bob"
    );
}

#[test]
fn removing_a_grouped_mount_notifies_and_cools_down_once() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 500);
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount = mounts_of(&mut world, p)[0];
    world.entity_mut(mount).insert(mud_world::GroupMember(p));
    super::info::cmd_unfollow(&mut world, mount, "");
    assert!(world.get_entity(mount).is_err(), "mount extracted");
    assert!(summon_cooldown_on(&world, p));
    let out = drain(&mut rx);
    assert_eq!(out.matches("has left the group").count(), 1, "{out}");
    assert_eq!(out.matches("group has been disbanded").count(), 1, "{out}");

    // Extracting a grouped mount directly (death/despawn path) is also single.
    cmd_summonmount(&mut world, p, "");
    world
        .get_mut::<Cooldowns>(p)
        .unwrap()
        .ready_at
        .insert(SUMMON_MOUNT, std::time::Instant::now());
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount2 = mounts_of(&mut world, p)[0];
    world.entity_mut(mount2).insert(mud_world::GroupMember(p));
    super::release_from(&mut world, mount2, p);
    assert!(world.get_entity(mount2).is_err());
    let out = drain(&mut rx);
    assert!(out.matches("left").count() <= 1, "{out}");
}

#[test]
fn the_sweep_removes_mounts_and_pets_whose_owner_is_gone() {
    let (mut world, p, mut rx) = mount_world(PALADIN, 500);
    cmd_summonmount(&mut world, p, "");
    drain(&mut rx);
    let mount = mounts_of(&mut world, p)[0];
    let room = world.get::<Located>(p).unwrap().0;
    let gone = world.spawn_empty().id();
    world.despawn(gone);
    let pet = world
        .spawn((Mob, Located(room), Follower(gone), mud_world::PersistentPet))
        .id();
    let tagalong = world.spawn((Mob, Located(room), Follower(gone))).id();
    let loose_mount = world.spawn((Mob, Located(room), SummonedMount)).id();
    let kept_pet = world
        .spawn((Mob, Located(room), Follower(p), mud_world::PersistentPet))
        .id();

    super::info::orphan_follower_sweep(&mut world);
    assert!(world.get_entity(mount).is_ok(), "owner online: kept");
    assert!(world.get_entity(kept_pet).is_ok());
    assert!(world.get_entity(pet).is_err(), "orphaned pet removed");
    assert!(world.get_entity(loose_mount).is_err(), "unattached mount");
    assert!(world.get_entity(tagalong).is_ok());
    assert!(
        world.get::<Follower>(tagalong).is_none(),
        "tagalong just stops following"
    );

    // The owner leaves without the logout path: the next sweep takes the
    // mount.
    world.despawn(p);
    super::info::orphan_follower_sweep(&mut world);
    assert!(world.get_entity(mount).is_err());
    assert!(world.get_entity(kept_pet).is_err());
}

// ---- Monk / Thief by core tag (name fallback) ----

#[test]
fn core_classes_resolve_by_plain_name_not_id() {
    let catalog = class_catalog();
    let core = CoreClasses::resolve_quiet(&catalog);
    assert_eq!(core.monk, Some(MONK));
    assert_eq!(core.thief, Some(THIEF));
    assert!(core.missing().is_empty());

    // Case-insensitive; the lowest id wins a duplicate.
    let mut dup = ClassCatalog::default();
    dup.by_id.insert(40, class(40, "MONK"));
    dup.by_id.insert(12, class(12, "monk"));
    let core = CoreClasses::resolve_quiet(&dup);
    assert_eq!(core.monk, Some(12));
    assert_eq!(core.thief, None);
    assert_eq!(core.missing(), vec!["Thief"]);

    assert_eq!(
        CoreClasses::resolve_quiet(&ClassCatalog::default()),
        CoreClasses::default()
    );
}

fn tagged_class(id: i32, name: &str, tags: &[&str]) -> ClassDef {
    let mut def = class(id, name);
    def.tags = tags.iter().map(|t| (*t).to_string()).collect();
    def
}

fn class_catalog_of(defs: Vec<ClassDef>) -> ClassCatalog {
    let mut cat = ClassCatalog::default();
    for d in defs {
        cat.by_id.insert(d.id, d);
    }
    cat
}

/// Writer that appends into a shared buffer for log assertions.
#[derive(Clone, Default)]
struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_subscriber(buf: &LogBuf) -> impl tracing::Subscriber + use<> {
    let sink = buf.clone();
    tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .finish()
}

#[test]
fn core_classes_bind_by_tag_after_a_rename() {
    // "Monk" was renamed in Muditor; the tag keeps it bound. A decoy row
    // still named "Monk" without the tag must not steal the role.
    let cat = class_catalog_of(vec![
        tagged_class(7, "Martial Artist", &["core:monk"]),
        tagged_class(8, "Monk", &[]),
        tagged_class(9, "Sneak", &["CORE:THIEF", "other"]),
    ]);
    let (core, report) = CoreClasses::resolve_with_report(&cat);
    assert_eq!(core.monk, Some(7));
    assert_eq!(core.thief, Some(9));
    assert!(report.duplicate_tags.is_empty());
    assert!(report.name_fallbacks.is_empty());
    assert!(core.missing().is_empty());
}

#[test]
fn core_classes_fall_back_to_name_and_warn_when_untagged() {
    let buf = LogBuf::default();
    let subscriber = log_subscriber(&buf);
    let cat = class_catalog_of(vec![
        tagged_class(7, "Martial Artist", &["core:monk"]),
        tagged_class(260, "Thief", &[]),
    ]);
    let (core, report) = CoreClasses::resolve_with_report(&cat);
    assert_eq!(core.monk, Some(7));
    assert_eq!(core.thief, Some(260));
    assert_eq!(report.name_fallbacks, vec!["Thief"]);
    let core2 = tracing::subscriber::with_default(subscriber, || CoreClasses::resolve(&cat));
    assert_eq!(core2, core);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("WARN"), "{log}");
    assert!(log.contains("bound by name"), "{log}");
    assert!(log.contains("class=\"Thief\""), "{log}");
    assert!(!log.contains("class=\"Monk\""), "{log}");
}

#[test]
fn core_classes_duplicate_tag_logs_error_and_lowest_id_wins() {
    let buf = LogBuf::default();
    let subscriber = log_subscriber(&buf);
    let cat = class_catalog_of(vec![
        tagged_class(30, "Monk B", &["core:monk"]),
        tagged_class(12, "Monk A", &["core:monk"]),
        tagged_class(21, "Monk C", &["core:monk"]),
    ]);
    let (core, report) = CoreClasses::resolve_with_report(&cat);
    assert_eq!(core.monk, Some(12));
    assert_eq!(report.duplicate_tags, vec![("core:monk", vec![12, 21, 30])]);
    let _ = tracing::subscriber::with_default(subscriber, || CoreClasses::resolve(&cat));
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("ERROR"), "{log}");
    assert!(log.contains("core:monk"), "{log}");
}

#[test]
fn monk_unarmed_dice_follow_the_resolved_class() {
    let core = CoreClasses::resolve_quiet(&class_catalog());
    // The old code keyed on id 14 (dev's Monk); here 14 is no Monk.
    assert!(crate::login::monk_natural_damage(core, Some(14), 30).is_none());
    let nd = crate::login::monk_natural_damage(core, Some(MONK), 30).expect("monk dice");
    assert_eq!((nd.num, nd.size, nd.bonus), (3, 6, 6));
    let low = crate::login::monk_natural_damage(core, Some(MONK), 3).unwrap();
    assert_eq!((low.num, low.bonus), (1, 0));
    assert!(crate::login::monk_natural_damage(core, Some(THIEF), 30).is_none());
    assert!(crate::login::monk_natural_damage(core, None, 30).is_none());
    // No Monk in the catalog: nobody gets the dice, `None` never matches
    // a classless character.
    let none = CoreClasses::default();
    assert!(crate::login::monk_natural_damage(none, None, 30).is_none());
    assert!(crate::login::monk_natural_damage(none, Some(MONK), 30).is_none());
}

#[test]
fn thief_hide_lag_follows_the_resolved_class() {
    let (mut world, p, _rx) = world();
    world.insert_resource(CoreClasses::resolve_quiet(&class_catalog()));
    set_class(&mut world, p, THIEF, 10);
    assert!(crate::hiding::is_thief(&world, p));
    // Assassin is a different class with its own id.
    set_class(&mut world, p, ASSASSIN, 10);
    assert!(!crate::hiding::is_thief(&world, p));
    // Dev's Thief id (3) means nothing here.
    set_class(&mut world, p, 3, 10);
    assert!(!crate::hiding::is_thief(&world, p));
    world.insert_resource(CoreClasses::default());
    set_class(&mut world, p, THIEF, 10);
    assert!(!crate::hiding::is_thief(&world, p));
}

// ---- Effect.prevents_* ----

fn effect_type(id: i32, name: &str, speaking: bool, movement: bool) -> EffectDef {
    EffectDef {
        id,
        name: name.into(),
        description: None,
        effect_type: "status".into(),
        tags: vec![],
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: speaking,
        prevents_casting: false,
        prevents_movement: movement,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

fn apply(world: &mut World, target: Entity, kind: i32, name: &str) {
    world.spawn((
        EffectInstance {
            kind,
            name: name.into(),
            strength: 1,
            remaining_secs: 30,
            source: EffectSource::Other("test".into()),
            ability_id: None,
        },
        AppliedTo(target),
    ));
}

#[test]
fn prevents_movement_flag_blocks_kick_and_trip() {
    let (mut world, p, _rx) = world();
    let target = world.spawn_empty().id();
    {
        let mut catalog = world.resource_mut::<EffectCatalog>();
        catalog
            .by_id
            .insert(70, effect_type(70, "rooted_by_data", false, true));
        catalog
            .by_id
            .insert(71, effect_type(71, "harmless", false, false));
    }
    let rules = vec![serde_json::json!({
        "type": "not_immobilized",
        "message": "You can't move!",
    })];
    // KICK and TRIP_UP both carry `not_immobilized`.
    assert_eq!(
        check_ability_restrictions(&mut world, p, target, &rules),
        None
    );

    // An unflagged effect with an unrecognised name does nothing.
    apply(&mut world, p, 71, "frobnicated");
    assert_eq!(
        check_ability_restrictions(&mut world, p, target, &rules),
        None
    );

    // The same name under a type flagged `prevents_movement` blocks it.
    apply(&mut world, p, 70, "frobnicated");
    assert!(effect_prevents(&mut world, p, Prevent::Movement));
    assert!(!effect_prevents(&mut world, p, Prevent::Speaking));
    assert_eq!(
        check_ability_restrictions(&mut world, p, target, &rules).as_deref(),
        Some("You can't move!")
    );
}

#[test]
fn stunned_marker_still_immobilizes_without_an_effect_catalog() {
    let (mut world, p, _rx) = world();
    world.remove_resource::<EffectCatalog>();
    assert!(!super::is_immobilized(&mut world, p));
    world.entity_mut(p).insert(Stunned);
    assert!(super::is_immobilized(&mut world, p));
}

/// The hard-coded `flag_prevents` table this data replaced, kept as the
/// oracle: the seeded `StatusFlagValue.prevents_*` rows must block exactly
/// what it did, for every flag spelling it knew and for names it did not.
fn legacy_flag_prevents(flag: &str, kind: Prevent) -> bool {
    let f = flag.to_ascii_lowercase();
    match kind {
        Prevent::Movement => matches!(
            f.as_str(),
            "webbed"
                | "held"
                | "hold_person"
                | "paralyzed"
                | "mesmerized"
                | "asleep"
                | "sleeping"
                | "rooted"
                | "entangled"
                | "stunned"
                | "stun"
        ),
        Prevent::Casting => matches!(
            f.as_str(),
            "silenced"
                | "silence"
                | "stunned"
                | "stun"
                | "asleep"
                | "sleeping"
                | "paralyzed"
                | "mesmerized"
                | "held"
                | "hold_person"
        ),
        Prevent::Speaking => matches!(
            f.as_str(),
            "silenced" | "silence" | "stunned" | "stun" | "asleep" | "sleeping"
        ),
    }
}

#[test]
fn seeded_status_flag_rows_block_exactly_what_the_old_table_did() {
    let known = [
        "webbed",
        "held",
        "hold_person",
        "paralyzed",
        "mesmerized",
        "asleep",
        "sleeping",
        "rooted",
        "entangled",
        "stunned",
        "stun",
        "silenced",
        "silence",
    ];
    // Spellings the old table did not know, including flags with an AI value
    // row and a differently-cased known name (the old lookup lowercased).
    let others = [
        "blind",
        "sleep",
        "major_paralysis",
        "sanctuary",
        "frobnicated",
        "WEBBED",
        "Silence",
        "Hold_Person",
    ];
    for kind in [Prevent::Movement, Prevent::Casting, Prevent::Speaking] {
        for flag in known.iter().chain(others.iter()) {
            let mut world = World::new();
            world.insert_resource(super::test_support::seeded_status_flag_values());
            let (p, _rx) = {
                let room = world.spawn(Room).id();
                player_in(&mut world, room)
            };
            apply(&mut world, p, 9999, flag);
            assert_eq!(
                effect_prevents(&mut world, p, kind),
                legacy_flag_prevents(flag, kind),
                "{flag} / {kind:?}"
            );
        }
    }
    // The 13 seeded flags are exactly the ones the old table named.
    let seeded = super::test_support::seeded_status_flag_values();
    let mut names: Vec<&str> = seeded.prevents.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut expected = known.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);
}

#[test]
fn without_status_flag_rows_name_alone_blocks_nothing() {
    // A database without the prevents data (or a world built without one)
    // falls back to the effect-type flags only.
    let mut world = World::new();
    let room = world.spawn(Room).id();
    let (p, _rx) = player_in(&mut world, room);
    apply(&mut world, p, 9999, "webbed");
    for kind in [Prevent::Movement, Prevent::Casting, Prevent::Speaking] {
        assert!(!effect_prevents(&mut world, p, kind), "{kind:?}");
    }
}
