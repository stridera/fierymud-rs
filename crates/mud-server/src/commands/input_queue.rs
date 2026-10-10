//! Ordered per-player input queue (issue #19).
//!
//! Legacy kept every typed or alias-expanded line in the descriptor's input
//! queue and ran one per pulse, holding the queue while the character sat in
//! a wait state. Here the same idea covers the one "slow" command kind players
//! chain: a cast. While a spell winds up (or a broken-off chant leaves its
//! lag) the dispatcher refuses every other command, so an alias such as
//! `cast 'armor';cast 'bless'` lost everything after its first spell.
//!
//! The rules:
//! * A line that is part of an alias / `;` expansion and meets the cast lock
//!   is not refused; it and every later line of that expansion are held in
//!   the player's [`InputQueue`], in order.
//! * Anything typed while the queue is non-empty joins the back of it, so a
//!   `north` typed after a buff alias cannot overtake the buffs. The
//!   exceptions are the commands allowed mid-cast (`score`, `tell`, ...),
//!   which still run at once, and `abort` / `flee` / `disengage`, which also
//!   throw the pending lines away. Death, a dropped link, a reconnect or
//!   takeover, `quit` and a completed `camp` throw them away too, so a chain
//!   never runs on after the respawn or into the next session.
//! * [`run_queued_input`] (driven once per tick by the router) releases at
//!   most one line per player per tick, and only when it is no longer held by
//!   the cast lock. Released lines were already alias-expanded and are never
//!   expanded again.
//! * A single typed line with an empty queue keeps the old behaviour: it is
//!   refused with "You are busy spellcasting..." rather than quietly queued.

use std::collections::VecDeque;

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, BoardDraft, Casting, MailDraft, SocialRegistry};

use super::{
    AliasRun, Command, abbrev_allowed, casting_blocks, dispatch_async_line, grants,
    longest_prefix_match, resolve_abbrev, send_to, split_symbol_command,
};

/// Most lines a player may have waiting. Alias expansion alone is capped at
/// 32 commands per typed line; this bounds repeated aliasing too.
pub(crate) const MAX_QUEUED_LINES: usize = 64;

/// Commands that stop a pending chain: the deliberate exits from a cast.
const FLUSH_VERBS: &[&str] = &["abort", "flee", "disengage"];

/// Commands a broken-off cast's lag holds back (see
/// `casting::cast_lag_active`): the casts themselves and the item verbs
/// that cast.
const LAG_HELD_VERBS: &[&str] = &[
    "cast", "chant", "perform", "recite", "quaff", "use", "zap", "wave", "tap", "play",
];

/// Lines waiting for the player's cast lock to lift, oldest first.
#[derive(Component, Debug, Default)]
pub(crate) struct InputQueue {
    lines: VecDeque<String>,
    /// The "queue is full" notice already went out for the current overflow
    /// streak; reset once the queue has room again.
    overflow_told: bool,
}

/// What a typed line resolves to, as far as the cast lock cares.
enum Resolved {
    Command(&'static Command),
    Social,
    Unknown,
}

fn resolve(world: &World, player: Entity, line: &str) -> Resolved {
    let line = split_symbol_command(line.trim());
    let lower = line.to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let Some(first) = tokens.first().copied() else {
        return Resolved::Unknown;
    };
    if let Some((cmd, _)) = longest_prefix_match(&tokens) {
        return Resolved::Command(cmd);
    }
    let socials = world.get_resource::<SocialRegistry>();
    if socials.is_some_and(|r| r.get(first).is_some()) {
        return Resolved::Social;
    }
    let (role, perms) = world.get::<Account>(player).map_or_else(
        || (UserRole::Player, Vec::new()),
        |a| (a.role, a.perms.clone()),
    );
    let grants = world.get::<mud_world::CommandGrants>(player);
    let allow = grants::mortal_allowlist_for(world, role, grants);
    match resolve_abbrev(first, role, &perms, grants, &allow, socials) {
        Some(super::Abbrev::Command(cmd)) if abbrev_allowed(cmd) => Resolved::Command(cmd),
        Some(super::Abbrev::Social(_)) => Resolved::Social,
        _ => Resolved::Unknown,
    }
}

/// True when `line` would be refused right now only because the player is
/// winding up a cast (or recovering from one that broke off).
pub(crate) fn must_wait(world: &World, player: Entity, line: &str) -> bool {
    let casting = world.get::<Casting>(player).is_some();
    let lagging = crate::casting::cast_lag_active(world, player);
    if !casting && !lagging {
        return false;
    }
    match resolve(world, player, line) {
        Resolved::Command(cmd) => {
            (casting && casting_blocks(world, player, cmd))
                || (lagging && LAG_HELD_VERBS.contains(&cmd.names[0]))
        }
        Resolved::Social => casting,
        Resolved::Unknown => false,
    }
}

fn is_flush_command(world: &World, player: Entity, line: &str) -> bool {
    matches!(resolve(world, player, line), Resolved::Command(cmd) if FLUSH_VERBS.contains(&cmd.names[0]))
}

fn pending(world: &World, player: Entity) -> bool {
    world
        .get::<InputQueue>(player)
        .is_some_and(|q| !q.lines.is_empty())
}

/// Whether `player` has lines waiting.
#[must_use]
pub fn has_queued_input(world: &World, player: Entity) -> bool {
    pending(world, player)
}

fn push(world: &mut World, player: Entity, line: &str) {
    if world.get::<InputQueue>(player).is_none() {
        super::try_insert(world, player, InputQueue::default());
    }
    let Some(mut q) = world.get_mut::<InputQueue>(player) else {
        return;
    };
    if q.lines.len() >= MAX_QUEUED_LINES {
        // One notice per overflow streak, not one per dropped line.
        let first = !std::mem::replace(&mut q.overflow_told, true);
        if first {
            send_to(
                world,
                player,
                "Your command queue is full; the extra commands were dropped.\r\n",
            );
        }
        return;
    }
    q.lines.push_back(line.to_string());
}

/// Put `lines` straight into the queue (tests that need a backlog without
/// a cast to hold it).
#[cfg(test)]
pub(crate) fn seed_for_test(world: &mut World, player: Entity, lines: &[&str]) {
    for l in lines {
        push(world, player, l);
    }
}

/// How many lines are waiting.
#[cfg(test)]
pub(crate) fn queued_len(world: &World, player: Entity) -> usize {
    world.get::<InputQueue>(player).map_or(0, |q| q.lines.len())
}

/// Drop everything waiting; returns how many lines went.
pub(crate) fn clear(world: &mut World, player: Entity) -> usize {
    world.get_mut::<InputQueue>(player).map_or(0, |mut q| {
        q.overflow_told = false;
        std::mem::take(&mut q.lines).len()
    })
}

/// Called with a fully alias-expanded `line` just before it would be
/// dispatched. Returns true when the line was taken into the queue (the
/// caller must not run it).
pub(super) fn defer_if_needed(
    world: &mut World,
    player: Entity,
    line: &str,
    run: &mut AliasRun,
) -> bool {
    if run.replaying || line.trim().is_empty() {
        return false;
    }
    let queued = pending(world, player);
    if queued && is_flush_command(world, player, line) {
        let dropped = clear(world, player);
        if dropped > 0 {
            send_to(
                world,
                player,
                format!("Your {dropped} queued command(s) were cleared.\r\n"),
            );
        }
        return false;
    }
    if !run.in_expansion() && !queued {
        // A lone typed line: the cast lock refuses it as before.
        return false;
    }
    let hold = run.deferred
        || must_wait(world, player, line)
        // Behind earlier waiting lines: keep order, except for the
        // commands that are allowed mid-cast, which run now.
        || (queued && world.get::<Casting>(player).is_none());
    if !hold {
        return false;
    }
    push(world, player, line);
    run.deferred = true;
    true
}

/// Take the oldest waiting line if the cast lock no longer holds it.
fn take_ready(world: &mut World, player: Entity) -> Option<String> {
    // A line typed while composing a mail / board post is post text; leave
    // anything queued until the draft is done.
    if world.get::<MailDraft>(player).is_some() || world.get::<BoardDraft>(player).is_some() {
        return None;
    }
    let front = world.get::<InputQueue>(player)?.lines.front()?.clone();
    if must_wait(world, player, &front) {
        return None;
    }
    let mut q = world.get_mut::<InputQueue>(player)?;
    // Room again: the next overflow is a new streak.
    q.overflow_told = false;
    q.lines.pop_front()
}

/// Run the player's oldest waiting line, if the cast lock allows. At most
/// one line per call (legacy: one command per pulse). Returns whether a
/// line ran.
pub async fn run_queued_input(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) -> bool {
    let Some(line) = take_ready(world, player) else {
        return false;
    };
    let mut run = AliasRun::replay();
    dispatch_async_line(world, player, pool, &line, &mut run).await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dispatch_with_async;
    use crate::commands::test_support::{Rx, ability_def, drain, player_in};
    use mud_db::abilities::AbilityKind;
    use mud_world::{
        AbilityCatalog, Aliases, EffectCatalog, EffectDef, Health, KnownAbilities, SpellSlotData,
    };

    const ARMOR: i32 = 1;
    const BLESS: i32 = 2;
    const EFFECT: i32 = 10;

    fn lazy_pool() -> mud_db::sqlx::PgPool {
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    /// A caster who knows two one-round buff spells and owns an alias
    /// `buff` = `cast 'armor';cast 'bless'`.
    fn buffer() -> (World, Entity, Rx) {
        let mut world = World::new();
        let mut catalog = AbilityCatalog::default();
        for (id, name) in [(ARMOR, "Armor"), (BLESS, "Bless")] {
            let mut def = ability_def(id, name, AbilityKind::Spell);
            def.cast_time_rounds = 1;
            catalog.by_name.insert(name.to_ascii_lowercase(), def);
            catalog
                .effects_for
                .insert(id, vec![(EFFECT, Some(serde_json::json!({ "amount": 1 })))]);
        }
        world.insert_resource(catalog);
        let mut effects = EffectCatalog::default();
        effects.by_id.insert(
            EFFECT,
            EffectDef {
                id: EFFECT,
                name: "mend".to_string(),
                description: None,
                effect_type: "heal".to_string(),
                tags: Vec::new(),
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
        world.insert_resource(effects);
        world.insert_resource(SpellSlotData::default());
        world.insert_resource(mud_world::ClassSkillsData::default());
        let room = world.spawn_empty().id();
        let (caster, rx) = player_in(&mut world, room);
        let mut aliases = Aliases::default();
        aliases
            .entries
            .push(("buff".into(), "cast 'armor';cast 'bless'".into()));
        world.entity_mut(caster).insert((
            Account {
                user_id: "u".into(),
                character_id: "c".into(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: vec![],
            },
            Health { hp: 50, max: 50 },
            KnownAbilities {
                entries: vec![(ARMOR, 500, true), (BLESS, 500, true)],
            },
            aliases,
        ));
        (world, caster, rx)
    }

    /// Tick the cast wind-up to completion, then let one queued line out.
    async fn finish_cast_and_release(
        world: &mut World,
        caster: Entity,
        pool: &mud_db::sqlx::PgPool,
    ) {
        for _ in 0..400 {
            if world.get::<Casting>(caster).is_none() {
                break;
            }
            crate::casting::casting_tick(world);
        }
        assert!(world.get::<Casting>(caster).is_none(), "cast never landed");
        run_queued_input(world, caster, pool).await;
    }

    fn started(out: &str, spell: &str) -> bool {
        out.to_lowercase().contains(spell)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn alias_chain_casts_every_spell_in_order() {
        let (mut world, caster, mut rx) = buffer();
        let pool = lazy_pool();
        dispatch_with_async(&mut world, caster, &pool, "buff").await;
        // The first spell is winding up; the second is waiting, not refused.
        let out = drain(&mut rx);
        assert!(started(&out, "armor"), "{out}");
        assert!(!out.contains("busy spellcasting"), "{out}");
        assert!(world.get::<Casting>(caster).is_some());
        assert_eq!(world.get::<InputQueue>(caster).unwrap().lines.len(), 1);
        // Nothing is released while the first cast is still going.
        assert!(!run_queued_input(&mut world, caster, &pool).await);

        finish_cast_and_release(&mut world, caster, &pool).await;
        let out = drain(&mut rx);
        assert!(started(&out, "bless"), "second cast started: {out}");
        assert!(
            world.get::<Casting>(caster).is_some(),
            "bless is winding up"
        );
        assert!(!has_queued_input(&world, caster));
        let armor_at = out.to_lowercase().find("armor");
        let bless_at = out.to_lowercase().find("bless");
        assert!(bless_at.is_some() && armor_at.is_none_or(|a| a < bless_at.unwrap()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn later_typed_lines_wait_behind_the_chain() {
        let (mut world, caster, mut rx) = buffer();
        let pool = lazy_pool();
        dispatch_with_async(&mut world, caster, &pool, "buff").await;
        let _ = drain(&mut rx);
        // Mid-cast: an allowed command runs now ...
        dispatch_with_async(&mut world, caster, &pool, "look").await;
        assert!(has_queued_input(&world, caster));
        // ... a blocked one joins the back of the queue.
        dispatch_with_async(&mut world, caster, &pool, "north").await;
        let q: Vec<&str> = world
            .get::<InputQueue>(caster)
            .unwrap()
            .lines
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(q, vec!["cast 'bless'", "north"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abort_clears_the_pending_chain() {
        let (mut world, caster, mut rx) = buffer();
        let pool = lazy_pool();
        dispatch_with_async(&mut world, caster, &pool, "buff").await;
        let _ = drain(&mut rx);
        dispatch_with_async(&mut world, caster, &pool, "abort").await;
        assert!(!has_queued_input(&world, caster));
        let out = drain(&mut rx);
        assert!(out.contains("queued command"), "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_lone_typed_line_is_still_refused_mid_cast() {
        let (mut world, caster, mut rx) = buffer();
        let pool = lazy_pool();
        dispatch_with_async(&mut world, caster, &pool, "cast 'armor'").await;
        let _ = drain(&mut rx);
        dispatch_with_async(&mut world, caster, &pool, "cast 'bless'").await;
        let out = drain(&mut rx);
        assert!(out.contains("busy spellcasting"), "{out}");
        assert!(!has_queued_input(&world, caster));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queue_is_bounded() {
        let (mut world, caster, mut rx) = buffer();
        let pool = lazy_pool();
        dispatch_with_async(&mut world, caster, &pool, "buff").await;
        for _ in 0..(MAX_QUEUED_LINES + 5) {
            dispatch_with_async(&mut world, caster, &pool, "north").await;
        }
        assert_eq!(
            world.get::<InputQueue>(caster).unwrap().lines.len(),
            MAX_QUEUED_LINES
        );
        let out = drain(&mut rx);
        assert_eq!(
            out.matches("queue is full").count(),
            1,
            "one notice per overflow streak: {out}"
        );
        // Room again, then a second overflow: a new streak, a new notice.
        world
            .get_mut::<InputQueue>(caster)
            .unwrap()
            .lines
            .pop_front();
        world.get_mut::<InputQueue>(caster).unwrap().overflow_told = false;
        dispatch_with_async(&mut world, caster, &pool, "north").await;
        dispatch_with_async(&mut world, caster, &pool, "north").await;
        dispatch_with_async(&mut world, caster, &pool, "north").await;
        let out = drain(&mut rx);
        assert_eq!(out.matches("queue is full").count(), 1, "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn death_clears_the_pending_chain() {
        let (mut world, caster, mut rx) = buffer();
        let pool = lazy_pool();
        world.insert_resource(crate::TickCount(0));
        dispatch_with_async(&mut world, caster, &pool, "buff").await;
        let _ = drain(&mut rx);
        assert!(has_queued_input(&world, caster));
        let room = world.get::<mud_world::Located>(caster).unwrap().0;
        world.get_mut::<Health>(caster).unwrap().hp = 0;
        crate::combat::handle_death(&mut world, caster, "Caster", room);
        assert!(world.get::<mud_world::Ghost>(caster).is_some());
        assert!(!has_queued_input(&world, caster));
    }

    #[test]
    fn quit_clears_the_pending_chain() {
        let (mut world, caster, _rx) = buffer();
        seed_for_test(&mut world, caster, &["cast 'bless'", "north"]);
        assert_eq!(queued_len(&world, caster), 2);
        assert!(crate::commands::info::begin_quit(
            &mut world, caster, "Bye.\r\n"
        ));
        assert_eq!(queued_len(&world, caster), 0);
    }
}
