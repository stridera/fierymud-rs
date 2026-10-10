//! Boot loading of the mob spell / skill AI tables (`ClassAiRules`,
//! `MobAbilities` rule columns, `Mobs.ai_mode`) into `MobAiData`. Needs the dev
//! database with `fierylib/data/sql/2026-10-10-mob-ai-schema.sql` applied;
//! skipped when it is unreachable or the patch is missing. The round-trip test
//! writes inside a transaction on a single connection and rolls it back.

use std::time::Duration;

use bevy_ecs::prelude::World;
use mud_db::PoolSettings;
use mud_db::enums::MobAiMode;
use mud_db::sqlx::{self, PgPool};
use mud_world::{AiCondition, AiKind, AiTarget, MobAiData};

use crate::commands::test_support::db_test_lock;

fn url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into())
}

/// One-connection pool: statements share a session, so an open transaction is
/// visible to the loader.
async fn single_conn_pool() -> Option<PgPool> {
    tokio::time::timeout(
        Duration::from_secs(3),
        mud_db::connect_with(
            &url(),
            PoolSettings {
                max_connections: 1,
                acquire_timeout: Duration::from_secs(60),
            },
        ),
    )
    .await
    .ok()?
    .ok()
}

// One linear scenario: seed rows, load, assert; splitting it would only hide the order.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn rules_round_trip_through_the_loader() {
    let _lock = db_test_lock().await;
    let Some(pool) = single_conn_pool().await else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    if sqlx::query("SELECT ai_mode FROM \"Mobs\" LIMIT 1")
        .execute(&pool)
        .await
        .is_err()
        || sqlx::query("SELECT conditions FROM \"ClassAiRules\" LIMIT 1")
            .execute(&pool)
            .await
            .is_err()
    {
        eprintln!("skipped: dev database without the mob-ai schema patch");
        return;
    }
    let class: Option<(i32,)> = sqlx::query_as("SELECT id FROM \"Class\" ORDER BY id LIMIT 1")
        .fetch_optional(&pool)
        .await
        .unwrap();
    let abilities: Vec<(i32, String)> = sqlx::query_as(
        "SELECT id, \"abilityType\" FROM \"Ability\" \
         WHERE \"abilityType\" IN ('SPELL','SKILL') ORDER BY \"abilityType\", id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mob: Option<(i32, i32)> =
        sqlx::query_as("SELECT zone_id, id FROM \"Mobs\" ORDER BY zone_id, id LIMIT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();
    let (Some((class_id,)), Some((zone, mob_id))) = (class, mob) else {
        eprintln!("skipped: dev database has no class or mob");
        return;
    };
    let Some(spell) = abilities
        .iter()
        .find(|(_, t)| t == "SPELL")
        .map(|(i, _)| *i)
    else {
        eprintln!("skipped: dev database has no spell");
        return;
    };
    let Some(skill) = abilities
        .iter()
        .find(|(_, t)| t == "SKILL")
        .map(|(i, _)| *i)
    else {
        eprintln!("skipped: dev database has no skill");
        return;
    };

    sqlx::query("BEGIN").execute(&pool).await.unwrap();
    let result = async {
        // Deterministic: only the rows this test writes.
        sqlx::query("DELETE FROM \"ClassAiRules\"").execute(&pool).await?;
        sqlx::query("DELETE FROM \"MobAbilities\" WHERE mob_zone_id = $1 AND mob_id = $2")
            .bind(zone)
            .bind(mob_id)
            .execute(&pool)
            .await?;
        // Class rules: inserted out of priority order, plus a disabled row, a
        // bad-value row and a bad-target row that must be filtered out.
        for (ability, priority, chance, cd, cond, target, min, enabled) in [
            (skill, 20, 50, 30, r#"{"in_combat": true, "future_key": 1}"#, "WEAKEST_ATTACKER", Some(10), true),
            (spell, 10, 100, 0, r#"{"self_hp_below": 60}"#, "SELF", None, true),
            (spell, 30, 100, 0, r"{}", "TANK", None, false),
            (spell, 40, 100, 0, r#"{"self_hp_below": "lots"}"#, "TANK", None, true),
            (spell, 50, 100, 0, r"{}", "NOBODY", None, true),
        ] {
            sqlx::query(
                "INSERT INTO \"ClassAiRules\" (class_id, ability_id, priority, chance_pct, cooldown_s, \
                 conditions, target, min_level, enabled) VALUES ($1,$2,$3,$4,$5,$6::jsonb,$7,$8,$9)",
            )
            .bind(class_id)
            .bind(ability)
            .bind(priority)
            .bind(chance)
            .bind(cd)
            .bind(cond)
            .bind(target)
            .bind(min)
            .bind(enabled)
            .execute(&pool)
            .await?;
        }
        sqlx::query(
            "INSERT INTO \"MobAbilities\" (mob_zone_id, mob_id, ability_id, priority, chance_pct, target) \
             VALUES ($1,$2,$3,5,80,NULL)",
        )
        .bind(zone)
        .bind(mob_id)
        .bind(spell)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE \"Mobs\" SET ai_mode = 'CUSTOM' WHERE zone_id = $1 AND id = $2")
            .bind(zone)
            .bind(mob_id)
            .execute(&pool)
            .await?;

        let mut world = World::new();
        mud_world::load_mob_ai_data(&mut world, &pool).await?;
        Ok::<_, sqlx::Error>(world)
    }
    .await;
    sqlx::query("ROLLBACK").execute(&pool).await.unwrap();
    let world = result.expect("round trip");

    let data = world.resource::<MobAiData>();
    let rules = &data.by_class[&class_id];
    assert_eq!(
        rules.len(),
        2,
        "disabled, bad-value and bad-target rows are dropped"
    );
    // Priority order, not insertion order.
    assert_eq!(rules[0].ability_id, spell);
    assert_eq!(rules[0].kind, AiKind::Spell);
    assert_eq!(rules[0].target, AiTarget::SelfTarget);
    assert_eq!(rules[0].when, vec![AiCondition::SelfHpBelow(60)]);
    assert_eq!(rules[1].ability_id, skill);
    assert_eq!(rules[1].kind, AiKind::Skill);
    assert_eq!(rules[1].chance_pct, 50);
    assert_eq!(rules[1].cooldown_s, 30);
    assert_eq!(rules[1].min_level, Some(10));
    assert_eq!(
        rules[1].when,
        vec![AiCondition::InCombat],
        "unknown key ignored"
    );

    let own = &data.by_mob[&(zone, mob_id)];
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].priority, 5);
    assert_eq!(own[0].chance_pct, 80);
    assert_eq!(own[0].target, AiTarget::Auto);
    assert_eq!(data.mode((zone, mob_id)), MobAiMode::Custom);
}

/// A database without the mob-ai objects (prod before the patch is applied)
/// must not fail the boot: the resource still exists, empty.
#[tokio::test]
async fn missing_tables_degrade_to_empty_rules() {
    let _lock = db_test_lock().await;
    let Some(pool) = single_conn_pool().await else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    sqlx::query("CREATE SCHEMA IF NOT EXISTS mob_ai_empty")
        .execute(&pool)
        .await
        .expect("create schema");
    sqlx::query("SET search_path TO mob_ai_empty")
        .execute(&pool)
        .await
        .expect("set search_path");

    let mut world = World::new();
    mud_world::load_mob_ai_data(&mut world, &pool)
        .await
        .expect("a missing table must not fail the boot");
    let data = world.resource::<MobAiData>();
    assert!(data.by_class.is_empty() && data.by_mob.is_empty() && data.modes.is_empty());

    sqlx::query("DROP SCHEMA mob_ai_empty")
        .execute(&pool)
        .await
        .expect("drop schema");
}

/// `RaceAiRules` rows load into `by_race`, keyed by lowercase race text, in
/// priority order; a disabled row and a row with a bad condition are dropped.
#[tokio::test]
async fn race_rules_round_trip_through_the_loader() {
    let _lock = db_test_lock().await;
    let Some(pool) = single_conn_pool().await else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    if sqlx::query("SELECT conditions FROM \"RaceAiRules\" LIMIT 1")
        .execute(&pool)
        .await
        .is_err()
    {
        eprintln!("skipped: dev database without the RaceAiRules patch");
        return;
    }
    let skill: Option<(i32,)> = sqlx::query_as(
        "SELECT id FROM \"Ability\" WHERE \"abilityType\" = 'SKILL' ORDER BY id LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let Some((skill,)) = skill else {
        eprintln!("skipped: dev database has no skill");
        return;
    };
    sqlx::query("BEGIN").execute(&pool).await.unwrap();
    let result = async {
        sqlx::query("DELETE FROM \"RaceAiRules\"").execute(&pool).await?;
        for (priority, cond, enabled) in [
            (20, r#"{"in_combat": true, "level_roll": {"min": 5, "max": 10}}"#, true),
            (10, r#"{"in_combat": true, "level_roll": {"min": 0, "max": 5}, "composition": ["FIRE", "LAVA"]}"#, true),
            (30, r"{}", false),
            (40, r#"{"level_roll": {"min": 9, "max": 2}}"#, true),
        ] {
            sqlx::query(
                "INSERT INTO \"RaceAiRules\" (race, ability_id, priority, chance_pct, cooldown_s, \
                 conditions, target, enabled) VALUES ('DRAGON_FIRE', $1, $2, 100, 0, $3::jsonb, 'auto', $4)",
            )
            .bind(skill)
            .bind(priority)
            .bind(cond)
            .bind(enabled)
            .execute(&pool)
            .await?;
        }
        let mut world = World::new();
        mud_world::load_mob_ai_data(&mut world, &pool).await?;
        Ok::<_, sqlx::Error>(world)
    }
    .await;
    sqlx::query("ROLLBACK").execute(&pool).await.unwrap();
    let world = result.expect("round trip");
    let data = world.resource::<MobAiData>();
    let rules = &data.by_race["dragon_fire"];
    assert_eq!(rules.len(), 2, "disabled and bad-value rows are dropped");
    assert_eq!(rules[0].priority, 10);
    assert_eq!(rules[0].kind, AiKind::Skill);
    assert_eq!(rules[0].target, AiTarget::Auto);
    assert!(
        rules[0]
            .when
            .contains(&AiCondition::LevelRoll { min: 0, max: 5 })
    );
    assert!(rules[0].when.contains(&AiCondition::Composition(vec![
        mud_db::enums::Composition::Fire,
        mud_db::enums::Composition::Lava,
    ])));
    assert_eq!(rules[1].priority, 20);
}

/// A table that exists but will not decode (here: a `conditions` column holding text that is not JSON)
/// is no reason to fail the boot either: the loader logs it and carries on
/// with no rows for that table.
#[tokio::test]
async fn an_undecodable_table_degrades_to_empty_rules() {
    let _lock = db_test_lock().await;
    let Some(pool) = single_conn_pool().await else {
        eprintln!("skipped: dev database unreachable");
        return;
    };
    for stmt in [
        "DROP SCHEMA IF EXISTS mob_ai_bad CASCADE",
        "CREATE SCHEMA mob_ai_bad",
        "SET search_path TO mob_ai_bad",
        "CREATE TABLE \"Ability\" (id int, \"abilityType\" text)",
        "INSERT INTO \"Ability\" VALUES (1, 'SKILL')",
        "CREATE TABLE \"ClassAiRules\" (id int, class_id int, ability_id int, priority int, \
         chance_pct int, cooldown_s int, conditions text, target text, min_level int, enabled bool)",
        "INSERT INTO \"ClassAiRules\" VALUES (1, 1, 1, 1, 100, 0, 'not json', 'self', NULL, true)",
    ] {
        sqlx::query(stmt).execute(&pool).await.expect(stmt);
    }
    let mut world = World::new();
    let res = mud_world::load_mob_ai_data(&mut world, &pool).await;
    sqlx::query("SET search_path TO public")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP SCHEMA mob_ai_bad CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    res.expect("an undecodable AI table must not fail the boot");
    let data = world.resource::<MobAiData>();
    assert!(data.by_class.is_empty() && data.by_race.is_empty());
}
