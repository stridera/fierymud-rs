//! Shared quest-objective completion pipeline.
//!
//! Every objective source (kill / collect / visit / talk / deliver /
//! use-skill bumps, dialogue keyword matches, the CUSTOM_LUA sweep, the
//! held-items recheck on phase entry) funnels through
//! [`record_progress`], so a completing objective always advances the
//! phase, announces it, and pays the rewards when the quest finishes.
//!
//! The functions here are async and DB-backed; the world side stays in
//! `PendingPlayerUpdate` messages drained on the tick.

#![allow(clippy::doc_markdown)]

use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;
use mud_world::{Account, Contents, EquippedSlot, Item, Located, Player, WorldKey};

use crate::commands::{Connection, DbPool, PendingPlayerUpdate, PlayerUpdateTx};

/// Where to tell one player about quest progress, and how to push the
/// resulting ECS mutations (XP, gold, items) back to the world thread.
#[derive(Clone)]
pub(crate) struct Notifier {
    pub character_id: String,
    pub out: mud_net::Outbound,
    pub update_tx: Option<tokio::sync::mpsc::Sender<PendingPlayerUpdate>>,
}

impl Notifier {
    pub(crate) fn say(&self, text: &str) {
        let _ = self.out.try_send(text.as_bytes().to_vec());
    }

    /// Build a notifier for an online player, or `None` when it has no
    /// account/connection (mobs, link-dead entities).
    pub(crate) fn for_player(world: &World, player: Entity) -> Option<Self> {
        Some(Self {
            character_id: world.get::<Account>(player)?.character_id.clone(),
            out: world.get::<Connection>(player)?.0.clone(),
            update_tx: world.get_resource::<PlayerUpdateTx>().map(|t| t.0.clone()),
        })
    }
}

/// The identity and display data of one objective row, independent of
/// which query produced it.
#[derive(Debug, Clone)]
pub struct ObjectiveRef {
    pub character_quest_id: String,
    pub quest_zone_id: i32,
    pub quest_id: i32,
    pub phase_id: i32,
    pub objective_id: i32,
    pub required_count: i32,
    pub show_progress: bool,
    pub player_description: String,
}

impl From<&mud_db::quest_objectives::ObjectiveProgressRow> for ObjectiveRef {
    fn from(r: &mud_db::quest_objectives::ObjectiveProgressRow) -> Self {
        Self {
            character_quest_id: r.character_quest_id.clone(),
            quest_zone_id: r.quest_zone_id,
            quest_id: r.quest_id,
            phase_id: r.phase_id,
            objective_id: r.objective_id,
            required_count: r.required_count,
            show_progress: r.show_progress,
            player_description: r.player_description.clone(),
        }
    }
}

impl From<&mud_db::quest_objectives::CollectObjectiveRow> for ObjectiveRef {
    fn from(r: &mud_db::quest_objectives::CollectObjectiveRow) -> Self {
        Self {
            character_quest_id: r.character_quest_id.clone(),
            quest_zone_id: r.quest_zone_id,
            quest_id: r.quest_id,
            phase_id: r.phase_id,
            objective_id: r.objective_id,
            required_count: r.required_count,
            show_progress: r.show_progress,
            player_description: r.player_description.clone(),
        }
    }
}

impl From<&mud_db::quest_objectives::CustomLuaObjective> for ObjectiveRef {
    fn from(r: &mud_db::quest_objectives::CustomLuaObjective) -> Self {
        Self {
            character_quest_id: r.character_quest_id.clone(),
            quest_zone_id: r.quest_zone_id,
            quest_id: r.quest_id,
            phase_id: r.phase_id,
            objective_id: r.objective_id,
            required_count: r.required_count,
            show_progress: r.show_progress,
            player_description: r.player_description.clone(),
        }
    }
}

/// Add one to `obj`'s progress, tell the player, and run the
/// phase-advance / quest-completion handling when it reached its
/// required count. `party_prefix` is `"(party) "` for a group member
/// credited by someone else's action, otherwise empty.
///
/// The increment is a single atomic SQL statement, so bumps that race
/// (several tasks for one player) neither lose steps nor complete the
/// objective twice; a bump that loses the race to completion is a
/// no-op.
pub(crate) async fn record_progress(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    obj: &ObjectiveRef,
    party_prefix: &str,
) {
    let (new_count, completed) = match mud_db::quest_objectives::increment_progress(
        pool,
        &obj.character_quest_id,
        obj.quest_zone_id,
        obj.quest_id,
        obj.phase_id,
        obj.objective_id,
        obj.required_count,
    )
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "objective increment failed");
            return;
        }
    };
    notify.say(&progress_line(obj, new_count, completed, party_prefix));
    if completed {
        advance_quest(
            pool,
            notify,
            &obj.character_quest_id,
            obj.quest_zone_id,
            obj.quest_id,
        )
        .await;
    }
}

/// The progress message for an objective at `new_count`.
fn progress_line(
    obj: &ObjectiveRef,
    new_count: i32,
    completed: bool,
    party_prefix: &str,
) -> String {
    if completed {
        format!(
            "{party_prefix}Quest objective complete: {}\r\n",
            obj.player_description
        )
    } else if obj.show_progress {
        format!(
            "{party_prefix}Quest objective: {} ({}/{})\r\n",
            obj.player_description, new_count, obj.required_count
        )
    } else {
        format!(
            "{party_prefix}Quest objective updated: {}\r\n",
            obj.player_description
        )
    }
}

/// Run the phase-advance check for one quest and announce the outcome:
/// a phase hop re-checks the held items for the new phase, a finished
/// quest pays its rewards.
pub(crate) async fn advance_quest(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    character_quest_id: &str,
    quest_zone_id: i32,
    quest_id: i32,
) {
    match mud_db::quest_objectives::try_advance_phase(pool, character_quest_id).await {
        Ok(mud_db::quest_objectives::PhaseAdvance::Advanced { name, .. }) => {
            notify.say(&format!("Quest phase complete — moving to: {name}\r\n"));
            // The new phase may already be satisfiable from what the
            // character carries; let the world thread look.
            if let Some(tx) = &notify.update_tx {
                let _ = tx
                    .send(PendingPlayerUpdate::QuestPhaseEntered {
                        character_id: notify.character_id.clone(),
                    })
                    .await;
            }
        }
        Ok(mud_db::quest_objectives::PhaseAdvance::QuestComplete) => {
            notify.say("*** Quest complete! ***\r\n");
            grant_completion_rewards(pool, notify, quest_zone_id, quest_id).await;
        }
        Ok(mud_db::quest_objectives::PhaseAdvance::Pending) => {}
        Err(e) => tracing::warn!(error = %e, "phase advance check failed"),
    }
}

/// Pay a finished quest's unconditional rewards: DB writes first, then
/// the matching ECS mutations so the player sees the gain without
/// relogging. Conditional rewards are deferred to `qreward`, which can
/// evaluate the condition on the world thread.
pub(crate) async fn grant_completion_rewards(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    quest_zone_id: i32,
    quest_id: i32,
) {
    let all_rewards = mud_db::quest_objectives::list_quest_rewards(pool, quest_zone_id, quest_id)
        .await
        .unwrap_or_default();
    let (deferred, rewards): (Vec<_>, Vec<_>) = all_rewards
        .into_iter()
        .partition(|r| r.condition.as_deref().is_some_and(|c| !c.trim().is_empty()));
    if !deferred.is_empty() {
        notify.say(&format!(
            "Conditional rewards available — \
             type 'qreward {quest_zone_id} {quest_id}' to view and claim.\r\n"
        ));
    }
    if rewards.is_empty() {
        return;
    }
    let cid = &notify.character_id;
    if let Err(e) = mud_db::quest_objectives::grant_simple_rewards(pool, cid, &rewards).await {
        tracing::warn!(error = %e, "reward grant failed");
    }
    for r in &rewards {
        let update = match r.reward_type.as_str() {
            "EXPERIENCE" => r.amount.map(|a| PendingPlayerUpdate::ExperienceDelta {
                character_id: cid.clone(),
                amount: a,
            }),
            "GOLD" => r.amount.map(|a| PendingPlayerUpdate::WealthDelta {
                character_id: cid.clone(),
                amount: i64::from(a),
            }),
            "SKILL_POINTS" => r.amount.map(|a| PendingPlayerUpdate::SkillPointsDelta {
                character_id: cid.clone(),
                amount: a,
            }),
            "ABILITY" => r.ability_id.map(|id| PendingPlayerUpdate::AbilityKnown {
                character_id: cid.clone(),
                ability_id: id,
            }),
            "ITEM" => match (r.object_zone_id, r.object_id) {
                (Some(z), Some(id)) => Some(PendingPlayerUpdate::SpawnItem {
                    character_id: cid.clone(),
                    object_zone: z,
                    object_id: id,
                    quantity: r.quantity,
                }),
                _ => None,
            },
            _ => None,
        };
        if let (Some(u), Some(tx)) = (update, notify.update_tx.as_ref()) {
            // Bounded channel — await until the tick drains a slot.
            // Failure means the receiver dropped (shutting down).
            let _ = tx.send(u).await;
        }
    }
    let mut buf = String::from("Rewards:\r\n");
    for r in &rewards {
        let line = match (r.reward_type.as_str(), r.amount, r.quantity) {
            ("EXPERIENCE", Some(a), _) => format!("  +{a} experience\r\n"),
            ("GOLD", Some(a), _) => format!("  +{a} gold\r\n"),
            ("SKILL_POINTS", Some(a), _) => format!("  +{a} skill points\r\n"),
            ("ABILITY", _, _) => "  +1 new ability\r\n".to_string(),
            ("ITEM", _, q) => format!("  +{q} item(s)\r\n"),
            ("HOUSING", _, _) => "  +housing access — see questgiver\r\n".to_string(),
            _ => continue,
        };
        buf.push_str(&line);
    }
    if buf.len() > "Rewards:\r\n".len() {
        notify.say(&buf);
    }
}

/// How many of each prototype `holder` carries directly (inventory and
/// worn gear; items inside containers do not count), read from the
/// holder's `Contents` reverse index - O(items carried), not O(world).
pub(crate) fn held_counts(world: &World, holder: Entity) -> HashMap<(i32, i32), i32> {
    held_matching(world, holder, |_| true)
}

/// [`held_counts`] restricted to the prototypes in `targets`.
fn held_target_counts(
    world: &World,
    holder: Entity,
    targets: &HashSet<(i32, i32)>,
) -> HashMap<(i32, i32), i32> {
    held_matching(world, holder, |key| targets.contains(&key))
}

fn held_matching(
    world: &World,
    holder: Entity,
    wanted: impl Fn((i32, i32)) -> bool,
) -> HashMap<(i32, i32), i32> {
    let mut counts: HashMap<(i32, i32), i32> = HashMap::new();
    let Some(contents) = world.get::<Contents>(holder) else {
        return counts;
    };
    for item in contents.iter() {
        if world.get::<Item>(item).is_none() {
            continue;
        }
        if let Some(wk) = world.get::<WorldKey>(item) {
            let key = (wk.zone, wk.id);
            if wanted(key) {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// World-side bookkeeping when a character starts (or restarts) a
/// quest: the database row's variables were reset, so drop the cached
/// copy, then credit COLLECT objectives from the pack.
pub(crate) fn on_quest_accepted(
    world: &mut World,
    player: Entity,
    character_id: &str,
    quest_zone: i32,
    quest_id: i32,
) {
    if let Some(mut cache) = world.get_resource_mut::<mud_world::QuestVariableCache>() {
        cache.reset_quest(character_id, quest_zone, quest_id);
    }
    recheck_collect_objectives(world, player);
}

/// What a COLLECT_ITEM objective should do given the quantity held.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CollectStep {
    /// Held enough: complete it (consuming the items).
    Complete,
    /// Not enough yet: record this many (only when it differs from the
    /// stored count).
    Progress(i32),
    /// Nothing to change.
    Unchanged,
}

/// COLLECT_ITEM progress is the number of matching items actually
/// held, capped at the requirement - it follows the pack down as well
/// as up, so pickups cannot be banked by dropping and re-getting.
pub(crate) fn collect_step(held: i32, current: i32, required: i32) -> CollectStep {
    if held >= required {
        CollectStep::Complete
    } else if held != current {
        CollectStep::Progress(held)
    } else {
        CollectStep::Unchanged
    }
}

/// Re-evaluate the player's COLLECT_ITEM objectives (current phase of
/// every quest they hold) against what they carry right now. Runs on
/// every phase entry (acceptance, advancing, `qload`/`qgive`) and
/// whenever the pack changes (`collect_watch_tick`).
pub(crate) fn recheck_collect_objectives(world: &mut World, player: Entity) {
    let held = held_counts(world, player);
    recheck_with(world, player, held);
}

fn recheck_with(world: &mut World, player: Entity, held: HashMap<(i32, i32), i32>) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let Some(notify) = Notifier::for_player(world, player) else {
        return;
    };
    tokio::spawn(async move {
        recheck_collect_task(&pool, &notify, &held).await;
    });
}

async fn recheck_collect_task(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    held: &HashMap<(i32, i32), i32>,
) {
    let rows =
        match mud_db::quest_objectives::list_current_collect_objectives(pool, &notify.character_id)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "collect recheck lookup failed");
                return;
            }
        };
    // Tell the world thread which prototypes this character's active
    // COLLECT objectives watch, so the pack watch only scans (and only
    // re-queries) for characters that have any.
    if let Some(tx) = &notify.update_tx {
        let targets = rows
            .iter()
            .map(|r| (r.object_zone_id, r.object_id))
            .collect();
        let _ = tx
            .send(PendingPlayerUpdate::CollectTargets {
                character_id: notify.character_id.clone(),
                targets,
            })
            .await;
    }
    for row in &rows {
        let n = held
            .get(&(row.object_zone_id, row.object_id))
            .copied()
            .unwrap_or(0);
        let obj = ObjectiveRef::from(row);
        match collect_step(n, row.current_count, row.required_count) {
            CollectStep::Unchanged => {}
            CollectStep::Progress(count) => {
                let saved = mud_db::quest_objectives::set_open_progress(
                    pool,
                    &obj.character_quest_id,
                    obj.quest_zone_id,
                    obj.quest_id,
                    obj.phase_id,
                    obj.objective_id,
                    count,
                )
                .await;
                match saved {
                    // Only announce gains; losing an item is silent.
                    Ok(()) if count > row.current_count => {
                        notify.say(&progress_line(&obj, count, false, ""));
                    }
                    Ok(()) => {}
                    Err(e) => tracing::warn!(error = %e, "collect progress write failed"),
                }
            }
            CollectStep::Complete => {
                claim_collect(pool, notify, &obj, (row.object_zone_id, row.object_id)).await;
            }
        }
    }
}

/// Claim a satisfied COLLECT objective (exactly one racing caller
/// wins) and hand it to the world thread, which takes the items.
async fn claim_collect(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    obj: &ObjectiveRef,
    object: (i32, i32),
) {
    let claimed = mud_db::quest_objectives::claim_objective(
        pool,
        &obj.character_quest_id,
        obj.quest_zone_id,
        obj.quest_id,
        obj.phase_id,
        obj.objective_id,
        obj.required_count,
    )
    .await;
    match claimed {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            tracing::warn!(error = %e, "collect claim failed");
            return;
        }
    }
    let sent = match &notify.update_tx {
        Some(tx) => tx
            .send(PendingPlayerUpdate::CollectClaimed {
                character_id: notify.character_id.clone(),
                obj: obj.clone(),
                object,
            })
            .await
            .is_ok(),
        None => false,
    };
    if !sent {
        release_collect(pool, obj).await;
    }
}

async fn release_collect(pool: &mud_db::sqlx::PgPool, obj: &ObjectiveRef) {
    if let Err(e) = mud_db::quest_objectives::release_objective(
        pool,
        &obj.character_quest_id,
        obj.quest_zone_id,
        obj.quest_id,
        obj.phase_id,
        obj.objective_id,
    )
    .await
    {
        tracing::warn!(error = %e, "collect release failed");
    }
}

/// Entities `holder` carries directly that are instances of `key`,
/// pack items before worn ones.
fn held_entities(world: &mut World, holder: Entity, key: (i32, i32)) -> Vec<Entity> {
    let mut q =
        world.query_filtered::<(Entity, &Located, &WorldKey, Has<EquippedSlot>), With<Item>>();
    let mut found: Vec<(bool, Entity)> = q
        .iter(world)
        .filter(|(_, l, wk, _)| l.0 == holder && (wk.zone, wk.id) == key)
        .map(|(e, _, _, worn)| (worn, e))
        .collect();
    found.sort_by_key(|(worn, _)| *worn);
    found.into_iter().map(|(_, e)| e).collect()
}

/// Give a claim back from the world thread (nothing to consume it
/// with). Without a database pool there is nothing to give back to.
pub(crate) fn release_claim(world: &World, obj: &ObjectiveRef) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let obj = obj.clone();
    tokio::spawn(async move { release_collect(&pool, &obj).await });
}

/// World-thread half of a COLLECT completion: verify the player still
/// holds the required quantity, TAKE those items (collecting is a
/// turn-in), then finish the objective and save the player so the item
/// removal and the completed objective become durable together.
///
/// Every path that does not reach the consume step gives the claim
/// back; a claim that is somehow lost anyway expires on its own (see
/// `claim_objective`).
pub(crate) fn finish_collect(
    world: &mut World,
    player: Entity,
    obj: &ObjectiveRef,
    object: (i32, i32),
) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let Some(notify) = Notifier::for_player(world, player) else {
        // Link-dead: nobody to tell, and nothing to consume against.
        release_claim(world, obj);
        return;
    };
    let need = usize::try_from(obj.required_count.max(1)).unwrap_or(1);
    let items = held_entities(world, player, object);
    if items.len() < need {
        release_claim(world, obj);
        return;
    }
    let name = world
        .get::<mud_world::Named>(items[0])
        .map(|n| n.name.clone())
        .unwrap_or_default();
    let mut unequipped = false;
    for item in items.into_iter().take(need) {
        if world.get::<EquippedSlot>(item).is_some() {
            // Worn: take it off properly first so its stat bonuses and
            // granted effects go with it.
            crate::equip_apply::unapply_object_from_wearer(world, item, player);
            crate::commands::try_remove::<EquippedSlot>(world, item);
            unequipped = true;
        }
        crate::commands::info::despawn_item_tree(world, item);
    }
    if unequipped {
        crate::commands::refresh_player_items_gmcp(world, player);
    }
    crate::commands::send_to(
        world,
        player,
        format!("You hand over {need} x {name} for the quest.\r\n"),
    );
    let obj = obj.clone();
    tokio::spawn(async move {
        // Items are gone: record the completion first so a crash can
        // never leave the player without both.
        if let Err(e) = mud_db::quest_objectives::upsert_progress(
            &pool,
            &obj.character_quest_id,
            obj.quest_zone_id,
            obj.quest_id,
            obj.phase_id,
            obj.objective_id,
            obj.required_count,
            true,
        )
        .await
        {
            tracing::warn!(error = %e, "collect completion write failed");
            return;
        }
        notify.say(&progress_line(&obj, obj.required_count, true, ""));
        advance_quest(
            &pool,
            &notify,
            &obj.character_quest_id,
            obj.quest_zone_id,
            obj.quest_id,
        )
        .await;
        // Persist the pack (the removed items) now rather than at the
        // next autosave.
        if let Some(tx) = &notify.update_tx {
            let _ = tx
                .send(PendingPlayerUpdate::SavePlayer {
                    character_id: notify.character_id.clone(),
                })
                .await;
        }
    });
}

/// World-thread half of [`PendingPlayerUpdate::SavePlayer`]: snapshot
/// now and write in the background under the character's save lock. If
/// a save is already in flight the `PendingSave` marker retries it on
/// the next tick.
pub(crate) fn save_player_soon(world: &mut World, player: Entity) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    if !crate::login::spawn_background_save(world, player, &pool) {
        crate::commands::try_insert(world, player, mud_world::PendingSave);
    }
}

fn pack_signature(counts: &HashMap<(i32, i32), i32>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut entries: Vec<_> = counts.iter().map(|(k, v)| (*k, *v)).collect();
    entries.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut h);
    h.finish()
}

/// Per-player state of the pack watch.
#[derive(Component, Debug, Default)]
pub(crate) struct CollectWatch {
    /// Prototypes the player's active COLLECT objectives want. Empty
    /// means the watch costs nothing for this player.
    pub(crate) targets: HashSet<(i32, i32)>,
    /// Signature of the watched part of the pack at the last check.
    signature: u64,
    /// Tick of the last recheck, so a quiet pack is still re-verified
    /// periodically (which also heals an expired claim).
    last_check: u64,
}

/// World-thread half of [`PendingPlayerUpdate::CollectTargets`].
pub(crate) fn set_collect_targets(world: &mut World, player: Entity, targets: HashSet<(i32, i32)>) {
    let tick = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
    let signature = pack_signature(&held_target_counts(world, player, &targets));
    crate::commands::try_insert(
        world,
        player,
        CollectWatch {
            targets,
            signature,
            last_check: tick,
        },
    );
}

/// Period of the pack watch, in ticks (0.2 s at 10 Hz).
const PACK_WATCH_PERIOD_TICKS: u64 = 2;
/// A watched pack is re-verified at least this often even if unchanged.
const PACK_RECHECK_TICKS: u64 = 600;

/// Notice when a player's pack changes by any route (get, drop, put,
/// give, buy, loot, junk, quest rewards...) and re-evaluate their
/// COLLECT_ITEM objectives.
///
/// Cost note (runs inside the timed tick): a player is scanned only if
/// they have active COLLECT objectives ([`CollectWatch::targets`]), and
/// the scan walks just their own `Contents` - O(items carried), no pass
/// over world items. Everyone else costs one component lookup. A newly
/// seen player gets one database check (login discovery); the database
/// is otherwise consulted only when a watched pack changes, or every
/// [`PACK_RECHECK_TICKS`] for a watched player.
pub(crate) fn collect_watch_tick(world: &mut World) {
    let tick = world.resource::<crate::TickCount>().0;
    if !tick.is_multiple_of(PACK_WATCH_PERIOD_TICKS) {
        return;
    }
    let players: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, (With<Player>, With<mud_world::Online>)>();
        q.iter(world).collect()
    };
    for player in players {
        let Some(watch) = world.get::<CollectWatch>(player) else {
            // First sight (login): find out what they are collecting.
            crate::commands::try_insert(world, player, CollectWatch::default());
            recheck_collect_objectives(world, player);
            continue;
        };
        if watch.targets.is_empty() {
            continue;
        }
        let held = held_target_counts(world, player, &watch.targets);
        let signature = pack_signature(&held);
        let overdue = tick.saturating_sub(watch.last_check) >= PACK_RECHECK_TICKS;
        if signature == watch.signature && !overdue {
            continue;
        }
        if let Some(mut w) = world.get_mut::<CollectWatch>(player) {
            w.signature = signature;
            w.last_check = tick;
        }
        recheck_with(world, player, held);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_step_completes_only_when_enough_is_held() {
        assert_eq!(collect_step(3, 0, 3), CollectStep::Complete);
        assert_eq!(collect_step(5, 0, 3), CollectStep::Complete);
        assert_eq!(collect_step(2, 0, 3), CollectStep::Progress(2));
    }

    #[test]
    fn collect_step_follows_the_pack_down() {
        // Dropping an item lowers the stored count: pickups are not banked.
        assert_eq!(collect_step(1, 2, 3), CollectStep::Progress(1));
        assert_eq!(collect_step(0, 1, 3), CollectStep::Progress(0));
        assert_eq!(collect_step(2, 2, 3), CollectStep::Unchanged);
        assert_eq!(collect_step(0, 0, 3), CollectStep::Unchanged);
    }

    #[test]
    fn pack_signature_ignores_order_and_tracks_counts() {
        let a: HashMap<(i32, i32), i32> = [((1, 1), 2), ((1, 2), 1)].into_iter().collect();
        let b: HashMap<(i32, i32), i32> = [((1, 2), 1), ((1, 1), 2)].into_iter().collect();
        let c: HashMap<(i32, i32), i32> = [((1, 1), 1), ((1, 2), 1)].into_iter().collect();
        assert_eq!(pack_signature(&a), pack_signature(&b));
        assert_ne!(pack_signature(&a), pack_signature(&c));
    }

    #[test]
    fn held_counts_tallies_direct_holdings_only() {
        let mut world = World::new();
        let me = world.spawn_empty().id();
        let other = world.spawn_empty().id();
        let bag = world.spawn_empty().id();
        let key = |id| WorldKey { zone: 30, id };
        world.spawn((Item, key(7), Located(me)));
        world.spawn((Item, key(7), Located(me)));
        world.spawn((Item, key(8), Located(me)));
        world.spawn((Item, key(7), Located(other)));
        world.spawn((Item, key(7), Located(bag)));
        let counts = held_counts(&world, me);
        assert_eq!(counts.get(&(30, 7)), Some(&2));
        assert_eq!(counts.get(&(30, 8)), Some(&1));
        assert_eq!(counts.len(), 2);
    }
}
