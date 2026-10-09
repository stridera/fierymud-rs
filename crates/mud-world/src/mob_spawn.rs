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
    NaturalAttackType, NaturalDamage, Posture, Shopkeeper, Sized, WorldKey,
};
use crate::resources::{MobProto, ShopCatalog, TriggerCatalog};

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
    let mut em = world.spawn((
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
    ));
    if let Some(reset_id) = reset_id {
        em.insert(FromMobReset(reset_id));
    }
    em.insert((
        Sized(proto.size),
        LifeForceTag(proto.life_force),
        NaturalAttackType(proto.damage_type),
        MobTraits(proto.traits.clone()),
        MovementModeTag(proto.default_movement_mode),
    ));
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
    crate::mob_effects::apply_mob_default_effects(world, mob, proto_key);
    crate::mob_effects::apply_race_effects(world, mob, &proto.race);
    // Spawned into an air room without wings (or a fly effect): it drops.
    crate::movement::begin_fall_if_unsupported(world, mob);
    mob
}
