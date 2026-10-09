//! The `extract` effect: legacy `spell_banish` (`fierymud_legacy/src/spells.cpp:174`).
//!
//! * The caster shouts "I banish thee!" (shown to the whole room, whatever
//!   the visibility), then the victim may resist: mob `NoSummon`, mob
//!   `NoCharm` (the importer turns the legacy flag into a `"charm": 0`
//!   resistance), a charmed pet whose master stands in the room, a
//!   `NoSummon` room, or a failed saving throw. A resisting mob attacks the
//!   caster, and the cast then falls through to "Nothing happens.".
//! * Otherwise `roll = random(0,100) + skill + cha_bonus - victim_level` and
//!   the banish lands when `roll > success_threshold` (legacy: 100).
//! * A banished mob disappears in a flash of light; when
//!   `random(0,100) + wis_bonus * gear_wis_multiplier > gear_destroy_threshold`
//!   (legacy: 66 and 2) everything it wears and carries is destroyed,
//!   otherwise it all drops to the room floor (`extract_char`).
//! * A banished player is not removed: they are sent to their home (recall)
//!   room.
//!
//! The thresholds are content: they come from the effect params
//! (`AbilityEffect.override_params` over `Effect.default_params`), seeded by
//! fierylib. The fallbacks below are the legacy values.

use bevy_ecs::prelude::*;
use mud_world::{
    Contents, EquippedSlot, Fighting, Follower, FromMobReset, Item, Located, Mob, MobBehaviors,
    MobPrototypes, Mounted, Player, Profile, RaceDefaults, RecallPoint, RiddenBy, WorldKey,
    WorldKeyIndex,
};

use super::attack_ok::{attack_ok, is_charmed};
use super::{
    broadcast_room_except_rendered, broadcast_room_visual, cap_sentence_start,
    disengage_attackers_of, engage_combat, name_of, send_to, try_remove,
};

/// Legacy `roll > 100`.
const DEFAULT_SUCCESS_THRESHOLD: i32 = 100;
/// Legacy `roll > 66`.
const DEFAULT_GEAR_THRESHOLD: i32 = 66;
/// Legacy `stat_bonus[GET_WIS(ch)].magic * 2`.
const DEFAULT_GEAR_WIS_MULTIPLIER: i32 = 2;

/// Tunables read from the `extract` effect params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BanishParams {
    /// The banish lands when the success roll is strictly above this.
    pub success_threshold: i32,
    /// Equipment is destroyed when the gear roll is strictly above this.
    pub gear_destroy_threshold: i32,
    /// Multiplier on the caster's wisdom bonus in the gear roll.
    pub gear_wis_multiplier: i32,
}

impl BanishParams {
    pub(super) fn parse(
        override_params: Option<&serde_json::Value>,
        default_params: Option<&serde_json::Value>,
    ) -> Self {
        let int = |key: &str| -> Option<i32> {
            override_params
                .and_then(|p| p.get(key))
                .or_else(|| default_params.and_then(|p| p.get(key)))
                .and_then(serde_json::Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
        };
        Self {
            success_threshold: int("success_threshold").unwrap_or(DEFAULT_SUCCESS_THRESHOLD),
            gear_destroy_threshold: int("gear_destroy_threshold").unwrap_or(DEFAULT_GEAR_THRESHOLD),
            gear_wis_multiplier: int("gear_wis_multiplier").unwrap_or(DEFAULT_GEAR_WIS_MULTIPLIER),
        }
    }
}

/// The caster-side numbers the formulas need (from the cast's `FormulaCtx`).
#[derive(Debug, Clone, Copy)]
pub(super) struct Caster {
    pub entity: Entity,
    /// Proficiency on the 0..=100 scale.
    pub skill: i32,
    pub cha_bonus: i32,
    pub wis_bonus: i32,
}

/// The two dice of a banish, both `random_number(0, 100)` in legacy.
#[derive(Debug, Clone, Copy)]
pub(super) struct Rolls {
    pub success: i32,
    pub gear: i32,
}

impl Rolls {
    pub(super) fn random() -> Self {
        Self {
            success: rand::random_range(0..=100),
            gear: rand::random_range(0..=100),
        }
    }
}

/// Why a victim shrugged the banish off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Resist {
    MobNoSummon,
    MobNoCharm,
    CharmedPet,
    RoomNoSummon,
    SavingThrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// `attack_ok` refused (peaceful room / mob, PK rules, ...).
    NotAllowed,
    /// Casting it on yourself does nothing (legacy returns before the shout).
    SelfTarget,
    Resisted(Resist),
    /// The success roll missed.
    Failed,
    MobBanished {
        gear_destroyed: bool,
    },
    PlayerSentHome,
    /// The player's home (and every fallback) bars them.
    NoHome,
}

impl Outcome {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NotAllowed => "refused: not allowed",
            Self::SelfTarget => "self target",
            Self::Resisted(_) => "refused: resisted",
            Self::Failed => "refused: nothing happens",
            Self::MobBanished {
                gear_destroyed: true,
            } => "banished, equipment destroyed",
            Self::MobBanished {
                gear_destroyed: false,
            } => "banished, equipment dropped",
            Self::PlayerSentHome => "banished home",
            Self::NoHome => "refused: entry restricted",
        }
    }
}

/// Legacy resist conditions other than the saving throw
/// (`MOB_NOSUMMON || MOB_NOCHARM || charmed pet with its master present ||
/// ROOM_NOSUMMON`), in legacy order.
pub(super) fn resist_reason(world: &mut World, victim: Entity) -> Option<Resist> {
    let is_mob = world.get::<Mob>(victim).is_some();
    if is_mob
        && world
            .get::<MobBehaviors>(victim)
            .is_some_and(|b| b.has(mud_db::enums::MobBehavior::NoSummon))
    {
        return Some(Resist::MobNoSummon);
    }
    if is_mob && mob_is_charm_immune(world, victim) {
        return Some(Resist::MobNoCharm);
    }
    if let Some(Follower(master)) = world.get::<Follower>(victim).copied()
        && is_charmed(world, victim)
        && let (Some(here), Some(there)) = (
            world.get::<Located>(victim).map(|l| l.0),
            world.get::<Located>(master).map(|l| l.0),
        )
        && here == there
    {
        return Some(Resist::CharmedPet);
    }
    if world
        .get::<Located>(victim)
        .is_some_and(|l| world.get::<mud_world::NoSummonRoom>(l.0).is_some())
    {
        return Some(Resist::RoomNoSummon);
    }
    None
}

/// Legacy `MOB_NOCHARM`: imported as a `"charm": 0` (immune) resistance on
/// the mob prototype.
fn mob_is_charm_immune(world: &World, mob: Entity) -> bool {
    let Some(key) = world.get::<WorldKey>(mob) else {
        return false;
    };
    let Some(protos) = world.get_resource::<MobPrototypes>() else {
        return false;
    };
    protos
        .by_key
        .get(&(key.zone, key.id))
        .and_then(|p| p.resistances.as_object())
        .is_some_and(|m| {
            m.iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("charm") && v.as_i64() == Some(0))
        })
}

/// `random(0,100) + skill + cha_bonus - victim_level` (spells.cpp:204).
pub(super) fn success_total(caster: &Caster, victim_level: i32, roll: i32) -> i32 {
    roll + caster.skill + caster.cha_bonus - victim_level
}

/// `random(0,100) + wis_bonus * 2 > 66` (spells.cpp:211).
pub(super) fn destroys_gear(caster: &Caster, params: &BanishParams, roll: i32) -> bool {
    roll + caster.wis_bonus * params.gear_wis_multiplier > params.gear_destroy_threshold
}

/// Run one banish. `saved` is the victim's saving-throw result (rolled by the
/// caller, so tests can force it).
pub(super) fn banish(
    world: &mut World,
    caster: &Caster,
    victim: Entity,
    params: &BanishParams,
    saved: bool,
    rolls: Rolls,
) -> Outcome {
    // Legacy opens with `attack_ok(ch, victim, true)`; a refusal costs nothing.
    if !attack_ok(world, caster.entity, victim, true) {
        return Outcome::NotAllowed;
    }
    if victim == caster.entity {
        return Outcome::SelfTarget;
    }
    let Some(room) = world.get::<Located>(victim).map(|l| l.0) else {
        return Outcome::Failed;
    };
    let caster_name = name_of(world, caster.entity);
    let victim_name = name_of(world, victim);
    let cap_caster = cap_sentence_start(&caster_name);
    let cap_victim = cap_sentence_start(&victim_name);

    send_to(
        world,
        caster.entity,
        format!("You look at {victim_name} and shout, '<b:red>I banish thee!</>'\r\n"),
    );
    send_to(
        world,
        victim,
        format!("{cap_caster} looks at you and shouts, '<b:red>I banish thee!</>'\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[caster.entity, victim],
        &format!("{cap_caster} looks at {victim_name} and shouts, '<b:red>I banish thee!</>'\r\n"),
    );

    let resist = resist_reason(world, victim).or(saved.then_some(Resist::SavingThrow));
    if let Some(why) = resist {
        send_to(
            world,
            caster.entity,
            format!("{cap_victim} resists you.\r\n"),
        );
        send_to(world, victim, "You resist.\r\n");
        if world.get::<Mob>(victim).is_some() && world.get::<Fighting>(victim).is_none() {
            engage_combat(world, victim, caster.entity, room);
        }
        // Legacy zeroes the roll: a resisted banish always ends in
        // "Nothing happens.".
        return nothing_happens(world, caster.entity, room, Outcome::Resisted(why));
    }
    let level = mud_world::effective_level(world, victim);
    if success_total(caster, level, rolls.success) <= params.success_threshold {
        return nothing_happens(world, caster.entity, room, Outcome::Failed);
    }

    if world.get::<Player>(victim).is_some() {
        send_home(world, caster.entity, victim, room)
    } else {
        broadcast_room_except_rendered(
            world,
            room,
            &[caster.entity, victim],
            &format!("{cap_victim} disappears in a flash of light!\r\n"),
        );
        let gear_destroyed = destroys_gear(caster, params, rolls.gear);
        extract_mob(world, victim, Some(room), gear_destroyed);
        Outcome::MobBanished { gear_destroyed }
    }
}

fn nothing_happens(world: &mut World, caster: Entity, room: Entity, outcome: Outcome) -> Outcome {
    broadcast_room_except_rendered(world, room, &[caster], "Nothing happens.\r\n");
    send_to(world, caster, "Nothing happens.\r\n");
    outcome
}

/// Legacy `extract_objects` (when `destroy_gear`) + `extract_char`: whatever
/// the mob wore or carried is destroyed, or else dropped on the floor of its
/// room; then the mob itself leaves the world. A mob with no room has nothing
/// to drop onto, so its gear is destroyed. Also the exit for a conjured mob
/// whose summon ends (`effects::despawn_summoned_mob`).
pub(crate) fn extract_mob(
    world: &mut World,
    mob: Entity,
    room: Option<Entity>,
    destroy_gear: bool,
) {
    let items: Vec<Entity> = world
        .get::<Contents>(mob)
        .map(|c| {
            c.iter()
                .filter(|e| world.get::<Item>(*e).is_some())
                .collect()
        })
        .unwrap_or_default();
    for item in items {
        match room {
            Some(room) if !destroy_gear => {
                crate::equip_apply::release_gear(world, item);
                try_remove::<EquippedSlot>(world, item);
                world.entity_mut(item).insert(Located(room));
            }
            _ => super::info::despawn_item_tree(world, item),
        }
    }
    // Pets that followed the mob stop following it (legacy `die_follower`).
    let followers: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Follower)>();
        q.iter(world)
            .filter(|(_, f)| f.0 == mob)
            .map(|(e, _)| e)
            .collect()
    };
    for f in followers {
        try_remove::<Follower>(world, f);
    }
    // A banished mount drops its rider (legacy `extract_char` dismounts).
    if let Some(RiddenBy(rider)) = world.get::<RiddenBy>(mob).copied() {
        try_remove::<Mounted>(world, rider);
    }
    if let Some(Mounted(mount)) = world.get::<Mounted>(mob).copied() {
        try_remove::<RiddenBy>(world, mount);
    }
    // The reset row's respawn timer starts now, exactly as after a death.
    if let Some(reset_id) = world.get::<FromMobReset>(mob).map(|f| f.0) {
        let now = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
        if let Some(mut timers) = world.get_resource_mut::<crate::respawn::MobRespawnTimers>() {
            timers.last_death_tick.insert(reset_id, now);
        }
    }
    disengage_attackers_of(world, mob);
    super::ungroup_on_despawn(world, mob);
    if let Ok(e) = world.get_entity_mut(mob) {
        e.despawn();
    }
}

/// Where a banished player lands, best first: their recall point (legacy
/// `GET_HOMEROOM`), their race's start room, then the Void (legacy falls back
/// to room 0).
fn home_candidates(world: &World, player: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    if let Some(r) = world.get::<RecallPoint>(player).map(|r| r.0)
        && world.get_entity(r).is_ok()
    {
        out.push(r);
    }
    if let Some(index) = world.get_resource::<WorldKeyIndex>() {
        if let Some(race) = world.get::<Profile>(player).map(|p| p.race.clone())
            && let Some(key) = world
                .get_resource::<RaceDefaults>()
                .and_then(|d| d.start_room_by_race.get(&race).copied())
            && let Some(r) = index.rooms.get(&key)
        {
            out.push(*r);
        }
        if let Some(r) = index.rooms.get(&(0, 0)) {
            out.push(*r);
        }
    }
    out
}

/// Legacy `perform_teleport` of the victim to their home room.
fn send_home(world: &mut World, caster: Entity, victim: Entity, from: Entity) -> Outcome {
    let candidates = home_candidates(world, victim);
    let mut dest = None;
    for room in candidates {
        // A god room's entry restriction still applies to a banished mortal.
        if crate::room_access::entry_allowed(world, victim, room) {
            dest = Some(room);
            break;
        }
    }
    let Some(dest) = dest else {
        send_to(
            world,
            caster,
            "A mysterious powerful force repels your magic.\r\n",
        );
        return Outcome::NoHome;
    };
    let victim_name = cap_sentence_start(&name_of(world, victim));

    // dismount_char
    if let Some(Mounted(mount)) = world.get::<Mounted>(victim).copied() {
        try_remove::<Mounted>(world, victim);
        try_remove::<RiddenBy>(world, mount);
    } else if let Some(RiddenBy(rider)) = world.get::<RiddenBy>(victim).copied() {
        try_remove::<RiddenBy>(world, victim);
        try_remove::<Mounted>(world, rider);
    }
    // char_from_room stops the victim's fight and everyone fighting them.
    crate::combat::stop_fighting_both_ways(world, victim);

    send_to(world, victim, "<b:black>You are banished!</>\r\n");
    broadcast_room_visual(
        world,
        from,
        victim,
        &[victim],
        &format!("{victim_name} disappears in a flash of light!\r\n"),
    );
    world.entity_mut(victim).insert(Located(dest));
    broadcast_room_visual(
        world,
        dest,
        victim,
        &[victim],
        &format!("{victim_name} appears in a flash of light!\r\n"),
    );
    super::cmd_look(world, victim, "");
    Outcome::PlayerSentHome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Connection;
    use crate::commands::test_support::{Rx, drain};
    use mud_db::enums::MobBehavior;
    use mud_world::{EffectInstance, Named, NoSummonRoom, Room};

    struct Fx {
        world: World,
        here: Entity,
        home: Entity,
    }

    fn fx() -> Fx {
        let mut world = World::new();
        world.insert_resource(WorldKeyIndex::default());
        world.insert_resource(MobPrototypes::default());
        world.insert_resource(mud_script::LuaHost::default());
        let here = world
            .spawn((
                Room,
                WorldKey { zone: 30, id: 1 },
                Named {
                    name: "Here".into(),
                },
            ))
            .id();
        let home = world
            .spawn((
                Room,
                WorldKey { zone: 30, id: 2 },
                Named {
                    name: "Home".into(),
                },
            ))
            .id();
        {
            let mut idx = world.resource_mut::<WorldKeyIndex>();
            idx.rooms.insert((30, 1), here);
            idx.rooms.insert((30, 2), home);
        }
        Fx { world, here, home }
    }

    impl Fx {
        /// The server-wide `pk_allowed` toggle (`game pk on`).
        fn pk_on(&mut self) {
            let mut rc = mud_world::RuntimeConfig::default();
            rc.by_key.insert(
                ("social".into(), "pk_allowed".into()),
                mud_world::ConfigValue::Bool(true),
            );
            self.world.insert_resource(rc);
        }
    }

    fn person(f: &mut Fx, name: &str, level: i32) -> (Entity, Rx) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        let e = f
            .world
            .spawn((
                Player,
                Named { name: name.into() },
                Located(f.here),
                Connection(tx),
                Profile {
                    level,
                    class_id: None,
                    race: "Human".into(),
                    experience: 0,
                    gender: "neutral".into(),
                },
            ))
            .id();
        (e, rx)
    }

    /// A spawned mob the way `respawn_tick` builds one: no `Profile`; its level
    /// is on the prototype it was spawned from.
    fn mob(f: &mut Fx, name: &str, level: i32) -> Entity {
        let n = i32::try_from(f.world.resource::<MobPrototypes>().by_key.len()).unwrap();
        let mut proto =
            crate::commands::test_support::mob_proto(77, n, mud_db::enums::MobProfession::Banker);
        proto.level = level;
        f.world
            .resource_mut::<MobPrototypes>()
            .by_key
            .insert((77, n), proto);
        f.world
            .spawn((
                Mob,
                Named { name: name.into() },
                WorldKey { zone: 77, id: n },
                Located(f.here),
            ))
            .id()
    }

    fn item(f: &mut Fx, holder: Entity, equipped: bool) -> Entity {
        let e = f
            .world
            .spawn((
                Item,
                Named {
                    name: "a sword".into(),
                },
                Located(holder),
            ))
            .id();
        if equipped {
            f.world
                .entity_mut(e)
                .insert(EquippedSlot(mud_world::Slot::Wield));
        }
        e
    }

    fn caster_of(e: Entity) -> Caster {
        Caster {
            entity: e,
            skill: 50,
            cha_bonus: 0,
            wis_bonus: 0,
        }
    }

    const P: BanishParams = BanishParams {
        success_threshold: 100,
        gear_destroy_threshold: 66,
        gear_wis_multiplier: 2,
    };
    /// Always lands (0..=100 + 50 - level > 100 needs roll > 50 + level).
    const HIT: Rolls = Rolls {
        success: 100,
        gear: 100,
    };

    #[test]
    fn params_default_to_legacy_and_read_overrides() {
        assert_eq!(BanishParams::parse(None, None), P);
        let over = serde_json::json!({"success_threshold": 90, "gear_destroy_threshold": 10});
        let def = serde_json::json!({"gear_wis_multiplier": 3, "success_threshold": 1});
        let p = BanishParams::parse(Some(&over), Some(&def));
        assert_eq!(p.success_threshold, 90);
        assert_eq!(p.gear_destroy_threshold, 10);
        assert_eq!(p.gear_wis_multiplier, 3);
    }

    #[test]
    fn success_roll_is_strictly_above_the_threshold() {
        let c = Caster {
            entity: Entity::PLACEHOLDER,
            skill: 60,
            cha_bonus: 5,
            wis_bonus: 0,
        };
        // 36 + 60 + 5 - 1 = 100: not above 100.
        assert_eq!(success_total(&c, 1, 36), 100);
        assert!(success_total(&c, 1, 37) > 100);
        // A higher level victim lowers it.
        assert_eq!(success_total(&c, 21, 37), 81);
    }

    #[test]
    fn gear_roll_is_strictly_above_66_with_wis_bonus() {
        let mut c = caster_of(Entity::PLACEHOLDER);
        assert!(!destroys_gear(&c, &P, 66));
        assert!(destroys_gear(&c, &P, 67));
        c.wis_bonus = 5; // +10
        assert!(!destroys_gear(&c, &P, 56));
        assert!(destroys_gear(&c, &P, 57));
    }

    #[test]
    fn mob_nosummon_resists() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world
            .entity_mut(m)
            .insert(MobBehaviors(vec![MobBehavior::NoSummon]));
        assert_eq!(resist_reason(&mut f.world, m), Some(Resist::MobNoSummon));
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, HIT);
        assert_eq!(out, Outcome::Resisted(Resist::MobNoSummon));
        let text = drain(&mut rx);
        assert!(text.contains("I banish thee!"), "{text}");
        assert!(text.contains("A demon resists you."), "{text}");
        assert!(text.contains("Nothing happens."), "{text}");
        assert!(f.world.get_entity(m).is_ok(), "the mob stays");
        // The resisting mob turns on the caster.
        assert_eq!(f.world.get::<Fighting>(m).map(|x| x.0), Some(c));
    }

    #[test]
    fn mob_nocharm_resists() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a golem", 10);
        f.world.entity_mut(m).insert(WorldKey { zone: 5, id: 7 });
        let mut proto =
            crate::commands::test_support::mob_proto(5, 7, mud_db::enums::MobProfession::Banker);
        proto.resistances = serde_json::json!({"CHARM": 0});
        f.world
            .resource_mut::<MobPrototypes>()
            .by_key
            .insert((5, 7), proto);
        assert_eq!(resist_reason(&mut f.world, m), Some(Resist::MobNoCharm));
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, HIT);
        assert_eq!(out, Outcome::Resisted(Resist::MobNoCharm));
        assert!(f.world.get_entity(m).is_ok());
    }

    #[test]
    fn a_partial_charm_resistance_is_not_nocharm() {
        let mut f = fx();
        let m = mob(&mut f, "a golem", 10);
        f.world.entity_mut(m).insert(WorldKey { zone: 5, id: 7 });
        let mut proto =
            crate::commands::test_support::mob_proto(5, 7, mud_db::enums::MobProfession::Banker);
        proto.resistances = serde_json::json!({"charm": 50});
        f.world
            .resource_mut::<MobPrototypes>()
            .by_key
            .insert((5, 7), proto);
        assert_eq!(resist_reason(&mut f.world, m), None);
    }

    fn charm(f: &mut Fx, pet: Entity, master: Entity) {
        f.world.entity_mut(pet).insert(Follower(master));
        f.world.spawn((
            EffectInstance {
                kind: 1,
                name: "charmed".into(),
                strength: 0,
                remaining_secs: 100,
                source: mud_world::EffectSource::Spell,
                ability_id: None,
            },
            mud_world::AppliedTo(pet),
        ));
    }

    #[test]
    fn charmed_pet_with_master_in_the_room_resists_but_not_when_master_is_away() {
        let mut f = fx();
        f.pk_on();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let (master, _mrx) = person(&mut f, "Druid", 40);
        let pet = mob(&mut f, "a wolf", 10);
        charm(&mut f, pet, master);
        assert_eq!(resist_reason(&mut f.world, pet), Some(Resist::CharmedPet));
        let out = banish(&mut f.world, &caster_of(c), pet, &P, false, HIT);
        assert_eq!(out, Outcome::Resisted(Resist::CharmedPet));
        assert!(f.world.get_entity(pet).is_ok());

        let away = f.home;
        f.world.entity_mut(master).insert(Located(away));
        assert_eq!(resist_reason(&mut f.world, pet), None);
    }

    #[test]
    fn an_uncharmed_follower_does_not_resist() {
        let mut f = fx();
        let (master, _mrx) = person(&mut f, "Druid", 40);
        let pet = mob(&mut f, "a hired guard", 10);
        f.world.entity_mut(pet).insert(Follower(master));
        assert_eq!(resist_reason(&mut f.world, pet), None);
    }

    #[test]
    fn nosummon_room_resists() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world.entity_mut(f.here).insert(NoSummonRoom);
        assert_eq!(resist_reason(&mut f.world, m), Some(Resist::RoomNoSummon));
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, HIT);
        assert_eq!(out, Outcome::Resisted(Resist::RoomNoSummon));
        assert!(f.world.get_entity(m).is_ok());
    }

    #[test]
    fn a_made_saving_throw_resists() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        let out = banish(&mut f.world, &caster_of(c), m, &P, true, HIT);
        assert_eq!(out, Outcome::Resisted(Resist::SavingThrow));
        assert!(drain(&mut rx).contains("resists you"));
        assert!(f.world.get_entity(m).is_ok());
    }

    #[test]
    fn a_missed_roll_is_nothing_happens_and_the_mob_does_not_retaliate() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let (b, mut brx) = person(&mut f, "Bystander", 5);
        let m = mob(&mut f, "a demon", 10);
        // 50 + 50 - 10 = 90: not above 100.
        let miss = Rolls {
            success: 50,
            gear: 100,
        };
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, miss);
        assert_eq!(out, Outcome::Failed);
        let text = drain(&mut rx);
        assert!(text.contains("Nothing happens."), "{text}");
        assert!(!text.contains("resists"), "{text}");
        assert!(drain(&mut brx).contains("Nothing happens."));
        assert!(f.world.get_entity(m).is_ok());
        assert!(f.world.get::<Fighting>(m).is_none());
        let _ = b;
    }

    #[test]
    fn the_roll_boundary_decides() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        // 60 + 50 - 10 = 100: still no.
        let edge = Rolls {
            success: 60,
            gear: 0,
        };
        assert_eq!(
            banish(&mut f.world, &caster_of(c), m, &P, false, edge),
            Outcome::Failed
        );
        let hit = Rolls {
            success: 61,
            gear: 0,
        };
        assert!(matches!(
            banish(&mut f.world, &caster_of(c), m, &P, false, hit),
            Outcome::MobBanished { .. }
        ));
    }

    #[test]
    fn the_threshold_comes_from_the_params() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        let easy = BanishParams {
            success_threshold: 0,
            ..P
        };
        let low = Rolls {
            success: 0,
            gear: 0,
        };
        assert!(matches!(
            banish(&mut f.world, &caster_of(c), m, &easy, false, low),
            Outcome::MobBanished { .. }
        ));
    }

    #[test]
    fn banished_mob_with_a_high_gear_roll_destroys_everything_it_had() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let (_b, mut brx) = person(&mut f, "Bystander", 5);
        let m = mob(&mut f, "a demon", 10);
        let worn = item(&mut f, m, true);
        let carried = item(&mut f, m, false);
        let bag = item(&mut f, m, false);
        let inner = item(&mut f, bag, false);
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, HIT);
        assert_eq!(
            out,
            Outcome::MobBanished {
                gear_destroyed: true
            }
        );
        assert!(f.world.get_entity(m).is_err());
        for e in [worn, carried, bag, inner] {
            assert!(f.world.get_entity(e).is_err(), "item should be destroyed");
        }
        // TO_NOTVICT: the bystander sees it, the caster does not.
        assert!(drain(&mut brx).contains("A demon disappears in a flash of light!"));
        assert!(!drain(&mut rx).contains("disappears"));
    }

    #[test]
    fn banished_mob_with_a_low_gear_roll_drops_everything_on_the_floor() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        let worn = item(&mut f, m, true);
        let carried = item(&mut f, m, false);
        let low_gear = Rolls {
            success: 100,
            gear: 66,
        };
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, low_gear);
        assert_eq!(
            out,
            Outcome::MobBanished {
                gear_destroyed: false
            }
        );
        assert!(f.world.get_entity(m).is_err());
        for e in [worn, carried] {
            assert_eq!(f.world.get::<Located>(e).map(|l| l.0), Some(f.here));
            assert!(f.world.get::<EquippedSlot>(e).is_none());
        }
    }

    #[test]
    fn gear_destruction_threshold_is_data_driven() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        let worn = item(&mut f, m, true);
        let never = BanishParams {
            gear_destroy_threshold: 1000,
            ..P
        };
        let out = banish(&mut f.world, &caster_of(c), m, &never, false, HIT);
        assert_eq!(
            out,
            Outcome::MobBanished {
                gear_destroyed: false
            }
        );
        assert!(f.world.get_entity(worn).is_ok());
    }

    #[test]
    fn a_banished_player_is_sent_home_not_refused() {
        let mut f = fx();
        f.pk_on();
        let (c, mut crx) = person(&mut f, "Cleric", 50);
        let (v, mut vrx) = person(&mut f, "Victim", 10);
        let (_b, mut brx) = person(&mut f, "Bystander", 5);
        f.world.entity_mut(v).insert(RecallPoint(f.home));
        let out = banish(&mut f.world, &caster_of(c), v, &P, false, HIT);
        assert_eq!(out, Outcome::PlayerSentHome);
        assert_eq!(f.world.get::<Located>(v).map(|l| l.0), Some(f.home));
        let vtext = drain(&mut vrx);
        assert!(vtext.contains("You are banished!"), "{vtext}");
        assert!(drain(&mut crx).contains("You look at Victim and shout"));
        assert!(drain(&mut brx).contains("Victim disappears in a flash of light!"));
    }

    #[test]
    fn a_banished_player_falls_back_when_home_is_unset_and_when_home_is_barred() {
        let mut f = fx();
        f.pk_on();
        let (c, _crx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        // No recall point, no race start, no void: nowhere to go.
        let out = banish(&mut f.world, &caster_of(c), v, &P, false, HIT);
        assert_eq!(out, Outcome::NoHome);
        assert_eq!(f.world.get::<Located>(v).map(|l| l.0), Some(f.here));

        // The recall point is barred to mortals; the Void is the fallback.
        let void = f.world.spawn((Room, WorldKey { zone: 0, id: 0 })).id();
        f.world
            .resource_mut::<WorldKeyIndex>()
            .rooms
            .insert((0, 0), void);
        f.world
            .entity_mut(f.home)
            .insert(mud_world::EntryRestriction("return false".into()));
        f.world.entity_mut(v).insert(RecallPoint(f.home));
        let out = banish(&mut f.world, &caster_of(c), v, &P, false, HIT);
        assert_eq!(out, Outcome::PlayerSentHome);
        assert_eq!(f.world.get::<Located>(v).map(|l| l.0), Some(void));
    }

    #[test]
    fn banishing_a_fighting_player_ends_the_fight_and_dismounts() {
        let mut f = fx();
        f.pk_on();
        let (c, _crx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        let foe = mob(&mut f, "an orc", 5);
        let steed = mob(&mut f, "a horse", 5);
        f.world
            .entity_mut(v)
            .insert((RecallPoint(f.home), Fighting(foe), Mounted(steed)));
        f.world.entity_mut(steed).insert(RiddenBy(v));
        f.world.entity_mut(foe).insert(Fighting(v));
        let out = banish(&mut f.world, &caster_of(c), v, &P, false, HIT);
        assert_eq!(out, Outcome::PlayerSentHome);
        assert!(f.world.get::<Fighting>(v).is_none());
        assert!(f.world.get::<Fighting>(foe).is_none());
        assert!(f.world.get::<Mounted>(v).is_none());
        assert!(f.world.get::<RiddenBy>(steed).is_none());
    }

    #[test]
    fn casting_it_on_yourself_does_nothing() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let out = banish(&mut f.world, &caster_of(c), c, &P, false, HIT);
        assert_eq!(out, Outcome::SelfTarget);
        assert_eq!(drain(&mut rx), "");
    }

    #[test]
    fn an_invisible_banished_player_is_not_announced_to_those_who_cannot_see_them() {
        let mut f = fx();
        f.pk_on();
        let (c, _crx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        let (_b, mut brx) = person(&mut f, "Bystander", 5);
        f.world
            .entity_mut(v)
            .insert((RecallPoint(f.home), mud_world::Invisible));
        let out = banish(&mut f.world, &caster_of(c), v, &P, false, HIT);
        assert_eq!(out, Outcome::PlayerSentHome);
        let text = drain(&mut brx);
        // The shout is unconditional (legacy hide_invisible = false) ...
        assert!(text.contains("I banish thee!"), "{text}");
        // ... the flash of light is not.
        assert!(!text.contains("disappears in a flash"), "{text}");
    }

    // -- attack_ok --------------------------------------------------------

    #[test]
    fn a_mortal_cannot_banish_a_non_pk_player() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        f.world.entity_mut(v).insert(RecallPoint(f.home));
        let out = banish(&mut f.world, &caster_of(c), v, &P, false, HIT);
        assert_eq!(out, Outcome::NotAllowed);
        assert_eq!(f.world.get::<Located>(v).map(|l| l.0), Some(f.here));
        let text = drain(&mut rx);
        assert!(text.contains("You must turn on PK first"), "{text}");
        assert!(!text.contains("I banish thee"), "{text}");
    }

    #[test]
    fn two_pk_flagged_players_may_banish_each_other() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        for e in [c, v] {
            f.world.entity_mut(e).insert(mud_world::PlayerFlags(vec![
                mud_db::enums::PlayerFlag::PkEnabled,
            ]));
        }
        f.world.entity_mut(v).insert(RecallPoint(f.home));
        assert_eq!(
            banish(&mut f.world, &caster_of(c), v, &P, false, HIT),
            Outcome::PlayerSentHome
        );
    }

    #[test]
    fn arena_rooms_allow_player_versus_player() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        f.world.entity_mut(f.here).insert(mud_world::ArenaRoom);
        f.world.entity_mut(v).insert(RecallPoint(f.home));
        assert_eq!(
            banish(&mut f.world, &caster_of(c), v, &P, false, HIT),
            Outcome::PlayerSentHome
        );
    }

    #[test]
    fn a_peaceful_mob_such_as_a_shopkeeper_cannot_be_banished() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "the shopkeeper", 10);
        f.world
            .entity_mut(m)
            .insert(MobBehaviors(vec![MobBehavior::Peaceful]));
        let out = banish(&mut f.world, &caster_of(c), m, &P, false, HIT);
        assert_eq!(out, Outcome::NotAllowed);
        assert!(f.world.get_entity(m).is_ok());
        let text = drain(&mut rx);
        assert!(text.contains("calm, peaceful feeling"), "{text}");
    }

    #[test]
    fn a_peaceful_room_forbids_banishing() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world.entity_mut(f.here).insert(mud_world::PeacefulRoom);
        assert_eq!(
            banish(&mut f.world, &caster_of(c), m, &P, false, HIT),
            Outcome::NotAllowed
        );
        assert!(drain(&mut rx).contains("ashamed"));
    }

    #[test]
    fn another_players_pet_cannot_be_banished_but_your_own_can() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let (master, _mrx) = person(&mut f, "Druid", 40);
        let pet = mob(&mut f, "a wolf", 10);
        charm(&mut f, pet, master);
        let out = banish(&mut f.world, &caster_of(c), pet, &P, false, HIT);
        assert_eq!(out, Outcome::NotAllowed);
        assert!(drain(&mut rx).contains("someone else's pet"));
        assert!(f.world.get_entity(pet).is_ok());
        // Your own pet: allowed past attack_ok; the charm-with-master rule
        // then makes it resist (legacy).
        assert_eq!(
            banish(&mut f.world, &caster_of(master), pet, &P, false, HIT),
            Outcome::Resisted(Resist::CharmedPet)
        );
    }

    #[test]
    fn a_pet_never_attacks_its_master_and_the_dead_are_off_limits() {
        let mut f = fx();
        let (master, _mrx) = person(&mut f, "Druid", 40);
        let pet = mob(&mut f, "a wolf", 10);
        f.world.entity_mut(pet).insert(Follower(master));
        assert!(!attack_ok(&mut f.world, pet, master, false));
        let (c, _crx) = person(&mut f, "Cleric", 50);
        f.pk_on();
        let (v, _vrx) = person(&mut f, "Victim", 10);
        f.world.entity_mut(v).insert(mud_world::Ghost);
        assert!(!attack_ok(&mut f.world, c, v, false));
    }

    // -- mob levels, respawn, mounts --------------------------------------

    #[test]
    fn the_victim_level_comes_from_the_mob_prototype_not_a_profile() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let weak = mob(&mut f, "an imp", 1);
        let strong = mob(&mut f, "an archdemon", 100);
        assert!(f.world.get::<Profile>(weak).is_none());
        // roll 60 + skill 50 - level: lands against level 1, not against 100.
        let roll = Rolls {
            success: 60,
            gear: 0,
        };
        assert!(matches!(
            banish(&mut f.world, &caster_of(c), weak, &P, false, roll),
            Outcome::MobBanished { .. }
        ));
        assert_eq!(
            banish(&mut f.world, &caster_of(c), strong, &P, false, roll),
            Outcome::Failed
        );
    }

    #[test]
    fn a_banished_mob_starts_its_reset_row_respawn_timer() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world.entity_mut(m).insert(FromMobReset(42));
        f.world.insert_resource(crate::TickCount(1234));
        f.world
            .insert_resource(crate::respawn::MobRespawnTimers::default());
        banish(&mut f.world, &caster_of(c), m, &P, false, HIT);
        assert!(f.world.get_entity(m).is_err());
        assert_eq!(
            f.world
                .resource::<crate::respawn::MobRespawnTimers>()
                .last_death_tick
                .get(&42),
            Some(&1234)
        );
    }

    #[test]
    fn banishing_a_mount_unseats_its_rider() {
        let mut f = fx();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let (rider, _rrx) = person(&mut f, "Rider", 10);
        let steed = mob(&mut f, "a horse", 5);
        f.world.entity_mut(rider).insert(Mounted(steed));
        f.world.entity_mut(steed).insert(RiddenBy(rider));
        f.pk_on();
        banish(&mut f.world, &caster_of(c), steed, &P, false, HIT);
        assert!(f.world.get_entity(steed).is_err());
        assert!(f.world.get::<Mounted>(rider).is_none());
    }

    #[test]
    fn a_violent_spell_at_a_peaceful_mob_is_refused_by_the_cast_path() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "the shopkeeper", 10);
        f.world.entity_mut(m).insert((
            MobBehaviors(vec![MobBehavior::Peaceful]),
            mud_world::Keywords(vec!["shopkeeper".into()]),
        ));
        banish_spell(
            &mut f,
            c,
            serde_json::json!({"success_threshold": -1000}),
            false,
        );
        cast_at(&mut f, c, "shopkeeper");
        let text = drain(&mut rx);
        assert!(text.contains("calm, peaceful feeling"), "{text}");
        assert!(!text.contains("I banish thee"), "{text}");
        assert!(f.world.get_entity(m).is_ok());
    }

    #[test]
    fn a_violent_spell_at_a_non_pk_player_is_refused_by_the_cast_path() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let (v, _vrx) = person(&mut f, "Victim", 10);
        f.world.entity_mut(v).insert((
            RecallPoint(f.home),
            mud_world::Keywords(vec!["victim".into()]),
        ));
        banish_spell(
            &mut f,
            c,
            serde_json::json!({"success_threshold": -1000}),
            false,
        );
        cast_at(&mut f, c, "victim");
        assert!(drain(&mut rx).contains("You must turn on PK first"));
        assert_eq!(f.world.get::<Located>(v).map(|l| l.0), Some(f.here));
    }

    // -- end to end through the spell pipeline ----------------------------

    const ABILITY: i32 = 127;
    const EFFECT: i32 = 20;

    /// Install BANISH (data-driven params, optional WILL save) and teach it
    /// to `caster` at full proficiency.
    fn banish_spell(f: &mut Fx, caster: Entity, params: serde_json::Value, save: bool) {
        use mud_db::abilities::AbilityKind;
        let mut abilities = mud_world::AbilityCatalog::default();
        let mut def =
            crate::commands::test_support::ability_def(ABILITY, "Banish", AbilityKind::Spell);
        def.cast_time_rounds = 0;
        abilities.by_name.insert("banish".to_string(), def);
        abilities
            .effects_for
            .insert(ABILITY, vec![(EFFECT, Some(params))]);
        if save {
            abilities.saves.insert(
                ABILITY,
                mud_world::SavingThrow {
                    save_type: "WILL".into(),
                    // Total is d20 + level >= 1: always saves.
                    dc_formula: "0".into(),
                    on_save_action: serde_json::json!("NEGATE"),
                },
            );
        }
        f.world.insert_resource(abilities);
        let mut effects = mud_world::EffectCatalog::default();
        effects.by_id.insert(
            EFFECT,
            mud_world::EffectDef {
                id: EFFECT,
                name: "extract".into(),
                description: None,
                effect_type: "extract".into(),
                tags: vec![],
                presence_override: None,
                default_params: serde_json::json!({"target": "mob"}),
                prevents_speaking: false,
                prevents_casting: false,
                prevents_movement: false,
                on_apply: None,
                on_tick: None,
                on_remove: None,
            },
        );
        f.world.insert_resource(effects);
        f.world
            .insert_resource(mud_world::WeatherCatalog::default());
        f.world.insert_resource(mud_world::RaceCatalog::default());
        f.world
            .entity_mut(caster)
            .insert(mud_world::KnownAbilities {
                entries: vec![(ABILITY, 1000, true)],
            });
    }

    fn cast_at(f: &mut Fx, caster: Entity, target: &str) {
        crate::commands::invoke_ability_with(
            &mut f.world,
            caster,
            &format!("banish {target}"),
            mud_db::abilities::AbilityKind::Spell,
            "cast",
            false,
            true,
            true,
            None,
        );
    }

    #[test]
    fn the_spell_banishes_a_mob_and_prints_only_the_legacy_messages() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world
            .entity_mut(m)
            .insert(mud_world::Keywords(vec!["demon".into()]));
        let worn = item(&mut f, m, true);
        // A threshold nothing can miss, and a gear threshold nothing can pass.
        banish_spell(
            &mut f,
            c,
            serde_json::json!({"success_threshold": -1000, "gear_destroy_threshold": 1000}),
            false,
        );
        cast_at(&mut f, c, "demon");
        let out = drain(&mut rx);
        assert!(out.contains("You look at a demon and shout"), "{out}");
        assert!(!out.contains("You cast"), "no generic header: {out}");
        assert!(!out.contains("effect(s)"), "{out}");
        assert!(f.world.get_entity(m).is_err(), "{out}");
        assert_eq!(f.world.get::<Located>(worn).map(|l| l.0), Some(f.here));
    }

    #[test]
    fn the_spell_can_miss_on_the_data_driven_threshold() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world
            .entity_mut(m)
            .insert(mud_world::Keywords(vec!["demon".into()]));
        banish_spell(
            &mut f,
            c,
            serde_json::json!({"success_threshold": 1000}),
            false,
        );
        cast_at(&mut f, c, "demon");
        let out = drain(&mut rx);
        assert!(out.contains("Nothing happens."), "{out}");
        assert!(f.world.get_entity(m).is_ok());
    }

    #[test]
    fn the_spell_save_resists_with_the_legacy_text_and_the_mob_fights_back() {
        let mut f = fx();
        let (c, mut rx) = person(&mut f, "Cleric", 50);
        let m = mob(&mut f, "a demon", 10);
        f.world
            .entity_mut(m)
            .insert(mud_world::Keywords(vec!["demon".into()]));
        banish_spell(
            &mut f,
            c,
            serde_json::json!({"success_threshold": -1000}),
            true,
        );
        cast_at(&mut f, c, "demon");
        let out = drain(&mut rx);
        assert!(out.contains("A demon resists you."), "{out}");
        assert!(out.contains("Nothing happens."), "{out}");
        assert!(f.world.get_entity(m).is_ok());
        assert_eq!(f.world.get::<Fighting>(m).map(|x| x.0), Some(c));
    }

    #[test]
    fn the_spell_sends_a_player_home() {
        let mut f = fx();
        f.pk_on();
        let (c, _rx) = person(&mut f, "Cleric", 50);
        let (v, mut vrx) = person(&mut f, "Victim", 10);
        f.world.entity_mut(v).insert((
            RecallPoint(f.home),
            mud_world::Keywords(vec!["victim".into()]),
        ));
        banish_spell(
            &mut f,
            c,
            serde_json::json!({"success_threshold": -1000}),
            false,
        );
        cast_at(&mut f, c, "victim");
        assert_eq!(f.world.get::<Located>(v).map(|l| l.0), Some(f.home));
        assert!(drain(&mut vrx).contains("You are banished!"));
    }
}
