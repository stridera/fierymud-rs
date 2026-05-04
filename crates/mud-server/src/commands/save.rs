//! `save` — force a checkpoint of the current character to the
//! database without disconnecting. The auto-save path runs only
//! on `on_disconnect` and graceful shutdown, so a paranoid
//! player who's about to attempt something risky (or who's
//! noticed flaky network) can use this to checkpoint.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;

use crate::commands::{
    AsyncCommand, Category, Command, Help, cmd_mail_stub, send_to,
};
use crate::login::save_player;

inventory::submit! {
    Command {
        names: &["save"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "save",
            summary: "Force a checkpoint of your character to the database.",
            long: "Persists hp / stamina / xp / inventory / location / \
                   flags / prompt / hunger / thirst / drunk / known \
                   abilities / memorized spells back to the schema. \
                   The same path runs automatically on disconnect and \
                   on graceful shutdown — `save` lets you force it \
                   right now without logging out, useful before a \
                   risky encounter or when the network feels unstable.",
        },
        // The sync stub is never the real handler; the AsyncCommand
        // entry below claims `save` first via dispatch order. Reusing
        // `cmd_mail_stub` matches the convention used by `visit` /
        // other async commands.
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, _args| match head {
            "save" => Some(Box::pin(cmd_save(world, player, pool))),
            _ => None,
        },
    }
}

async fn cmd_save(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
) {
    save_player(world, player, pool).await;
    send_to(world, player, "Saved.\r\n");
}
