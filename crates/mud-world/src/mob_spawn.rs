//! The one place a mob entity is built from a `MobProto`.
//!
//! Every path that puts a mob into the world (boot reset pass, zone
//! respawn, Lua `spawn_mob_proto`, admin API and in-game spawn, shop
//! pets, mount summons, summon / animate spells) goes through
//! [`spawn_mob_from_proto`], so prototype state (combat stats, latent
//! components, shop / trigger / behavior markers, `MobDefaultEffects` and
//! the race's `RaceEffects`)
//! can not drift between them. Legacy `read_mobile` behaves the same:
//! every instance is cloned from its prototype, affects included.
//! Callers layer their own extras (follower link, renamed display
//! name, scaled HP) on top of the returned entity.

use bevy_ecs::prelude::*;

use crate::components::{
    AttachedTriggers, Description, ExamineText, FromMobReset, Health, Keywords, LifeForceTag,
    Located, Mob, MobBehaviors, MobTraits, Mountable, MovementModeTag, MovementPoints, Named,
    NaturalAttackType, NaturalDamage, Posture, Shopkeeper, Sized, SpellSlots, WorldKey,
};
use crate::mob_ai_rules::{MobAi, MobAiData};
use crate::resources::{MobProto, RuntimeConfig, ShopCatalog, SpellSlotData, TriggerCatalog};

/// Spawn a fresh mob from `proto` into `room` and apply its
/// `MobDefaultEffects`. `reset_id` tags the instance with the
/// `MobResets` row that owns it (boot / respawn paths); `None` for
/// everything else.
///
/// Spawn posture follows the proto's default position. HP is rolled
/// from the proto; callers that need a different pool overwrite
/// `Health` afterwards. Does not fire `LOAD` triggers, announce the
/// arrival, equip gear or run aggro: those are the caller's turn
/// because they depend on the surrounding system's borrows.
pub fn spawn_mob_from_proto(
    world: &mut World,
    proto: &MobProto,
    room: Entity,
    reset_id: Option<i32>,
) -> Entity {
    let proto_key = (proto.zone_id, proto.id);
    let hp = proto.rolled_hp();
    let shop_key = world
        .get_resource::<ShopCatalog>()
        .and_then(|c| c.keeper_index.get(&proto_key).copied());
    let trigger_keys = world
        .get_resource::<TriggerCatalog>()
        .and_then(|c| c.mob_attachments.get(&proto_key).cloned());
    // Everything every mob has goes in one spawn: each later `insert` is a
    // structural move, and the first move out of each of a prod world's
    // thousands of archetypes builds the target archetype and its table.
    let base = (
        (
            Mob,
            Named {
                name: proto.name.clone(),
            },
            Keywords(proto.keywords.clone()),
            Description(proto.room_description.clone()),
            WorldKey {
                zone: proto.zone_id,
                id: proto.id,
            },
            Located(room),
            Health { hp, max: hp },
            proto.derived_combat_stats(),
            Posture(Posture::from_default_position(proto.default_position)),
            NaturalDamage {
                num: proto.damage_dice_num,
                size: proto.damage_dice_size,
                bonus: proto.damage_dice_bonus,
            },
        ),
        (
            Sized(proto.size),
            LifeForceTag(proto.life_force),
            NaturalAttackType(proto.damage_type),
            MobTraits(proto.traits.clone()),
            MovementModeTag(proto.default_movement_mode),
        ),
    );
    let mut em = match reset_id {
        Some(reset_id) => world.spawn((base, FromMobReset(reset_id))),
        None => world.spawn(base),
    };
    // Zero movement points = "unconstrained" per legacy.
    if proto.move_points > 0 {
        em.insert(MovementPoints {
            current: proto.move_points,
            max: proto.move_points,
        });
    }
    if let Some((shop_zone_id, shop_id)) = shop_key {
        em.insert(Shopkeeper {
            shop_zone_id,
            shop_id,
        });
    }
    if let Some(keys) = trigger_keys {
        em.insert(AttachedTriggers(keys));
    }
    if !proto.behaviors.is_empty() {
        em.insert(MobBehaviors(proto.behaviors.clone()));
    }
    if !proto.examine_description.trim().is_empty() {
        em.insert(ExamineText(proto.examine_description.clone()));
    }
    if proto.is_mountable() {
        em.insert(Mountable);
    }
    // Legacy `PERC:` / `HIDE:` (`Mobs.perception` / `Mobs.concealment`):
    // how well the mob spots hidden characters, and how well it hides.
    if proto.perception != 0 {
        em.insert(crate::components::Perception(proto.perception));
    }
    if proto.concealment > 0 {
        em.insert(crate::components::Hiddenness(
            proto.concealment.min(crate::components::MAX_HIDDENNESS),
        ));
    }
    let mob = em.id();
    let default_flags = crate::mob_effects::apply_mob_default_effects(world, mob, proto_key);
    crate::mob_effects::apply_race_effects_to_new_mob(world, mob, &proto.race, &default_flags);
    // Spawned into an air room without wings (or a fly effect): it drops.
    crate::movement::begin_fall_if_unsupported(world, mob);
    attach_mob_ai(world, mob, proto);
    mob
}

/// Whether the mob spellcasting / skill AI is switched on
/// (`GameConfig mob_ai.enabled`, default off).
#[must_use]
pub fn mob_ai_enabled(world: &World) -> bool {
    world
        .get_resource::<RuntimeConfig>()
        .is_some_and(|c| c.get_bool("mob_ai", "enabled", false))
}

/// Mob AI part of spawning, a no-op while `mob_ai.enabled` is off (so a
/// disabled build spawns byte-identical mobs). When on: a mob whose AI is
/// active (see [`MobAiData::ai_active`]) gets
///   * [`MobAi`] with its shared rule list (class rules for `CLASS`, its own
///     `MobAbilities` rows for `CUSTOM`), when it has any, and
///   * [`SpellSlots`] when its class has spell circles open at its level, the
///     same `SpellSlotData::slots_for` the player `slots` command uses, so a
///     mob's slot budget per circle is the player progression for its level.
///
/// `ai_mode = NONE`, and `NoClassAi` on a `CLASS` mob, get neither.
fn attach_mob_ai(world: &mut World, mob: Entity, proto: &MobProto) {
    if !mob_ai_enabled(world) {
        return;
    }
    let Some(data) = world.get_resource::<MobAiData>() else {
        return;
    };
    if !data.ai_active(proto) {
        return;
    }
    let rules = data.rules_for(proto);
    let caster = proto.class_id.is_some_and(|class_id| {
        world.get_resource::<SpellSlotData>().is_some_and(|d| {
            d.slots_for(class_id, proto.level)
                .iter()
                .any(|(_, slots)| *slots > 0)
        })
    });
    let mut em = world.entity_mut(mob);
    if let Some(rules) = rules {
        em.insert(MobAi::new(rules));
    }
    if caster {
        em.insert(SpellSlots::default());
    }
}
