use mud_db::{
    character_items::{list_for, save_inventory_diff, CharacterItemSnap},
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
    assert!(rooms.len() > 1000, "expected many rooms, got {}", rooms.len());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_exits() {
    let exits = list_exits(&pool().await).await.expect("list exits");
    assert!(exits.len() > 1000, "expected many exits, got {}", exits.len());
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
    let resets = list_mob_resets(&pool().await).await.expect("list mob resets");
    // Imported world has thousands of mob resets.
    assert!(resets.len() > 1000, "expected many mob resets, got {}", resets.len());
    // Probability is a fraction in [0, 1].
    for r in &resets {
        assert!(r.probability >= 0.0 && r.probability <= 1.0, "probability oob: {r:?}");
        assert!(r.max_instances >= 1, "max_instances < 1: {r:?}");
    }
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_object_resets() {
    let resets = list_object_resets(&pool().await).await.expect("list object resets");
    assert!(!resets.is_empty(), "expected some object resets");
    for r in &resets {
        assert!(r.probability >= 0.0 && r.probability <= 1.0);
        assert!(r.max_instances >= 1);
    }
}

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
    let pool = pool().await;

    // Find TestWarrior's character_id.
    let row = sqlx::query!(
        r#"SELECT id FROM "Characters" WHERE name = 'TestWarrior' LIMIT 1"#
    )
    .fetch_optional(&pool)
    .await
    .expect("query")
    .expect("seed user TestWarrior must exist");
    let cid = row.id;

    // Pick two real Object keys we can FK to.
    let keys: Vec<(i32, i32)> = sqlx::query!(
        r#"SELECT zone_id, id FROM "Objects" ORDER BY zone_id, id LIMIT 2"#
    )
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
        },
    ];
    let mut conn = pool.acquire().await.expect("acquire conn");
    let assigned = save_inventory_diff(&mut conn, &cid, &payload).await.expect("save");
    assert_eq!(assigned.len(), 2, "both rows INSERTed → both ids returned");

    let after = list_for(&pool, &cid).await.expect("list after");
    assert_eq!(after.len(), 2, "two rows after save");
    let worn: Vec<_> = after.iter().filter(|r| r.equipped_location.as_deref() == Some("BODY")).collect();
    assert_eq!(worn.len(), 1, "one worn-on-body row");
    let carried: Vec<_> = after.iter().filter(|r| r.equipped_location.is_none()).collect();
    assert_eq!(carried.len(), 1, "one carried row");

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
            charges: if r.charges >= 0 { Some(r.charges) } else { None },
            liquid_remaining: r.liquid_type.as_ref().map(|_| r.liquid_remaining),
            liquid_type: r.liquid_type.clone(),
        })
        .collect();
    save_inventory_diff(&mut conn, &cid, &restore).await.expect("restore");
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

/// Round-trip a Discord link: create → lookup → mark_verified → unlink.
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

/// Character name-approval gate (replaces the legacy LoginRequests
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
    assert!(!row.name_approved, "fresh char with toggle ON starts unapproved");

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
    assert!(row.name_approved, "set_name_approved(true) flipped the flag");

    // Re-approve is idempotent (1 row touched, no error).
    let reapproved = mud_db::characters::set_name_approved(&pool, &char_id, true)
        .await
        .expect("re-approve");
    assert_eq!(reapproved, 1, "UPDATE always touches the row even when value matches");

    // Flip back to unapproved for the next assertion.
    let unapproved = mud_db::characters::set_name_approved(&pool, &char_id, false)
        .await
        .expect("unapprove");
    assert_eq!(unapproved, 1);
    let row = mud_db::characters::find_by_name(&pool, &name)
        .await
        .expect("find post-unapprove")
        .expect("present");
    assert!(!row.name_approved, "set_name_approved(false) clears the flag");

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
    };
    let char_id = mud_db::characters::create(&pool, &new_char)
        .await
        .expect("create approved");
    let row = mud_db::characters::find_by_name(&pool, &name)
        .await
        .expect("find")
        .expect("present");
    assert!(row.name_approved, "default path lands at name_approved = true");

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
