use mud_db::{
    character_items::{CharacterItemSnap, list_for, save_inventory_diff},
    connect,
    effects::list_effects,
    help::list_all as list_help_entries,
    mob_resets::list_all as list_mob_resets,
    mobs::list_mobs,
    object_resets::list_all as list_object_resets,
    objects::list_objects,
    room_exits::list_exits,
    rooms::list_rooms,
    zones::list_zones,
};
use sqlx::PgPool;

async fn pool() -> PgPool {
    let _ = dotenvy::from_path("../../.env");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    connect(&url).await.expect("connect to fierydev")
}

#[tokio::test]
#[ignore = "requires live fierydev DB; run with: cargo test -p mud-db -- --ignored"]
async fn lists_zones() {
    let zones = list_zones(&pool().await).await.expect("list zones");
    assert!(!zones.is_empty());
    let void = zones.iter().find(|z| z.id == 0).expect("zone 0 (Void)");
    assert_eq!(void.name, "Void");
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_rooms() {
    let rooms = list_rooms(&pool().await).await.expect("list rooms");
    assert!(
        rooms.len() > 1000,
        "expected many rooms, got {}",
        rooms.len()
    );
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_exits() {
    let exits = list_exits(&pool().await).await.expect("list exits");
    assert!(
        exits.len() > 1000,
        "expected many exits, got {}",
        exits.len()
    );
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_mobs() {
    let mobs = list_mobs(&pool().await).await.expect("list mobs");
    assert!(!mobs.is_empty());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_objects() {
    let objects = list_objects(&pool().await).await.expect("list objects");
    assert!(!objects.is_empty());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_effects() {
    let effects = list_effects(&pool().await).await.expect("list effects");
    assert!(!effects.is_empty());
}

/// `HelpEntry` is builder-authored; a fresh DB may have zero rows.
/// We only assert the query *runs* (i.e. the schema matches the
/// loader). Once the import lands content, tighten to `> 100`.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_help_entries() {
    let entries = list_help_entries(&pool().await)
        .await
        .expect("list help entries");
    // Sanity: every loaded row has at least one keyword and a title.
    // (Schema allows empty `keywords` but the in-game lookup can't
    // index those, so the import should always seed at least one.)
    for e in &entries {
        assert!(!e.title.is_empty(), "row {} has empty title", e.id);
    }
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_mob_resets() {
    let resets = list_mob_resets(&pool().await)
        .await
        .expect("list mob resets");
    // Imported world has thousands of mob resets.
    assert!(
        resets.len() > 1000,
        "expected many mob resets, got {}",
        resets.len()
    );
    // Probability is a fraction in [0, 1].
    for r in &resets {
        assert!(
            r.probability >= 0.0 && r.probability <= 1.0,
            "probability oob: {r:?}"
        );
        assert!(r.max_instances >= 1, "max_instances < 1: {r:?}");
    }
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_object_resets() {
    let resets = list_object_resets(&pool().await)
        .await
        .expect("list object resets");
    assert!(!resets.is_empty(), "expected some object resets");
    for r in &resets {
        assert!(r.probability >= 0.0 && r.probability <= 1.0);
        assert!(r.max_instances >= 1);
    }
}

/// Both inventory tests rewrite `TestWarrior`'s items; keep them serial.
static INVENTORY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Round-trip a small inventory through `CharacterItems`. Uses the seeded
/// `TestWarrior` account ('testplayer') so we don't need to spin up a fresh
/// character. Restores whatever was there before so re-running the test
/// doesn't permanently nuke real data.
///
/// We reference real (zone, id) keys from the Objects table so the FK
/// constraint passes. Picks the lowest two object IDs we can find.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn round_trips_character_items() {
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;

    // Find TestWarrior's character_id.
    let row = sqlx::query!(r#"SELECT id FROM "Characters" WHERE name = 'TestWarrior' LIMIT 1"#)
        .fetch_optional(&pool)
        .await
        .expect("query")
        .expect("seed user TestWarrior must exist");
    let cid = row.id;

    // Pick two real Object keys we can FK to.
    let keys: Vec<(i32, i32)> =
        sqlx::query!(r#"SELECT zone_id, id FROM "Objects" ORDER BY zone_id, id LIMIT 2"#)
            .fetch_all(&pool)
            .await
            .expect("query")
            .into_iter()
            .map(|r| (r.zone_id, r.id))
            .collect();
    assert_eq!(keys.len(), 2, "expected at least two Objects in the DB");

    // Snapshot whatever's already on TestWarrior so we restore at end.
    let before = list_for(&pool, &cid).await.expect("list before");

    // Save a known set: one carried, one worn (BODY). Both are
    // INSERTs (`persisted_id = None`) since we want the diff path
    // to delete the existing inventory and add these.
    let payload = vec![
        CharacterItemSnap {
            persisted_id: None,
            object_zone_id: keys[0].0,
            object_id: keys[0].1,
            equipped_location: None,
            parent_persisted_id: None,
            parent_idx: None,
            charges: None,
            liquid_remaining: None,
            liquid_type: None,
            lit: true,
            custom: None,
            alter: None,
            in_corpse: false,
        },
        CharacterItemSnap {
            persisted_id: None,
            object_zone_id: keys[1].0,
            object_id: keys[1].1,
            equipped_location: Some("BODY".to_string()),
            parent_persisted_id: None,
            parent_idx: None,
            charges: None,
            liquid_remaining: None,
            liquid_type: None,
            lit: false,
            custom: None,
            alter: None,
            in_corpse: false,
        },
    ];
    let mut conn = pool.acquire().await.expect("acquire conn");
    let assigned = save_inventory_diff(&mut conn, &cid, &payload, None)
        .await
        .expect("save");
    assert_eq!(assigned.len(), 2, "both rows INSERTed → both ids returned");

    let after = list_for(&pool, &cid).await.expect("list after");
    assert_eq!(after.len(), 2, "two rows after save");
    let worn: Vec<_> = after
        .iter()
        .filter(|r| r.equipped_location.as_deref() == Some("BODY"))
        .collect();
    assert_eq!(worn.len(), 1, "one worn-on-body row");
    let carried: Vec<_> = after
        .iter()
        .filter(|r| r.equipped_location.is_none())
        .collect();
    assert_eq!(carried.len(), 1, "one carried row");
    assert!(
        carried[0].lit,
        "lit state round-trips through custom_values"
    );
    assert!(!worn[0].lit, "unlit item stays unlit");

    // Restore the original set so re-runs are idempotent. Treat each
    // pre-existing row as an INSERT (the test's save above already
    // dropped them).
    let restore: Vec<CharacterItemSnap> = before
        .iter()
        .map(|r| CharacterItemSnap {
            persisted_id: None,
            object_zone_id: r.object_zone_id,
            object_id: r.object_id,
            equipped_location: r.equipped_location.clone(),
            parent_persisted_id: None,
            parent_idx: None,
            charges: if r.charges >= 0 {
                Some(r.charges)
            } else {
                None
            },
            liquid_remaining: r.liquid_type.as_ref().map(|_| r.liquid_remaining),
            liquid_type: r.liquid_type.clone(),
            lit: r.lit,
            custom: None,
            alter: None,
            in_corpse: false,
        })
        .collect();
    save_inventory_diff(&mut conn, &cid, &restore, None)
        .await
        .expect("restore");
}

// ---------------------------------------------------------------------------
// Wave 6 — federated identity
// ---------------------------------------------------------------------------

/// Helper: resolve `testplayer`'s Users id so the link tests can FK
/// against a real account. Uses the seeded test user.
async fn testplayer_user_id(pool: &PgPool) -> String {
    let row = sqlx::query!(
        r#"SELECT id FROM "Users" WHERE email LIKE 'testplayer%' OR id LIKE 'testplayer%' ORDER BY id LIMIT 1"#
    )
    .fetch_optional(pool)
    .await
    .expect("query users");
    if let Some(r) = row {
        return r.id;
    }
    // Fallback: any user. We only need one valid FK target.
    sqlx::query!(r#"SELECT id FROM "Users" LIMIT 1"#)
        .fetch_one(pool)
        .await
        .expect("at least one Users row")
        .id
}

/// Round-trip a Discord link: create → lookup → `mark_verified` → unlink.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn discord_link_round_trip() {
    let pool = pool().await;
    let user_id = testplayer_user_id(&pool).await;
    // Use a randomized discord_id so re-running doesn't trip the
    // discord_id unique. testplayer's user_id is fixed, but a stale
    // row from a previous run might exist — clean up first.
    let _ = mud_db::discord_links::unlink(&pool, &user_id).await;
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let discord_id = format!("test_discord_{suffix}");
    let discord_name = "TestUser#0001";

    // Initial link → unverified.
    let id = mud_db::discord_links::link(&pool, &user_id, &discord_id, discord_name)
        .await
        .expect("link");
    assert!(!id.is_empty(), "row id returned");

    let row = mud_db::discord_links::for_user(&pool, &user_id)
        .await
        .expect("lookup")
        .expect("link must exist after insert");
    assert_eq!(row.discord_id, discord_id);
    assert_eq!(row.discord_name, discord_name);
    assert!(!row.verified, "fresh link starts unverified");

    // Reverse lookup by discord_id.
    let by_did = mud_db::discord_links::for_discord_id(&pool, &discord_id)
        .await
        .expect("reverse lookup")
        .expect("must find the row");
    assert_eq!(by_did.user_id, user_id);

    // Mark verified.
    let updated = mud_db::discord_links::mark_verified(&pool, &user_id)
        .await
        .expect("verify");
    assert_eq!(updated, 1, "exactly one row flipped");
    let row = mud_db::discord_links::for_user(&pool, &user_id)
        .await
        .expect("lookup post-verify")
        .expect("link still present");
    assert!(row.verified, "verified flag flipped");

    // Unlink.
    let removed = mud_db::discord_links::unlink(&pool, &user_id)
        .await
        .expect("unlink");
    assert_eq!(removed, 1);
    assert!(
        mud_db::discord_links::for_user(&pool, &user_id)
            .await
            .expect("lookup post-unlink")
            .is_none(),
        "link removed"
    );
    // Idempotent — second unlink returns 0.
    let removed_again = mud_db::discord_links::unlink(&pool, &user_id)
        .await
        .expect("unlink again");
    assert_eq!(removed_again, 0);
}

/// Round-trip a Google link: create → lookup → unlink.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn google_link_round_trip() {
    let pool = pool().await;
    let user_id = testplayer_user_id(&pool).await;
    let _ = mud_db::google_links::unlink(&pool, &user_id).await;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let google_id = format!("google_sub_{suffix}");
    let google_email = "test@example.com";
    let google_name = Some("Test User");
    let avatar_url = Some("https://example.com/avatar.png");

    let id = mud_db::google_links::link(
        &pool,
        &user_id,
        &google_id,
        google_email,
        google_name,
        avatar_url,
    )
    .await
    .expect("link");
    assert!(!id.is_empty());

    let row = mud_db::google_links::for_user(&pool, &user_id)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(row.google_id, google_id);
    assert_eq!(row.google_email, google_email);
    assert_eq!(row.google_name.as_deref(), google_name);
    assert_eq!(row.avatar_url.as_deref(), avatar_url);

    let removed = mud_db::google_links::unlink(&pool, &user_id)
        .await
        .expect("unlink");
    assert_eq!(removed, 1);
    assert!(
        mud_db::google_links::for_user(&pool, &user_id)
            .await
            .expect("lookup post-unlink")
            .is_none()
    );
}

/// Character name-approval gate (replaces the legacy `LoginRequests`
/// row-based approval flow). Verifies the three runtime paths:
/// 1. `create` honors the caller-supplied `name_approved = false`.
/// 2. `find_by_name` / `list_for_user` round-trip the column.
/// 3. `set_name_approved` flips the gate (and is idempotent for
///    "already approved" no-ops).
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn character_name_approval_round_trip() {
    let pool = pool().await;
    let user_id = testplayer_user_id(&pool).await;
    // Pick a randomized character name so re-runs don't trip the
    // unique-name index. The character is cleaned up at the end.
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("ApprovTest{suffix}");

    // Insert at name_approved = false (the "toggle is ON" path).
    let new_char = mud_db::characters::NewCharacter {
        user_id: &user_id,
        name: &name,
        race: "HUMAN",
        gender: "neutral",
        class_id: 1,
        strength: 13,
        intelligence: 13,
        wisdom: 13,
        dexterity: 13,
        constitution: 13,
        charisma: 13,
        name_approved: false,
        password_hash: "",
    };
    let char_id = mud_db::characters::create(&pool, &new_char)
        .await
        .expect("create unapproved");

    // Round-trip through find_by_name — flag must come back false.
    let row = mud_db::characters::find_by_name(&pool, &name)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(row.id, char_id);
    assert!(
        !row.name_approved,
        "fresh char with toggle ON starts unapproved"
    );

    // And through list_for_user.
    let listed = mud_db::characters::list_for_user(&pool, &user_id)
        .await
        .expect("list");
    let found = listed
        .iter()
        .find(|c| c.id == char_id)
        .expect("char in user roster");
    assert!(!found.name_approved, "list_for_user round-trips the flag");

    // Approve.
    let approved = mud_db::characters::set_name_approved(&pool, &char_id, true)
        .await
        .expect("approve");
    assert_eq!(approved, 1);
    let row = mud_db::characters::find_by_name(&pool, &name)
        .await
        .expect("find post-approve")
        .expect("present");
    assert!(
        row.name_approved,
        "set_name_approved(true) flipped the flag"
    );

    // Re-approve is idempotent (1 row touched, no error).
    let reapproved = mud_db::characters::set_name_approved(&pool, &char_id, true)
        .await
        .expect("re-approve");
    assert_eq!(
        reapproved, 1,
        "UPDATE always touches the row even when value matches"
    );

    // Flip back to unapproved for the next assertion.
    let unapproved = mud_db::characters::set_name_approved(&pool, &char_id, false)
        .await
        .expect("unapprove");
    assert_eq!(unapproved, 1);
    let row = mud_db::characters::find_by_name(&pool, &name)
        .await
        .expect("find post-unapprove")
        .expect("present");
    assert!(
        !row.name_approved,
        "set_name_approved(false) clears the flag"
    );

    // Unknown id → 0 rows touched.
    let missing = mud_db::characters::set_name_approved(&pool, "no-such-id", true)
        .await
        .expect("set on missing id");
    assert_eq!(missing, 0);

    // Cleanup — drop the synthetic character row so re-runs don't
    // pile up unapproved test characters.
    sqlx::query!(r#"DELETE FROM "Characters" WHERE id = $1"#, char_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// Default-true grandfathering: a character created without an
/// explicit `name_approved` override must land at `true`. The schema
/// default carries this, but the test pins it so a future migration
/// can't silently flip the column default to `false`.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn character_name_approval_defaults_true() {
    let pool = pool().await;
    let user_id = testplayer_user_id(&pool).await;
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("ApprovDefault{suffix}");
    let new_char = mud_db::characters::NewCharacter {
        user_id: &user_id,
        name: &name,
        race: "HUMAN",
        gender: "neutral",
        class_id: 1,
        strength: 13,
        intelligence: 13,
        wisdom: 13,
        dexterity: 13,
        constitution: 13,
        charisma: 13,
        name_approved: true,
        password_hash: "",
    };
    let char_id = mud_db::characters::create(&pool, &new_char)
        .await
        .expect("create approved");
    let row = mud_db::characters::find_by_name(&pool, &name)
        .await
        .expect("find")
        .expect("present");
    assert!(
        row.name_approved,
        "default path lands at name_approved = true"
    );

    sqlx::query!(r#"DELETE FROM "Characters" WHERE id = $1"#, char_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// `DiscordConfig::get` reads the singleton row at PK 1. The fixture
/// row may or may not be present depending on the operator's setup;
/// the test just exercises the query path and asserts the shape.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn discord_config_get_shape() {
    let pool = pool().await;
    // Ensure a row exists so the test asserts something meaningful.
    // Use ON CONFLICT to avoid clobbering an operator-authored row.
    sqlx::query!(
        r#"
        INSERT INTO discord_config (id, guild_id, enabled, updated_at)
        VALUES (1, 'test-guild', false, NOW())
        ON CONFLICT (id) DO NOTHING
        "#
    )
    .execute(&pool)
    .await
    .expect("seed discord_config");

    let row = mud_db::discord_config::get(&pool)
        .await
        .expect("get")
        .expect("PK 1 row present");
    assert!(!row.guild_id.is_empty(), "guild_id is NOT NULL in schema");
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn pool_sessions_use_utc_for_naive_now() {
    let pool = pool().await;
    let db_now: chrono::NaiveDateTime = sqlx::query_scalar("SELECT NOW()::timestamp")
        .fetch_one(&pool)
        .await
        .expect("select now");
    let rust_now = chrono::Utc::now().naive_utc();
    let skew = (db_now - rust_now).num_seconds().abs();
    assert!(skew <= 5, "NOW()::timestamp is {skew}s off naive UTC");
}

/// The seeded `LevelDefinition` table is the legacy `init_exp_table`
/// (row N = `exp_table[N - 1]`), and `Class.exp_gain_factor` carries the
/// legacy per-class factors. Needs
/// `fierylib/data/sql/2026-10-07-legacy-exp-table.sql` (or `seed levels`).
#[tokio::test]
#[ignore = "requires live fierydev DB seeded with the legacy exp table"]
async fn level_table_is_the_legacy_curve_with_class_factors() {
    let pool = pool().await;
    let levels = mud_db::levels::list_all(&pool).await.expect("levels");
    let exp = |l: i32| {
        levels
            .iter()
            .find(|r| r.level == l)
            .unwrap_or_else(|| panic!("level {l}"))
            .exp_required
    };
    assert_eq!(exp(1), 0);
    assert_eq!(exp(2), 5_500);
    assert_eq!(exp(10), 254_500);
    assert_eq!(exp(85), 67_772_000);
    assert_eq!(exp(99), 99_938_000);
    assert_eq!(exp(100), 105_806_000);
    assert_eq!(exp(101), 299_999_999);
    let classes = mud_db::classes::list_all(&pool).await.expect("classes");
    let factor = |name: &str| {
        classes
            .iter()
            .find(|c| c.plain_name == name)
            .unwrap_or_else(|| panic!("class {name}"))
            .exp_gain_factor
    };
    assert!((factor("Cleric") - 1.0).abs() < f64::EPSILON);
    assert!((factor("Sorcerer") - 1.2).abs() < f64::EPSILON);
    assert!((factor("Necromancer") - 1.3).abs() < f64::EPSILON);
    assert!((factor("Paladin") - 1.15).abs() < f64::EPSILON);
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn race_innates_are_granted_to_a_fresh_ability_set() {
    use mud_db::character_abilities::CharacterAbilityRow;
    use mud_db::race_abilities::{list_for_race, merge_innates};
    let innates = list_for_race(&pool().await, "ELF")
        .await
        .expect("list elf innates");
    assert!(
        innates.iter().any(|r| r.ability_name == "MAGIC_MISSILE"),
        "elf innate rows: {innates:?}"
    );
    let mut rows: Vec<CharacterAbilityRow> = Vec::new();
    let granted = merge_innates(&mut rows, &innates);
    assert_eq!(granted, innates.len());
    assert!(rows.iter().all(|r| r.known && r.proficiency > 0));
}

/// The lit flag lives in the `lit` key of `custom_values`; toggling it
/// through the UPDATE path must leave the row's other keys alone, and a
/// non-object `custom_values` must neither abort the save nor the read.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
#[allow(clippy::too_many_lines)]
async fn lit_flag_preserves_other_custom_values_keys() {
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;
    let cid = sqlx::query!(r#"SELECT id FROM "Characters" WHERE name = 'TestWarrior' LIMIT 1"#)
        .fetch_one(&pool)
        .await
        .expect("seed user TestWarrior must exist")
        .id;
    let key = sqlx::query!(r#"SELECT zone_id, id FROM "Objects" ORDER BY zone_id, id LIMIT 1"#)
        .fetch_one(&pool)
        .await
        .expect("an Objects row");
    let before = list_for(&pool, &cid).await.expect("list before");
    let mut conn = pool.acquire().await.expect("acquire conn");

    let snap = |persisted_id: Option<i32>, lit: bool| CharacterItemSnap {
        persisted_id,
        object_zone_id: key.zone_id,
        object_id: key.id,
        equipped_location: None,
        parent_persisted_id: None,
        parent_idx: None,
        charges: None,
        liquid_remaining: None,
        liquid_type: None,
        lit,
        custom: None,
        alter: None,
        in_corpse: false,
    };
    // Start from an empty inventory, then insert one unlit item.
    save_inventory_diff(&mut conn, &cid, &[], None)
        .await
        .expect("clear");
    let assigned = save_inventory_diff(&mut conn, &cid, &[snap(None, false)], None)
        .await
        .expect("insert");
    let id = assigned[&0];

    let custom = |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, serde_json::Value>(
                r#"SELECT custom_values FROM "CharacterItems" WHERE id = $1"#,
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("custom_values")
        }
    };
    sqlx::query(
        r#"UPDATE "CharacterItems" SET custom_values = '{"note": "keep", "n": 3}' WHERE id = $1"#,
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("seed keys");

    // Toggle lit on, then off, through the UPDATE path.
    save_inventory_diff(&mut conn, &cid, &[snap(Some(id), true)], None)
        .await
        .expect("lit on");
    let on = custom(&pool).await;
    assert_eq!(on["lit"], true);
    assert_eq!(on["note"], "keep");
    assert_eq!(on["n"], 3);
    let row = &list_for(&pool, &cid).await.expect("list")[0];
    assert!(row.lit);

    save_inventory_diff(&mut conn, &cid, &[snap(Some(id), false)], None)
        .await
        .expect("lit off");
    let off = custom(&pool).await;
    assert!(off.get("lit").is_none(), "lit key removed: {off}");
    assert_eq!(off["note"], "keep");
    assert_eq!(off["n"], 3);

    // A scalar/null/odd `lit` value never aborts the save or the read.
    for bad in ["42", "null", r#""x""#, r#"{"lit": "maybe"}"#] {
        sqlx::query(r#"UPDATE "CharacterItems" SET custom_values = $2::jsonb WHERE id = $1"#)
            .bind(id)
            .bind(bad)
            .execute(&pool)
            .await
            .expect("seed bad value");
        let row = &list_for(&pool, &cid).await.expect("tolerant read")[0];
        assert!(!row.lit, "{bad} reads as unlit");
        save_inventory_diff(&mut conn, &cid, &[snap(Some(id), true)], None)
            .await
            .expect("save over bad value");
        assert_eq!(custom(&pool).await["lit"], true);
    }

    // Restore the original inventory.
    let restore: Vec<CharacterItemSnap> = before
        .iter()
        .map(|r| CharacterItemSnap {
            persisted_id: None,
            object_zone_id: r.object_zone_id,
            object_id: r.object_id,
            equipped_location: r.equipped_location.clone(),
            parent_persisted_id: None,
            parent_idx: None,
            charges: (r.charges >= 0).then_some(r.charges),
            liquid_remaining: r.liquid_type.as_ref().map(|_| r.liquid_remaining),
            liquid_type: r.liquid_type.clone(),
            lit: r.lit,
            custom: None,
            alter: None,
            in_corpse: false,
        })
        .collect();
    save_inventory_diff(&mut conn, &cid, &restore, None)
        .await
        .expect("restore");
}

// ---------------------------------------------------------------------------
// DB-backed player corpses
// ---------------------------------------------------------------------------

/// Throwaway character (and its corpses / items) so the corpse tests never
/// touch the seeded accounts.
async fn corpse_test_char(pool: &PgPool, tag: &str) -> String {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = format!("zt-{tag}-{suffix}");
    sqlx::query(r#"INSERT INTO "Characters" (id, name, updated_at) VALUES ($1, $2, NOW())"#)
        .bind(&id)
        .bind(format!("Zt{tag}{}", suffix % 1_000_000_000_000))
        .execute(pool)
        .await
        .expect("insert character");
    id
}

async fn corpse_test_cleanup(pool: &PgPool, ids: &[&String]) {
    let ids: Vec<String> = ids.iter().map(|s| (*s).clone()).collect();
    for sql in [
        r#"DELETE FROM "PlayerCorpses" WHERE owner_id = ANY($1)"#,
        r#"DELETE FROM "CharacterItems" WHERE character_id = ANY($1)"#,
        r#"DELETE FROM "Characters" WHERE id = ANY($1)"#,
    ] {
        sqlx::query(sql)
            .bind(&ids)
            .execute(pool)
            .await
            .expect("cleanup");
    }
}

fn corpse_snap(
    persisted_id: Option<i32>,
    key: (i32, i32),
    parent_idx: Option<usize>,
    in_corpse: bool,
) -> CharacterItemSnap {
    CharacterItemSnap {
        persisted_id,
        object_zone_id: key.0,
        object_id: key.1,
        equipped_location: None,
        parent_persisted_id: None,
        parent_idx,
        charges: Some(3),
        liquid_remaining: None,
        liquid_type: None,
        lit: false,
        custom: None,
        alter: None,
        in_corpse,
    }
}

async fn object_key(pool: &PgPool) -> (i32, i32) {
    let k = sqlx::query!(r#"SELECT zone_id, id FROM "Objects" ORDER BY zone_id, id LIMIT 1"#)
        .fetch_one(pool)
        .await
        .expect("an Objects row");
    (k.zone_id, k.id)
}

/// The death transaction files a bag and its contents under the corpse; a
/// racing (stale or empty) owner save cannot delete them, and the owner's
/// carried listing excludes them.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn corpse_items_survive_racing_owner_saves_and_keep_nesting() {
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;
    let key = object_key(&pool).await;
    let owner = corpse_test_char(&pool, "own").await;

    let mut tx = pool.begin().await.expect("begin");
    let corpse_id = mud_db::player_corpses::insert(&mut tx, &owner, 30, 45, 321, 600)
        .await
        .expect("insert corpse");
    // bag (idx 0) with a gem inside (idx 1), both in the corpse.
    let assigned = save_inventory_diff(
        &mut tx,
        &owner,
        &[
            corpse_snap(None, key, None, true),
            corpse_snap(None, key, Some(0), true),
        ],
        Some(corpse_id),
    )
    .await
    .expect("death save");
    tx.commit().await.expect("commit");
    let (bag, gem) = (assigned[&0], assigned[&1]);

    // An empty save (the owner's racing / pending save) must not touch them.
    let mut conn = pool.acquire().await.expect("conn");
    save_inventory_diff(&mut conn, &owner, &[], None)
        .await
        .expect("racing save");
    assert!(list_for(&pool, &owner).await.unwrap().is_empty());
    let rows = mud_db::character_items::list_for_corpse(&pool, corpse_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().find(|r| r.id == gem).unwrap().container_id,
        Some(bag)
    );
    assert!(rows.iter().all(|r| r.character_id == owner));

    let listed = mud_db::player_corpses::list_all(&pool).await.unwrap();
    let mine = listed.iter().find(|c| c.id == corpse_id).expect("listed");
    assert_eq!((mine.coins, mine.room_zone_id, mine.room_id), (321, 30, 45));
    assert!(mine.remaining_secs > 590 && mine.remaining_secs <= 600);

    corpse_test_cleanup(&pool, &[&owner]).await;
}

/// Looting re-homes the row (new `character_id`, `corpse_id` cleared) through
/// the normal UPDATE path without touching editor-owned columns.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn looting_moves_the_row_and_keeps_instance_columns() {
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;
    let key = object_key(&pool).await;
    let owner = corpse_test_char(&pool, "lo").await;
    let looter = corpse_test_char(&pool, "lt").await;

    let mut tx = pool.begin().await.unwrap();
    let corpse_id = mud_db::player_corpses::insert(&mut tx, &owner, 30, 45, 0, 600)
        .await
        .unwrap();
    let assigned = save_inventory_diff(
        &mut tx,
        &owner,
        &[corpse_snap(None, key, None, true)],
        Some(corpse_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let row = assigned[&0];
    sqlx::query(
        r#"UPDATE "CharacterItems"
           SET custom_name = 'Fancy', custom_examine_description = 'Shiny', condition = 55,
               custom_values = '{"note": "keep"}'
           WHERE id = $1"#,
    )
    .bind(row)
    .execute(&pool)
    .await
    .unwrap();

    // Another player's save claims the row (carried, not in corpse).
    let mut conn = pool.acquire().await.unwrap();
    save_inventory_diff(
        &mut conn,
        &looter,
        &[corpse_snap(Some(row), key, None, false)],
        None,
    )
    .await
    .unwrap();

    let (owner_now, corpse_now, custom_name, examine, wear, custom): (
        String,
        Option<i32>,
        Option<String>,
        Option<String>,
        i32,
        serde_json::Value,
    ) = sqlx::query_as(
        r#"SELECT character_id, corpse_id, custom_name, custom_examine_description,
                  condition, custom_values
           FROM "CharacterItems" WHERE id = $1"#,
    )
    .bind(row)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner_now, looter);
    assert_eq!(corpse_now, None);
    assert_eq!(custom_name.as_deref(), Some("Fancy"));
    assert_eq!(examine.as_deref(), Some("Shiny"));
    assert_eq!(wear, 55);
    assert_eq!(custom["note"], "keep");
    // The owner's later save can't delete the looter's row.
    save_inventory_diff(&mut conn, &owner, &[], None)
        .await
        .unwrap();
    assert_eq!(list_for(&pool, &looter).await.unwrap().len(), 1);

    corpse_test_cleanup(&pool, &[&owner, &looter]).await;
}

/// Coins leave the corpse only in the transaction that credits the looter.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn corpse_coins_are_taken_atomically_with_the_looters_wealth() {
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;
    let owner = corpse_test_char(&pool, "co").await;
    let looter = corpse_test_char(&pool, "cl").await;
    let mut conn = pool.acquire().await.unwrap();
    let corpse_id = mud_db::player_corpses::insert(&mut conn, &owner, 30, 45, 500, 600)
        .await
        .unwrap();
    drop(conn);
    let coins = || async {
        sqlx::query_scalar::<_, i64>(r#"SELECT coins FROM "PlayerCorpses" WHERE id = $1"#)
            .bind(corpse_id)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let wealth = || async {
        sqlx::query_scalar::<_, i64>(r#"SELECT wealth FROM "Characters" WHERE id = $1"#)
            .bind(&looter)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    // Rolled back: neither side moves.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Characters" SET wealth = 200 WHERE id = $1"#)
        .bind(&looter)
        .execute(&mut *tx)
        .await
        .unwrap();
    mud_db::player_corpses::take_coins(&mut tx, corpse_id, 200)
        .await
        .unwrap();
    drop(tx);
    assert_eq!((coins().await, wealth().await), (500, 0));

    // Committed: both move together.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Characters" SET wealth = 200 WHERE id = $1"#)
        .bind(&looter)
        .execute(&mut *tx)
        .await
        .unwrap();
    mud_db::player_corpses::take_coins(&mut tx, corpse_id, 200)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((coins().await, wealth().await), (300, 200));

    // Over-taking clamps at zero instead of going negative.
    let mut conn = pool.acquire().await.unwrap();
    mud_db::player_corpses::take_coins(&mut conn, corpse_id, 10_000)
        .await
        .unwrap();
    assert_eq!(coins().await, 0);

    corpse_test_cleanup(&pool, &[&owner, &looter]).await;
}

/// Decay deletes the corpse row and (by cascade) whatever is still in it,
/// leaving looted items alone; expired corpses list with remaining <= 0.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn deleting_a_corpse_cascades_to_its_remaining_items() {
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;
    let key = object_key(&pool).await;
    let owner = corpse_test_char(&pool, "de").await;

    let mut tx = pool.begin().await.unwrap();
    let corpse_id = mud_db::player_corpses::insert(&mut tx, &owner, 30, 45, 5, 600)
        .await
        .unwrap();
    save_inventory_diff(
        &mut tx,
        &owner,
        &[
            corpse_snap(None, key, None, true),
            corpse_snap(None, key, None, false),
        ],
        Some(corpse_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        r#"UPDATE "PlayerCorpses" SET decay_at = NOW() - INTERVAL '2 minutes' WHERE id = $1"#,
    )
    .bind(corpse_id)
    .execute(&pool)
    .await
    .unwrap();
    let listed = mud_db::player_corpses::list_all(&pool).await.unwrap();
    assert!(
        listed
            .iter()
            .find(|c| c.id == corpse_id)
            .unwrap()
            .remaining_secs
            <= 0
    );

    mud_db::player_corpses::delete(&pool, corpse_id)
        .await
        .unwrap();
    mud_db::player_corpses::delete(&pool, corpse_id)
        .await
        .unwrap(); // idempotent
    assert!(
        mud_db::character_items::list_for_corpse(&pool, corpse_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        list_for(&pool, &owner).await.unwrap().len(),
        1,
        "the carried (non-corpse) row is untouched"
    );

    corpse_test_cleanup(&pool, &[&owner]).await;
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn mail_unread_count_tracks_read_and_delete() {
    let pool = pool().await;
    let user_id = testplayer_user_id(&pool).await;
    let before = mud_db::mail::unread_count(&pool, &user_id)
        .await
        .expect("count");
    let id = mud_db::mail::send(&pool, &user_id, &user_id, "unread-count test", "body")
        .await
        .expect("send");
    let after = mud_db::mail::unread_count(&pool, &user_id)
        .await
        .expect("count");
    assert_eq!(after, before + 1);

    mud_db::mail::mark_read(&pool, id).await.expect("read");
    assert_eq!(
        mud_db::mail::unread_count(&pool, &user_id).await.unwrap(),
        before
    );

    sqlx::query(r#"DELETE FROM "AccountMail" WHERE id = $1"#)
        .bind(id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// A Curse's delta (`ItemAlter`, the `curse` key of `custom_values`) is
/// written by INSERT, survives the corpse and a looter's save untouched,
/// is overwritten only when the snapshot says the runtime changed it, and
/// never disturbs the row's other `custom_values` keys.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
#[allow(clippy::too_many_lines)]
async fn curse_delta_persists_through_corpse_loot_and_overwrite() {
    use mud_db::character_items::{ItemAlter, ItemAlterSnap};
    let _guard = INVENTORY_LOCK.lock().await;
    let pool = pool().await;
    let key = object_key(&pool).await;
    let owner = corpse_test_char(&pool, "cu").await;
    let looter = corpse_test_char(&pool, "cl").await;
    let cursed = ItemAlter {
        restrictions_added: vec!["NO_DROP".to_string()],
        restrictions_removed: Vec::new(),
        weapon_dice_size: -1,
    };
    let with_alter = |persisted_id: Option<i32>, in_corpse: bool, alter: Option<ItemAlterSnap>| {
        let mut s = corpse_snap(persisted_id, key, None, in_corpse);
        s.alter = alter;
        s
    };

    // Death: the cursed item goes into the corpse (INSERT always writes).
    let mut tx = pool.begin().await.unwrap();
    let corpse_id = mud_db::player_corpses::insert(&mut tx, &owner, 30, 45, 0, 600)
        .await
        .unwrap();
    let assigned = save_inventory_diff(
        &mut tx,
        &owner,
        &[with_alter(
            None,
            true,
            Some(ItemAlterSnap {
                state: cursed.clone(),
                overwrite: false,
            }),
        )],
        Some(corpse_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let row = assigned[&0];
    let corpse_rows = mud_db::character_items::list_for_corpse(&pool, corpse_id)
        .await
        .unwrap();
    assert_eq!(corpse_rows[0].alter(), Some(cursed.clone()));

    sqlx::query(
        r#"UPDATE "CharacterItems" SET custom_values = custom_values || '{"note": "keep"}' WHERE id = $1"#,
    )
    .bind(row)
    .execute(&pool)
    .await
    .unwrap();

    // The looter's save carries a clean (non-overwriting) snapshot: the
    // stored delta must stay, and so must the other key.
    let mut conn = pool.acquire().await.unwrap();
    save_inventory_diff(
        &mut conn,
        &looter,
        &[with_alter(
            Some(row),
            false,
            Some(ItemAlterSnap {
                state: ItemAlter::default(),
                overwrite: false,
            }),
        )],
        None,
    )
    .await
    .unwrap();
    let carried = list_for(&pool, &looter).await.unwrap();
    assert_eq!(carried[0].alter(), Some(cursed.clone()));

    // Remove Curse: a dirty, now-empty snapshot clears the key only.
    save_inventory_diff(
        &mut conn,
        &looter,
        &[with_alter(
            Some(row),
            false,
            Some(ItemAlterSnap {
                state: ItemAlter::default(),
                overwrite: true,
            }),
        )],
        None,
    )
    .await
    .unwrap();
    let values: serde_json::Value =
        sqlx::query_scalar(r#"SELECT custom_values FROM "CharacterItems" WHERE id = $1"#)
            .bind(row)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(values, serde_json::json!({"note": "keep"}));
    assert_eq!(list_for(&pool, &looter).await.unwrap()[0].alter(), None);

    // Cursed again (dirty) writes it back next to the other key.
    save_inventory_diff(
        &mut conn,
        &looter,
        &[with_alter(
            Some(row),
            false,
            Some(ItemAlterSnap {
                state: cursed.clone(),
                overwrite: true,
            }),
        )],
        None,
    )
    .await
    .unwrap();
    let carried = list_for(&pool, &looter).await.unwrap();
    assert_eq!(carried[0].alter(), Some(cursed));
    drop(conn);

    corpse_test_cleanup(&pool, &[&owner, &looter]).await;
}
