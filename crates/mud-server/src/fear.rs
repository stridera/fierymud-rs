//! Fear: making frightened actors actually run.
//!
//! Legacy sources ported here (`fierymud_legacy/src`):
//!
//! * `spells.cpp:3089` `inflict_fear` (FEAR, HYSTERIA, ...): see
//!   [`inflict_fear`]. The spell's `feared` status effect and the WILL save
//!   that gates it are Rust-side and still run first; a victim that fails
//!   the save then goes through legacy's cascade of level-difference rolls.
//!   Only the flee branch leaves the lasting `feared` effect behind.
//! * `spells.cpp:1215` `chant_ivory_symphony` (the area flee chant): awake,
//!   non-group targets flee unless they save (skipped in dark rooms),
//!   sentinel mobs save a second time, then a `skill`% roll gates the flee.
//! * `act.offensive.cpp:262` `do_roar` / `howl`: per target, a PARA save,
//!   a second one for sentinels, `Aware` / `NoSummon` mobs and alignment
//!   protection are immune; then a sleeper may wake (50%), an awake target
//!   trips (`dex - 15 < d100`) or flees. Roar leaves no lasting effect.
//!
//! Legacy fear has no `MOB_NOFEAR` flag. Immunity here is data: a mob
//! proto whose `resistances` lists `"fear": 0` (the same shape the importer
//! writes for `NOCHARM`) cannot be frightened.
//!
//! While a `feared` status effect lasts (the [`Feared`] marker), a mob
//! neither starts nor resumes a fight and tries to run each combat round;
//! one with no open exit is cornered and fights on. Players stay in
//! control: a feared player panics once when the effect lands (legacy).

use std::collections::HashSet;

use bevy_ecs::prelude::*;
use mud_db::enums::MobBehavior;
use mud_world::{
    AppliedTo, CombatStats, CoreStats, EffectCatalog, EffectInstance, EffectSource, EquippedSlot,
    Feared, Fighting, Frozen, Ghost, Item, Located, Mob, MobBehaviors, MobPrototypes, Player,
    Posture, PostureKind, RiddenBy, SavingThrows, Slot, Stunned, WorldKey,
};

use crate::combat::{mob_flee, remember_attacker};
use crate::commands::{
    Prevent, broadcast_room_except_rendered, cap_sentence_start, effect_prevents,
    flee_through_exit, has_effect_named, name_of, name_or, room_is_dark, send_to, try_insert,
    try_remove,
};
use crate::effects::{FREEZE_SOURCE, sync_stunned};

/// Is `flag` (a status effect's `flag` param) the fear flag?
pub(crate) fn is_fear_flag(flag: &str) -> bool {
    flag.eq_ignore_ascii_case("feared") || flag.eq_ignore_ascii_case("fear")
}

/// Effect rows (id, name) whose default params carry the fear flag. An
/// ability whose flag lives only in the generic `status` effect's defaults
/// leaves its instances named `status`, so they are recognised by row.
fn fear_default_rows(world: &World) -> Vec<(i32, String)> {
    world
        .get_resource::<EffectCatalog>()
        .map(|c| {
            c.by_id
                .values()
                .filter(|def| {
                    def.default_params
                        .get("flag")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(is_fear_flag)
                })
                .map(|def| (def.id, def.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Does this effect instance carry the fear flag? The status arm names an
/// instance after the `flag` in its override params, so that is the usual
/// case; otherwise see [`fear_default_rows`].
fn instance_is_fear(inst: &EffectInstance, default_rows: &[(i32, String)]) -> bool {
    is_fear_flag(&inst.name)
        || default_rows
            .iter()
            .any(|(id, name)| *id == inst.kind && name.eq_ignore_ascii_case(&inst.name))
}

/// Every effect instance on `actor` that carries the fear flag.
fn fear_effects_on(world: &mut World, actor: Entity) -> Vec<Entity> {
    let rows = fear_default_rows(world);
    let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
    q.iter(world)
        .filter(|(_, inst, applied)| applied.0 == actor && instance_is_fear(inst, &rows))
        .map(|(e, _, _)| e)
        .collect()
}

/// Despawn every fear effect on `actor`, whatever it is named.
fn remove_fear_effects(world: &mut World, actor: Entity) {
    for e in fear_effects_on(world, actor) {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
}

/// Legacy `do_flee`: a berserk fighter is too angry to leave the fight.
fn berserk_holds(world: &mut World, actor: Entity) -> bool {
    world.get::<Fighting>(actor).is_some() && has_effect_named(world, actor, "berserk")
}

/// Is the actor under a fear effect right now?
pub(crate) fn is_feared(world: &World, actor: Entity) -> bool {
    world.get::<Feared>(actor).is_some()
}

/// A mob proto listing `fear: 0` in its resistances is immune to fear.
/// Players are never immune.
pub(crate) fn is_fear_immune(world: &World, actor: Entity) -> bool {
    if world.get::<Mob>(actor).is_none() {
        return false;
    }
    let Some(key) = world.get::<WorldKey>(actor) else {
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
                .any(|(k, v)| k.eq_ignore_ascii_case("fear") && v.as_i64() == Some(0))
        })
}

/// What a panic attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Panic {
    /// Left the room through an exit.
    Fled,
    /// Wanted to run but every way out is shut.
    Cornered,
    /// Couldn't run at all (asleep, paralysed, webbed, stunned, berserk) or
    /// spent the turn getting to its feet.
    Unable,
}

/// Legacy `do_flee` refusals, shared by every flee path: the `flee`
/// command, a wimpy player's auto-flee and [`panic_flee`] (fear).
///
/// Returns `true` when `actor` may bolt through an exit right now. When it
/// returns `false` the turn is spent or refused and `actor` has already been
/// told why; a sitter is stood up (legacy: scrambling to its feet is the
/// whole turn). Refuses ghosts, the frozen or stunned, sleepers, anyone held
/// by a `Prevent::Movement` effect, a ridden mount (it goes where its rider
/// goes) and a berserker mid-fight.
pub(crate) fn can_flee_now(world: &mut World, actor: Entity) -> bool {
    let Some(room) = world.get::<Located>(actor).map(|l| l.0) else {
        return false;
    };
    if world.get::<Ghost>(actor).is_some()
        || world.get::<Frozen>(actor).is_some()
        || world.get::<Stunned>(actor).is_some()
    {
        return false;
    }
    match world.get::<Posture>(actor).map(|p| p.0) {
        None | Some(PostureKind::Standing) => {}
        Some(PostureKind::Sleeping) => {
            send_to(world, actor, "You dream of fleeing!\r\n");
            return false;
        }
        Some(_) => {
            // Legacy `do_flee`: a panicked sitter scrambles to its feet
            // and that is the whole turn.
            crate::casting::cancel_own_cast(world, actor);
            try_insert(world, actor, Posture(PostureKind::Standing));
            let name = crate::commands::cap_sentence_start(&name_of(world, actor));
            send_to(world, actor, "You scramble madly to your feet!\r\n");
            broadcast_room_except_rendered(
                world,
                room,
                &[actor],
                &format!("Looking panicked, {name} scrambles madly to their feet!\r\n"),
            );
            return false;
        }
    }
    if effect_prevents(world, actor, Prevent::Movement) {
        send_to(world, actor, "You can't move!\r\n");
        return false;
    }
    if world.get::<RiddenBy>(actor).is_some() {
        return false;
    }
    if berserk_holds(world, actor) {
        send_to(world, actor, "You're too angry to leave this fight!\r\n");
        return false;
    }
    true
}

/// Make `victim` panic: stop its fight and cast, then run through the same
/// flee primitive the wimpy path (`mob_flee`) and the `flee` command
/// (`flee_through_exit`) use, so movement rules and messages stay
/// consistent. Guarded by [`can_flee_now`] (legacy `do_flee`).
pub(crate) fn panic_flee(world: &mut World, victim: Entity, source: Option<Entity>) -> Panic {
    let Some(room) = world.get::<Located>(victim).map(|l| l.0) else {
        return Panic::Unable;
    };
    if !can_flee_now(world, victim) {
        return Panic::Unable;
    }
    let is_player = world.get::<Player>(victim).is_some();

    crate::casting::cancel_own_cast(world, victim);
    try_remove::<Fighting>(world, victim);
    if let Some(source) = source
        && !is_player
    {
        remember_attacker(world, victim, source);
    }
    let moved = if is_player {
        flee_through_exit(world, victim);
        world.get::<Located>(victim).map(|l| l.0) != Some(room)
    } else {
        mob_flee(world, victim, room)
    };
    if moved { Panic::Fled } else { Panic::Cornered }
}

/// Legacy `get_base_saves` default for `SAVING_PARA` (no class table here),
/// improved by one point per two levels, plus the actor's own modifier.
fn para_save_number(world: &World, actor: Entity) -> i32 {
    let level = mud_world::effective_level(world, actor);
    let modifier = world.get::<SavingThrows>(actor).map_or(0, |s| s.para);
    105 - level / 2 + modifier
}

/// Legacy `mag_savingthrow(.., SAVING_PARA)`: `max(1, save) < d(0..99)`.
/// `roll` is that d100 so tests can pin it.
pub(crate) fn para_save(world: &World, actor: Entity, roll: i32) -> bool {
    para_save_number(world, actor).max(1) < roll
}

fn has_behavior(world: &World, mob: Entity, flag: MobBehavior) -> bool {
    world.get::<MobBehaviors>(mob).is_some_and(|b| b.has(flag))
}

/// The dice of the area-chant gates (`chant_ivory_symphony`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChantRolls {
    /// d(0..99) for the PARA save.
    pub save: i32,
    /// d(0..99) for the sentinel's second save.
    pub sentinel_save: i32,
    /// d(0..100) against the caster's skill.
    pub chance: i32,
}

impl ChantRolls {
    pub(crate) fn random() -> Self {
        Self {
            save: rand::random_range(0..=99),
            sentinel_save: rand::random_range(0..=99),
            chance: rand::random_range(0..=100),
        }
    }
}

/// The extra gates an area fear chant puts in front of the flee. False
/// means the victim shrugs it off.
pub(crate) fn chant_gates_pass(
    world: &World,
    victim: Entity,
    skill: i32,
    rolls: ChantRolls,
) -> bool {
    let dark = world
        .get::<Located>(victim)
        .is_some_and(|l| room_is_dark(world, l.0));
    if !dark && para_save(world, victim, rolls.save) {
        return false;
    }
    if has_behavior(world, victim, MobBehavior::Sentinel)
        && para_save(world, victim, rolls.sentinel_save)
    {
        return false;
    }
    rolls.chance <= skill
}

/// Real seconds in one MUD hour (legacy effect duration tick).
const SECS_PER_MUD_HOUR: i32 = 75;
/// Legacy `PULSE_VIOLENCE`: one combat round of wait state, in seconds.
const ROUND_SECS: i32 = 4;
/// Marks a [`stun_for`] lag so a panicking victim can have it cleared
/// (legacy zeroes the wait state before `flee`) without touching real
/// stuns.
const LAG_SOURCE: &str = "fear-lag";

/// The dice of legacy `inflict_fear`, each `random_number(0, 100)`, one per
/// branch of the cascade.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FearRolls {
    /// Frozen in terror.
    pub freeze: i32,
    /// Drop the wielded weapon.
    pub drop: i32,
    /// Panic and flee.
    pub flee: i32,
    /// Falter.
    pub falter: i32,
}

impl FearRolls {
    pub(crate) fn random() -> Self {
        Self {
            freeze: rand::random_range(0..=100),
            drop: rand::random_range(0..=100),
            flee: rand::random_range(0..=100),
            falter: rand::random_range(0..=100),
        }
    }
}

/// Test seam for the spell path: a world carrying this resource uses its
/// rolls instead of random ones.
#[cfg(test)]
#[derive(Resource, Clone, Copy)]
pub(crate) struct PinnedFearRolls(pub FearRolls);

/// What `inflict_fear` did to its victim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FearOutcome {
    /// Asleep: in no condition to notice the illusion.
    Unnoticed,
    /// Already paralysed: couldn't even move.
    AlreadyParalysed,
    /// Frozen in terror (paralysed), fight stopped.
    Frozen,
    /// Dropped the wielded weapon, fight stopped, lagged one round.
    DroppedWeapon,
    /// Panicked; the result of the flee attempt.
    Fled(Panic),
    /// Faltered: fight stopped, half a round of lag.
    Faltered,
    /// Barely raised an eyebrow.
    Shrugged,
}

/// Wear off a victim's lasting `feared` status (the Rust-side effect the
/// spell applied): only legacy's flee branch leaves fear behind.
fn clear_fear(world: &mut World, victim: Entity) {
    remove_fear_effects(world, victim);
    try_remove::<Feared>(world, victim);
}

/// Hold `victim` for `secs` under an effect called `name` (`"paralyzed"`
/// for terror, `"stun"` for a wait state): the [`Stunned`] marker keeps it
/// from swinging or fleeing, the instance blocks movement and casting. A
/// longer effect already running is not shortened.
fn stun_for(world: &mut World, victim: Entity, name: &str, secs: i32, source: &str) {
    let mut found = false;
    {
        let mut q = world.query::<(&mut EffectInstance, &AppliedTo)>();
        for (mut inst, applied) in q.iter_mut(world) {
            if applied.0 == victim && inst.name.eq_ignore_ascii_case(name) {
                inst.remaining_secs = inst.remaining_secs.max(secs);
                found = true;
            }
        }
    }
    if found {
        sync_stunned(world, victim);
        return;
    }
    let kind = world
        .get_resource::<EffectCatalog>()
        .and_then(|c| c.find_by_name(name))
        .map_or(0, |d| d.id);
    world.spawn((
        EffectInstance {
            kind,
            name: name.to_string(),
            strength: 1,
            remaining_secs: secs,
            source: EffectSource::Other(source.to_string()),
            ability_id: None,
        },
        AppliedTo(victim),
    ));
    sync_stunned(world, victim);
}

/// Legacy `WAIT_STATE(victim, secs)`: a short stun that fear itself put on.
fn lag_for(world: &mut World, victim: Entity, secs: i32) {
    stun_for(world, victim, "stun", secs, LAG_SOURCE);
}

/// Legacy "turn off wait states so they can flee": strip fear's own lag.
fn clear_lag(world: &mut World, victim: Entity) {
    let lags: Vec<Entity> = {
        let mut q = world.query::<(Entity, &EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, inst, applied)| {
                applied.0 == victim
                    && matches!(&inst.source, EffectSource::Other(s) if s == LAG_SOURCE)
            })
            .map(|(e, _, _)| e)
            .collect()
    };
    for e in lags {
        if let Ok(em) = world.get_entity_mut(e) {
            em.despawn();
        }
    }
    sync_stunned(world, victim);
}

/// The victim's wielded weapon, if any (the one slot disarm also reads).
fn wielded(world: &mut World, victim: Entity) -> Option<Entity> {
    let mut q = world.query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
    q.iter(world)
        .find(|(_, l, eq)| l.0 == victim && eq.0 == Slot::Wield)
        .map(|(e, _, _)| e)
}

/// One `act` line, built from the (caster, victim) names.
type Line<'a> = dyn Fn(&str, &str) -> String + 'a;

/// Who is in a fear scene and what to call them in its three `act` views.
struct Scene {
    caster: Entity,
    victim: Entity,
    room: Option<Entity>,
    /// Capitalised, as every line starts with a name.
    caster_name: String,
    victim_name: String,
}

impl Scene {
    fn new(world: &World, caster: Entity, victim: Entity) -> Self {
        Self {
            caster,
            victim,
            room: world.get::<Located>(victim).map(|l| l.0),
            caster_name: cap_sentence_start(&name_of(world, caster)),
            victim_name: cap_sentence_start(&name_of(world, victim)),
        }
    }

    /// Legacy `act` to `TO_CHAR`, `TO_VICT` and `TO_NOTVICT`; each line is
    /// built from `(caster name, victim name)`.
    fn act(&self, world: &mut World, lines: (&Line, &Line, &Line)) {
        let (c, v) = (self.caster_name.as_str(), self.victim_name.as_str());
        send_to(world, self.caster, format!("{}\r\n", lines.0(c, v)));
        send_to(world, self.victim, format!("{}\r\n", lines.1(c, v)));
        if let Some(room) = self.room {
            broadcast_room_except_rendered(
                world,
                room,
                &[self.caster, self.victim],
                &format!("{}\r\n", lines.2(c, v)),
            );
        }
    }
}

/// The three terror lines of the frozen branch.
fn act_frozen(world: &mut World, scene: &Scene) {
    scene.act(
        world,
        (
            &|_, v| format!("You frighten {v} so bad that they are frozen in terror!"),
            &|c, _| {
                format!(
                    "<magenta>{c} shows you a vision so <b>terrifying</><magenta> that you \
                     freeze in horror!</>"
                )
            },
            &|c, v| format!("<magenta>{v} is frozen in shock at {c}'s vision of <b>terror!</></>"),
        ),
    );
}

/// Frozen branch: paralysis, fight stopped on both sides, mob remembers.
fn fear_freezes(world: &mut World, scene: &Scene, power: i32) -> FearOutcome {
    act_frozen(world, scene);
    try_remove::<Fighting>(world, scene.victim);
    crate::casting::cancel_own_cast(world, scene.victim);
    stun_for(
        world,
        scene.victim,
        "paralyzed",
        (2 + power / 30) * SECS_PER_MUD_HOUR,
        FREEZE_SOURCE,
    );
    if world.get::<Fighting>(scene.caster).map(|f| f.0) == Some(scene.victim) {
        try_remove::<Fighting>(world, scene.caster);
    }
    if world.get::<Mob>(scene.victim).is_some() {
        crate::combat::remember_attacker(world, scene.victim, scene.caster);
    }
    FearOutcome::Frozen
}

/// Drop branch. Legacy `unequip_char` + `obj_to_room`: a cursed or no-drop
/// weapon falls like any other, as it does for `disarm`.
fn fear_drops_weapon(world: &mut World, scene: &Scene, weapon: Entity) -> FearOutcome {
    let w = name_or(world, weapon, "<weapon>");
    scene.act(
        world,
        (
            &|_, v| format!("You made {v} drop their {w}!"),
            &|c, _| format!("{c} frightens you so badly that you forget to hold on to your {w}!"),
            &|_, v| format!("{v} is so terrified that they drop {w}!"),
        ),
    );
    if let (Ok(mut e), Some(room)) = (world.get_entity_mut(weapon), scene.room) {
        e.remove::<EquippedSlot>();
        e.insert(Located(room));
    }
    try_remove::<Fighting>(world, scene.victim);
    crate::casting::cancel_own_cast(world, scene.victim);
    lag_for(world, scene.victim, ROUND_SECS);
    FearOutcome::DroppedWeapon
}

/// Flee branch: fight and cast stopped, wait cleared, panic, then a round
/// of wait. A mob remembers the caster (inside [`panic_flee`]).
fn fear_flees(world: &mut World, scene: &Scene) -> FearOutcome {
    try_remove::<Fighting>(world, scene.victim);
    crate::casting::cancel_own_cast(world, scene.victim);
    scene.act(
        world,
        (
            &|_, v| format!("{v} shrieks madly at your vision of terror!"),
            &|c, _| format!("{c} fills you with such horror that you panic!"),
            &|_, v| format!("{v} shrieks uncontrollably!"),
        ),
    );
    clear_lag(world, scene.victim);
    let panic = panic_flee(world, scene.victim, Some(scene.caster));
    lag_for(world, scene.victim, ROUND_SECS);
    FearOutcome::Fled(panic)
}

/// Falter branch: fight stopped, half a round of wait.
fn fear_falters(world: &mut World, scene: &Scene) -> FearOutcome {
    scene.act(
        world,
        (
            &|_, v| format!("{v} gets a scared look, but soldiers on."),
            &|c, _| format!("{c} frightens you, but you recover."),
            &|c, v| format!("{v} looks frightened at {c}'s fearful illusion, but recovers."),
        ),
    );
    try_remove::<Fighting>(world, scene.victim);
    crate::casting::cancel_own_cast(world, scene.victim);
    lag_for(world, scene.victim, ROUND_SECS / 2);
    FearOutcome::Faltered
}

/// Last branch: the illusion is a joke.
fn fear_shrugged(world: &mut World, scene: &Scene) -> FearOutcome {
    scene.act(
        world,
        (
            &|_, v| format!("{v} barely raises an eyebrow at your fearful illusion."),
            &|c, _| format!("{c} tries to frighten you with a pitiful illusion.  Yawn."),
            &|c, v| format!("{v} barely notices when {c} tries to frighten them."),
        ),
    );
    FearOutcome::Shrugged
}

/// Legacy `inflict_fear` for a victim the caller already vetted with
/// `attack_ok` and that failed the `feared` effect's WILL save. `power` is
/// the caster's skill. Each branch is a level-difference roll
/// (`d = power - victim level`, `random_number(0, 100) < threshold`),
/// tried in order:
///
/// * frozen in terror: `min(80, 17d/10)`; paralysed `2 + power/30` MUD hours
/// * drop weapon (needs one): `min(85, 1 + 18d/10)`; lagged one round
/// * flee: `min(90, 3 + 19d/10)`; lagged one round after running
/// * falter: `min(95, 5 + 20d/10)`; lagged half a round
/// * otherwise nothing
///
/// Everything but the flee branch ends the lasting `feared` status. Drop,
/// falter and nothing make the victim fight back (when `attack_ok` allows).
pub(crate) fn inflict_fear(
    world: &mut World,
    caster: Entity,
    victim: Entity,
    power: i32,
    rolls: FearRolls,
) -> FearOutcome {
    let scene = Scene::new(world, caster, victim);
    if world.get::<Posture>(victim).map(|p| p.0) == Some(PostureKind::Sleeping) {
        let v = &scene.victim_name;
        send_to(
            world,
            caster,
            format!("{v} is in no condition to notice your illusion.\r\n"),
        );
        clear_fear(world, victim);
        return FearOutcome::Unnoticed;
    }
    if has_effect_named(world, victim, "paralyzed") {
        scene.act(
            world,
            (
                &|_, v| format!("{v} doesn't even move."),
                &|c, _| format!("{c} shows you visions of great horror, but you can't even move!"),
                &|_, v| format!("{v} doesn't doesn't appear to notice."),
            ),
        );
        clear_fear(world, victim);
        return FearOutcome::AlreadyParalysed;
    }

    let weapon = wielded(world, victim);
    let diff = power - mud_world::effective_level(world, victim);

    let outcome = if rolls.freeze < 80.min(17 * diff / 10) {
        fear_freezes(world, &scene, power)
    } else if let Some(weapon) = weapon.filter(|_| rolls.drop < 85.min(1 + 18 * diff / 10)) {
        fear_drops_weapon(world, &scene, weapon)
    } else if rolls.flee < 90.min(3 + 19 * diff / 10) {
        // Only this branch leaves the lasting status.
        return fear_flees(world, &scene);
    } else if rolls.falter < 95.min(5 + 20 * diff / 10) {
        fear_falters(world, &scene)
    } else {
        fear_shrugged(world, &scene)
    };

    clear_fear(world, victim);
    if matches!(
        outcome,
        FearOutcome::DroppedWeapon | FearOutcome::Faltered | FearOutcome::Shrugged
    ) && crate::commands::attack_ok(world, victim, caster, false)
    {
        try_insert(world, victim, Fighting(caster));
    }
    outcome
}

/// A `feared` status effect just landed on `victim`: mark it, then act on
/// it. `area_skill` is `Some(skill)` for an area ability (the ivory
/// symphony chant), which keeps its own extra gates and a plain panic; a
/// victim that passes them keeps neither the flee nor the effect. Any
/// other fear spell runs legacy's [`inflict_fear`] cascade with `power`.
pub(crate) fn on_fear_applied(
    world: &mut World,
    caster: Entity,
    victim: Entity,
    power: i32,
    area: bool,
) {
    try_insert(world, victim, Feared);
    if area {
        if !chant_gates_pass(world, victim, power, ChantRolls::random()) {
            clear_fear(world, victim);
            return;
        }
        panic_flee(world, victim, Some(caster));
        return;
    }
    #[cfg(test)]
    let rolls = world
        .get_resource::<PinnedFearRolls>()
        .map_or_else(FearRolls::random, |p| p.0);
    #[cfg(not(test))]
    let rolls = FearRolls::random();
    inflict_fear(world, caster, victim, power, rolls);
}

/// Drop the [`Feared`] marker from anyone with no `feared` effect left
/// (expired, dispelled, cleansed). Cheap: only marked actors are visited.
pub(crate) fn sync_markers(world: &mut World) {
    let marked: Vec<Entity> = world
        .query_filtered::<Entity, With<Feared>>()
        .iter(world)
        .collect();
    if marked.is_empty() {
        return;
    }
    let backed: HashSet<Entity> = {
        let rows = fear_default_rows(world);
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(eff, _)| instance_is_fear(eff, &rows))
            .map(|(_, applied)| applied.0)
            .collect()
    };
    for e in marked {
        if !backed.contains(&e) {
            try_remove::<Feared>(world, e);
        }
    }
}

/// Combat-tick pass: every feared mob in a fight tries to run instead of
/// swinging. A cornered one keeps its fight and swings as usual.
pub(crate) fn feared_mobs_flee(world: &mut World) {
    let scared: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<(Entity, &Located), (
            With<Mob>,
            With<Feared>,
            With<Fighting>,
            Without<Stunned>,
            Without<Frozen>,
            Without<Ghost>,
        )>();
        q.iter(world).map(|(e, l)| (e, l.0)).collect()
    };
    for (mob, room) in scared {
        let standing = world
            .get::<Posture>(mob)
            .is_none_or(|p| p.0 == PostureKind::Standing);
        let ridden = world.get::<RiddenBy>(mob).is_some();
        if standing
            && !ridden
            && !berserk_holds(world, mob)
            && !effect_prevents(world, mob, Prevent::Movement)
        {
            mob_flee(world, mob, room);
        }
    }
}

/// The dice of one roar victim.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoarRolls {
    /// d(0..99) PARA save.
    pub save: i32,
    /// d(0..99) sentinel second save.
    pub sentinel_save: i32,
    /// A sleeper wakes up (`random_number(0, 1)`).
    pub wake: bool,
    /// d(0..100) against `dex - 15` for the panicked trip.
    pub trip: i32,
}

impl RoarRolls {
    pub(crate) fn random() -> Self {
        Self {
            save: rand::random_range(0..=99),
            sentinel_save: rand::random_range(0..=99),
            wake: rand::random_bool(0.5),
            trip: rand::random_range(0..=100),
        }
    }
}

/// What a roar did to one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoarOutcome {
    /// Saved, immune, or already out cold: nothing happened.
    Resisted,
    /// Woke a sleeper.
    Woke,
    /// Slept on.
    SleptOn,
    /// Tripped over its own feet.
    Tripped,
    /// Panicked; the result of the flee attempt.
    Panicked(Panic),
}

/// Legacy `do_roar` / `howl` for one victim the caller already vetted with
/// `attack_ok` and group exclusion.
pub(crate) fn roar_target(
    world: &mut World,
    caster: Entity,
    victim: Entity,
    rolls: RoarRolls,
) -> RoarOutcome {
    if world.get::<Ghost>(victim).is_some() || is_fear_immune(world, victim) {
        return RoarOutcome::Resisted;
    }
    if para_save(world, victim, rolls.save) {
        return RoarOutcome::Resisted;
    }
    // Twice as hard to roar at a sentinel mob.
    if has_behavior(world, victim, MobBehavior::Sentinel)
        && para_save(world, victim, rolls.sentinel_save)
    {
        return RoarOutcome::Resisted;
    }
    if has_behavior(world, victim, MobBehavior::Aware)
        || has_behavior(world, victim, MobBehavior::NoSummon)
    {
        return RoarOutcome::Resisted;
    }
    let alignment = world.get::<CombatStats>(caster).map_or(0, |c| c.alignment);
    if world.get::<mud_world::ProtectFromEvil>(victim).is_some() && alignment <= -500 {
        let caster_name = name_of(world, caster);
        send_to(
            world,
            victim,
            format!(
                "Your holy protection strengthens your resolve against \
                 {caster_name}'s roar!\r\n"
            ),
        );
        return RoarOutcome::Resisted;
    }
    if world.get::<mud_world::ProtectFromGood>(victim).is_some() && alignment <= 500 {
        let caster_name = name_of(world, caster);
        send_to(
            world,
            victim,
            format!(
                "Your unholy protection strengthens your resolve against \
                 {caster_name}'s roar!\r\n"
            ),
        );
        return RoarOutcome::Resisted;
    }

    let room = world.get::<Located>(victim).map(|l| l.0);
    let posture = world.get::<Posture>(victim).map(|p| p.0);
    if posture == Some(PostureKind::Sleeping) {
        if !rolls.wake {
            return RoarOutcome::SleptOn;
        }
        try_insert(world, victim, Posture(PostureKind::Sitting));
        send_to(
            world,
            victim,
            "A loud ROAAARRRRRR jolts you from your slumber!\r\n",
        );
        if let Some(room) = room {
            let name = crate::commands::cap_sentence_start(&name_of(world, victim));
            broadcast_room_except_rendered(
                world,
                room,
                &[victim],
                &format!("{name} jumps up dazedly, awakened by the noise!\r\n"),
            );
        }
        return RoarOutcome::Woke;
    }
    let dex = world.get::<CoreStats>(victim).map_or(50, |s| s.dexterity);
    let standing = matches!(posture, None | Some(PostureKind::Standing));
    if standing && dex - 15 < rolls.trip {
        try_insert(world, victim, Posture(PostureKind::Sitting));
        send_to(
            world,
            victim,
            "In your panicked rush to flee, you trip!\r\n",
        );
        if let Some(room) = room {
            let name = crate::commands::cap_sentence_start(&name_of(world, victim));
            broadcast_room_except_rendered(
                world,
                room,
                &[victim],
                &format!("In a panicked rush to flee, {name} trips!\r\n"),
            );
        }
        return RoarOutcome::Tripped;
    }
    RoarOutcome::Panicked(panic_flee(world, victim, Some(caster)))
}

#[cfg(test)]
mod tests;
