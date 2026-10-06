//! Admin development / ops commands closing the C++ parity gaps:
//! zone and ability hot-reload (`reloadzone`, `reloadallzones`,
//! `areload`), trigger tooling (`tlist`, `vscripts`, `dtrig`), ability
//! debugging (`alist`, `asearch`), `aggrodebug`, graceful `shutdown`, the
//! DB-backed stubs (`savezone`, `filewatch`), and the small info commands
//! (`uptime`, `date`, `users`).
//!
//! World data lives in Postgres and is written by Muditor, so the "save" /
//! "file watch" halves of the C++ zone workflow have no meaning here; the
//! reload commands pull the DB state into the running world instead.

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    AbilityCatalog, Account, CombatStats, EffectCatalog, Located, LoggedInAt, Mob, MobPrototypes,
    MudClock, Named, Online, Player, Profile, ScriptErrorLog, SpellSlotData, TriggerAttach,
    TriggerCatalog, WorldKey,
};

use crate::commands::{
    AsyncCommand, Category, Command, Help, aggro_alignment, cmd_mail_stub, name_of,
    record_admin_action, send_to,
};
use crate::{ServerStart, TickCount};

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

inventory::submit! {
    Command {
        names: &["reloadzone"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "reloadzone [<zone>]",
            summary: "Reload one zone's rooms, mobs, objects and resets from the DB.",
            long: "Builder+. Re-reads the zone's room definitions (name, \
                   description, flags, exits, extra descriptions), the \
                   mob/object prototypes it owns, and its reset rows from \
                   the database, so edits made in the editor take effect \
                   without a restart. Live mobs and items are left alone; \
                   new prototypes apply to future spawns. Defaults to your \
                   current zone. Exit door states of reloaded rooms reset \
                   to their authored default; rooms deleted from the DB \
                   stay until restart.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["reloadallzones"],
        min_role: UserRole::Coder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "reloadallzones",
            summary: "Reload every zone's world data from the DB.",
            long: "Coder+. Same as `reloadzone` applied to every zone: \
                   rooms, prototypes and resets are re-read from the \
                   database; live instances are untouched.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["areload"],
        min_role: UserRole::Coder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "areload",
            summary: "Reload the ability and effect catalogs from the DB.",
            long: "Coder+. Re-reads every ability (with restrictions, \
                   effects, targeting, saves, components, damage \
                   components and messages) and the effect catalog, then \
                   swaps them into the running server.",
        },
        run: cmd_mail_stub,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, args| match head {
            "reloadzone" => Some(Box::pin(cmd_reloadzone(world, player, pool, args))),
            "reloadallzones" => Some(Box::pin(cmd_reloadallzones(world, player, pool, args))),
            "areload" => Some(Box::pin(cmd_areload(world, player, pool, args))),
            _ => None,
        },
    }
}

inventory::submit! {
    Command {
        names: &["savezone"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "savezone [<zone>]",
            summary: "No-op: world data is saved by the editor.",
            long: "Builder+. The C++ server wrote zone files to disk. \
                   Here the database is the source of truth and the \
                   editor writes it, so there is nothing to save; use \
                   `reloadzone` to pull editor changes into the world.",
        },
        run: cmd_savezone,
    }
}

inventory::submit! {
    Command {
        names: &["filewatch"],
        min_role: UserRole::Coder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "filewatch [on|off]",
            summary: "Not applicable: world data is database-backed.",
            long: "Coder+. The C++ server watched zone files for changes. \
                   World data now lives in the database, so there are no \
                   files to watch; use `reloadzone` instead.",
        },
        run: cmd_filewatch,
    }
}

inventory::submit! {
    Command {
        names: &["tlist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "tlist [<zone>]",
            summary: "List the triggers defined in a zone.",
            long: "Builder+. Lists every trigger in the catalog whose id \
                   is in the zone (default: your current zone) with its \
                   attach type, event flags and attachment counts.",
        },
        run: cmd_tlist,
    }
}

inventory::submit! {
    Command {
        names: &["validate_scripts", "vscripts"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "validate_scripts [zone <id>]",
            summary: "Compile-check every loaded Lua trigger.",
            long: "Builder+. Syntax-checks each trigger body in the \
                   catalog without running it and lists the failures. \
                   `validate_scripts zone <id>` limits the pass to one \
                   zone. Also available as GET \
                   /api/admin/triggers/validate.",
        },
        run: cmd_validate_scripts,
    }
}

inventory::submit! {
    Command {
        names: &["dtrig"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "dtrig <list|info|fire> <args>",
            summary: "Debug triggers: list, inspect with errors, or fire.",
            long: "Builder+.\r\n\
                   \x20 dtrig list <target>       - triggers attached to a mob/object\r\n\
                   \x20 dtrig info <zone:id>      - details, fire stats and recent errors\r\n\
                   \x20 dtrig fire <target> <id>  - run a trigger body with <target> as self\r\n\
                   Trigger ids are `zone:id` or a bare id in your current zone.",
        },
        run: cmd_dtrig,
    }
}

inventory::submit! {
    Command {
        names: &["alist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "alist [--type spell|skill|chant|song] [--circle N] [--effect type] [--limit N]",
            summary: "List loaded abilities with optional filters.",
            long: "Builder+. Lists abilities from the ability catalog. \
                   Abilities with no effects are flagged (NO EFFECTS). \
                   Default limit 50.",
        },
        run: cmd_alist,
    }
}

inventory::submit! {
    Command {
        names: &["asearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "asearch <name> | --effect <type> | --no-effects",
            summary: "Search abilities by name, effect type, or find broken ones.",
            long: "Builder+. `asearch fire` matches ability names; \
                   `--effect heal` finds abilities applying an effect of \
                   that type; `--no-effects` finds abilities that would \
                   cast but do nothing.",
        },
        run: cmd_asearch,
    }
}

inventory::submit! {
    Command {
        names: &["aggrodebug"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "aggrodebug [on|off]",
            summary: "Analyse aggression in this room, or toggle aggro logging.",
            long: "Immortal+. With no argument, lists every mob in the \
                   room with its alignment and aggression formula and \
                   reports whether it would attack you. `aggrodebug on` \
                   makes the server log every aggression check (target \
                   `aggression`, level info) until `aggrodebug off`.",
        },
        run: cmd_aggrodebug,
    }
}

inventory::submit! {
    Command {
        names: &["shutdown"],
        min_role: UserRole::Coder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "shutdown <now|cancel|seconds> [reason]",
            summary: "Gracefully shut the server down (saves all players).",
            long: "Coder+. `shutdown now` stops immediately; \
                   `shutdown 60 maintenance` announces and counts down \
                   60 seconds; `shutdown cancel` aborts a pending \
                   countdown. Every online player is saved before the \
                   process exits; the process manager restarts it.",
        },
        run: cmd_shutdown,
    }
}

inventory::submit! {
    Command {
        names: &["uptime"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "uptime",
            summary: "Show how long the server has been running.",
            long: "Time since the server booted.",
        },
        run: cmd_uptime,
    }
}

inventory::submit! {
    Command {
        names: &["date"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "date",
            summary: "Show the real-world server date and the in-game date.",
            long: "Real-world server time (UTC) and the in-game calendar date.",
        },
        run: cmd_date,
    }
}

inventory::submit! {
    Command {
        names: &["users"],
        min_role: UserRole::Immortal,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "users",
            summary: "List connected players with level, room, idle and online time.",
            long: "Immortal+. One row per online player: name, role, \
                   level, current room, idle time and time online. \
                   (Peer addresses are not tracked by the runtime yet.)",
        },
        run: cmd_users,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_dur(secs: u64) -> String {
    format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
}

fn format_short(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Zone of the room the player is standing in.
fn current_zone(world: &World, player: Entity) -> Option<i32> {
    world
        .get::<Located>(player)
        .and_then(|l| world.get::<WorldKey>(l.0))
        .map(|k| k.zone)
}

/// Parse `zone:id`, `zone id`, or a bare `id` (current zone) into a key.
fn parse_trigger_key(world: &World, player: Entity, tokens: &[&str]) -> Option<(i32, i32)> {
    match tokens {
        [one] => {
            if let Some((z, i)) = one.split_once(':') {
                return Some((z.parse().ok()?, i.parse().ok()?));
            }
            Some((current_zone(world, player)?, one.parse().ok()?))
        }
        [z, i] => Some((z.parse().ok()?, i.parse().ok()?)),
        _ => None,
    }
}

/// Send `text` to every online player.
fn broadcast_all(world: &mut World, text: &str) {
    let targets: Vec<Entity> = world
        .query_filtered::<Entity, (With<Player>, With<Online>)>()
        .iter(world)
        .collect();
    for e in targets {
        send_to(world, e, text.to_string());
    }
}

// ---------------------------------------------------------------------------
// reloadzone / reloadallzones / areload
// ---------------------------------------------------------------------------

fn format_reload(label: &str, s: &mud_world::ReloadStats) -> String {
    let mut out = format!(
        "{label} reloaded: {} zone(s) updated, {} added; {} room(s) updated, {} added; \
         {} mob and {} object prototype(s); {} mob and {} object reset(s).\r\n",
        s.zones_updated,
        s.zones_added,
        s.rooms_updated,
        s.rooms_added,
        s.mob_protos,
        s.object_protos,
        s.mob_resets,
        s.object_resets,
    );
    if s.rooms_orphaned > 0 {
        out.push_str(&format!(
            "Note: {} loaded room(s) no longer exist in the database; they stay until restart.\r\n",
            s.rooms_orphaned
        ));
    }
    if s.mob_protos_orphaned + s.object_protos_orphaned > 0 {
        out.push_str(&format!(
            "Note: {} mob and {} object prototype(s) no longer exist in the database; \
             they stay loaded for existing instances (see the log for keys).\r\n",
            s.mob_protos_orphaned, s.object_protos_orphaned
        ));
    }
    out.push_str("Live mobs and items are unchanged; new prototypes apply to future spawns.\r\n");
    out
}

async fn cmd_reloadzone(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    record_admin_action(world, player, "reloadzone", args);
    let arg = args.trim();
    let zone = if arg.is_empty() {
        current_zone(world, player)
    } else {
        arg.parse::<i32>().ok()
    };
    let Some(zone) = zone else {
        send_to(world, player, "Usage: reloadzone [<zone>]\r\n");
        return;
    };
    send_to(
        world,
        player,
        format!("Reloading zone {zone} from the database...\r\n"),
    );
    match mud_world::reload_zones(world, pool, Some(zone)).await {
        Ok(stats) => send_to(
            world,
            player,
            format_reload(&format!("Zone {zone}"), &stats),
        ),
        Err(mud_db::sqlx::Error::RowNotFound) => {
            send_to(world, player, format!("Zone {zone} does not exist.\r\n"));
        }
        Err(e) => send_to(
            world,
            player,
            format!("Failed to reload zone {zone}: {e}\r\n"),
        ),
    }
}

async fn cmd_reloadallzones(
    world: &mut World,
    player: Entity,
    pool: &mud_db::sqlx::PgPool,
    args: &str,
) {
    record_admin_action(world, player, "reloadallzones", args);
    send_to(
        world,
        player,
        "Reloading all zones from the database...\r\n",
    );
    match mud_world::reload_zones(world, pool, None).await {
        Ok(stats) => send_to(world, player, format_reload("All zones", &stats)),
        Err(e) => send_to(
            world,
            player,
            format!("Failed to reload all zones: {e}\r\n"),
        ),
    }
}

/// Replace the live ability and effect catalogs. Split out so the swap is
/// testable without a database.
fn swap_ability_catalogs(world: &mut World, abilities: AbilityCatalog, effects: EffectCatalog) {
    world.insert_resource(abilities);
    world.insert_resource(effects);
}

async fn cmd_areload(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool, args: &str) {
    record_admin_action(world, player, "areload", args);
    send_to(
        world,
        player,
        "Reloading ability cache from the database...\r\n",
    );
    let abilities = match mud_world::load_ability_catalog(pool).await {
        Ok(a) => a,
        Err(e) => {
            send_to(
                world,
                player,
                format!("Failed to reload abilities: {e}\r\n"),
            );
            return;
        }
    };
    let effects = match mud_world::load_effect_catalog(pool).await {
        Ok(e) => e,
        Err(e) => {
            send_to(world, player, format!("Failed to reload effects: {e}\r\n"));
            return;
        }
    };
    let (na, ne) = (abilities.by_name.len(), effects.by_id.len());
    swap_ability_catalogs(world, abilities, effects);
    send_to(
        world,
        player,
        format!("Ability cache reloaded: {na} abilities, {ne} effects.\r\n"),
    );
}

// ---------------------------------------------------------------------------
// savezone / filewatch (DB-backed stubs)
// ---------------------------------------------------------------------------

fn cmd_savezone(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "savezone", args);
    send_to(
        world,
        player,
        "World data is saved by the editor; use reloadzone to pull changes.\r\n",
    );
}

fn cmd_filewatch(world: &mut World, player: Entity, _args: &str) {
    send_to(
        world,
        player,
        "File watching is not applicable: world data is stored in the database. \
         Use reloadzone <zone> after editing in the editor.\r\n",
    );
}

// ---------------------------------------------------------------------------
// tlist / vscripts / dtrig
// ---------------------------------------------------------------------------

fn tlist_text(catalog: &TriggerCatalog, zone: i32) -> String {
    let mut keys: Vec<&(i32, i32)> = catalog.by_key.keys().filter(|k| k.0 == zone).collect();
    keys.sort();
    let mut out = format!("--- Triggers in Zone {zone} ---\r\n");
    if keys.is_empty() {
        out.push_str("No triggers loaded for this zone. Use `treload` to re-read the catalog.\r\n");
        return out;
    }
    for key in &keys {
        let def = &catalog.by_key[*key];
        let attach = match def.attach_type {
            TriggerAttach::Mob => "MOB",
            TriggerAttach::Object => "OBJECT",
            TriggerAttach::World => "WORLD",
        };
        let flags: Vec<String> = def.flags.iter().map(|f| format!("{f:?}")).collect();
        let used = catalog
            .mob_attachments
            .values()
            .chain(catalog.object_attachments.values())
            .chain(catalog.room_attachments.values())
            .filter(|v| v.contains(key))
            .count();
        out.push_str(&format!(
            "  [{}:{}] {} - {attach} [{}] ({used} attachment(s))\r\n",
            key.0,
            key.1,
            def.name,
            flags.join(", ")
        ));
    }
    out.push_str(&format!("Total: {} trigger(s)\r\n", keys.len()));
    out
}

fn cmd_tlist(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let zone = if arg.is_empty() {
        current_zone(world, player)
    } else {
        arg.parse::<i32>().ok()
    };
    let Some(zone) = zone else {
        send_to(world, player, "Usage: tlist [<zone>]\r\n");
        return;
    };
    let text = tlist_text(world.resource::<TriggerCatalog>(), zone);
    send_to(world, player, text);
}

fn validation_text(report: &crate::triggers::ScriptValidation, zone: Option<i32>) -> String {
    let mut out = match zone {
        Some(z) => format!("Validating scripts in zone {z}...\r\n"),
        None => "Validating all scripts...\r\n".to_string(),
    };
    let failed = report.failures.len();
    out.push_str(&format!(
        "Validation results:\r\n  Total:  {}\r\n  Passed: {}\r\n  Failed: {failed}\r\n",
        report.total,
        report.total - failed
    ));
    if failed > 0 {
        out.push_str("Failed triggers:\r\n");
        for f in report.failures.iter().take(20) {
            let first_line = f.error.lines().next().unwrap_or("");
            out.push_str(&format!(
                "  [{}:{}] {} - {first_line}\r\n",
                f.zone_id, f.id, f.name
            ));
        }
        if failed > 20 {
            out.push_str(&format!("  ... and {} more\r\n", failed - 20));
        }
    }
    out
}

fn cmd_validate_scripts(world: &mut World, player: Entity, args: &str) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let zone = match tokens.as_slice() {
        [] => None,
        [kw, z] if kw.eq_ignore_ascii_case("zone") => {
            if let Ok(z) = z.parse::<i32>() {
                Some(z)
            } else {
                send_to(world, player, "Invalid zone id.\r\n");
                return;
            }
        }
        _ => {
            send_to(world, player, "Usage: validate_scripts [zone <id>]\r\n");
            return;
        }
    };
    let report = crate::triggers::validate_catalog(world.resource::<TriggerCatalog>(), zone);
    send_to(world, player, validation_text(&report, zone));
}

fn trigger_info_text(world: &World, key: (i32, i32)) -> String {
    let catalog = world.resource::<TriggerCatalog>();
    let Some(def) = catalog.by_key.get(&key) else {
        return format!("Trigger {}:{} not found.\r\n", key.0, key.1);
    };
    let attach = match def.attach_type {
        TriggerAttach::Mob => "MOB",
        TriggerAttach::Object => "OBJECT",
        TriggerAttach::World => "WORLD",
    };
    let flags: Vec<String> = def.flags.iter().map(|f| format!("{f:?}")).collect();
    let mut out = format!(
        "Trigger {}:{} - {}\r\nType: {attach}  Flags: [{}]\r\n",
        key.0,
        key.1,
        def.name,
        flags.join(", ")
    );
    let preview: String = def.commands.chars().take(200).collect();
    out.push_str("Script:\r\n");
    out.push_str(&preview.replace('\n', "\r\n"));
    if def.commands.chars().count() > 200 {
        out.push_str("...");
    }
    out.push_str("\r\n");
    let syntax = crate::triggers::validate_catalog(catalog, Some(key.0));
    if let Some(f) = syntax
        .failures
        .iter()
        .find(|f| f.zone_id == key.0 && f.id == key.1)
    {
        out.push_str(&format!("Compile error: {}\r\n", f.error));
    }
    let errors: Vec<_> = world
        .get_resource::<ScriptErrorLog>()
        .map(|log| {
            log.entries
                .iter()
                .rev()
                .filter(|e| (e.trigger_zone, e.trigger_id) == key)
                .take(5)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if errors.is_empty() {
        out.push_str("No recent runtime errors.\r\n");
    } else {
        out.push_str("Recent runtime errors:\r\n");
        for e in errors {
            let ago = std::time::SystemTime::now()
                .duration_since(e.at)
                .map_or(0, |d| d.as_secs());
            out.push_str(&format!(
                "  [{}s ago] {}: {}\r\n",
                ago,
                e.event,
                e.message.lines().next().unwrap_or("")
            ));
        }
    }
    if let Some(stats) = world.get_resource::<crate::triggers::TriggerStats>() {
        out.push_str(&format!(
            "Server-wide fires: {} total, {} failed.\r\n",
            stats.total_fired, stats.total_failed
        ));
    }
    out
}

fn cmd_dtrig(world: &mut World, player: Entity, args: &str) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let Some((&sub, rest)) = tokens.split_first() else {
        send_to(
            world,
            player,
            "Debug Trigger Command\r\n\
             Usage:\r\n\
             \x20 dtrig list <target>           - list triggers on a mob/object\r\n\
             \x20 dtrig info <zone:id>          - trigger details and recent errors\r\n\
             \x20 dtrig fire <target> <zone:id> - run a trigger with <target> as self\r\n\
             (A bare id uses your current zone.)\r\n",
        );
        return;
    };
    match sub.to_ascii_lowercase().as_str() {
        "list" => {
            if rest.is_empty() {
                send_to(world, player, "Usage: dtrig list <target>\r\n");
                return;
            }
            super::admin_inspect::cmd_triggers(world, player, &rest.join(" "));
        }
        "info" => {
            let Some(key) = parse_trigger_key(world, player, rest) else {
                send_to(
                    world,
                    player,
                    "Usage: dtrig info <zone:id>  (zone:id, `zone id`, or id in this zone)\r\n",
                );
                return;
            };
            let text = trigger_info_text(world, key);
            send_to(world, player, text);
        }
        "fire" => {
            let Some((target, key_tokens)) = rest.split_first() else {
                send_to(world, player, "Usage: dtrig fire <target> <zone:id>\r\n");
                return;
            };
            let Some((zone, id)) = parse_trigger_key(world, player, key_tokens) else {
                send_to(world, player, "Usage: dtrig fire <target> <zone:id>\r\n");
                return;
            };
            super::admin_inspect::cmd_firetrig(world, player, &format!("{zone} {id} {target}"));
        }
        _ => send_to(
            world,
            player,
            "Unknown dtrig subcommand. Use list, info, or fire.\r\n",
        ),
    }
}

// ---------------------------------------------------------------------------
// alist / asearch
// ---------------------------------------------------------------------------

/// Lower-case effect-type labels of every effect an ability applies.
fn effect_types_of(
    abilities: &AbilityCatalog,
    effects: &EffectCatalog,
    ability_id: i32,
) -> Vec<String> {
    abilities
        .effects_for
        .get(&ability_id)
        .map(|v| {
            v.iter()
                .map(|(eid, _)| {
                    effects.by_id.get(eid).map_or_else(
                        || format!("missing#{eid}"),
                        |d| d.effect_type.to_lowercase(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ability_line(
    abilities: &AbilityCatalog,
    effects: &EffectCatalog,
    slots: Option<&SpellSlotData>,
    def: &mud_world::AbilityDef,
) -> String {
    let mut circles: Vec<i32> = slots
        .map(|s| {
            s.ability_circle
                .iter()
                .filter(|((_, aid), _)| *aid == def.id)
                .map(|(_, c)| *c)
                .collect()
        })
        .unwrap_or_default();
    circles.sort_unstable();
    circles.dedup();
    let circle = if circles.is_empty() {
        String::new()
    } else {
        format!(
            " C{}",
            circles
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("/")
        )
    };
    let types = effect_types_of(abilities, effects, def.id);
    let summary = if types.is_empty() {
        "(NO EFFECTS)".to_string()
    } else {
        let mut shown: Vec<&str> = types.iter().take(3).map(String::as_str).collect();
        if types.len() > 3 {
            shown.push("...");
        }
        shown.join(", ")
    };
    format!(
        "  [{:>4}] {} ({}){circle} - {summary}\r\n",
        def.id,
        def.plain_name,
        def.kind.label()
    )
}

fn sorted_abilities(abilities: &AbilityCatalog) -> Vec<&mud_world::AbilityDef> {
    let mut v: Vec<&mud_world::AbilityDef> = abilities.by_name.values().collect();
    v.sort_by_key(|d| d.id);
    v
}

fn cmd_alist(world: &mut World, player: Entity, args: &str) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut kind: Option<String> = None;
    let mut circle: Option<i32> = None;
    let mut effect: Option<String> = None;
    let mut limit: usize = 50;
    let mut i = 0;
    while i < tokens.len() {
        match (tokens[i], tokens.get(i + 1)) {
            ("--type", Some(v)) => {
                kind = Some(v.to_lowercase());
                i += 2;
            }
            ("--circle", Some(v)) => {
                let Ok(n) = v.parse() else {
                    send_to(world, player, "Invalid circle number.\r\n");
                    return;
                };
                circle = Some(n);
                i += 2;
            }
            ("--effect", Some(v)) => {
                effect = Some(v.to_lowercase());
                i += 2;
            }
            ("--limit", Some(v)) => {
                let Ok(n) = v.parse() else {
                    send_to(world, player, "Invalid limit number.\r\n");
                    return;
                };
                limit = n;
                i += 2;
            }
            _ => {
                send_to(
                    world,
                    player,
                    "Usage: alist [--type spell|skill|chant|song] [--circle N] \
                     [--effect type] [--limit N]\r\n",
                );
                return;
            }
        }
    }
    let abilities = world.resource::<AbilityCatalog>();
    let effects = world.resource::<EffectCatalog>();
    let slots = world.get_resource::<SpellSlotData>();
    let mut out = String::from("--- Abilities ---\r\n");
    let mut shown = 0usize;
    let mut matched = 0usize;
    for def in sorted_abilities(abilities) {
        if kind.as_deref().is_some_and(|k| def.kind.label() != k) {
            continue;
        }
        if let Some(c) = circle
            && !slots.is_some_and(|s| {
                s.ability_circle
                    .iter()
                    .any(|((_, aid), circ)| *aid == def.id && *circ == c)
            })
        {
            continue;
        }
        if let Some(e) = &effect
            && !effect_types_of(abilities, effects, def.id)
                .iter()
                .any(|t| t.contains(e.as_str()))
        {
            continue;
        }
        matched += 1;
        if shown < limit {
            out.push_str(&ability_line(abilities, effects, slots, def));
            shown += 1;
        }
    }
    if matched > shown {
        out.push_str(&format!(
            "... limited to {shown} of {matched} results, use --limit to show more\r\n"
        ));
    }
    out.push_str(&format!(
        "Total: {matched} matching of {} loaded\r\n",
        abilities.by_name.len()
    ));
    send_to(world, player, out);
}

fn cmd_asearch(world: &mut World, player: Entity, args: &str) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() {
        send_to(
            world,
            player,
            "Usage: asearch <partial name>\r\n\
             \x20      asearch --effect <type>\r\n\
             \x20      asearch --no-effects\r\n",
        );
        return;
    }
    let mut name_parts: Vec<&str> = Vec::new();
    let mut effect: Option<String> = None;
    let mut no_effects = false;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "--effect" if i + 1 < tokens.len() => {
                effect = Some(tokens[i + 1].to_lowercase());
                i += 2;
            }
            "--no-effects" => {
                no_effects = true;
                i += 1;
            }
            t if t.starts_with("--") => i += 1,
            t => {
                name_parts.push(t);
                i += 1;
            }
        }
    }
    let needle = name_parts.join(" ").to_lowercase();
    let abilities = world.resource::<AbilityCatalog>();
    let effects = world.resource::<EffectCatalog>();
    let slots = world.get_resource::<SpellSlotData>();
    let mut out = String::from("--- Searching abilities ---\r\n");
    let mut found = 0usize;
    for def in sorted_abilities(abilities) {
        if !needle.is_empty() && !def.plain_name.to_lowercase().contains(&needle) {
            continue;
        }
        let types = effect_types_of(abilities, effects, def.id);
        if let Some(e) = &effect
            && !types.iter().any(|t| t.contains(e.as_str()))
        {
            continue;
        }
        if no_effects && !types.is_empty() {
            continue;
        }
        found += 1;
        if found <= 50 {
            out.push_str(&ability_line(abilities, effects, slots, def));
        }
    }
    if found > 50 {
        out.push_str(&format!("... and {} more\r\n", found - 50));
    }
    out.push_str(&format!("Found {found} abilit(ies)\r\n"));
    send_to(world, player, out);
}

// ---------------------------------------------------------------------------
// aggrodebug
// ---------------------------------------------------------------------------

fn cmd_aggrodebug(world: &mut World, player: Entity, args: &str) {
    match args.trim().to_ascii_lowercase().as_str() {
        "" => {}
        "on" | "off" => {
            let on = args.trim().eq_ignore_ascii_case("on");
            world.insert_resource(crate::aggression::AggroDebug(on));
            tracing::warn!(
                by = %name_of(world, player),
                on,
                "aggression debug logging toggled"
            );
            send_to(
                world,
                player,
                format!(
                    "Aggression debug logging {}.\r\n",
                    if on { "enabled" } else { "disabled" }
                ),
            );
            return;
        }
        _ => {
            send_to(world, player, "Usage: aggrodebug [on|off]\r\n");
            return;
        }
    }
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        send_to(world, player, "You are not in a room.\r\n");
        return;
    };
    let logging = crate::aggression::debug_enabled(world);
    let threshold = aggro_alignment(world);
    let player_align = world.get::<CombatStats>(player).map_or(0, |c| c.alignment);
    let mobs: Vec<(Entity, Option<(i32, i32)>)> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&WorldKey>), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, _)| l.0 == room)
            .map(|(e, _, k)| (e, k.map(|k| (k.zone, k.id))))
            .collect()
    };
    let total_mobs = world.query_filtered::<(), With<Mob>>().iter(world).count();
    let mut out = format!(
        "--- Aggression Debug ---\r\nTotal spawned mobiles: {total_mobs}\r\n\
         Alignment aggro threshold: {threshold}  (your alignment: {player_align})\r\n\
         Debug logging: {}\r\n\
         Current room: {} ({})\r\n\
         Mobs in room ({}):\r\n",
        if logging { "on" } else { "off" },
        name_of(world, room),
        world
            .get::<WorldKey>(room)
            .map_or_else(|| "?".to_string(), |k| format!("{}:{}", k.zone, k.id)),
        mobs.len(),
    );
    let mut any = false;
    for (mob, key) in mobs {
        let align = world.get::<CombatStats>(mob).map_or(0, |c| c.alignment);
        let level = world.get::<Profile>(mob).map_or(0, |p| p.level);
        let formula = key.and_then(|k| {
            world
                .resource::<MobPrototypes>()
                .by_key
                .get(&k)
                .and_then(|p| p.aggression_formula.clone())
        });
        let by_threshold = align <= threshold;
        let by_formula = formula.as_ref().is_some_and(|f| {
            let ctx = crate::aggression::EvalCtx {
                alignment: player_align,
                race_alignment: mud_db::enums::Alignment::from_score(0),
            };
            world
                .resource_mut::<crate::aggression::AggressionFormulaCache>()
                .eval(f, ctx)
        });
        let verdict = if by_threshold {
            "WOULD ATTACK (alignment threshold)"
        } else if by_formula {
            "WOULD ATTACK (formula)"
        } else {
            "passive toward you"
        };
        any |= by_threshold || by_formula;
        out.push_str(&format!(
            "  [MOB] {} (L{level}, align {align}) formula: {} - {verdict}\r\n",
            name_of(world, mob),
            formula.as_deref().unwrap_or("none"),
        ));
    }
    out.push_str(if any {
        "Attack analysis: at least one mob here would engage you on entry.\r\n"
    } else {
        "Attack analysis: no mob here would engage you on entry.\r\n"
    });
    send_to(world, player, out);
}

// ---------------------------------------------------------------------------
// shutdown
// ---------------------------------------------------------------------------

/// Pending / in-progress graceful shutdown. Polled once per tick from the
/// main loop via [`shutdown_poll`]; when it reports `true` the loop breaks
/// and the normal exit path saves every online player before the process
/// ends.
#[derive(Resource, Debug, Default)]
pub struct ShutdownState {
    /// When the countdown expires. `None` = no shutdown scheduled.
    deadline: Option<Instant>,
    reason: String,
    /// Smallest "seconds remaining" threshold already announced.
    last_announced: u64,
    /// Set once the shutdown has fired; the main loop should exit.
    fire: bool,
}

/// Countdown announcement thresholds in seconds remaining.
const SHUTDOWN_ANNOUNCE_AT: &[u64] = &[300, 120, 60, 30, 10, 5, 4, 3, 2, 1];

impl ShutdownState {
    /// Schedule a shutdown `secs` from `now`.
    fn schedule(&mut self, now: Instant, secs: u64, reason: String) {
        self.deadline = Some(now + Duration::from_secs(secs));
        self.reason = reason;
        self.last_announced = secs;
        self.fire = secs == 0;
    }

    fn cancel(&mut self) {
        self.deadline = None;
        self.fire = false;
        self.last_announced = 0;
    }

    /// Advance the countdown. Returns the announcement to broadcast (if
    /// any) and whether the shutdown has fired.
    fn poll(&mut self, now: Instant) -> (Option<String>, bool) {
        if self.fire {
            return (None, true);
        }
        let Some(deadline) = self.deadline else {
            return (None, false);
        };
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            self.fire = true;
            return (Some("SYSTEM: MUD is shutting down NOW!".to_string()), true);
        }
        // Round up so "1s remaining" covers (0, 1].
        let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
        let due = SHUTDOWN_ANNOUNCE_AT
            .iter()
            .rev()
            .copied()
            .find(|t| secs <= *t && *t < self.last_announced);
        if let Some(t) = due {
            self.last_announced = t;
            let reason = if self.reason.is_empty() {
                String::new()
            } else {
                format!(" Reason: {}", self.reason)
            };
            return (
                Some(format!(
                    "SYSTEM: MUD is shutting down in {t} second{}.{reason}",
                    if t == 1 { "" } else { "s" }
                )),
                false,
            );
        }
        (None, false)
    }

    fn seconds_left(&self, now: Instant) -> Option<u64> {
        self.deadline
            .map(|d| d.saturating_duration_since(now).as_secs())
    }
}

/// Per-tick hook for the main loop: broadcast countdown announcements and
/// report whether the server should now stop. Cheap no-op when no shutdown
/// is scheduled.
pub fn shutdown_poll(world: &mut World) -> bool {
    let Some(mut state) = world.get_resource_mut::<ShutdownState>() else {
        return false;
    };
    let (announce, fire) = state.poll(Instant::now());
    if let Some(msg) = announce {
        broadcast_all(world, &format!("\r\n{msg}\r\n"));
    }
    fire
}

fn cmd_shutdown(world: &mut World, player: Entity, args: &str) {
    record_admin_action(world, player, "shutdown", args);
    let now = Instant::now();
    if !world.contains_resource::<ShutdownState>() {
        world.insert_resource(ShutdownState::default());
    }
    let mut parts = args.split_whitespace();
    let Some(first) = parts.next() else {
        let state = world.resource::<ShutdownState>();
        let text = match state.seconds_left(now) {
            Some(left) if !state.fire => format!(
                "Shutdown scheduled in {left} seconds. Reason: {}\r\n",
                state.reason
            ),
            Some(_) => "Shutdown in progress...\r\n".to_string(),
            None => "Usage: shutdown <now|cancel|seconds> [reason]\r\n\
                     \x20 shutdown now     - immediate shutdown\r\n\
                     \x20 shutdown cancel  - cancel a pending shutdown\r\n\
                     \x20 shutdown 60      - shut down in 60 seconds\r\n"
                .to_string(),
        };
        send_to(world, player, text);
        return;
    };
    let first = first.to_ascii_lowercase();
    if first == "cancel" {
        let pending = world
            .resource::<ShutdownState>()
            .deadline
            .is_some_and(|_| !world.resource::<ShutdownState>().fire);
        if !pending {
            send_to(world, player, "No shutdown is currently scheduled.\r\n");
            return;
        }
        world.resource_mut::<ShutdownState>().cancel();
        broadcast_all(world, "\r\nSYSTEM: Shutdown has been cancelled.\r\n");
        tracing::warn!(by = %name_of(world, player), "shutdown cancelled");
        return;
    }
    let secs = if first == "now" {
        0
    } else if let Ok(n) = first.parse::<u64>() {
        n
    } else {
        send_to(
            world,
            player,
            format!(
                "Invalid argument: '{first}'. Use 'now', 'cancel', or a number of seconds.\r\n"
            ),
        );
        return;
    };
    let reason_words: Vec<&str> = parts.collect();
    let reason = if reason_words.is_empty() {
        format!("Initiated by {}", name_of(world, player))
    } else {
        reason_words.join(" ")
    };
    world
        .resource_mut::<ShutdownState>()
        .schedule(now, secs, reason.clone());
    let announce = if secs == 0 {
        "SYSTEM: MUD is shutting down NOW!".to_string()
    } else {
        format!("SYSTEM: MUD is shutting down in {secs} seconds. Reason: {reason}")
    };
    broadcast_all(world, &format!("\r\n{announce}\r\n"));
    tracing::warn!(
        by = %name_of(world, player),
        secs,
        %reason,
        "shutdown initiated"
    );
}

// ---------------------------------------------------------------------------
// uptime / date / users
// ---------------------------------------------------------------------------

fn cmd_uptime(world: &mut World, player: Entity, _args: &str) {
    let secs = world.resource::<ServerStart>().0.elapsed().as_secs();
    let tick = world.resource::<TickCount>().0;
    send_to(
        world,
        player,
        format!(
            "Server uptime: <b:yellow>{}</> (world tick {tick})\r\n",
            format_dur(secs)
        ),
    );
}

fn ordinal(n: i32) -> &'static str {
    match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    }
}

fn cmd_date(world: &mut World, player: Entity, _args: &str) {
    let now = chrono::Utc::now();
    let mut out = format!(
        "Current server time: {}\r\n",
        now.format("%A, %B %d, %Y at %H:%M:%S UTC")
    );
    if let Some(clock) = world.get_resource::<MudClock>() {
        out.push_str(&format!(
            "In-game date: the {}{} day of {}, Year {} ({:02}:{:02}).\r\n",
            clock.day,
            ordinal(clock.day),
            clock.month_name(),
            clock.year,
            clock.hour,
            clock.minute
        ));
    }
    send_to(world, player, out);
}

fn cmd_users(world: &mut World, player: Entity, _args: &str) {
    let now = Instant::now();
    let mut rows: Vec<(String, String, i32, String, u64, u64)> = Vec::new();
    {
        let mut q = world.query_filtered::<(
            &Named,
            Option<&Account>,
            Option<&Profile>,
            Option<&Located>,
            Option<&mud_world::LastInputAt>,
            Option<&LoggedInAt>,
        ), (With<Player>, With<Online>)>();
        let found: Vec<_> = q
            .iter(world)
            .map(|(n, a, p, l, i, t)| {
                (
                    n.name.clone(),
                    a.map_or("Player", |a| a.role.label()).to_string(),
                    p.map_or(0, |p| p.level),
                    l.map(|l| l.0),
                    i.map_or(0, |i| now.duration_since(i.0).as_secs()),
                    t.map_or(0, |t| now.duration_since(t.0).as_secs()),
                )
            })
            .collect();
        for (name, role, level, room, idle, online) in found {
            let room_text = room.map_or_else(
                || "---".to_string(),
                |r| {
                    world.get::<WorldKey>(r).map_or_else(
                        || "---".to_string(),
                        |k| format!("{}:{} {}", k.zone, k.id, name_of(world, r)),
                    )
                },
            );
            rows.push((name, role, level, room_text, idle, online));
        }
    }
    rows.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    let mut out = format!(
        "{:<14} {:<12} {:>3}  {:<28} {:>8} {:>9}\r\n{}\r\n",
        "Name",
        "Role",
        "Lvl",
        "Room",
        "Idle",
        "Online",
        "-".repeat(80)
    );
    for (name, role, level, room, idle, online) in &rows {
        let room_trunc: String = room.chars().take(28).collect();
        out.push_str(&format!(
            "{name:<14} {role:<12} {level:>3}  {room_trunc:<28} {:>8} {:>9}\r\n",
            format_short(*idle),
            format_short(*online)
        ));
    }
    out.push_str(&format!("{} player(s) connected.\r\n", rows.len()));
    send_to(world, player, out);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Connection, dispatch, try_dispatch_async};
    use mud_world::{AbilityDef, EffectDef, PostureKind, TriggerDef, TriggerEvent};

    /// Spawn a player with a capturing outbound channel.
    fn spawn(
        world: &mut World,
        name: &str,
        role: UserRole,
    ) -> (Entity, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        let e = world
            .spawn((
                Player,
                Online,
                Named {
                    name: name.to_string(),
                },
                Account {
                    user_id: "u".into(),
                    character_id: "c".into(),
                    role,
                    perms: vec![],
                },
                mud_world::Posture(PostureKind::Standing),
                Connection(tx),
            ))
            .id();
        (e, rx)
    }

    fn drain(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> String {
        let mut out = String::new();
        while let Ok(b) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&b));
        }
        out
    }

    fn base_world() -> World {
        let mut w = World::new();
        w.insert_resource(ServerStart(Instant::now()));
        w.insert_resource(TickCount(42));
        w.insert_resource(TriggerCatalog::default());
        w.insert_resource(AbilityCatalog::default());
        w.insert_resource(EffectCatalog::default());
        w.insert_resource(MobPrototypes::default());
        w.insert_resource(crate::aggression::AggressionFormulaCache::default());
        w
    }

    fn trig(zone: i32, id: i32, body: &str) -> TriggerDef {
        TriggerDef {
            zone_id: zone,
            id,
            name: format!("trig {zone}:{id}"),
            attach_type: TriggerAttach::Mob,
            commands: body.to_string(),
            flags: vec![TriggerEvent::Greet],
            arg_list: vec![],
            num_args: 0,
        }
    }

    fn ability(id: i32, name: &str, kind: mud_db::abilities::AbilityKind) -> AbilityDef {
        AbilityDef {
            id,
            name: name.to_string(),
            plain_name: name.to_string(),
            description: None,
            kind,
            violent: false,
            combat_ok: true,
            in_combat_only: false,
            cast_time_rounds: 0,
            cooldown_ms: 0,
            is_area: false,
            min_position_label: "STANDING".into(),
            min_posture_rank: 9,
            target_scope: "SINGLE".into(),
            is_magical: true,
            sphere: None,
            damage_type: None,
            memorization_time: 0,
        }
    }

    fn effect(id: i32, ty: &str) -> EffectDef {
        EffectDef {
            id,
            name: format!("effect{id}"),
            description: None,
            effect_type: ty.to_string(),
            tags: vec![],
            presence_override: None,
            default_params: serde_json::Value::Null,
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_remove: None,
            on_tick: None,
        }
    }

    fn ability_world() -> World {
        use mud_db::abilities::AbilityKind;
        let mut w = base_world();
        let mut ab = AbilityCatalog::default();
        for d in [
            ability(1, "fireball", AbilityKind::Spell),
            ability(2, "bash", AbilityKind::Skill),
            ability(3, "broken spell", AbilityKind::Spell),
        ] {
            ab.by_name.insert(d.plain_name.clone(), d);
        }
        ab.effects_for.insert(1, vec![(10, None)]);
        ab.effects_for.insert(2, vec![(11, None)]);
        let mut ef = EffectCatalog::default();
        ef.by_id.insert(10, effect(10, "DAMAGE"));
        ef.by_id.insert(11, effect(11, "STATUS"));
        w.insert_resource(ab);
        w.insert_resource(ef);
        let mut slots = SpellSlotData::default();
        slots.ability_circle.insert((1, 1), 3);
        w.insert_resource(slots);
        w
    }

    // ---- level gating ----

    const NEW_ADMIN_COMMANDS: &[&str] = &[
        "reloadzone 1",
        "reloadallzones",
        "areload",
        "savezone 1",
        "filewatch on",
        "tlist 1",
        "validate_scripts",
        "vscripts",
        "dtrig list x",
        "alist",
        "asearch fire",
        "aggrodebug",
        "shutdown now",
        "users",
    ];

    #[test]
    fn mortal_is_refused_every_sync_admin_command() {
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Mortal", UserRole::Player);
        for cmd in NEW_ADMIN_COMMANDS {
            // Async-dispatched commands are covered by the async test below;
            // the sync dispatcher refuses them via their registry stub too.
            dispatch(&mut world, p, cmd);
            let out = drain(&mut rx);
            assert!(
                out.contains("You can't do that."),
                "`{cmd}` was not refused for a mortal: {out:?}"
            );
        }
        assert!(!world.contains_resource::<ShutdownState>());
    }

    #[tokio::test]
    async fn mortal_is_refused_async_admin_commands() {
        let pool = mud_db::sqlx::PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .expect("lazy pool");
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Mortal", UserRole::Player);
        for cmd in ["reloadzone 1", "reloadallzones", "areload"] {
            let handled = try_dispatch_async(&mut world, p, &pool, cmd).await;
            assert!(handled, "`{cmd}` fell through for a mortal");
            let out = drain(&mut rx);
            assert!(
                out.contains("You can't do that."),
                "`{cmd}` was not refused: {out:?}"
            );
        }
    }

    #[test]
    fn registry_contains_all_new_commands() {
        let names: Vec<&str> = crate::commands::all_commands()
            .flat_map(|c| c.names.iter().copied())
            .collect();
        for n in [
            "reloadzone",
            "reloadallzones",
            "areload",
            "savezone",
            "filewatch",
            "tlist",
            "validate_scripts",
            "vscripts",
            "dtrig",
            "alist",
            "asearch",
            "aggrodebug",
            "shutdown",
            "uptime",
            "date",
            "users",
        ] {
            assert!(names.contains(&n), "{n} not registered");
        }
    }

    // ---- reload (pure part) ----

    #[test]
    fn swap_ability_catalogs_replaces_resources() {
        let mut world = ability_world();
        assert_eq!(world.resource::<AbilityCatalog>().by_name.len(), 3);
        swap_ability_catalogs(
            &mut world,
            AbilityCatalog::default(),
            EffectCatalog::default(),
        );
        assert!(world.resource::<AbilityCatalog>().by_name.is_empty());
        assert!(world.resource::<EffectCatalog>().by_id.is_empty());
    }

    #[test]
    fn format_reload_mentions_orphans_only_when_present() {
        let stats = mud_world::ReloadStats {
            rooms_updated: 5,
            ..Default::default()
        };
        let s = format_reload("Zone 1", &stats);
        assert!(s.contains("5 room(s) updated"));
        assert!(!s.contains("no longer exist"));
        let stats = mud_world::ReloadStats {
            rooms_orphaned: 2,
            ..Default::default()
        };
        assert!(format_reload("Zone 1", &stats).contains("2 loaded room(s) no longer exist"));
    }

    #[test]
    #[ignore = "needs a live Postgres (fierydev) — run with DATABASE_URL set"]
    fn reload_zones_against_live_db() {
        // Exercised manually: `reloadzone <zone>` in-game, or via the
        // mud-world loader against fierydev. The pure merge logic is covered
        // by mud-world's `loader::reload_tests`.
    }

    // ---- savezone / filewatch ----

    #[test]
    fn savezone_is_a_documented_noop() {
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Coder", UserRole::Coder);
        dispatch(&mut world, p, "savezone 30");
        let out = drain(&mut rx);
        assert!(
            out.contains("World data is saved by the editor; use reloadzone"),
            "{out:?}"
        );
    }

    #[test]
    fn filewatch_is_a_stub() {
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Coder", UserRole::Coder);
        dispatch(&mut world, p, "filewatch on");
        assert!(drain(&mut rx).contains("not applicable"));
    }

    // ---- tlist / vscripts / dtrig ----

    #[test]
    fn tlist_lists_zone_triggers_only() {
        let mut world = base_world();
        {
            let mut cat = world.resource_mut::<TriggerCatalog>();
            cat.by_key.insert((30, 2), trig(30, 2, "return"));
            cat.by_key.insert((30, 1), trig(30, 1, "return"));
            cat.by_key.insert((31, 1), trig(31, 1, "return"));
            cat.mob_attachments.insert((30, 5), vec![(30, 1)]);
        }
        let (p, mut rx) = spawn(&mut world, "Builder", UserRole::Builder);
        dispatch(&mut world, p, "tlist 30");
        let out = drain(&mut rx);
        assert!(out.contains("Triggers in Zone 30"), "{out:?}");
        assert!(out.contains("[30:1]") && out.contains("[30:2]"));
        assert!(!out.contains("[31:1]"));
        assert!(out.contains("1 attachment(s)"));
        assert!(out.find("[30:1]") < out.find("[30:2]"), "sorted by id");
        dispatch(&mut world, p, "tlist 99");
        assert!(drain(&mut rx).contains("No triggers loaded"));
    }

    #[test]
    fn vscripts_reports_compile_failures() {
        let mut world = base_world();
        {
            let mut cat = world.resource_mut::<TriggerCatalog>();
            cat.by_key.insert((1, 1), trig(1, 1, "return true"));
            cat.by_key.insert((1, 2), trig(1, 2, "if then"));
        }
        let (p, mut rx) = spawn(&mut world, "Builder", UserRole::Builder);
        dispatch(&mut world, p, "vscripts");
        let out = drain(&mut rx);
        assert!(
            out.contains("Total:  2") && out.contains("Failed: 1"),
            "{out:?}"
        );
        assert!(out.contains("[1:2]"));
        dispatch(&mut world, p, "validate_scripts zone 1");
        assert!(drain(&mut rx).contains("zone 1"));
        dispatch(&mut world, p, "validate_scripts bogus");
        assert!(drain(&mut rx).contains("Usage"));
    }

    #[test]
    fn dtrig_info_shows_details_and_recent_errors() {
        let mut world = base_world();
        world
            .resource_mut::<TriggerCatalog>()
            .by_key
            .insert((5, 3), trig(5, 3, "say('hi')"));
        let mut log = ScriptErrorLog::default();
        log.push(mud_world::ScriptError {
            at: std::time::SystemTime::now(),
            trigger_zone: 5,
            trigger_id: 3,
            trigger_name: "trig 5:3".into(),
            event: "Greet".into(),
            message: "boom\nsecond line".into(),
        });
        world.insert_resource(log);
        let (p, mut rx) = spawn(&mut world, "Builder", UserRole::Builder);
        dispatch(&mut world, p, "dtrig info 5:3");
        let out = drain(&mut rx);
        assert!(out.contains("Trigger 5:3 - trig 5:3"), "{out:?}");
        assert!(out.contains("say('hi')"));
        assert!(out.contains("Greet: boom"));
        assert!(!out.contains("second line"));
        dispatch(&mut world, p, "dtrig info 5 9");
        assert!(drain(&mut rx).contains("not found"));
        dispatch(&mut world, p, "dtrig");
        assert!(drain(&mut rx).contains("Debug Trigger Command"));
        dispatch(&mut world, p, "dtrig nonsense");
        assert!(drain(&mut rx).contains("Unknown dtrig subcommand"));
    }

    // ---- alist / asearch ----

    #[test]
    fn alist_filters_by_type_circle_effect_and_limit() {
        let mut world = ability_world();
        let (p, mut rx) = spawn(&mut world, "Builder", UserRole::Builder);
        dispatch(&mut world, p, "alist");
        let all = drain(&mut rx);
        assert!(all.contains("fireball") && all.contains("bash") && all.contains("broken spell"));
        assert!(all.contains("(NO EFFECTS)"));
        assert!(all.contains("C3"), "circle shown: {all:?}");
        dispatch(&mut world, p, "alist --type skill");
        let skills = drain(&mut rx);
        assert!(skills.contains("bash") && !skills.contains("fireball"));
        dispatch(&mut world, p, "alist --circle 3");
        let c3 = drain(&mut rx);
        assert!(c3.contains("fireball") && !c3.contains("bash"));
        dispatch(&mut world, p, "alist --effect status");
        let st = drain(&mut rx);
        assert!(st.contains("bash") && !st.contains("fireball"));
        dispatch(&mut world, p, "alist --limit 1");
        assert!(drain(&mut rx).contains("limited to 1 of 3"));
        dispatch(&mut world, p, "alist --circle x");
        assert!(drain(&mut rx).contains("Invalid circle"));
    }

    #[test]
    fn asearch_by_name_effect_and_no_effects() {
        let mut world = ability_world();
        let (p, mut rx) = spawn(&mut world, "Builder", UserRole::Builder);
        dispatch(&mut world, p, "asearch fire");
        let r = drain(&mut rx);
        assert!(r.contains("fireball") && !r.contains("bash"));
        dispatch(&mut world, p, "asearch --no-effects");
        let r = drain(&mut rx);
        assert!(r.contains("broken spell") && !r.contains("fireball"));
        dispatch(&mut world, p, "asearch --effect damage");
        let r = drain(&mut rx);
        assert!(r.contains("fireball") && !r.contains("bash"));
        dispatch(&mut world, p, "asearch");
        assert!(drain(&mut rx).contains("Usage: asearch"));
    }

    // ---- aggrodebug ----

    #[test]
    fn aggrodebug_toggles_logging_flag() {
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Imm", UserRole::Immortal);
        assert!(!crate::aggression::debug_enabled(&world));
        dispatch(&mut world, p, "aggrodebug on");
        assert!(crate::aggression::debug_enabled(&world));
        assert!(drain(&mut rx).contains("enabled"));
        dispatch(&mut world, p, "aggrodebug off");
        assert!(!crate::aggression::debug_enabled(&world));
        dispatch(&mut world, p, "aggrodebug sideways");
        assert!(drain(&mut rx).contains("Usage: aggrodebug"));
    }

    #[test]
    fn aggrodebug_room_analysis_lists_mobs() {
        let mut world = base_world();
        let room = world
            .spawn((
                mud_world::Room,
                Named {
                    name: "Hall".into(),
                },
                WorldKey { zone: 1, id: 1 },
            ))
            .id();
        let (p, mut rx) = spawn(&mut world, "Imm", UserRole::Immortal);
        world.entity_mut(p).insert(Located(room));
        world.spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Located(room),
            CombatStats {
                alignment: -900,
                ..Default::default()
            },
        ));
        dispatch(&mut world, p, "aggrodebug");
        let out = drain(&mut rx);
        assert!(out.contains("Aggression Debug"), "{out:?}");
        assert!(out.contains("a goblin"));
        assert!(out.contains("WOULD ATTACK (alignment threshold)"));
    }

    // ---- shutdown ----

    #[test]
    fn shutdown_state_counts_down_and_fires() {
        let t0 = Instant::now();
        let mut s = ShutdownState::default();
        s.schedule(t0, 60, "maint".into());
        assert_eq!(
            s.poll(t0),
            (None, false),
            "no re-announce of the initial 60"
        );
        let (msg, fire) = s.poll(t0 + Duration::from_secs(31));
        assert!(!fire);
        assert!(msg.unwrap().contains("in 30 seconds"));
        assert_eq!(s.poll(t0 + Duration::from_secs(32)), (None, false));
        let (msg, _) = s.poll(t0 + Duration::from_secs(59));
        assert!(msg.unwrap().contains("in 1 second."));
        let (msg, fire) = s.poll(t0 + Duration::from_secs(60));
        assert!(fire);
        assert!(msg.unwrap().contains("NOW"));
        assert_eq!(s.poll(t0 + Duration::from_secs(61)), (None, true));
    }

    #[test]
    fn shutdown_now_fires_on_first_poll_and_cancel_clears() {
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Coder", UserRole::Coder);
        assert!(!shutdown_poll(&mut world), "no state yet means no shutdown");
        dispatch(&mut world, p, "shutdown 600 testing");
        assert!(drain(&mut rx).contains("shutting down in 600 seconds. Reason: testing"));
        assert!(!shutdown_poll(&mut world));
        dispatch(&mut world, p, "shutdown");
        assert!(drain(&mut rx).contains("scheduled in"));
        dispatch(&mut world, p, "shutdown cancel");
        assert!(drain(&mut rx).contains("cancelled"));
        dispatch(&mut world, p, "shutdown cancel");
        assert!(drain(&mut rx).contains("No shutdown is currently scheduled"));
        assert!(!shutdown_poll(&mut world));
        dispatch(&mut world, p, "shutdown now");
        assert!(drain(&mut rx).contains("NOW"));
        assert!(shutdown_poll(&mut world));
        dispatch(&mut world, p, "shutdown garbage");
        assert!(drain(&mut rx).contains("Invalid argument"));
    }

    // ---- uptime / date / users ----

    #[test]
    fn uptime_reports_hms() {
        let mut world = base_world();
        let (p, mut rx) = spawn(&mut world, "Mortal", UserRole::Player);
        dispatch(&mut world, p, "uptime");
        let out = drain(&mut rx);
        assert!(
            out.contains("Server uptime:") && out.contains("0h 0m"),
            "{out:?}"
        );
        assert!(out.contains("world tick 42"));
    }

    #[test]
    fn date_shows_real_and_game_date() {
        let mut world = base_world();
        world.insert_resource(MudClock::default());
        let (p, mut rx) = spawn(&mut world, "Mortal", UserRole::Player);
        dispatch(&mut world, p, "date");
        let out = drain(&mut rx);
        assert!(out.contains("Current server time:"), "{out:?}");
        assert!(out.contains("In-game date:"));
        assert_eq!(ordinal(1), "st");
        assert_eq!(ordinal(12), "th");
        assert_eq!(ordinal(23), "rd");
    }

    #[test]
    fn users_lists_players_for_immortals_only() {
        let mut world = base_world();
        let (imm, mut rx) = spawn(&mut world, "Imm", UserRole::Immortal);
        let (_mortal, _rx2) = spawn(&mut world, "Alice", UserRole::Player);
        dispatch(&mut world, imm, "users");
        let out = drain(&mut rx);
        assert!(out.contains("Alice") && out.contains("Imm"), "{out:?}");
        assert!(out.contains("2 player(s) connected."));
    }
}
