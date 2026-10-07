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

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_world::{Account, Item, Located, WorldKey};

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
    fn say(&self, text: &str) {
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
pub(crate) struct ObjectiveRef {
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

/// Persist `new_count` for `obj`, tell the player, and run the
/// phase-advance / quest-completion handling when it reached its
/// required count. `party_prefix` is `"(party) "` for a group member
/// credited by someone else's action, otherwise empty.
pub(crate) async fn record_progress(
    pool: &mud_db::sqlx::PgPool,
    notify: &Notifier,
    obj: &ObjectiveRef,
    new_count: i32,
    party_prefix: &str,
) {
    let completed = new_count >= obj.required_count;
    if let Err(e) = mud_db::quest_objectives::upsert_progress(
        pool,
        &obj.character_quest_id,
        obj.quest_zone_id,
        obj.quest_id,
        obj.phase_id,
        obj.objective_id,
        new_count,
        completed,
    )
    .await
    {
        tracing::warn!(error = %e, "objective upsert failed");
        return;
    }
    let line = if completed {
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
    };
    notify.say(&line);
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
             type `qreward {quest_zone_id} {quest_id}` to view and claim.\r\n"
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
/// worn gear; items inside containers do not count).
pub(crate) fn held_counts(world: &mut World, holder: Entity) -> HashMap<(i32, i32), i32> {
    let mut counts: HashMap<(i32, i32), i32> = HashMap::new();
    let mut q = world.query_filtered::<(&Located, &WorldKey), With<Item>>();
    for (l, wk) in q.iter(world) {
        if l.0 == holder {
            *counts.entry((wk.zone, wk.id)).or_insert(0) += 1;
        }
    }
    counts
}

/// New progress for a COLLECT objective given what is held right now:
/// the held quantity capped at the requirement, only when that is an
/// improvement on what is already recorded.
pub(crate) fn collect_credit(held: i32, current: i32, required: i32) -> Option<i32> {
    let credit = held.min(required);
    (credit > current).then_some(credit)
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

/// Credit COLLECT_ITEM objectives in the player's current phase with
/// the items they already carry. Pickups made before a phase began
/// never counted towards it (progress is phase-gated), so this runs on
/// every phase entry: acceptance, advancing from the previous phase,
/// and `qload`/`qgive`.
pub(crate) fn recheck_collect_objectives(world: &mut World, player: Entity) {
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let Some(notify) = Notifier::for_player(world, player) else {
        return;
    };
    let held = held_counts(world, player);
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
    for row in &rows {
        let n = held
            .get(&(row.object_zone_id, row.object_id))
            .copied()
            .unwrap_or(0);
        if let Some(new_count) = collect_credit(n, row.current_count, row.required_count) {
            record_progress(pool, notify, &ObjectiveRef::from(row), new_count, "").await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_credit_caps_at_requirement() {
        assert_eq!(collect_credit(5, 0, 3), Some(3));
        assert_eq!(collect_credit(2, 0, 3), Some(2));
    }

    #[test]
    fn collect_credit_never_goes_backwards() {
        assert_eq!(collect_credit(0, 0, 3), None);
        assert_eq!(collect_credit(1, 2, 3), None);
        assert_eq!(collect_credit(3, 3, 3), None);
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
        let counts = held_counts(&mut world, me);
        assert_eq!(counts.get(&(30, 7)), Some(&2));
        assert_eq!(counts.get(&(30, 8)), Some(&1));
        assert_eq!(counts.len(), 2);
    }
}
