//! `game` — Admin meta-command that lists / flips server-wide
//! boolean toggles backed by `GameConfig` rows. Mirrors the legacy
//! `do_game` (act.wizard.cpp:2440) — same surface, same idea: one
//! command to inspect every "is this rule on?" knob.
//!
//! Each toggle below is a `(category, key)` pair in `GameConfig`.
//! Flipping a toggle updates the in-memory `RuntimeConfig` AND
//! upserts the row so the choice survives a restart. Unrecognised
//! sub-names print the full toggle list as a usage hint.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;

use crate::commands::{Category, Command, DbPool, Help, name_of, record_admin_action, send_to};

inventory::submit! {
    Command {
        names: &["game"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "game [<toggle> [on|off]]",
            summary: "List or flip server-wide rule toggles.",
            long: "Without args, prints every toggle the caller is \
                   allowed to see along with its current state. With a \
                   toggle name, prints just that toggle's state; add \
                   `on` / `off` to flip it. Implementor-only flips \
                   like ``devmode`` upsert the matching ``GameConfig`` \
                   row so the choice survives a restart.\n\
                   Known toggles: devmode, pk, summon, charm, sleep, \
                   roomeffect, names, ooc.",
        },
        run: cmd_game,
    }
}

/// One togglable rule — `(category, key)` resolves the backing
/// `GameConfig` row, `min_role` gates both viewing and flipping
/// (matches legacy `min_level` semantics in `do_game`), and the
/// human-readable label is what shows in the status table.
struct Toggle {
    name: &'static str,
    category: &'static str,
    key: &'static str,
    min_role: UserRole,
    default: bool,
    on_msg: &'static str,
    off_msg: &'static str,
}

const TOGGLES: &[Toggle] = &[
    Toggle {
        name: "devmode",
        category: "server",
        key: "dev_mode",
        min_role: UserRole::Implementor,
        default: false,
        on_msg: "Open playtest: every player is Implementor.",
        off_msg: "Normal role gating.",
    },
    Toggle {
        name: "pk",
        category: "social",
        key: "pk_allowed",
        min_role: UserRole::Implementor,
        default: false,
        on_msg: "PKilling allowed.",
        off_msg: "PKilling not allowed.",
    },
    Toggle {
        name: "summon",
        category: "social",
        key: "summon_allowed",
        min_role: UserRole::Implementor,
        default: true,
        on_msg: "Players can summon one another.",
        off_msg: "Player-to-player summoning disabled.",
    },
    Toggle {
        name: "charm",
        category: "social",
        key: "charm_allowed",
        min_role: UserRole::Implementor,
        default: false,
        on_msg: "Players can charm one another.",
        off_msg: "Player-to-player charm disabled.",
    },
    Toggle {
        name: "sleep",
        category: "social",
        key: "sleep_allowed",
        min_role: UserRole::Implementor,
        default: false,
        on_msg: "Players can cast sleep on one another.",
        off_msg: "Player-to-player sleep casting disabled.",
    },
    Toggle {
        name: "roomeffect",
        category: "social",
        key: "roomeffect_allowed",
        min_role: UserRole::Implementor,
        default: false,
        on_msg: "Room-effect spells hurt other players.",
        off_msg: "Room-effect spells skip other players.",
    },
    Toggle {
        name: "names",
        category: "social",
        key: "name_approval_required",
        min_role: UserRole::Implementor,
        default: true,
        on_msg: "Name approval is required.",
        off_msg: "Name approval is NOT required.",
    },
    Toggle {
        name: "ooc",
        category: "social",
        key: "ooc_enabled",
        min_role: UserRole::Implementor,
        default: true,
        on_msg: "OOC channel is enabled.",
        off_msg: "OOC channel is disabled.",
    },
];

fn caller_role(world: &World, player: Entity) -> UserRole {
    world
        .get::<mud_world::Account>(player)
        .map_or(UserRole::Player, |a| a.role)
}

fn current_state(world: &World, t: &Toggle) -> bool {
    world
        .get_resource::<mud_world::RuntimeConfig>()
        .map_or(t.default, |rc| rc.get_bool(t.category, t.key, t.default))
}

fn write_state(world: &mut World, t: &Toggle, new_state: bool) {
    if let Some(rt) = world.get_resource_mut::<mud_world::RuntimeConfig>() {
        rt.into_inner().by_key.insert(
            (t.category.into(), t.key.into()),
            mud_world::ConfigValue::Bool(new_state),
        );
    }
    if t.name == "devmode" {
        world.insert_resource(crate::DevMode(new_state));
    }
    let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone()) else {
        return;
    };
    let category = t.category.to_string();
    let key = t.key.to_string();
    let value = if new_state { "true" } else { "false" };
    let description = format!("`game` toggle for {category}.{key}");
    tokio::spawn(async move {
        let res = mud_db::sqlx::query(
            r#"INSERT INTO "GameConfig" (category, key, value, value_type, description, updated_at)
               VALUES ($1, $2, $3, 'BOOL', $4, NOW())
               ON CONFLICT (category, key) DO UPDATE
                 SET value = EXCLUDED.value, updated_at = NOW()"#,
        )
        .bind(&category)
        .bind(&key)
        .bind(value)
        .bind(&description)
        .execute(&pool)
        .await;
        if let Err(e) = res {
            tracing::warn!(error = %e, %category, %key, "failed to persist game-toggle write");
        }
    });
}

fn render_status(world: &World, role: UserRole) -> String {
    let mut body = String::from("\r\n[Current game status:]\r\n\r\n");
    let mut visible = 0u32;
    for t in TOGGLES {
        if role.rank() < t.min_role.rank() {
            continue;
        }
        visible += 1;
        let on = current_state(world, t);
        let tag = if on {
            format!("[<green>{}</>]", t.name)
        } else {
            format!("[<red>{}</>]", t.name)
        };
        let msg = if on { t.on_msg } else { t.off_msg };
        body.push_str(&format!("  {tag:<24}  {msg}\r\n"));
    }
    if visible == 0 {
        return String::from("You don't have access to any game toggles.\r\n");
    }
    body.push_str("\r\nUsage: game <toggle> [on|off]\r\n");
    body
}

pub(crate) fn cmd_game(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "game", args);
    let role = caller_role(world, player);
    let mut parts = args.split_whitespace();
    let Some(name) = parts.next() else {
        let status = render_status(world, role);
        send_to(world, player, status);
        return;
    };
    let arg = parts.next().unwrap_or("").to_ascii_lowercase();
    let lc = name.to_ascii_lowercase();
    let Some(toggle) = TOGGLES.iter().find(|t| t.name == lc) else {
        send_to(world, player, format!("Unknown toggle '{name}'.\r\n"));
        let status = render_status(world, role);
        send_to(world, player, status);
        return;
    };
    if role.rank() < toggle.min_role.rank() {
        send_to(world, player, "You don't have access to that toggle.\r\n");
        return;
    }
    let current = current_state(world, toggle);
    if arg.is_empty() || arg == "status" {
        let state = if current { "<b:green>ON</>" } else { "<dim>off</>" };
        let label = if current { toggle.on_msg } else { toggle.off_msg };
        send_to(
            world,
            player,
            format!("game {} is {state} — {label}\r\n", toggle.name),
        );
        return;
    }
    let new_state = match arg.as_str() {
        "on" | "true" | "1" | "enable" | "yes" => true,
        "off" | "false" | "0" | "disable" | "no" => false,
        _ => {
            send_to(world, player, format!("Usage: game {} [on|off]\r\n", toggle.name));
            return;
        }
    };
    if new_state == current {
        send_to(
            world,
            player,
            format!(
                "game {} is already {}.\r\n",
                toggle.name,
                if current { "on" } else { "off" }
            ),
        );
        return;
    }
    let actor = name_of(world, player);
    write_state(world, toggle, new_state);
    if new_state {
        tracing::warn!(by = %actor, toggle = %toggle.name, "game toggle ENABLED");
        send_to(
            world,
            player,
            format!("<b:green>game {} ENABLED</> — {}\r\n", toggle.name, toggle.on_msg),
        );
    } else {
        tracing::warn!(by = %actor, toggle = %toggle.name, "game toggle disabled");
        send_to(
            world,
            player,
            format!("<dim>game {} disabled</> — {}\r\n", toggle.name, toggle.off_msg),
        );
    }
}
