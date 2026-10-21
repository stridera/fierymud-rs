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
use crate::query_cache::with_query;

/// Where to tell one player about quest progress, and how to push the
/// resulting ECS mutations (XP, gold, items) back to the world thread.
#[derive(Clone)]
pub(crate) struct Notifier {
    pub character_id: String,
    pub out: mud_net::Outbound,
    pub update_tx: Option<tokio::sync::mpsc::Sender<PendingPlayerUpdate>>,
    /// The save-turn coordinator rewards are paid under (see
    /// [`grant_completion_rewards`]); `None` in a world without one.
    pub coordinator: Option<crate::autosave::SaveCoordinator>,
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
            coordinator: world
                .get_resource::<crate::autosave::SaveCoordinator>()
                .cloned(),
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

/// What paying a finished quest's unconditional rewards produced: the
/// database writes are done, the world side and the text are not.
#[derive(Default)]
pub(crate) struct CompletionPayout {
    /// "Conditional rewards are waiting" line, shown first.
    pub hint: Option<String>,
    /// The matching ECS mutations (XP, gold, items, ...), in order.
    pub updates: Vec<PendingPlayerUpdate>,
    /// The "Rewards:" block, shown last.
    pub summary: Option<String>,
}

impl CompletionPayout {
    /// Both text parts, for a caller that prints once.
    pub(crate) fn text(&self) -> String {
        let mut out = self.hint.clone().unwrap_or_default();
        out.push_str(self.summary.as_deref().unwrap_or_default());
        out
    }
}

/// A finished quest's rewards, read before anything is paid: the
/// unconditional ones (paid at completion) and whether conditional ones
/// are waiting for `qreward`.
pub(crate) struct RewardPlan {
    /// "Conditional rewards are waiting" line, shown first.
    hint: Option<String>,
    rewards: Vec<mud_db::quest_objectives::QuestRewardRow>,
}

impl RewardPlan {
    pub(crate) async fn load(
        pool: &mud_db::sqlx::PgPool,
        quest_zone_id: i32,
        quest_id: i32,
    ) -> Self {
        let all_rewards =
            mud_db::quest_objectives::list_quest_rewards(pool, quest_zone_id, quest_id)
                .await
                .unwrap_or_default();
        let (deferred, rewards): (Vec<_>, Vec<_>) = all_rewards
            .into_iter()
            .partition(|r| r.condition.as_deref().is_some_and(|c| !c.trim().is_empty()));
        let hint = (!deferred.is_empty()).then(|| {
            format!(
                "Conditional rewards available — \
                 type 'qreward {quest_zone_id} {quest_id}' to view and claim.\r\n"
            )
        });
        Self { hint, rewards }
    }

    /// The most world updates paying this plan can queue: one per reward,
    /// plus the house a HOUSING reward may have just created. This much
    /// queue room is reserved BEFORE the payout is written
    /// ([`reserve_under_turn`]).
    pub(crate) fn update_bound(&self) -> usize {
        if self.rewards.is_empty() {
            0
        } else {
            self.rewards.len() + 1
        }
    }
}

/// How paying a finished quest's rewards went.
pub(crate) enum Payment {
    /// Written to the database; the world side and the text are not.
    Paid(CompletionPayout),
    /// The quest was no longer in progress (a repeat, a racing
    /// completion): nothing was completed and nothing was paid.
    NotInProgress,
    /// The write failed and was rolled back.
    Failed(String),
}

/// Pay a finished quest's unconditional rewards to the database (each
/// numeric reward is a relative `x = x + n` update in one transaction) and
/// work out what the world must mirror, without sending anything: the
/// caller decides how and when the updates reach the tick. Conditional
/// rewards are deferred to `qreward`, which can evaluate the condition on
/// the world thread.
///
/// `complete` is `Some(character_quest_id)` when this payment is also the
/// completion (staff `qcomplete`): the status flip then runs in the SAME
/// transaction as the rewards, so a failure leaves the quest in progress
/// and unpaid, and a repeat pays nothing. With `None` the quest was
/// already flipped (the objective path); a failed grant is logged and the
/// world still mirrors the reward.
pub(crate) async fn pay_completion_rewards(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
    plan: &RewardPlan,
    complete: Option<&str>,
) -> Payment {
    let mut payout = CompletionPayout {
        hint: plan.hint.clone(),
        ..CompletionPayout::default()
    };
    let rewards = &plan.rewards;
    let cid = character_id;
    let outcome = match complete {
        Some(character_quest_id) => {
            match mud_db::quest_objectives::complete_and_grant(
                pool,
                cid,
                character_quest_id,
                rewards,
            )
            .await
            {
                Ok(Some(o)) => o,
                Ok(None) => return Payment::NotInProgress,
                Err(e) => return Payment::Failed(e.to_string()),
            }
        }
        None if rewards.is_empty() => mud_db::quest_objectives::GrantOutcome::default(),
        None => match mud_db::quest_objectives::grant_simple_rewards(pool, cid, rewards).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(error = %e, "reward grant failed");
                mud_db::quest_objectives::GrantOutcome::default()
            }
        },
    };
    if rewards.is_empty() {
        return Payment::Paid(payout);
    }
    if outcome.house_created
        && let Some(granted) = house_granted_update(pool, cid).await
    {
        payout.updates.push(granted);
    }
    for r in rewards {
        if let Some(update) = reward_update(cid, r) {
            payout.updates.push(update);
        }
    }
    let mut buf = String::from("Rewards:\r\n");
    for r in rewards {
        let line = match (r.reward_type.as_str(), r.amount, r.quantity) {
            ("EXPERIENCE", Some(a), _) => format!("  +{a} experience\r\n"),
            ("GOLD", Some(a), _) => format!("  +{a} gold\r\n"),
            ("SKILL_POINTS", Some(a), _) => format!("  +{a} skill points\r\n"),
            ("ABILITY", _, _) => "  +1 new ability\r\n".to_string(),
            ("ITEM", _, q) => format!("  +{q} item(s)\r\n"),
            ("HOUSING", _, _) if outcome.house_created => {
                "  +a house of your own — type 'home'\r\n".to_string()
            }
            ("HOUSING", _, _) => "  housing (you already own a house)\r\n".to_string(),
            _ => continue,
        };
        buf.push_str(&line);
    }
    if buf.len() > "Rewards:\r\n".len() {
        payout.summary = Some(buf);
    }
    Payment::Paid(payout)
}

/// The ECS mutation that mirrors one paid reward (the database side is
/// already written), if it has one.
pub(crate) fn reward_update(
    character_id: &str,
    r: &mud_db::quest_objectives::QuestRewardRow,
) -> Option<PendingPlayerUpdate> {
    let cid = character_id.to_string();
    match r.reward_type.as_str() {
        "EXPERIENCE" => r.amount.map(|a| PendingPlayerUpdate::ExperienceDelta {
            character_id: cid,
            amount: a,
        }),
        "GOLD" => r.amount.map(|a| PendingPlayerUpdate::WealthDelta {
            character_id: cid,
            amount: mud_db::quest_objectives::gold_reward_copper(a),
        }),
        "SKILL_POINTS" => r.amount.map(|a| PendingPlayerUpdate::SkillPointsDelta {
            character_id: cid,
            amount: a,
        }),
        "ABILITY" => r.ability_id.map(|id| PendingPlayerUpdate::AbilityKnown {
            character_id: cid,
            ability_id: id,
        }),
        "ITEM" => match (r.object_zone_id, r.object_id) {
            (Some(z), Some(id)) => Some(PendingPlayerUpdate::SpawnItem {
                character_id: cid,
                object_zone: z,
                object_id: id,
                quantity: r.quantity,
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Pay a finished quest's unconditional rewards from the objective path:
/// DB writes first, then the matching ECS mutations so the player sees the
/// gain without relogging. The work runs in a task of its own that holds
/// the character's write turn ([`SaveCoordinator::spawn_ordered`]), with
/// the same guarantees as `qcomplete` (see `start_completion`): the numeric
/// rewards are relative SQL updates, queue room is reserved before they are
/// written, and the updates are queued under the turn after
/// `supersede_earlier_snapshots`. So an autosave whose snapshot predates the
/// reward cannot write the old balance back, and a relog that reads the row
/// after the payment is not given it a second time.
pub(crate) async fn grant_completion_rewards(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    quest_zone_id: i32,
    quest_id: i32,
) {
    let plan = RewardPlan::load(pool, quest_zone_id, quest_id).await;
    let coordinator = notify.coordinator.clone().unwrap_or_default();
    let (pool, notify) = (pool.clone(), notify.clone());
    let turn_for = notify.character_id.clone();
    coordinator
        .clone()
        .spawn_ordered(&turn_for, move |ordered| async move {
            let (ordered, reservation) = match notify.update_tx.as_ref() {
                Some(tx) => {
                    reserve_under_turn(
                        &coordinator,
                        &notify.character_id,
                        ordered,
                        tx,
                        plan.update_bound(),
                    )
                    .await
                }
                None => (ordered, Reservation::none()),
            };
            let payout =
                match pay_completion_rewards(&pool, &notify.character_id, &plan, None).await {
                    Payment::Paid(p) => p,
                    Payment::NotInProgress | Payment::Failed(_) => return,
                };
            if let Some(hint) = &payout.hint {
                notify.say(hint);
            }
            deliver_under_turn(ordered, reservation, payout.updates);
            if let Some(summary) = &payout.summary {
                notify.say(summary);
            }
        });
}

/// The world-side `HouseSummary` insert for the house a HOUSING reward
/// just created: load the new rows. `None` when there is no house to show.
pub(crate) async fn house_granted_update(
    pool: &mud_db::sqlx::PgPool,
    character_id: &str,
) -> Option<PendingPlayerUpdate> {
    let house = match mud_db::housing::for_character(pool, character_id).await {
        Ok(Some(h)) => h,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "housing reward: reload failed");
            return None;
        }
    };
    let rooms = mud_db::housing::rooms_for_house(pool, house.id)
        .await
        .unwrap_or_default();
    Some(PendingPlayerUpdate::HouseGranted {
        character_id: character_id.to_string(),
        house,
        rooms,
    })
}

/// Room in the player-update queue, reserved by [`reserve_under_turn`]
/// before a database-first payout is written. Sending into it cannot wait
/// and cannot fail for lack of room.
pub(crate) struct Reservation<'a> {
    permits: Option<tokio::sync::mpsc::PermitIterator<'a, PendingPlayerUpdate>>,
}

impl Reservation<'_> {
    /// No room reserved: nothing to deliver to (no queue, a closed queue, or
    /// a payout with no world side).
    pub(crate) fn none() -> Self {
        Self { permits: None }
    }
}

/// Reserve queue room for up to `n` updates while holding the character's
/// write turn, WITHOUT ever waiting for it under the turn: the tick can
/// itself be waiting for the turn (a relog's `begin_ordered`) and is also
/// what drains the queue, so waiting for room while holding the turn would
/// deadlock. If the queue is full the turn is released and the wait happens
/// without it; the room won there is KEPT while the turn is taken again
/// (handing it back to re-reserve could starve under heavy queue traffic).
///
/// That release is safe only because nothing has been paid yet: a snapshot
/// that takes the turn in the gap sees the old balance, and the payment
/// that follows is relative (`x = x + n`) and supersedes it. Releasing the
/// turn AFTER paying is what lost rewards. Pay only once this returns, then
/// hand the updates to [`deliver_under_turn`], which cannot block.
///
/// No room is waited for once shutdown has begun (nothing drains the queue
/// then) or if the queue closes: the result carries no reservation and the
/// caller pays in the database alone. The reward is stored there, so it is
/// restored on the next login; only the world-side mirror is skipped.
pub(crate) async fn reserve_under_turn<'a>(
    coordinator: &crate::autosave::SaveCoordinator,
    character_id: &str,
    ordered: crate::autosave::OrderedSave,
    tx: &'a tokio::sync::mpsc::Sender<PendingPlayerUpdate>,
    n: usize,
) -> (crate::autosave::OrderedSave, Reservation<'a>) {
    use tokio::sync::mpsc::error::TrySendError;
    // A request larger than the whole queue could never be granted; reserve
    // what fits (the queue is orders of magnitude larger than any payout).
    let n = n.min(tx.max_capacity());
    if n == 0 {
        return (ordered, Reservation::none());
    }
    match tx.try_reserve_many(n) {
        Ok(permits) => {
            return (
                ordered,
                Reservation {
                    permits: Some(permits),
                },
            );
        }
        Err(TrySendError::Closed(())) => return (ordered, Reservation::none()),
        Err(TrySendError::Full(())) => {}
    }
    if coordinator.is_shutting_down() {
        tracing::warn!(
            character_id,
            "update queue full at shutdown: paying without the world update"
        );
        return (ordered, Reservation::none());
    }
    drop(ordered);
    // Wait for room without the turn, or for shutdown, whichever is first.
    let permits = tokio::select! {
        room = tx.reserve_many(n) => room.ok(),
        () = coordinator.shutting_down() => None,
    };
    let ordered = coordinator.begin_ordered(character_id).await;
    if let Some(permits) = permits {
        return (
            ordered,
            Reservation {
                permits: Some(permits),
            },
        );
    }
    tracing::warn!(
        character_id,
        "no update queue room: paying without the world update"
    );
    (ordered, Reservation::none())
}

/// Queue paid-reward `updates` for the tick into the room reserved by
/// [`reserve_under_turn`] and only then release the turn `ordered`, so a
/// snapshot taken under the turn drains them before it writes. Snapshots
/// taken before are made stale first (`supersede_earlier_snapshots`). The
/// database already holds the reward, so a closed queue (shutdown) cannot
/// lose it. Never waits: the turn is never held across an `.await`.
pub(crate) fn deliver_under_turn(
    mut ordered: crate::autosave::OrderedSave,
    mut reservation: Reservation<'_>,
    updates: Vec<PendingPlayerUpdate>,
) {
    if updates.is_empty() {
        return;
    }
    ordered.supersede_earlier_snapshots();
    let Some(permits) = reservation.permits.as_mut() else {
        tracing::warn!(
            "quest reward paid in the database but not mirrored to the world \
             (update queue closed or shutting down); it applies at next login"
        );
        return;
    };
    for update in updates {
        let Some(permit) = permits.next() else {
            // `update_bound` covers every update a payout makes.
            tracing::error!("quest reward could not be delivered: reservation too small");
            return;
        };
        permit.send(update);
    }
    drop(ordered);
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
    let players: Vec<Entity> =
        with_query::<Entity, (With<Player>, With<mud_world::Online>), _>(world, |world, q| {
            q.iter(world).collect()
        });
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

    fn reply(who: &str) -> PendingPlayerUpdate {
        PendingPlayerUpdate::Reply {
            character_id: who.to_string(),
            text: String::new(),
        }
    }

    /// With a full queue the payment still never gets ahead of its
    /// delivery: room is waited for BEFORE paying (turn released, nothing
    /// to lose), and once the payment is written the update is queued under
    /// the same turn. A retire-like observer that takes the turn at every
    /// chance must never see "paid but not queued". Single-threaded runtime,
    /// so the payer's last step (queue, release the turn, set its flag)
    /// cannot be interleaved with the observer.
    #[tokio::test(flavor = "current_thread")]
    async fn a_full_queue_never_splits_the_payment_from_its_delivery() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let coordinator = crate::autosave::SaveCoordinator::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        tx.try_send(reply("filler")).unwrap();
        tx.try_send(reply("filler")).unwrap();
        assert!(tx.try_send(reply("filler")).is_err(), "the queue is full");

        let paid = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicBool::new(false));
        {
            let (paid, delivered) = (Arc::clone(&paid), Arc::clone(&delivered));
            let (turns, tx) = (coordinator.clone(), tx.clone());
            coordinator.spawn_ordered("payer", move |ordered| async move {
                let (ordered, reservation) =
                    reserve_under_turn(&turns, "payer", ordered, &tx, 1).await;
                // The payment: it takes a while, the turn is held throughout.
                paid.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                deliver_under_turn(ordered, reservation, vec![reply("payer")]);
                delivered.store(true, Ordering::SeqCst);
            });
        }
        let observer = {
            let (turns, paid, delivered) = (
                coordinator.clone(),
                Arc::clone(&paid),
                Arc::clone(&delivered),
            );
            tokio::spawn(async move {
                let mut saw_paid = false;
                for _ in 0..150 {
                    let turn = turns.begin_ordered("payer").await;
                    let (p, d) = (
                        paid.load(Ordering::SeqCst),
                        delivered.load(Ordering::SeqCst),
                    );
                    assert!(
                        !p || d,
                        "the turn was released between the payment and its update"
                    );
                    saw_paid |= p;
                    drop(turn);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                saw_paid
            })
        };
        // Nothing is paid while the queue is full and nobody drains it.
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            !paid.load(Ordering::SeqCst),
            "paid with no room to report it"
        );
        // The tick drains one slot: now the payer can go.
        assert!(rx.try_recv().is_ok());
        assert!(observer.await.unwrap(), "the payment never happened");
        assert!(delivered.load(Ordering::SeqCst));
        let mut who = Vec::new();
        while let Ok(PendingPlayerUpdate::Reply { character_id, .. }) = rx.try_recv() {
            who.push(character_id);
        }
        assert_eq!(who, ["filler", "payer"]);
    }

    /// A reservation covers every update a payout makes, delivered in order
    /// and without waiting; an unused part is handed back; a closed queue
    /// loses nothing but the (already written) world mirror.
    #[tokio::test(flavor = "current_thread")]
    async fn reserved_updates_arrive_in_order_and_the_rest_is_given_back() {
        let coordinator = crate::autosave::SaveCoordinator::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let turn = coordinator.begin_ordered("c").await;
        let (turn, reservation) = reserve_under_turn(&coordinator, "c", turn, &tx, 3).await;
        assert_eq!(tx.capacity(), 1, "three slots are spoken for");
        deliver_under_turn(turn, reservation, vec![reply("a"), reply("b")]);
        assert_eq!(tx.capacity(), 2, "the unused slot is given back");
        assert!(
            coordinator.try_begin_ordered("c").is_some(),
            "turn released"
        );
        let mut who = Vec::new();
        while let Ok(PendingPlayerUpdate::Reply { character_id, .. }) = rx.try_recv() {
            who.push(character_id);
        }
        assert_eq!(who, ["a", "b"]);

        // A request larger than the whole queue is cut to what fits.
        let turn = coordinator.begin_ordered("c").await;
        let (turn, reservation) = reserve_under_turn(&coordinator, "c", turn, &tx, 99).await;
        assert_eq!(tx.capacity(), 0);
        drop((turn, reservation));
        assert_eq!(tx.capacity(), 4);

        // Closed queue: no reservation, delivery is a no-op, nothing panics.
        drop(rx);
        let turn = coordinator.begin_ordered("c").await;
        let (turn, reservation) = reserve_under_turn(&coordinator, "c", turn, &tx, 1).await;
        deliver_under_turn(turn, reservation, vec![reply("x")]);
        assert!(coordinator.try_begin_ordered("c").is_some());
    }

    /// Shutdown with a full queue: nothing drains it, so the payer must not
    /// wait. No reservation, the turn is still held, and delivery is a
    /// harmless no-op (the database already has the reward).
    #[tokio::test(flavor = "current_thread")]
    async fn a_full_queue_at_shutdown_pays_without_waiting() {
        let coordinator = crate::autosave::SaveCoordinator::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(reply("filler")).unwrap();
        coordinator.begin_shutdown();
        let turn = coordinator.begin_ordered("c").await;
        let (turn, reservation) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reserve_under_turn(&coordinator, "c", turn, &tx, 1),
        )
        .await
        .expect("must not wait for room at shutdown");
        assert!(reservation.permits.is_none());
        assert!(
            coordinator.try_begin_ordered("c").is_none(),
            "the turn is still held while the caller pays"
        );
        deliver_under_turn(turn, reservation, vec![reply("lost-mirror")]);
        assert!(coordinator.try_begin_ordered("c").is_some());
        let mut who = Vec::new();
        while let Ok(PendingPlayerUpdate::Reply { character_id, .. }) = rx.try_recv() {
            who.push(character_id);
        }
        assert_eq!(who, ["filler"], "the skipped update was not queued");
    }

    /// A payer already waiting for room when shutdown begins is released.
    #[tokio::test(flavor = "current_thread")]
    async fn a_payer_waiting_for_room_is_released_by_shutdown() {
        let coordinator = crate::autosave::SaveCoordinator::default();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(reply("filler")).unwrap();
        let payer = {
            let (coordinator, tx) = (coordinator.clone(), tx.clone());
            tokio::spawn(async move {
                let turn = coordinator.begin_ordered("c").await;
                let (_turn, reservation) =
                    reserve_under_turn(&coordinator, "c", turn, &tx, 1).await;
                reservation.permits.is_none()
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!payer.is_finished(), "waits for room while running");
        coordinator.begin_shutdown();
        let skipped = tokio::time::timeout(std::time::Duration::from_secs(5), payer)
            .await
            .expect("shutdown must release the payer")
            .unwrap();
        assert!(skipped, "no room was reserved");
    }

    /// Room won while waiting is kept across the wait for the turn, so
    /// other queue traffic cannot take it in between.
    #[tokio::test(flavor = "current_thread")]
    async fn room_won_while_waiting_is_kept_across_the_wait_for_the_turn() {
        use std::time::Duration;
        let coordinator = crate::autosave::SaveCoordinator::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(reply("filler")).unwrap();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (payer_done, payer_result) = tokio::sync::oneshot::channel::<bool>();
        let turn = coordinator.begin_ordered("c").await;
        // Queued behind the payer's turn: gets it the moment the payer lets
        // go, then sits on it.
        coordinator.spawn_ordered("c", move |ordered| async move {
            let _ordered = ordered;
            let _ = released.await;
        });
        {
            let (coordinator, tx) = (coordinator.clone(), tx.clone());
            tokio::spawn(async move {
                let (turn, reservation) = reserve_under_turn(&coordinator, "c", turn, &tx, 1).await;
                let kept = reservation.permits.is_some();
                deliver_under_turn(turn, reservation, vec![reply("payer")]);
                let _ = payer_done.send(kept);
            });
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        // The tick drains the filler: the payer gets the room, but the turn
        // is held by the blocker.
        assert!(rx.try_recv().is_ok());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            tx.try_send(reply("traffic")).is_err(),
            "the payer's room was handed back while it waited for the turn"
        );
        release.send(()).unwrap();
        let kept = tokio::time::timeout(Duration::from_secs(5), payer_result)
            .await
            .expect("payer finishes once the turn frees")
            .unwrap();
        assert!(kept);
        match rx.try_recv() {
            Ok(PendingPlayerUpdate::Reply { character_id, .. }) => {
                assert_eq!(character_id, "payer");
            }
            other => panic!("expected the payer's update, got {}", other.is_ok()),
        }
    }
}
