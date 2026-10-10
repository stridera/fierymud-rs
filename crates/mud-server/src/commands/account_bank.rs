//! Account-shared bank commands. Distinct from the per-character
//! `balance` / `deposit` / `withdraw` family because the coin lives on
//! `Users.account_wealth` rather than `Characters.bank_wealth` — every
//! character on the same account sees the same pool, and a transfer by
//! one character is immediately visible to every other online sibling.
//!
//! Internal pattern:
//! * `cmd_account_balance` is a pure read off the `AccountWealth`
//!   component. No mutation; safe everywhere.
//! * `account_deposit` moves coin from the per-character bank into
//!   the shared pool. The opposite direction is `account_withdraw`.
//!   Neither touches on-hand wealth — players use the existing
//!   `deposit` / `withdraw` for that, then `adeposit` / `awithdraw` to
//!   shuttle into the shared pool.
//! * The pool is shared by every character on the account, so the
//!   database is its only authority: a transfer is a guarded SQL delta
//!   (`account_wealth + n WHERE account_wealth + n >= 0`) in one transaction
//!   with the character's own bank row, taken under the character's
//!   save-order turn (`AsyncCommand`, like the account chest). The
//!   in-memory `AccountWealth` is only a display copy refreshed from the
//!   result, and the character save never writes it back, so a stale sibling
//!   can neither spend coin the pool no longer holds nor overwrite a
//!   transfer.
//! * Cross-character sync: after the mutation, `fanout_account_wealth`
//!   walks every online character whose `Account.user_id` matches and
//!   updates their `AccountWealth` component.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, AccountWealth, BankWealth, Online, Player};

use crate::autosave::SaveCoordinator;
use crate::commands::{
    AsyncCommand, Category, Command, Help, cmd_mail_stub, format_amount, format_wealth,
    require_linked_account, send_to,
};

inventory::submit! {
    Command {
        names: &["account_balance", "abal", "accountbal", "accountbalance"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "account_balance",
            summary: "Show the account-shared bank balance.",
            long: "Read-only display of 'Users.account_wealth'. \
                   Every character on this account sees the same \
                   pool; pair with 'account_deposit' / \
                   'account_withdraw' to transfer between the \
                   per-character bank ('balance') and the shared pool.",
        },
        run: cmd_account_balance,
    }
}

inventory::submit! {
    Command {
        names: &["account_deposit", "adeposit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "account_deposit <amount>",
            summary: "Move copper from per-character bank to the shared pool.",
            long: "Source is your 'bank' balance, not on-hand. \
                   Refuses if your per-character bank can't cover \
                   the amount. After success every online character \
                   on this account sees the new shared pool size.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["account_withdraw", "awithdraw"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "account_withdraw <amount>",
            summary: "Move copper from the shared pool to your per-character bank.",
            long: "Inverse of 'account_deposit'. Refuses if the \
                   shared pool can't cover the amount. Cross-character \
                   sync runs after the transfer — all sibling \
                   characters online refresh their view of the pool.",
        },
        run: cmd_mail_stub,
    }
}

fn cmd_account_balance(world: &mut World, player: Entity, _args: &str) {
    if require_linked_account(world, player, "the account chest/bank").is_none() {
        return;
    }
    let pool = world.get::<AccountWealth>(player).map_or(0, |a| a.0);
    let line = format_wealth(pool).map_or_else(
        || "Your account chest is empty.".to_string(),
        |parts| format!("Your account shares hold {parts}."),
    );
    send_to(world, player, format!("\r\n{line}\r\n"));
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "account_deposit" | "adeposit" => Some(Box::pin(account_transfer(
                world,
                player,
                pool,
                args,
                AccountDir::Deposit,
            ))),
            "account_withdraw" | "awithdraw" => Some(Box::pin(account_transfer(
                world,
                player,
                pool,
                args,
                AccountDir::Withdraw,
            ))),
            _ => None,
        },
    }
}

#[derive(Clone, Copy)]
enum AccountDir {
    /// Per-character bank → shared pool.
    Deposit,
    /// Shared pool → per-character bank.
    Withdraw,
}

/// Shared body for `account_deposit` / `account_withdraw`. Validates the
/// amount, then moves the coin in the database (guarded delta on the shared
/// pool plus the character's bank row, one transaction, under the
/// character's save-order turn) and only then mirrors the result into the
/// calling character and every online sibling.
///
/// The on-hand `Wealth` component is intentionally untouched —
/// players already have `deposit` / `withdraw` for the on-hand
/// ↔ per-character-bank step. Splitting the two transfers keeps
/// each command predictable about which side it touches.
async fn account_transfer(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
    direction: AccountDir,
) {
    let label = match direction {
        AccountDir::Deposit => "account_deposit",
        AccountDir::Withdraw => "account_withdraw",
    };
    let Some(account) = require_linked_account(world, player, "the account chest/bank") else {
        return;
    };
    let amount = match args.trim().parse::<i64>() {
        Ok(n) if n > 0 => n,
        _ => {
            send_to(
                world,
                player,
                format!("Usage: {label} <amount of copper>\r\n"),
            );
            return;
        }
    };
    // Take the character's save-order turn before reading the bank: a save
    // already in flight must have landed (and been folded in) first, and
    // none queued behind us may write the old bank over this transfer.
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let mut ordered = coordinator.begin_ordered(&account.character_id).await;
    coordinator.apply_completions(world);
    let bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    if matches!(direction, AccountDir::Deposit) && bank < amount {
        send_to(
            world,
            player,
            "Your per-character bank doesn't have that much.\r\n",
        );
        return;
    }
    let (new_bank, pool_delta) = match direction {
        AccountDir::Deposit => (bank - amount, amount),
        AccountDir::Withdraw => (bank.saturating_add(amount), -amount),
    };
    let uid = account.user_id;
    let new_pool = match mud_db::users::transfer_with_bank(
        pool,
        &uid,
        &account.character_id,
        pool_delta,
        new_bank,
    )
    .await
    {
        Ok(Some(new_pool)) => new_pool,
        Ok(None) => {
            // The shared pool no longer holds that much (a sibling spent it).
            // Show the real balance instead of the stale one.
            if let Ok(real) = mud_db::users::load_account_wealth(pool, &uid).await {
                set_account_wealth(world, player, &uid, real);
            }
            send_to(
                world,
                player,
                "The account shares don't have that much.\r\n",
            );
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, user_id = %uid, "account transfer failed");
            send_to(
                world,
                player,
                "The bank can't process that right now. Try again in a moment.\r\n",
            );
            return;
        }
    };
    // Snapshots taken before this point carry the old bank balance.
    ordered.supersede_earlier_snapshots();
    drop(ordered);
    if let Some(mut b) = world.get_mut::<BankWealth>(player) {
        b.0 = new_bank;
    }
    set_account_wealth(world, player, &uid, new_pool);
    let verb = match direction {
        AccountDir::Deposit => "Moved",
        AccountDir::Withdraw => "Withdrew",
    };
    let suffix = match direction {
        AccountDir::Deposit => "to the account shares",
        AccountDir::Withdraw => "from the account shares",
    };
    send_to(
        world,
        player,
        format!("{verb} {} {suffix}.\r\n", format_amount(amount)),
    );
}

/// Show `new_pool` on the calling character and every online sibling.
fn set_account_wealth(world: &mut World, player: Entity, user_id: &str, new_pool: i64) {
    if let Some(mut a) = world.get_mut::<AccountWealth>(player) {
        a.0 = new_pool;
    } else if let Ok(mut em) = world.get_entity_mut(player) {
        em.insert(AccountWealth(new_pool));
    }
    fanout_account_wealth(world, user_id, new_pool, Some(player));
}

/// Refresh `AccountWealth` on every online character matching
/// `user_id`. Called after a per-character transfer so siblings on
/// the same account see the new shared pool size on their next
/// `account_balance` read — without a DB roundtrip.
///
/// `except` skips one entity (typically the caller) when the caller
/// already mutated its own component before computing the new value.
/// Public to the crate so tests can drive it directly.
pub(crate) fn fanout_account_wealth(
    world: &mut World,
    user_id: &str,
    new_pool: i64,
    except: Option<Entity>,
) {
    // Two-step: collect the entities under a borrow, then mutate.
    // `query_filtered` with `&Account` makes the user_id read cheap;
    // the With<Online> filter keeps disconnected entities out of
    // the fanout (their save path picks up the new value on next
    // login, since AccountWealth is loaded from Users.account_wealth).
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(e, acc)| acc.user_id == user_id && Some(*e) != except)
            .map(|(e, _)| e)
            .collect()
    };
    for e in targets {
        if let Some(mut a) = world.get_mut::<AccountWealth>(e) {
            a.0 = new_pool;
        } else if let Ok(mut em) = world.get_entity_mut(e) {
            em.insert(AccountWealth(new_pool));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_db::enums::Permission;
    use mud_world::Named;

    fn account(user_id: &str, character_id: &str) -> Account {
        Account {
            user_id: user_id.to_string(),
            character_id: character_id.to_string(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: Vec::<Permission>::new(),
        }
    }

    /// An unlinked legacy character: `Account.user_id` is empty and a
    /// `Connection` captures what the player is told.
    fn make_unlinked_world() -> (World, Entity, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        let mut world = World::new();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let e = world
            .spawn((
                Player,
                Online,
                Named {
                    name: "Legacy".to_string(),
                },
                account("", "char-l"),
                crate::commands::Connection(tx),
                BankWealth(1000),
                AccountWealth(0),
            ))
            .id();
        (world, e, rx)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlinked_character_gets_link_hint_instead_of_account_bank() {
        let (mut world, e, mut rx) = make_unlinked_world();
        // Never connects: the link check must short-circuit before any DB use.
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap();
        cmd_account_balance(&mut world, e, "");
        account_transfer(&mut world, e, &pool, "300", AccountDir::Deposit).await;
        let mut out = String::new();
        while let Ok(b) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&b));
        }
        assert_eq!(
            out.matches(
                "Link this character to a website account at \
                 https://muditor.fierymud.org to use the account chest/bank."
            )
            .count(),
            2,
            "{out}"
        );
        // Nothing moved.
        assert_eq!(world.get::<BankWealth>(e).unwrap().0, 1000);
        assert_eq!(world.get::<AccountWealth>(e).unwrap().0, 0);
    }

    #[test]
    fn async_dispatch_claims_every_transfer_name() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let mut world = World::new();
        let player = world.spawn_empty().id();
        for head in [
            "account_deposit",
            "adeposit",
            "account_withdraw",
            "awithdraw",
        ] {
            let claimed = inventory::iter::<AsyncCommand>()
                .any(|cmd| (cmd.dispatch)(&mut world, player, &pool, head, "5").is_some());
            assert!(claimed, "no async dispatch claims '{head}'");
        }
    }

    /// Live-DB scaffolding: one account with two characters, torn down by `end`.
    struct Fx {
        /// Serialises live-DB tests; released when the fixture drops.
        _db_lock: tokio::sync::MutexGuard<'static, ()>,
        pool: mud_db::sqlx::PgPool,
        user_id: String,
        a_id: String,
        b_id: String,
    }

    async fn fixture(pool_copper: i64, bank_a: i64, bank_b: i64) -> Option<Fx> {
        let db_lock = crate::commands::test_support::db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let Ok(Ok(pool)) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            mud_db::connect_with(&url, crate::commands::test_support::db_test_pool_settings()),
        )
        .await
        else {
            eprintln!("skipping: dev database unavailable");
            return None;
        };
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let user_id = format!("zz-bank-u-{tag}");
        let a_id = format!("zz-bank-a-{tag}");
        let b_id = format!("zz-bank-b-{tag}");
        mud_db::sqlx::query(
            "INSERT INTO \"Users\" (id, email, display_name, account_wealth, updated_at) \
             VALUES ($1, $2, $1, $3, NOW())",
        )
        .bind(&user_id)
        .bind(format!("{user_id}@test.invalid"))
        .bind(pool_copper)
        .execute(&pool)
        .await
        .unwrap();
        for (id, bank, n) in [(&a_id, bank_a, "a"), (&b_id, bank_b, "b")] {
            mud_db::sqlx::query(
                "INSERT INTO \"Characters\" (id, name, user_id, bank_wealth, updated_at) \
                 VALUES ($1, $2, $3, $4, NOW())",
            )
            .bind(id)
            .bind(format!("Zzbk{n}{}", tag % 1_000_000_000_000))
            .bind(&user_id)
            .bind(bank)
            .execute(&pool)
            .await
            .unwrap();
        }
        Some(Fx {
            _db_lock: db_lock,
            pool,
            user_id,
            a_id,
            b_id,
        })
    }

    impl Fx {
        /// Both characters online on the account, each believing the pool
        /// holds `seen_pool` and its bank holds the given amount.
        fn world(&self, seen_pool: i64, bank_a: i64, bank_b: i64) -> (World, Entity, Entity) {
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            let mut spawn = |name: &str, cid: &str, bank: i64| {
                world
                    .spawn((
                        Player,
                        Online,
                        Named {
                            name: name.to_string(),
                        },
                        account(&self.user_id, cid),
                        BankWealth(bank),
                        AccountWealth(seen_pool),
                    ))
                    .id()
            };
            let a = spawn("Alpha", &self.a_id, bank_a);
            let b = spawn("Beta", &self.b_id, bank_b);
            (world, a, b)
        }

        async fn pool_copper(&self) -> i64 {
            mud_db::users::load_account_wealth(&self.pool, &self.user_id)
                .await
                .unwrap()
        }

        async fn bank_of(&self, cid: &str) -> i64 {
            mud_db::sqlx::query_scalar("SELECT bank_wealth FROM \"Characters\" WHERE id = $1")
                .bind(cid)
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }

        async fn end(self) {
            for sql in [
                "DELETE FROM \"Characters\" WHERE user_id = $1",
                "DELETE FROM \"Users\" WHERE id = $1",
            ] {
                mud_db::sqlx::query(sql)
                    .bind(&self.user_id)
                    .execute(&self.pool)
                    .await
                    .unwrap();
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deposit_moves_bank_to_account_pool() {
        let Some(fx) = fixture(0, 1000, 500).await else {
            return;
        };
        let (mut world, a, b) = fx.world(0, 1000, 500);
        account_transfer(&mut world, a, &fx.pool, "300", AccountDir::Deposit).await;
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 700);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 300);
        assert_eq!(
            world.get::<AccountWealth>(b).unwrap().0,
            300,
            "sibling on same account should reflect new pool size"
        );
        // And the database says the same, with no save needed.
        assert_eq!(fx.pool_copper().await, 300);
        assert_eq!(fx.bank_of(&fx.a_id).await, 700);
        assert_eq!(fx.bank_of(&fx.b_id).await, 500);
        fx.end().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deposit_refuses_when_bank_short() {
        let Some(fx) = fixture(0, 1000, 500).await else {
            return;
        };
        let (mut world, a, _b) = fx.world(0, 1000, 500);
        account_transfer(&mut world, a, &fx.pool, "5000", AccountDir::Deposit).await;
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 1000);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 0);
        assert_eq!(fx.pool_copper().await, 0);
        assert_eq!(fx.bank_of(&fx.a_id).await, 1000);
        fx.end().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn withdraw_moves_account_pool_to_bank() {
        let Some(fx) = fixture(700, 1000, 500).await else {
            return;
        };
        let (mut world, a, _b) = fx.world(700, 1000, 500);
        account_transfer(&mut world, a, &fx.pool, "200", AccountDir::Withdraw).await;
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 1200);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 500);
        assert_eq!(fx.pool_copper().await, 500);
        assert_eq!(fx.bank_of(&fx.a_id).await, 1200);
        fx.end().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fanout_skips_other_users() {
        let Some(fx) = fixture(0, 1000, 500).await else {
            return;
        };
        let (mut world, a, _b) = fx.world(0, 1000, 500);
        let other = world
            .spawn((
                Player,
                Online,
                Named {
                    name: "Other".to_string(),
                },
                account("different-user", "char-c"),
                BankWealth(0),
                AccountWealth(0),
            ))
            .id();
        account_transfer(&mut world, a, &fx.pool, "300", AccountDir::Deposit).await;
        assert_eq!(
            world.get::<AccountWealth>(other).unwrap().0,
            0,
            "different-account character should not see Alpha's deposit"
        );
        fx.end().await;
    }

    /// The duplication race: both characters believe the pool holds 1000.
    /// Alpha takes it; Beta's view is stale (its copy was taken before the
    /// fanout) but the database guard refuses the second withdrawal, so the
    /// coin exists exactly once.
    #[tokio::test(flavor = "current_thread")]
    async fn two_characters_cannot_both_withdraw_the_same_pool() {
        let Some(fx) = fixture(1000, 0, 0).await else {
            return;
        };
        let (mut world, a, b) = fx.world(1000, 0, 0);
        account_transfer(&mut world, a, &fx.pool, "1000", AccountDir::Withdraw).await;
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 1000);
        // Beta never saw the fanout.
        world.get_mut::<AccountWealth>(b).unwrap().0 = 1000;
        account_transfer(&mut world, b, &fx.pool, "1000", AccountDir::Withdraw).await;

        assert_eq!(
            world.get::<BankWealth>(b).unwrap().0,
            0,
            "nothing dispensed"
        );
        assert_eq!(
            world.get::<AccountWealth>(b).unwrap().0,
            0,
            "stale view corrected from the database"
        );
        assert_eq!(fx.pool_copper().await, 0);
        assert_eq!(fx.bank_of(&fx.a_id).await, 1000);
        assert_eq!(fx.bank_of(&fx.b_id).await, 0);
        fx.end().await;
    }

    /// The other half of the race: a character save carrying a stale pool
    /// value must not write it back over a sibling's transfer.
    #[tokio::test(flavor = "current_thread")]
    async fn character_save_never_writes_the_account_pool() {
        let Some(fx) = fixture(1000, 0, 0).await else {
            return;
        };
        let (mut world, a, b) = fx.world(1000, 0, 0);
        account_transfer(&mut world, a, &fx.pool, "1000", AccountDir::Withdraw).await;
        assert_eq!(fx.pool_copper().await, 0);

        // Beta still shows the old pool; saving it must leave the row alone.
        world.get_mut::<AccountWealth>(b).unwrap().0 = 1000;
        let room = world.spawn_empty().id();
        world.entity_mut(b).insert((
            mud_world::Health { hp: 10, max: 10 },
            mud_world::Located(room),
        ));
        let out = crate::login::save_player(&mut world, b, &fx.pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert_eq!(fx.pool_copper().await, 0, "save resurrected the pool");
        fx.end().await;
    }
}
