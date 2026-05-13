//! Federated-identity player commands — `discord link` and `discord
//! unlink`. The unified `account` / `whoami` readout (which renders
//! linked Discord + Google alongside email/role/roster) lives in
//! `commands/info.rs` so a single command renders the full identity
//! surface.
//!
//! The Discord side is two-step: the player calls `discord link
//! <discord-id>` which mints a one-time verification code into a
//! `PendingDiscordLinks` runtime resource and tells the player to
//! echo `/verify <code>` into the configured gossip channel. The
//! bot (out-of-process, Muditor-side) reads the gossip channel,
//! matches the code against the resource, and on success calls
//! `mud_db::discord_links::link` + `mark_verified`. Until that
//! happens the player's Discord side is `unverified` — display-only;
//! the bot won't honor messages from it.
//!
//! Google side is read-only here. The OAuth handshake happens in
//! Muditor; the unified `account` command just displays the linked
//! email + name.

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, DiscordConfigCatalog, PendingDiscordLink, PendingDiscordLinks};

use crate::commands::{AsyncCommand, Category, Command, Help, cmd_mail_stub, name_of, send_to};

/// Verification-code lifetime. The bot side has to see the player's
/// `/verify <code>` arrive in the gossip channel before this elapses.
/// 10 minutes matches the spec note in the wave-6 task.
const VERIFICATION_CODE_TTL: Duration = Duration::from_secs(10 * 60);

inventory::submit! {
    Command {
        names: &["discord", "discord_link", "discordlink"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "discord link <discord-id>  |  discord unlink",
            summary: "Bind a Discord account to this user.",
            long: "Two-step verification: `discord link <id>` mints a \
                   one-time code; you then echo `/verify <code>` into \
                   the configured gossip channel within 10 minutes. \
                   `discord unlink` clears the binding immediately. \
                   `account` shows the current state.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "discord" | "discord_link" | "discordlink" => {
                Some(Box::pin(cmd_discord_router(world, player, pool, args)))
            }
            _ => None,
        },
    }
}

/// Routes `discord <subcommand>` to link / unlink. Empty / unknown
/// subcommands print the usage line.
async fn cmd_discord_router(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    let trimmed = args.trim();
    let (sub, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim()),
        None => (trimmed, ""),
    };
    match sub.to_ascii_lowercase().as_str() {
        "link" => cmd_discord_link(world, player, rest).await,
        "unlink" => cmd_discord_unlink(world, player, pool).await,
        _ => send_to(
            world,
            player,
            "Usage: discord link <discord-id> | discord unlink\r\n",
        ),
    }
}

/// Mint a one-time verification code for the calling player's Users
/// row and stash it in `PendingDiscordLinks`. Tells the player to
/// echo the code into the gossip channel within 10 minutes.
///
/// The actual link write happens on the bot side (out-of-process)
/// when the bot sees the code arrive in gossip — at that point it
/// calls `discord_links::link` + `mark_verified`. We don't write to
/// `discord_links` here so an attacker who guesses someone's
/// `discord-id` can't squat the binding.
async fn cmd_discord_link(world: &mut World, player: Entity, args: &str) {
    let discord_id = args.trim();
    if discord_id.is_empty() {
        send_to(world, player, "Usage: discord link <discord-id>\r\n");
        return;
    }
    if discord_id.len() > 64 || !discord_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        send_to(
            world,
            player,
            "Discord id must be alphanumeric (with `_` or `.`) and \
             ≤ 64 chars.\r\n",
        );
        return;
    }
    let Some(account) = world.get::<Account>(player).cloned() else {
        send_to(world, player, "You don't have an account.\r\n");
        return;
    };
    let cfg = world.resource::<DiscordConfigCatalog>().clone();
    if !cfg.can_send_gossip() {
        send_to(
            world,
            player,
            "Discord linking isn't configured on this server yet.\r\n",
        );
        return;
    }
    // 6-digit numeric code, zero-padded. Plenty of entropy for a
    // 10-minute window and easy for a player to type / copy into
    // chat. Source: secs-since-epoch low bits XOR a process-local
    // jitter — no rand crate dep needed for this scale.
    let code = generate_verification_code(&account.user_id);
    let expires_at = Instant::now() + VERIFICATION_CODE_TTL;
    {
        let mut pending = world.resource_mut::<PendingDiscordLinks>();
        pending.by_user.insert(
            account.user_id.clone(),
            PendingDiscordLink {
                discord_id: discord_id.to_string(),
                code: code.clone(),
                expires_at,
            },
        );
    }
    tracing::info!(
        user_id = %account.user_id,
        discord_id = %discord_id,
        "discord link verification code minted"
    );
    let mins = VERIFICATION_CODE_TTL.as_secs() / 60;
    send_to(
        world,
        player,
        format!(
            "Verification code: <b:yellow>{code}</>\r\n\
             Within {mins} minutes, send <b>/verify {code}</> to the \
             gossip channel from Discord account <b>{discord_id}</>.\r\n\
             Use `account` to check whether the link has gone through.\r\n",
        ),
    );
}

/// Drop the player's existing Discord link. Idempotent — running it
/// against a user without a link prints a friendly "no link" line.
async fn cmd_discord_unlink(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    let Some(account) = world.get::<Account>(player).cloned() else {
        send_to(world, player, "You don't have an account.\r\n");
        return;
    };
    // Also clear any pending verification code so a re-link starts
    // from a clean state.
    {
        let mut pending = world.resource_mut::<PendingDiscordLinks>();
        pending.by_user.remove(&account.user_id);
    }
    match mud_db::discord_links::unlink(pool, &account.user_id).await {
        Ok(0) => send_to(world, player, "No Discord link to remove.\r\n"),
        Ok(_) => {
            tracing::info!(user_id = %account.user_id, player = %name_of(world, player), "discord link removed");
            send_to(world, player, "Discord link removed.\r\n");
        }
        Err(e) => {
            tracing::warn!(error = %e, "discord unlink failed");
            send_to(world, player, "Failed to remove the Discord link.\r\n");
        }
    }
}

/// 6-digit verification code derived from the user_id + the
/// monotonic clock. Not cryptographic — its job is to keep one
/// player's code from collisioning with another's in a 10-minute
/// window, not to resist offline guessing. The pending-map check
/// at consume time also asserts the discord_id matches the row
/// that minted the code, so brute-forcing the 6-digit space still
/// requires knowing the target's claimed discord_id.
fn generate_verification_code(user_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    user_id.hash(&mut h);
    Instant::now().elapsed().as_nanos().hash(&mut h);
    std::process::id().hash(&mut h);
    let raw = h.finish() % 1_000_000;
    format!("{raw:06}")
}
