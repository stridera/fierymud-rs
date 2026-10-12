//! Composition-based interaction restrictions (issue #137, legacy
//! `RIGID` / `SOLIDCHAR`): a body that is not solid (water, mist, air,
//! fire, ether) cannot pick things up, work doors, trade, or ride, and
//! solid people cannot grab, ride, or hand things to it.
//!
//! Every gate here returns `true` after telling the actor why when the
//! action is refused. Staff (`mud_world::bypasses_restrictions`, legacy
//! `GET_LEVEL >= LVL_IMMORT`) are never refused. The phase of each
//! composition is a runtime invariant (`Composition::phase`), so the
//! rules are code; which *spells* need a solid body is data (the
//! `requires_rigid` ability tag, see [`strip_unsuitable_effects`]).

use bevy_ecs::prelude::*;
use mud_db::enums::Composition;
use mud_world::{
    AppliedTo, EffectInstance, Located, actor_composition, actor_is_illusory, actor_is_rigid,
    actor_is_solid_char, bypasses_restrictions, composition_state_phrase,
};

use super::{
    Pronouns, broadcast_room_except_rendered, name_of, name_or, send_msg, send_rendered, send_to,
};

/// Legacy `!RIGID(ch) && GET_LEVEL(ch) < LVL_IMMORT`.
fn fluid_mortal(world: &World, actor: Entity) -> bool {
    !actor_is_rigid(world, actor) && !bypasses_restrictions(world, actor)
}

/// Legacy `!SOLIDCHAR(ch) && GET_LEVEL(ch) < LVL_IMMORT`.
fn insubstantial_mortal(world: &World, actor: Entity) -> bool {
    !actor_is_solid_char(world, actor) && !bypasses_restrictions(world, actor)
}

/// `get` from the room floor (legacy `perform_get_from_room` entry gate).
pub(crate) fn refuse_get_from_room(world: &World, actor: Entity) -> bool {
    if !fluid_mortal(world, actor) {
        return false;
    }
    send_to(
        world,
        actor,
        "In your fluid state, you can't pick things up.\r\n",
    );
    true
}

/// `get` out of a container that is not in the actor's own hands.
pub(crate) fn refuse_get_from_container(world: &World, actor: Entity, container: Entity) -> bool {
    if !fluid_mortal(world, actor) || world.get::<Located>(container).map(|l| l.0) == Some(actor) {
        return false;
    }
    send_to(
        world,
        actor,
        "You can't handle solid objects in your condition.\r\n",
    );
    true
}

/// Shop `buy` / `sell` (legacy `shopping_buy`).
pub(crate) fn refuse_shop_trade(world: &World, actor: Entity) -> bool {
    if !fluid_mortal(world, actor) {
        return false;
    }
    send_to(
        world,
        actor,
        "You can't handle solid objects in your condition.\r\n",
    );
    true
}

/// `steal` (legacy `do_steal`).
pub(crate) fn refuse_steal(world: &World, actor: Entity) -> bool {
    if !fluid_mortal(world, actor) {
        return false;
    }
    send_to(
        world,
        actor,
        "You can't handle objects in your condition.\r\n",
    );
    true
}

/// `open` / `close` / `lock` / `unlock` / `pick` on a door (legacy
/// `perform_door_action`).
pub(crate) fn refuse_door_action(world: &World, actor: Entity) -> bool {
    if !insubstantial_mortal(world, actor) {
        return false;
    }
    let state = composition_state_phrase(world, actor);
    send_rendered(
        world,
        actor,
        &format!("You can't operate doors in your {state} state.\r\n"),
    );
    true
}

/// The five door verbs on an openable object: only one in the actor's own
/// inventory can be worked (legacy `do_gen_door`).
pub(crate) fn refuse_object_manipulation(world: &World, actor: Entity, object: Entity) -> bool {
    if !insubstantial_mortal(world, actor)
        || world.get::<Located>(object).map(|l| l.0) == Some(actor)
    {
        return false;
    }
    let state = composition_state_phrase(world, actor);
    send_rendered(
        world,
        actor,
        &format!("You can't manipulate solid objects in your {state} state.\r\n"),
    );
    true
}

/// Legacy `perform_drop` / `perform_put` flavour: what a fluid hand lets go
/// of turns solid again.
pub(crate) fn note_item_turns_solid(world: &World, actor: Entity, item_name: &str) {
    if actor_is_rigid(world, actor) {
        return;
    }
    send_msg(
        world,
        actor,
        format!("{item_name} becomes solid again as it leaves your grasp.\r\n"),
    );
}

/// What a `give` came to once the givers' and receivers' bodies are
/// considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GiveFate {
    /// Hand it over as usual.
    Handed,
    /// The receiver could not hold it: it is on the floor now, the lines
    /// are sent, and the give is over.
    Fell,
}

/// Legacy `perform_give` composition rules. Two fluid bodies trade like
/// anyone (unless the receiver is an illusion); a fluid giver's item turns
/// solid as it leaves; a solid giver's gift slips through a fluid or
/// illusory receiver and drops.
pub(crate) fn give_item_fate(
    world: &mut World,
    giver: Entity,
    receiver: Entity,
    item: Entity,
) -> GiveFate {
    let item_name = name_of(world, item);
    let both_fluid = !actor_is_rigid(world, giver)
        && !actor_is_rigid(world, receiver)
        && !actor_is_illusory(world, receiver);
    if !both_fluid {
        note_item_turns_solid(world, giver, &item_name);
    }
    if !(actor_is_solid_char(world, giver)
        && !actor_is_solid_char(world, receiver)
        && !bypasses_restrictions(world, giver)
        && !bypasses_restrictions(world, receiver))
    {
        return GiveFate::Handed;
    }
    let Some(room) = world.get::<Located>(giver).map(|l| l.0) else {
        return GiveFate::Handed;
    };
    let giver_name = name_of(world, giver);
    let receiver_name = name_of(world, receiver);
    let (self_fall, room_fall) = if crate::flight::is_splashy(world, room) {
        ("it simply falls into the water", "it falls into the water")
    } else {
        ("it simply falls to the ground", "it falls to the ground")
    };
    send_msg(
        world,
        receiver,
        format!("{giver_name} gives you {item_name}, but you can't hold onto it.\r\n"),
    );
    send_msg(
        world,
        giver,
        format!("You hand {item_name} to {receiver_name}, but {self_fall}.\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[giver, receiver],
        &format!("{giver_name} gives {item_name} to {receiver_name}, but {room_fall}.\r\n"),
    );
    if world.get::<Located>(item).is_some() {
        world.entity_mut(item).insert(Located(room));
    }
    GiveFate::Fell
}

/// `give <coins>`: coins handed to something that is not solid fall
/// through (legacy `perform_give_money`). Returns whether they did; when
/// so the giver's purse is debited and the loose pile on the floor is
/// what was taken. A staff giver (`debit` false) keeps their purse and
/// so leaves no pile: the fall is only described, no coins are conjured.
/// When false nothing happened.
pub(crate) fn coins_fall_through(
    world: &mut World,
    giver: Entity,
    receiver: Entity,
    copper: i64,
    debit: bool,
) -> bool {
    if actor_is_solid_char(world, receiver) || bypasses_restrictions(world, receiver) {
        return false;
    }
    let Some(room) = world.get::<Located>(giver).map(|l| l.0) else {
        return false;
    };
    if debit && let Some(mut wealth) = world.get_mut::<mud_world::Wealth>(giver) {
        wealth.0 = wealth.0.saturating_sub(copper);
    }
    let giver_name = name_of(world, giver);
    let receiver_name = name_of(world, receiver);
    let water = crate::flight::is_splashy(world, room);
    send_msg(
        world,
        giver,
        format!("The coins seem to fall right through {receiver_name}'s hands!\r\n"),
    );
    let fate = if water {
        "fell into the water"
    } else {
        "fell to the ground"
    };
    broadcast_room_except_rendered(
        world,
        room,
        &[giver, receiver],
        &format!("{giver_name} tries to give some coins to {receiver_name}, but they {fate}!\r\n"),
    );
    if debit {
        crate::item_decay::spawn_loose_coin_pile(world, room, copper);
    }
    true
}

/// The three lines of a failed contact: actor, other party, bystanders.
fn tell_three(
    world: &mut World,
    actor: Entity,
    other: Entity,
    to_actor: &str,
    to_other: &str,
    to_room: &str,
) {
    send_msg(world, actor, format!("{to_actor}\r\n"));
    send_msg(world, other, format!("{to_other}\r\n"));
    if let Some(room) = world.get::<Located>(actor).map(|l| l.0) {
        broadcast_room_except_rendered(world, room, &[actor, other], &format!("{to_room}\r\n"));
    }
}

/// `drag <object>`: a fluid body cannot handle a solid object (legacy
/// `do_drag`).
pub(crate) fn refuse_drag_object(world: &World, actor: Entity) -> bool {
    if !fluid_mortal(world, actor) {
        return false;
    }
    let state = composition_state_phrase(world, actor);
    send_rendered(
        world,
        actor,
        &format!("You can't handle solid objects in your {state} condition.\r\n"),
    );
    true
}

/// `drag <character>`: grabbing across the solid / fluid divide fails.
pub(crate) fn refuse_drag_body(world: &mut World, actor: Entity, other: Entity) -> bool {
    if bypasses_restrictions(world, actor) {
        return false;
    }
    let actor_rigid = actor_is_rigid(world, actor);
    let other_rigid = actor_is_rigid(world, other);
    if actor_rigid == other_rigid {
        return false;
    }
    let actor_name = name_of(world, actor);
    let other_name = name_of(world, other);
    if actor_rigid {
        let comp = actor_composition(world, other);
        let (open, mass, adj) = (comp.color_open_tag(), comp.mass_noun(), comp.adjective());
        let they = Pronouns::of(world, actor).subjective;
        tell_three(
            world,
            actor,
            other,
            &format!("{other_name}'s {open}{mass}</> seems to pass right through your fingers."),
            &format!(
                "{actor_name} tries to grab you, but {they} can't get a grip on your {open}{adj}</> flesh."
            ),
            &format!(
                "{actor_name} tries to grab {other_name}, but {they} can't get a grip on {other_name}'s {open}{adj}</> flesh."
            ),
        );
    } else {
        let comp = actor_composition(world, actor);
        let (open, adj) = (comp.color_open_tag(), comp.adjective());
        let his = Pronouns::of(world, actor).possessive;
        tell_three(
            world,
            actor,
            other,
            &format!("You can't get hold of {other_name} with your {open}{adj}</> grip."),
            &format!(
                "{actor_name} tries to grab you, but {his} {open}{adj}</> fingers can't get a grip."
            ),
            &format!(
                "{actor_name} tries to grab {other_name}, but {his} {open}{adj}</> fingers can't get a grip."
            ),
        );
    }
    true
}

/// `mount`: only a solid rider can sit a solid mount (legacy `do_mount`).
pub(crate) fn refuse_mount(world: &mut World, rider: Entity, mount: Entity) -> bool {
    if bypasses_restrictions(world, rider) {
        return false;
    }
    let rider_rigid = actor_is_rigid(world, rider);
    let mount_rigid = actor_is_rigid(world, mount);
    if rider_rigid == mount_rigid {
        return false;
    }
    let rider_name = name_of(world, rider);
    let mount_name = name_of(world, mount);
    let mount_it = Pronouns::of(world, mount);
    let rider_it = Pronouns::of(world, rider);
    if rider_rigid {
        if actor_composition(world, mount) == Composition::Ether {
            tell_three(
                world,
                rider,
                mount,
                &format!(
                    "You go to mount {mount_name}, but you pass right through {}.",
                    mount_it.objective
                ),
                &format!("{rider_name} tries to mount you"),
                &format!(
                    "{rider_name} tries to mount {mount_name}, but only flails about in a vain attempt to touch {}.",
                    mount_it.objective
                ),
            );
        } else {
            tell_three(
                world,
                rider,
                mount,
                &format!("You lay a hand on {mount_name}, but it slips right through."),
                &format!(
                    "{rider_name} tries to mount you, but you are too fluid to support {}",
                    rider_it.objective
                ),
                &format!(
                    "{rider_name} tries to mount {mount_name}, but {} isn't solid enough to support a rider.",
                    mount_it.subjective
                ),
            );
        }
    } else if actor_composition(world, rider) == Composition::Ether {
        tell_three(
            world,
            rider,
            mount,
            &format!(
                "You try to climb {mount_name}, but you simply pass through {}.",
                mount_it.objective
            ),
            &format!(
                "{rider_name} tries to mount you, but {} can't seem to touch you.",
                rider_it.subjective
            ),
            &format!(
                "{rider_name} tries to mount {mount_name}, but makes {} shudder as {} passes through.",
                mount_it.objective, rider_it.subjective
            ),
        );
    } else {
        tell_three(
            world,
            rider,
            mount,
            &format!(
                "You try to climb {}, but you just flow right off.",
                mount_it.objective
            ),
            &format!("{rider_name} tries to mount you, but just flows right off."),
            &format!("{rider_name} tries to mount {mount_name}, but just flows right off."),
        );
    }
    true
}

/// Legacy `remove_unsuitable_spells`: strip every effect whose ability is
/// tagged `requires_rigid` from `target` while its body is not solid. They
/// go silently through the same teardown a natural expiry uses
/// ([`crate::effects::remove_effect_instance`]). Returns how many ended.
pub(crate) fn strip_unsuitable_effects(world: &mut World, target: Entity) -> usize {
    let rigid_only = requires_rigid_abilities(world);
    strip_effects_of(world, target, &rigid_only)
}

/// The ids of every ability tagged `requires_rigid`.
fn requires_rigid_abilities(world: &World) -> std::collections::HashSet<i32> {
    world
        .get_resource::<mud_world::AbilityCatalog>()
        .map(|catalog| {
            catalog
                .by_name
                .values()
                .filter(|d| d.requires_rigid)
                .map(|d| d.id)
                .collect()
        })
        .unwrap_or_default()
}

/// [`strip_unsuitable_effects`] with the `requires_rigid` set already built,
/// so a sweep over many bodies builds it once.
fn strip_effects_of(
    world: &mut World,
    target: Entity,
    rigid_only: &std::collections::HashSet<i32>,
) -> usize {
    if rigid_only.is_empty()
        || actor_is_rigid(world, target)
        || bypasses_restrictions(world, target)
    {
        return 0;
    }
    let doomed: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, applied)| {
                applied.0 == target && inst.ability_id.is_some_and(|id| rigid_only.contains(&id))
            })
            .map(|(e, ..)| e)
            .collect()
    };
    let mut removed = 0;
    for e in doomed {
        if world.get::<EffectInstance>(e).is_some() {
            crate::effects::remove_effect_instance(world, target, e);
            removed += 1;
        }
    }
    removed
}

/// Safety net for the paths that make a body fluid without going through
/// a cast (a worn item's effect, a script): every holder of a composition
/// override sheds its unsuitable spells. Cheap, since only overrides are
/// visited.
pub(crate) fn sweep_fluid_bearers(world: &mut World) {
    let bearers: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<mud_world::CompositionOverride>>();
        q.iter(world).collect()
    };
    if bearers.is_empty() {
        return;
    }
    let rigid_only = requires_rigid_abilities(world);
    for e in bearers {
        strip_effects_of(world, e, &rigid_only);
    }
}

/// Refuse a `requires_rigid` spell cast onto a body that is not solid
/// (legacy `check_fluid_spell_ok`). `true` when refused.
pub(crate) fn refuse_fluid_spell(
    world: &mut World,
    caster: Entity,
    target: Entity,
    ability: &mud_world::AbilityDef,
) -> bool {
    if !ability.requires_rigid
        || actor_is_rigid(world, target)
        || bypasses_restrictions(world, target)
    {
        return false;
    }
    send_to(
        world,
        target,
        "The spell is unable to take hold in your substance.\r\n",
    );
    if caster != target {
        let name = name_or(world, target, "someone");
        send_msg(
            world,
            caster,
            format!("The spell is unable to alter {name}'s substance.\r\n"),
        );
    }
    true
}
