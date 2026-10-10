use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, AppliedTo, EffectCatalog, EffectInstance, Item, Located, ModifyDelta, Stunned,
};
use tracing::{info, warn};

use crate::TickCount;
use crate::commands::{
    apply_damage, drain_lua_outbox, name_of, name_or, send_rendered, send_to, try_insert,
    try_remove,
};

/// Marker added to an `EffectInstance` after its `on_apply` Lua
/// hook has fired. Lets the lifecycle scan distinguish "freshly
/// spawned" effects from ones the loop has already seen, without
/// needing every spawn site to call into Lua synchronously.
#[derive(Component, Debug, Clone, Copy)]
pub struct EffectInstanceApplied;

/// What lifecycle hook to fire. Picked off the `EffectDef` field
/// of the matching name; missing or blank hooks are silently
/// skipped, so content authors only pay for what they use.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum EffectHook {
    OnApply,
    OnTick,
    OnRemove,
}

impl EffectHook {
    fn label(self) -> &'static str {
        match self {
            Self::OnApply => "on_apply",
            Self::OnTick => "on_tick",
            Self::OnRemove => "on_remove",
        }
    }

    fn body(self, def: &mud_world::EffectDef) -> Option<&str> {
        match self {
            Self::OnApply => def.on_apply.as_deref(),
            Self::OnTick => def.on_tick.as_deref(),
            Self::OnRemove => def.on_remove.as_deref(),
        }
    }
}

/// Look up the `EffectDef` for `effect_name` in the catalog and
/// fire the matching lifecycle hook against `target`. `self` binds
/// to the target inside the Lua body. Logs failures via tracing
/// rather than the script error log — these are runtime hooks not
/// dispatched triggers, and the catalog has no (zone, id).
fn run_effect_hook(world: &mut World, hook: EffectHook, target: Entity, effect_name: &str) {
    let body = {
        let Some(catalog) = world.get_resource::<EffectCatalog>() else {
            return;
        };
        let Some(def) = catalog.find_by_name(effect_name) else {
            return;
        };
        let Some(b) = hook.body(def) else {
            return;
        };
        b.to_string()
    };
    if world.get_entity(target).is_err() {
        return;
    }
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_actor(world, target, &body)
    });
    drain_lua_outbox(world);
    if let Err(e) = result {
        warn!(
            effect = %effect_name,
            hook = %hook.label(),
            error = %e,
            "effect hook failed",
        );
    }
}

/// Does an effect of this name hold its bearer still? A `stun` (bash and
/// friends, fear's wait states), a `paralyzed` status (Minor / Major
/// Paralysis, fear's "frozen in terror") or a `mesmerized` one (legacy
/// `EFF_MESMERIZED` blocks acting and attacking the same way).
pub(crate) fn is_stun_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("stun")
        || name.eq_ignore_ascii_case("paralyzed")
        || name.eq_ignore_ascii_case("mesmerized")
}

/// Does an effect of this name hold its bearer in a trance rather than
/// merely lag it? Paralysis and mesmerize do; a plain `stun` does not.
/// Legacy `mag_affect` ends the bearer's fight (and everyone's against it)
/// when one lands.
pub(crate) fn is_hold_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("paralyzed") || name.eq_ignore_ascii_case("mesmerized")
}

/// Does an effect of this name keep its bearer asleep (legacy `EFF_SLEEP`,
/// the Sleep spell)?
pub(crate) fn is_sleep_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("sleeping")
        || name.eq_ignore_ascii_case("sleep")
        || name.eq_ignore_ascii_case("asleep")
}

/// True while any sleep effect is active on `target`.
pub(crate) fn has_sleep_effect(world: &mut World, target: Entity) -> bool {
    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
    q.iter(world)
        .any(|(eff, applied)| applied.0 == target && is_sleep_name(&eff.name))
}

/// The Sleep spell lands: `target` drops into the sleeping posture, so it
/// renders as asleep, stops wandering and acting, and a caster mid-chant
/// loses the cast (`casting_tick`). Legacy sets `STANCE_SLEEPING`.
pub(crate) fn fall_asleep(world: &mut World, target: Entity) {
    try_remove::<mud_world::Meditating>(world, target);
    try_insert(
        world,
        target,
        mud_world::Posture(mud_world::PostureKind::Sleeping),
    );
}

/// The last sleep effect on `target` ended (expiry, a hit, dispel): it
/// wakes. A player stands; a mob returns to its prototype's default
/// posture. A mob the day/night cycle tucked in stays asleep until
/// morning. The wear-off or jolt text is the caller's.
fn wake_after_sleep(world: &mut World, target: Entity) {
    if has_sleep_effect(world, target)
        || world.get::<crate::sleep::SleptByNight>(target).is_some()
        || world.get::<mud_world::Posture>(target).map(|p| p.0)
            != Some(mud_world::PostureKind::Sleeping)
    {
        return;
    }
    let posture = world
        .get::<mud_world::WorldKey>(target)
        .filter(|_| world.get::<mud_world::Mob>(target).is_some())
        .and_then(|k| {
            world
                .get_resource::<mud_world::MobPrototypes>()
                .and_then(|p| p.by_key.get(&(k.zone, k.id)))
        })
        .map_or(mud_world::PostureKind::Standing, |p| {
            mud_world::Posture::from_default_position(p.default_position)
        });
    try_insert(world, target, mud_world::Posture(posture));
}

/// Make `target`'s [`Stunned`] marker match its effects: present exactly
/// while at least one stun or paralysis instance is active, whatever
/// overlapped with what. Legacy paralysis stops the victim fighting and
/// acting (`perform_violence`, `attack_ok`, the command interpreter), so
/// both kinds back the same marker.
pub(crate) fn sync_stunned(world: &mut World, target: Entity) {
    let backed = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .any(|(eff, applied)| applied.0 == target && is_stun_name(&eff.name))
    };
    if backed {
        try_insert(world, target, Stunned);
    } else {
        try_remove::<Stunned>(world, target);
    }
}

/// [`sync_stunned`] for everyone with the marker or a backing instance.
fn sync_stunned_all(world: &mut World) {
    let mut targets: std::collections::HashSet<Entity> = world
        .query_filtered::<Entity, With<Stunned>>()
        .iter(world)
        .collect();
    {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        targets.extend(
            q.iter(world)
                .filter(|(eff, _)| is_stun_name(&eff.name))
                .map(|(_, applied)| applied.0),
        );
    }
    for t in targets {
        if world.get_entity(t).is_ok() {
            sync_stunned(world, t);
        }
    }
}

/// `EffectSource::Other` tag of the "frozen in terror" paralysis: legacy
/// gives it `EFF_MINOR_PARALYSIS`, which any hit breaks.
pub(crate) const FREEZE_SOURCE: &str = "fear-freeze";

/// Does this status instance break when its bearer is hit? Data first:
/// the ability's own effect row says `breakOnDamage` in its override
/// params (else the effect's defaults) for the status flag the instance
/// carries (its name), so any status flag can opt in: true for Minor
/// Paralysis, Entangle and Mesmerize, false for Major Paralysis, which
/// holds. Fear's own freeze has no ability row and is tagged
/// [`FREEZE_SOURCE`] instead.
fn breaks_on_hit(world: &World, inst: &EffectInstance) -> bool {
    if matches!(&inst.source, mud_world::EffectSource::Other(s) if s == FREEZE_SOURCE) {
        return true;
    }
    let (Some(ability), Some(abilities), Some(effects)) = (
        inst.ability_id,
        world.get_resource::<AbilityCatalog>(),
        world.get_resource::<EffectCatalog>(),
    ) else {
        return false;
    };
    let field = |v: Option<&serde_json::Value>, key: &str| v.and_then(|v| v.get(key)).cloned();
    abilities
        .effects_for
        .get(&ability)
        .into_iter()
        .flatten()
        .any(|(effect_id, over)| {
            let defaults = effects.by_id.get(effect_id).map(|d| &d.default_params);
            let pick = |key| field(over.as_ref(), key).or_else(|| field(defaults, key));
            pick("flag")
                .and_then(|f| f.as_str().map(str::to_ascii_lowercase))
                .is_some_and(|f| f == inst.name.to_ascii_lowercase())
                && pick("breakOnDamage").and_then(|b| b.as_bool()) == Some(true)
        })
}

/// Legacy `damage()` (fight.cpp:1650-1666): which message set a broken
/// status plays. Minor Paralysis (and Entangle, which legacy casts as
/// Minor Paralysis) shatters; Mesmerize jolts. Other flags break silently.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BreakKind {
    Frozen,
    Mesmerized,
    Silent,
}

fn break_kind(name: &str) -> BreakKind {
    if name.eq_ignore_ascii_case("paralyzed") || name.eq_ignore_ascii_case("webbed") {
        BreakKind::Frozen
    } else if name.eq_ignore_ascii_case("mesmerized") {
        BreakKind::Mesmerized
    } else {
        BreakKind::Silent
    }
}

/// Legacy `damage()` (fight.cpp:1650): a hit shatters Minor Paralysis,
/// Entangle and Mesmerize. Called from the central attacker-damage entry
/// for any positive hit (legacy also breaks them on a 0-damage hit; the
/// swing path only reaches here with damage, so a plain miss does not).
/// Breakable instances (see [`breaks_on_hit`]) are removed with legacy's
/// three lines per kind; Major Paralysis stays. Returns whether something
/// broke.
pub(crate) fn break_on_hit(world: &mut World, attacker: Entity, victim: Entity) -> bool {
    let breaking: Vec<(Entity, BreakKind)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, applied)| applied.0 == victim && breaks_on_hit(world, inst))
            .map(|(e, inst, _)| (e, break_kind(&inst.name)))
            .collect()
    };
    if breaking.is_empty() {
        return false;
    }
    for (e, _) in &breaking {
        remove_effect_instance(world, victim, *e);
    }
    sync_stunned(world, victim);
    let attacker_name = crate::commands::cap_sentence_start(&name_of(world, attacker));
    // The victim only ever appears mid-sentence ("keeping a creeping vine
    // frozen"), so its name keeps the case it was given in.
    let victim_name = name_of(world, victim);
    let room = world.get::<Located>(victim).map(|l| l.0);
    for kind in [BreakKind::Frozen, BreakKind::Mesmerized] {
        if !breaking.iter().any(|(_, k)| *k == kind) {
            continue;
        }
        let (to_char, to_vict, to_room) = match kind {
            BreakKind::Frozen => (
                format!("Your blow disrupts the magic keeping {victim_name} frozen.\r\n"),
                format!("{attacker_name}'s blow shatters the magic paralyzing you!\r\n"),
                format!(
                    "{attacker_name}'s attack frees {victim_name} from magic which held them motionless.\r\n"
                ),
            ),
            _ => (
                format!(
                    "You drew {victim_name}'s attention from whatever they were pondering.\r\n"
                ),
                format!("{attacker_name} attacks, jolting you out of your reverie!\r\n"),
                format!(
                    "{attacker_name}'s attack distracts {victim_name} from whatever was fascinating them.\r\n"
                ),
            ),
        };
        send_to(world, attacker, to_char);
        send_to(world, victim, to_vict);
        if let Some(room) = room {
            crate::commands::broadcast_room_except_rendered(
                world,
                room,
                &[attacker, victim],
                &to_room,
            );
        }
    }
    true
}

/// One effect tick = one second.
const EFFECT_PERIOD_TICKS: u64 = 10;
/// Damage-per-tick for the `bleed` debuff. 2/s for 30s = 60 total
/// — comparable to one rend hit, kept low so multiple stacked damage-over-time effects
/// stay within combat budgets.
const BLEED_DPS: i32 = 2;

/// Drop the marker component a just-removed `status` effect named `name`
/// backed, once no other `EffectInstance` of that name remains on
/// `target`. Shared by expiry ([`effects_tick`]) and by dispel / cleanse
/// removals so a dispelled `fly` / `bless` / ... cannot leave its marker
/// without backing.
pub(crate) fn teardown_markers_after_removal(world: &mut World, target: Entity, name: &str) {
    // Flag markers (stealth, fly, bless, sanctuary, haste, detect
    // invisible, protect evil / good, ...): alive only while at least
    // one backing instance remains, race innates and worn items
    // included. The flag -> component table lives in
    // `mob_effects::FLAG_MARKERS`. Manually-toggled `hide` / `visible`
    // install / remove Stealth directly without a backing effect, so a
    // target with manual stealth and no status effects is unaffected.
    let was_flying = world.get::<mud_world::Flying>(target).is_some();
    mud_world::mob_effects::teardown_flag_marker(world, target, name);
    if was_flying && world.get::<mud_world::Flying>(target).is_none() {
        // Nothing holds the bearer up any more: fall through an air
        // room, or drop to the ground anywhere else.
        crate::flight::on_flight_lost(world, target);
    }
    // Invisible: the backing is any `InvisibleSource`-tagged instance
    // (INVISIBLE / MASS_INVIS, a permanent `invisible` flag), whatever
    // it is named; the fade message and aggro recheck live in
    // `invisibility_faded`. (The spell instances are named for the stat
    // they move, not "invisible": `teardown_effect_instance` covers
    // them by their tag.)
    if name.eq_ignore_ascii_case("invisible") {
        let still_invisible = {
            let mut q = world.query::<(&mud_world::InvisibleSource, &AppliedTo)>();
            q.iter(world).any(|(_, applied)| applied.0 == target)
        };
        if !still_invisible {
            crate::commands::invisibility_faded(world, target);
        }
    }
    if name.eq_ignore_ascii_case("empowered") {
        let still_empowered = {
            let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
            q.iter(world).any(|(eff, applied)| {
                applied.0 == target && eff.name.eq_ignore_ascii_case("empowered")
            })
        };
        if !still_empowered {
            try_remove::<mud_world::Empowered>(world, target);
        }
    }
    // Globe teardown: MINOR/MAJOR_GLOBE spells and worn globe items both
    // carry an EffectInstance named "globe" with the circle in
    // `strength`. The marker is recomputed from whatever remains
    // (max-wins) rather than removed outright, so stacking MAJOR over
    // MINOR, or a spell over a worn globe, never drops coverage when
    // only one source goes.
    if name.eq_ignore_ascii_case("globe") {
        let highest = {
            let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
            q.iter(world)
                .filter(|(eff, applied)| {
                    applied.0 == target && eff.name.eq_ignore_ascii_case("globe")
                })
                .map(|(eff, _)| eff.strength)
                .max()
        };
        match highest {
            Some(s) if s > 0 => {
                try_insert(world, target, mud_world::MaxAbsorbCircle(s));
            }
            _ => {
                try_remove::<mud_world::MaxAbsorbCircle>(world, target);
            }
        }
    }
}
/// Undo the numeric side of one `EffectInstance` (stat `ModifyDelta`,
/// refreshed regen bonus, `SpellResistanceDelta`) and despawn it, leaving
/// every flag marker alone. Returns the instance name and alignment-protect
/// tag read before the despawn. The first half of
/// [`teardown_effect_instance`]; a recast calls it directly through
/// [`replace_effect_instance`], where the replacement keeps the markers up.
fn reverse_effect_companions(
    world: &mut World,
    target: Entity,
    eff_entity: Entity,
) -> (Option<String>, Option<mud_world::AlignmentProtectionTag>) {
    let name = world
        .get::<EffectInstance>(eff_entity)
        .map(|i| i.name.clone());
    let target_alive = world.get_entity(target).is_ok();
    // Reverse a `ModifyDelta` companion before despawning the effect,
    // so stacking buffs from each other's removals don't double-clear.
    if let Some(delta) = world.get::<ModifyDelta>(eff_entity).cloned()
        && target_alive
    {
        crate::commands::reverse_modify_delta(world, target, &delta.target, delta.amount);
    }
    // Rest / repose R6: when the Refreshed effect goes, subtract the
    // RegenBonus delta the wake path stamped.
    crate::rest::unwind_refreshed_bonus(world, eff_entity, target);
    let align_tag = world
        .get::<mud_world::AlignmentProtectionTag>(eff_entity)
        .copied();
    // Reverse a `SpellResistanceDelta` companion the same way
    // `ModifyDelta` unwinds. Stacked PROT_*/STONE_SKIN cleanly peel back
    // to whatever the underlying item-resistance value was.
    if let Some(delta) = world
        .get::<mud_world::SpellResistanceDelta>(eff_entity)
        .copied()
        && target_alive
        && let Some(mut r) = world.get_mut::<mud_world::Resistances>(target)
    {
        let entry = r.0.entry(delta.element).or_insert(0);
        *entry = entry.saturating_sub(delta.percent);
        if *entry == 0 {
            r.0.remove(&delta.element);
        }
    }
    if let Ok(e) = world.get_entity_mut(eff_entity) {
        e.despawn();
    }
    (name, align_tag)
}

/// Drop one `EffectInstance` that a recast of the same spell is about to
/// replace: reverses every numeric companion (`ModifyDelta`,
/// `SpellResistanceDelta`, refreshed regen bonus) exactly like expiry, so
/// recasting `PROT_FIRE` / `STONE_SKIN` cannot stack. Markers (fly, invisible,
/// stun, protect_*) stay up because the replacement re-backs them, and no
/// wear-off hook or text fires: legacy refresh shows neither.
pub(crate) fn replace_effect_instance(world: &mut World, target: Entity, eff_entity: Entity) {
    reverse_effect_companions(world, target, eff_entity);
}

/// Undo everything one `EffectInstance` did to `target` and despawn it:
/// the `ModifyDelta` stat change, a Refreshed regen bonus, the elemental
/// `SpellResistanceDelta` bump, the stun marker, then the flag / globe
/// markers and the evil / good protection marker. The single reversal
/// behind expiry ([`effects_tick`]) and every early removal (dispel,
/// cleanse, `cancel`, staff strip) so none of them can leave a bonus
/// behind. The alignment tag is read before the despawn, and the marker
/// drops only when no other tagged instance and no worn `protect_*` item
/// still backs it.
fn teardown_effect_instance(world: &mut World, target: Entity, eff_entity: Entity) {
    // INVISIBLE / MASS_INVIS instances are named for the stat they move
    // ("evasion"), so the name-keyed marker teardown below cannot see
    // them: remember the tag before the despawn.
    let was_invisibility_source = world
        .get::<mud_world::InvisibleSource>(eff_entity)
        .is_some();
    let (name, align_tag) = reverse_effect_companions(world, target, eff_entity);
    let Some(name) = name else {
        return;
    };
    // The Stunned marker follows the union of stun and paralysis
    // instances (see `sync_stunned`).
    if is_stun_name(&name) {
        sync_stunned(world, target);
    }
    if is_sleep_name(&name) {
        wake_after_sleep(world, target);
    }
    teardown_markers_after_removal(world, target, &name);
    if was_invisibility_source && !name.eq_ignore_ascii_case("invisible") {
        let still_invisible = {
            let mut q = world.query::<(&mud_world::InvisibleSource, &AppliedTo)>();
            q.iter(world).any(|(_, applied)| applied.0 == target)
        };
        if !still_invisible {
            crate::commands::invisibility_faded(world, target);
        }
    }
    // J2 alignment-protect teardown: PROT_FROM_EVIL / PROT_FROM_GOOD
    // spawn instances named "resistance" (shared with element-resistance
    // flavors), tagged with the alignment they guard against.
    if let Some(tag) = align_tag {
        let flag = match tag {
            mud_world::AlignmentProtectionTag::Evil => "protect_evil",
            mud_world::AlignmentProtectionTag::Good => "protect_good",
        };
        mud_world::mob_effects::teardown_flag_marker(world, target, flag);
    }
}

/// Conjuration spells spawn an `EffectInstance` named `summoned-{mobType}`
/// pointing at the spawned mob via `AppliedTo(mob)`. When the instance goes
/// (expiry, dispel, cleanse, `cancel`, staff strip) the conjured mob
/// vanishes with it, the legacy "follower fades" behaviour: drop a final
/// flavor line into the mob's room so observers see it, then extract the
/// mob like legacy `extract_char` (gear to the floor, fighters disengaged). Players are never despawned, and a mob a sibling instance already
/// took is a no-op.
fn despawn_summoned_mob(world: &mut World, target: Entity) {
    if world.get::<mud_world::Player>(target).is_some() {
        return;
    }
    if let Some(mob_room) = world.get::<mud_world::Located>(target).map(|l| l.0) {
        let mob_name = world
            .get::<mud_world::Named>(target)
            .map_or("the summoned creature".to_string(), |n| n.name.clone());
        let players: Vec<Entity> = {
            let mut q =
                world.query_filtered::<(Entity, &mud_world::Located), With<mud_world::Player>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == mob_room)
                .map(|(e, _)| e)
                .collect()
        };
        let msg = format!("{mob_name} fades back to where it was summoned from.\r\n");
        for p in players {
            send_to(world, p, msg.clone());
        }
    }
    // Legacy `extract_char`: carried and worn items drop to the room, the
    // mob's followers and rider let go, and anyone fighting it disengages.
    let room = world.get::<mud_world::Located>(target).map(|l| l.0);
    crate::commands::extract_mob(world, target, room, false);
}

/// Remove one `EffectInstance` before its time (dispel, cleanse, `cancel`,
/// staff strip): fire its `on_remove` hook, then the same reversal expiry
/// does ([`teardown_effect_instance`]). The one entry point for early
/// removal; callers never despawn an `EffectInstance` themselves.
pub(crate) fn remove_effect_instance(world: &mut World, target: Entity, eff_entity: Entity) {
    let name = world
        .get::<EffectInstance>(eff_entity)
        .map(|i| i.name.clone());
    if let Some(name) = &name {
        run_effect_hook(world, EffectHook::OnRemove, target, name);
    }
    teardown_effect_instance(world, target, eff_entity);
    // A conjured mob dies with its instance on early removal too, same as
    // expiry; only the wear-off text stays expiry-only.
    if name.is_some_and(|n| n.starts_with("summoned-")) {
        despawn_summoned_mob(world, target);
    }
}

/// Strip every non-innate `EffectInstance` from `target` through
/// [`remove_effect_instance`]: legacy `perform_death` (fight.cpp:839) runs
/// `effect_remove` over the whole effect list. Race innates and worn-item
/// grants (see [`mud_world::mob_effects::is_innate_effect`]) stay, since
/// their lifetime belongs to the race / the item. Returns how many went.
pub(crate) fn strip_non_innate_effects(world: &mut World, target: Entity) -> usize {
    let doomed: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, applied)| {
                applied.0 == target && !mud_world::mob_effects::is_innate_effect(&inst.source)
            })
            .map(|(e, ..)| e)
            .collect()
    };
    let mut removed = 0;
    for e in doomed {
        // A hook or sibling removal may already have taken it.
        if world.get::<EffectInstance>(e).is_some() {
            remove_effect_instance(world, target, e);
            removed += 1;
        }
    }
    removed
}

/// Decrement remaining duration on every active effect; despawn ones whose
/// duration hit zero (with a "fades" message to the target if it has a
/// connection); also despawn any effect whose target entity has gone away.
#[allow(clippy::too_many_lines)]
pub fn effects_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(EFFECT_PERIOD_TICKS) {
        return;
    }

    // Drop fear markers whose backing effect expired or was removed.
    crate::fear::sync_markers(world);
    // Same for Stunned, which also catches cleansed / dispelled paralysis.
    sync_stunned_all(world);

    // Pre-pass: fire `on_apply` hooks for any EffectInstance that
    // hasn't been seen yet, then mark it `EffectInstanceApplied`.
    // Spawn sites are too scattered to thread the hook through;
    // a once-per-second sweep with a marker is correct enough and
    // doesn't push a Lua call into every callsite.
    let fresh: Vec<(Entity, Entity, String)> = {
        let mut q = world.query_filtered::<
            (Entity, &EffectInstance, &AppliedTo),
            Without<EffectInstanceApplied>,
        >();
        q.iter(world)
            .map(|(eff, inst, applied)| (eff, applied.0, inst.name.clone()))
            .collect()
    };
    for (eff_entity, target, name) in fresh {
        run_effect_hook(world, EffectHook::OnApply, target, &name);
        try_insert(world, eff_entity, EffectInstanceApplied);
    }

    // Snapshot all active effects: (effect_entity, target_entity, remaining_secs, name, ability_id).
    // Doing this in a scoped block releases the query borrow before we mutate.
    let snapshots: Vec<(Entity, Entity, i32, String, Option<i32>)> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .map(|(eff, inst, applied)| {
                (
                    eff,
                    applied.0,
                    inst.remaining_secs,
                    inst.name.clone(),
                    inst.ability_id,
                )
            })
            .collect()
    };

    let mut expired = 0usize;
    let mut orphaned = 0usize;
    let mut killed_by_bleed = 0usize;
    for (eff_entity, target, ticks, name, ability_id) in snapshots {
        if world.get_entity(target).is_err() {
            // Target gone — orphaned effect.
            if let Ok(e) = world.get_entity_mut(eff_entity) {
                e.despawn();
            }
            orphaned += 1;
            continue;
        }
        // Damage-over-time effects: apply HP loss before the duration
        // tick. Lethal damage despawns the target plus all its effects,
        // so we short-circuit the rest of this iteration.
        if name.eq_ignore_ascii_case("bleed") {
            let (dead, _) = apply_damage(world, target, BLEED_DPS);
            send_to(
                world,
                target,
                format!("You bleed for {BLEED_DPS} damage.\r\n"),
            );
            if dead {
                let target_name = name_or(world, target, "<unknown>");
                let room = world.get::<Located>(target).copied().map(|l| l.0);
                if let Some(r) = room {
                    crate::combat::handle_death(world, target, &target_name, r);
                }
                killed_by_bleed += 1;
                // The bleed effect entity is already despawned by
                // handle_death's cleanup of all child effects (or
                // orphan-pickup on the next tick). Don't touch
                // eff_entity here.
                continue;
            }
        }
        // on_tick: fired once per second for every still-alive
        // effect, including permanent ones. Hook bodies typically
        // do their own internal throttling via `time.stamp` if
        // they need a slower cadence.
        run_effect_hook(world, EffectHook::OnTick, target, &name);
        if world.get_entity(eff_entity).is_err() {
            // Hook may have despawned the effect or its target;
            // bail before we touch a stale entity.
            continue;
        }
        if ticks < 0 {
            // Permanent — leave alone.
            continue;
        }
        let new_ticks = ticks - 1;
        if let Some(mut inst) = world.get_mut::<EffectInstance>(eff_entity) {
            inst.remaining_secs = new_ticks;
        }
        if new_ticks <= 0 {
            // on_remove: fire before any of the despawn or marker
            // cleanup runs, so the hook body can still inspect the
            // effect / target relationship if it wants.
            run_effect_hook(world, EffectHook::OnRemove, target, &name);
            // Look up wearoff_to_target / wearoff_to_room from the
            // AbilityMessages catalog. ability_id is None for hardcoded
            // effects (rend's bleed, gouge's blind, admin tests) — those
            // fall through to the terse default.
            let (wearoff_target, wearoff_room) = ability_id
                .and_then(|aid| {
                    world
                        .resource::<AbilityCatalog>()
                        .messages
                        .get(&aid)
                        .map(|m| (m.wearoff_to_target.clone(), m.wearoff_to_room.clone()))
                })
                .unwrap_or((None, None));
            // Send_to registers the target for end-of-tick prompt refresh
            // via commands::flush_prompts; no per-system tracking here.
            let target_msg = wearoff_target
                .as_deref()
                .map_or_else(|| format!("Your {name} fades.\r\n"), |t| format!("{t}\r\n"));
            send_rendered(world, target, &target_msg);
            if let Some(line) = wearoff_room.as_deref()
                && let Some(located) = world.get::<Located>(target).copied()
            {
                let target_name = name_of(world, target);
                let rendered = line.replace("{target.name}", &target_name);
                crate::commands::broadcast_room_except_rendered(
                    world,
                    located.0,
                    &[target],
                    &format!("{rendered}\r\n"),
                );
            }
            // Same reversal as a dispel / cleanse (stat delta, resistance,
            // alignment tag, markers); only the wear-off text and the
            // per-kind teardown below are expiry's own.
            teardown_effect_instance(world, target, eff_entity);
            expired += 1;
            // Object-decay: an effect named "decay" applied to an
            // Item entity acts as the object's lifetime gate (used
            // by the `portal` effect-type for spawned gates,
            // moonwells, etc.). When it fades, despawn the object
            // itself so the portal closes.
            if name.eq_ignore_ascii_case("decay")
                && world.get::<Item>(target).is_some()
                && let Ok(e) = world.get_entity_mut(target)
            {
                e.despawn();
            }
            // L3 summon teardown: the conjured mob goes with its
            // instance (shared with early removal).
            if name.starts_with("summoned-") {
                despawn_summoned_mob(world, target);
            }
            // Wall teardown — WALL_OF_STONE / WALL_OF_ICE spawn an
            // EffectInstance named "wall-{type}" with
            // AppliedTo(room). The room carries a RoomBlockedExits
            // map keyed on Direction; on expiry, remove whichever
            // entry was backed by THIS expiring entity (the cast
            // arm overwrites stale entries on re-cast, so there's
            // at most one match). Despawn the (now-empty) map when
            // it's the last wall.
            if name.starts_with("wall-") {
                // Capture the (direction, kind_label) of the entry
                // about to drop so the post-cleanup broadcast can
                // name what just faded. Done before the retain so
                // the map still has the entry.
                let expiring: Option<(mud_db::enums::Direction, String)> = world
                    .get::<mud_world::RoomBlockedExits>(target)
                    .and_then(|b| {
                        b.by_direction
                            .iter()
                            .find(|(_, e)| e.backed_by == eff_entity)
                            .map(|(d, e)| (*d, e.kind_label.clone()))
                    });
                if let Some(mut blocked) = world.get_mut::<mud_world::RoomBlockedExits>(target) {
                    blocked
                        .by_direction
                        .retain(|_, entry| entry.backed_by != eff_entity);
                }
                let empty = world
                    .get::<mud_world::RoomBlockedExits>(target)
                    .is_some_and(|b| b.by_direction.is_empty());
                if empty {
                    try_remove::<mud_world::RoomBlockedExits>(world, target);
                }
                // Broadcast the fade to everyone in the affected
                // room. Otherwise a wall just silently vanishes —
                // a player who was waiting for it to expire (or who
                // got cornered behind one) would have no signal it's
                // safe to move now. Mirrors the bash-crumble UX.
                if let Some((dir, kind_label)) = expiring {
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let dir_name = crate::commands::direction_name(dir);
                    let msg = format!(
                        "The {kind_label} {dir_name} shudders and dissolves into nothing.\r\n"
                    );
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // Room-light teardown: ILLUMINATION / MAGIC_TORCH land
            // an EffectInstance named "light" with AppliedTo(room).
            // When the last one fades, drop the RoomMagicalLight
            // marker so room_is_dark / room_has_light read normally.
            if name.eq_ignore_ascii_case("light") {
                let still_lit = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("light")
                    })
                };
                if !still_lit {
                    try_remove::<mud_world::RoomMagicalLight>(world, target);
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = "The magical radiance fades.\r\n".to_string();
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // Mirror for DARKNESS.
            if name.eq_ignore_ascii_case("darkness") {
                let still_dark = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("darkness")
                    })
                };
                if !still_dark {
                    try_remove::<mud_world::RoomMagicalDarkness>(world, target);
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = "The unnatural darkness dissipates.\r\n".to_string();
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // CIRCLE_OF_FIRE teardown.
            if name.eq_ignore_ascii_case("burning") {
                let still_burning = {
                    let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
                    q.iter(world).any(|(eff, applied)| {
                        applied.0 == target && eff.name.eq_ignore_ascii_case("burning")
                    })
                };
                if !still_burning {
                    try_remove::<mud_world::RoomBurningEffect>(world, target);
                    // Broadcast the flames dying down to anyone in
                    // the room so they know it's safe to walk again
                    // without taking damage. Without it the hazard
                    // just silently lifts.
                    let players: Vec<bevy_ecs::entity::Entity> = {
                        let mut q = world.query_filtered::<(bevy_ecs::entity::Entity, &mud_world::Located), bevy_ecs::prelude::With<mud_world::Player>>();
                        q.iter(world)
                            .filter(|(_, l)| l.0 == target)
                            .map(|(e, _)| e)
                            .collect()
                    };
                    let msg = "The flames lash one last time, then sputter out.\r\n".to_string();
                    for p in players {
                        crate::commands::send_to(world, p, msg.clone());
                    }
                }
            }
            // Invisible marker: the expiring effect itself doesn't
            // need a name match — we tag the install with
            // `InvisibleSource` and walk those at expiry. When no
            // other tagged effect remains on the target, the
            // Invisible marker drops.
            {
                let still_invisible = {
                    let mut q = world.query::<(&mud_world::InvisibleSource, &AppliedTo)>();
                    q.iter(world).any(|(_, applied)| applied.0 == target)
                };
                if !still_invisible {
                    crate::commands::invisibility_faded(world, target);
                }
            }
        }
    }
    if killed_by_bleed > 0 {
        info!(killed_by_bleed, "bleed deaths");
    }

    if expired > 0 || orphaned > 0 {
        info!(expired, orphaned, "effects tick");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::EffectSource;

    fn make_effect(world: &mut World, target: Entity, secs: i32) -> Entity {
        world
            .spawn((
                EffectInstance {
                    kind: 1,
                    name: "test ward".to_string(),
                    strength: 1,
                    remaining_secs: secs,
                    source: EffectSource::Admin,
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id()
    }

    fn run_effects_tick(world: &mut World) {
        // effects_tick is gated on tick % 10 == 0.
        world.insert_resource(TickCount(EFFECT_PERIOD_TICKS));
        effects_tick(world);
    }

    #[test]
    fn decrements_remaining_secs() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 5);

        run_effects_tick(&mut world);

        let inst = world
            .get::<EffectInstance>(eff)
            .expect("effect still alive");
        assert_eq!(inst.remaining_secs, 4);
    }

    #[test]
    fn despawns_when_duration_expires() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 1);

        run_effects_tick(&mut world);

        assert!(
            world.get_entity(eff).is_err(),
            "effect despawned when remaining_secs hit 0"
        );
    }

    #[test]
    fn permanent_effect_is_left_alone() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, -1);

        run_effects_tick(&mut world);
        run_effects_tick(&mut world);

        let inst = world.get::<EffectInstance>(eff).expect("permanent stays");
        assert_eq!(inst.remaining_secs, -1, "permanent flag preserved");
    }

    #[test]
    fn orphaned_effect_when_target_despawns() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 100);
        // Target goes away mid-game.
        world.get_entity_mut(target).unwrap().despawn();

        run_effects_tick(&mut world);

        assert!(
            world.get_entity(eff).is_err(),
            "orphan cleanup despawned the effect"
        );
    }

    #[test]
    fn stun_marker_cleared_when_last_stun_expires() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        // Two stacked stuns: one expires this tick, one keeps going.
        let _stun_a = world
            .spawn((
                EffectInstance {
                    kind: 21,
                    name: "stun".to_string(),
                    strength: 1,
                    remaining_secs: 1,
                    source: EffectSource::Spell,
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id();
        let _stun_b = world
            .spawn((
                EffectInstance {
                    kind: 21,
                    name: "stun".to_string(),
                    strength: 1,
                    remaining_secs: 5,
                    source: EffectSource::Spell,
                    ability_id: None,
                },
                AppliedTo(target),
            ))
            .id();
        world.entity_mut(target).insert(Stunned);

        run_effects_tick(&mut world);
        // First stun expires; second still active → Stunned must remain.
        assert!(
            world.get::<Stunned>(target).is_some(),
            "Stunned stays while another stun is alive"
        );

        // Tick down four more times to expire the longer stun.
        for _ in 0..5 {
            run_effects_tick(&mut world);
        }
        assert!(
            world.get::<Stunned>(target).is_none(),
            "Stunned cleared after the last stun fades"
        );
    }

    #[test]
    fn skips_off_period_ticks() {
        let mut world = World::new();
        let target = world.spawn_empty().id();
        let eff = make_effect(&mut world, target, 5);
        world.insert_resource(TickCount(EFFECT_PERIOD_TICKS - 1));
        effects_tick(&mut world);
        let inst = world.get::<EffectInstance>(eff).expect("untouched");
        assert_eq!(inst.remaining_secs, 5, "off-period tick is a no-op");
    }

    // -- break on hit (legacy damage(), fight.cpp:1650-1666) ---------------

    use crate::commands::test_support::{Rx, drain, player_in};

    /// A room with a victim ("Tester") and an attacker ("Hitter"), each
    /// with a connection, and one effect instance of status `flag` from
    /// ability 77 whose effect row carries `break_on_damage`.
    struct Hit {
        world: World,
        attacker: Entity,
        victim: Entity,
        arx: Rx,
        vrx: Rx,
        wrx: Rx,
    }

    fn hit_fixture(flag: &str, break_on_damage: Option<bool>) -> Hit {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (victim, vrx) = player_in(&mut world, room);
        let (attacker, arx) = player_in(&mut world, room);
        world.entity_mut(attacker).insert(mud_world::Named {
            name: "Hitter".into(),
        });
        let (watcher, wrx) = player_in(&mut world, room);
        world.entity_mut(watcher).insert(mud_world::Named {
            name: "Watcher".into(),
        });
        world.init_resource::<AbilityCatalog>();
        world.init_resource::<EffectCatalog>();
        let mut over = serde_json::json!({ "flag": flag });
        if let Some(b) = break_on_damage {
            over["breakOnDamage"] = b.into();
        }
        world
            .resource_mut::<AbilityCatalog>()
            .effects_for
            .insert(77, vec![(900, Some(over))]);
        world.spawn((
            EffectInstance {
                kind: 900,
                name: flag.into(),
                strength: 1,
                remaining_secs: 300,
                source: mud_world::EffectSource::Spell,
                ability_id: Some(77),
            },
            AppliedTo(victim),
        ));
        Hit {
            world,
            attacker,
            victim,
            arx,
            vrx,
            wrx,
        }
    }

    fn effect_count(h: &mut Hit, flag: &str) -> usize {
        let victim = h.victim;
        let mut q = h.world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(&h.world)
            .filter(|(i, a)| a.0 == victim && i.name == flag)
            .count()
    }

    #[test]
    fn a_hit_breaks_mesmerize_with_legacy_messages() {
        let mut h = hit_fixture("mesmerized", Some(true));
        h.world.entity_mut(h.victim).insert(Stunned);
        crate::commands::apply_attacker_damage(&mut h.world, h.victim, 5, h.attacker);
        assert_eq!(effect_count(&mut h, "mesmerized"), 0);
        assert!(h.world.get::<Stunned>(h.victim).is_none());
        let out = drain(&mut h.arx);
        assert!(
            out.contains("You drew Tester's attention from whatever they were pondering."),
            "{out}"
        );
        let out = drain(&mut h.vrx);
        assert!(
            out.contains("Hitter attacks, jolting you out of your reverie!"),
            "{out}"
        );
        let out = drain(&mut h.wrx);
        assert!(
            out.contains("Hitter's attack distracts Tester from whatever was fascinating them."),
            "{out}"
        );
    }

    #[test]
    fn a_hit_breaks_entangle_with_the_minor_paralysis_messages() {
        let mut h = hit_fixture("webbed", Some(true));
        crate::commands::apply_attacker_damage(&mut h.world, h.victim, 5, h.attacker);
        assert_eq!(effect_count(&mut h, "webbed"), 0);
        let out = drain(&mut h.arx);
        assert!(
            out.contains("Your blow disrupts the magic keeping Tester frozen."),
            "{out}"
        );
        let out = drain(&mut h.vrx);
        assert!(
            out.contains("Hitter's blow shatters the magic paralyzing you!"),
            "{out}"
        );
    }

    /// A mob victim's name leads with a lowercase article; mid-sentence it
    /// must stay lowercase, and a leading attacker name still opens its
    /// sentence capitalised (#104).
    #[test]
    fn a_break_message_keeps_the_victims_article_lowercase_mid_sentence() {
        for (flag, to_room) in [
            (
                "webbed",
                "Hitter's attack frees a creeping vine from magic which held them motionless.",
            ),
            (
                "mesmerized",
                "Hitter's attack distracts a creeping vine from whatever was fascinating them.",
            ),
        ] {
            let mut h = hit_fixture(flag, Some(true));
            h.world.entity_mut(h.victim).insert(mud_world::Named {
                name: "a creeping vine".into(),
            });
            crate::commands::apply_attacker_damage(&mut h.world, h.victim, 5, h.attacker);
            let out = drain(&mut h.arx);
            assert!(
                out.contains("keeping a creeping vine frozen.")
                    || out.contains("You drew a creeping vine's attention"),
                "{flag}: {out}"
            );
            assert!(!out.contains("A creeping vine"), "{flag}: {out}");
            let out = drain(&mut h.wrx);
            assert!(out.contains(to_room), "{flag}: {out}");
        }
    }

    #[test]
    fn a_status_not_marked_break_on_damage_survives_the_hit() {
        for flag in ["webbed", "mesmerized", "paralyzed"] {
            for marked in [Some(false), None] {
                let mut h = hit_fixture(flag, marked);
                crate::commands::apply_attacker_damage(&mut h.world, h.victim, 5, h.attacker);
                assert_eq!(effect_count(&mut h, flag), 1, "{flag} {marked:?}");
                assert!(drain(&mut h.arx).is_empty());
            }
        }
    }

    #[test]
    fn a_dot_tick_without_an_attacker_does_not_break_it() {
        let mut h = hit_fixture("mesmerized", Some(true));
        crate::commands::apply_damage(&mut h.world, h.victim, 5);
        assert_eq!(effect_count(&mut h, "mesmerized"), 1);
    }
}
