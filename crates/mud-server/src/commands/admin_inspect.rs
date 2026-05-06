//! Admin stat / show / set / scripterror / lua / trigger
//! commands. Both Command records and handler bodies live here.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, MobBehavior, UserRole};
use mud_world::{
    AbilityCatalog, Account, AppliedTo, AttachedTriggers, ClassCatalog, CombatStats,
    EffectInstance, EquippedSlot, ExitData, Exits, Fighting, FromObjectReset, Health, Item,
    Keywords, Located, Mob, MobPrototypes, Named, ObjectPrototypes, Online, Player, PlayerFlags,
    Posture, Profile, RoomSector, ShopCatalog, Stamina, TriggerCatalog, Wealth, WorldKey,
    WorldKeyIndex, ZoneClimate,
};

use crate::TickCount;
use crate::commands::{
    AdminAuditLog, Category, Command, Connection, DbPool, Help, direction_name, direction_rank,
    drain_lua_outbox, find_actor_in_room, find_in_room, matches, name_of, name_or, send_to,
};

inventory::submit! {
    Command {
        names: &["zstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "zstat [<zone_id>]",
            summary: "Dump ECS state of a zone.",
            long: "Builder+. With no arg, inspects the zone you're in. \
                   Prints zone metadata + entity / mob / item counts.",
        },
        run: cmd_zstat,
    }
}

inventory::submit! {
    Command {
        names: &["mstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mstat <zone> <id> | <id> | <name>",
            summary: "Dump a mob PROTOTYPE (template, not a live mob).",
            long: "Builder+. Reads `MobPrototypes` and prints the \
                   proto fields + linked behaviors / professions / \
                   abilities / triggers. Three target forms:\r\n\
                   \x20 mstat <zone> <id>   composite key\r\n\
                   \x20 mstat <id>          id in your current zone\r\n\
                   \x20 mstat <name>        substring against proto name\r\n\
                   \r\n\
                   For a LIVE mob's per-instance state, use `stat <name>` — \
                   stat reads every component on a live entity, mstat \
                   reads the static catalog row that produced it.",
        },
        run: cmd_mstat,
    }
}

inventory::submit! {
    Command {
        names: &["mob-ai", "mai"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mob-ai <zone> <id> | <id> | <name>",
            summary: "Plain-English brief of a mob's behavior.",
            long: "Builder+. Reads the mob proto and `TriggerCatalog` \
                   attachments, then summarises in plain English: \
                   alignment-driven aggression, behaviors (sentinel, \
                   wimpy, etc.), and whether the mob is scripted. For \
                   the raw fields use `mstat`; this is the form you \
                   want when explaining a mob to a designer.",
        },
        run: cmd_mob_ai,
    }
}

inventory::submit! {
    Command {
        names: &["ostat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ostat <zone> <id> | <id> | <name>",
            summary: "Dump an object PROTOTYPE.",
            long: "Builder+. Mirrors `mstat` for objects: type, weight, \
                   wear flags, restrictions, special-values per type. \
                   Same target forms — composite key, id in current \
                   zone, or substring against proto name.",
        },
        run: cmd_ostat,
    }
}

inventory::submit! {
    Command {
        names: &["sstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "sstat <zone> <id> | <id>",
            summary: "Dump a shop's metadata.",
            long: "Builder+. Reads `ShopCatalog` for keeper, accept \
                   rules, items offered, pet roster. <id> alone \
                   defaults to your current zone.",
        },
        run: cmd_sstat,
    }
}

inventory::submit! {
    Command {
        names: &["tstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "tstat <zone> <id> | <id>",
            summary: "Dump a trigger's metadata.",
            long: "Builder+. Reads `TriggerCatalog` and prints flags, \
                   body, fire stats. <id> alone defaults to current \
                   zone.",
        },
        run: cmd_tstat,
    }
}

inventory::submit! {
    Command {
        names: &["astat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "astat <ability>",
            summary: "Dump an ability's metadata.",
            long: "Builder+. Looks up the ability by name (or id) and \
                   shows its school, cost, duration, restrictions, \
                   linked effects.",
        },
        run: cmd_astat,
    }
}

inventory::submit! {
    Command {
        names: &["rstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rstat [<zone> <id> | <id>]",
            summary: "Dump a room's ECS state (live entity, not catalog).",
            long: "Builder+. With no arg, inspects your current room. \
                   <id> alone looks up the room in your current zone. \
                   Composite <zone> <id> works too.",
        },
        run: cmd_rstat,
    }
}

inventory::submit! {
    Command {
        names: &["stat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "stat <name>",
            summary: "Dump a LIVE entity's per-instance component state.",
            long: "Builder+. Reads every component on the named live \
                   entity (player, mob, or item by name in your room) \
                   and prints a structured dump. \
                   \r\nNote the difference from mstat / ostat: stat \
                   shows what the entity actually has right now \
                   (e.g. current HP, active effects, stored vars); \
                   mstat / ostat show the catalog template the \
                   entity was spawned from.",
        },
        run: cmd_stat,
    }
}

inventory::submit! {
    Command {
        names: &["setweather"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "setweather <zone> <kind>",
            summary: "Override a zone's weather.",
            long: "Builder+. Forces a precip kind in the zone's \
                   `WeatherCatalog` entry until the next natural \
                   weather tick rolls a new value.",
        },
        run: cmd_setweather,
    }
}

inventory::submit! {
    Command {
        names: &["set"],
        min_role: UserRole::Implementor,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "set <player> <field> <value>  |  set fields",
            summary: "Mutate a player field directly.",
            long: "Implementor-only. Run `set fields` to list every \
                   writable field with its aliases. `stat <player>` \
                   dumps the full read-only component state — pair \
                   the two when poking at a character.",
        },
        run: cmd_set,
    }
}

inventory::submit! {
    Command {
        names: &["show"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "show <category>",
            summary: "Dump runtime catalogs / counts.",
            long: "Builder+. Categories include `audit`, `effects`, \
                   `weather`, `triggers`, `tickrate`. See in-source \
                   for the full list.",
        },
        run: cmd_show,
    }
}

inventory::submit! {
    Command {
        names: &["scripterrors", "scripterr"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "scripterrors [<n>]",
            summary: "Show recent trigger fire failures.",
            long: "Builder+. Prints the in-memory `ScriptErrorLog` \
                   ring (most-recent first). Default `n=20`.",
        },
        run: cmd_scripterrors,
    }
}

inventory::submit! {
    Command {
        names: &["syslog"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "syslog [<n>]",
            summary: "Show recent server log lines.",
            long: "Builder+. In-memory tail of the tracing log.",
        },
        run: cmd_syslog,
    }
}

inventory::submit! {
    Command {
        names: &["lua"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "lua <code>",
            summary: "Run a snippet of Lua code.",
            long: "Runs `code` with `actor` bound to your character. \
                   Same Lua API surface as triggers.",
        },
        run: cmd_lua,
    }
}

inventory::submit! {
    Command {
        names: &["triggers", "trigs"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "triggers [here | <name> | <zone> <id> | full]",
            summary: "Inspect attached triggers on entities or by id.",
            long: "Builder+. Forms:\r\n\
                   \x20 triggers                — list attachments on \
                       every entity in the current room.\r\n\
                   \x20 triggers here           — same as bare form.\r\n\
                   \x20 triggers <name>         — list attachments on \
                       a specific mob/item by name.\r\n\
                   \x20 triggers <zone> <id>    — look up a trigger \
                       by catalog id (alias of `tstat`).\r\n\
                   \x20 triggers full [<name>]  — include the trigger \
                       body inline alongside each attachment.",
        },
        run: cmd_triggers,
    }
}

inventory::submit! {
    Command {
        names: &["trighistory"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "trighistory [<target>] [<n>]",
            summary: "Show recent trigger fires for an entity.",
            long: "Builder+. Filters the in-memory trigger fire log \
                   to fires whose listener was <target> (`here` / \
                   `me` / `self` / a name in the current room). With \
                   no <target>, shows the last <n> fires across all \
                   entities. <n> defaults to 20, capped at 200.",
        },
        run: cmd_trighistory,
    }
}

inventory::submit! {
    Command {
        names: &["trigattach"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "trigattach <target> <zone> <id>",
            summary: "Bolt a trigger onto a live entity without reloading.",
            long: "Builder+. Mutates `AttachedTriggers` on the target \
                   so a trigger fires for it on the next relevant \
                   event. Doesn't touch the DB — survives only this \
                   session. Use for builder iteration; persist via \
                   muditor when the trigger is ready. <target> is \
                   `here` (room), `me` / `self`, or a name in the \
                   current room.",
        },
        run: cmd_trigattach,
    }
}

inventory::submit! {
    Command {
        names: &["trigdetach"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "trigdetach <target> <zone> <id>",
            summary: "Remove a runtime trigger attachment.",
            long: "Builder+. Inverse of `trigattach`. Removes the \
                   matching (zone, id) entry from the target's \
                   `AttachedTriggers`. Silent no-op if no match.",
        },
        run: cmd_trigdetach,
    }
}

inventory::submit! {
    Command {
        names: &["trace"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "trace <N>",
            summary: "Drill into a captured scripterror entry.",
            long: "Builder+. Looks up entry #N from `scripterrors` \
                   (1 = most recent) and prints the full trigger \
                   body alongside the captured error message — so \
                   the builder doesn't have to chase \
                   (zone, id) → tstat after every failure. The \
                   body shown is the *current* catalog row; if \
                   the trigger was edited after the failure that \
                   text may not match what actually ran.",
        },
        run: cmd_trace,
    }
}

inventory::submit! {
    Command {
        names: &["varset"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "varset <target> <key> <value...>",
            summary: "Set a script variable on an entity (Lua-side state).",
            long: "Builder+. <target> is `here` (current room), `me` \
                   (caller), or a mob/item name in the current room. \
                   Stores into the entity's `ScriptVars` map; reads \
                   back via `varlist`. Lua trigger bodies will (once \
                   the binding lands) see the same map via \
                   `actor:varget(name)`.",
        },
        run: cmd_varset,
    }
}

inventory::submit! {
    Command {
        names: &["varlist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "varlist [<target>]",
            summary: "List script variables on an entity.",
            long: "Builder+. <target> defaults to `here` (current \
                   room). Renders the entity's `ScriptVars` map sorted \
                   by key. Empty maps print a contained 'no script \
                   vars' note.",
        },
        run: cmd_varlist,
    }
}

inventory::submit! {
    Command {
        names: &["varclear", "varunset"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "varclear <target> [<key>]",
            summary: "Clear one script variable, or wipe the whole map.",
            long: "Builder+. With <key>, removes that single entry. \
                   Without, wipes every variable on the target. \
                   <target> resolves the same way as `varset` / \
                   `varlist`.",
        },
        run: cmd_varclear,
    }
}

inventory::submit! {
    Command {
        names: &["firetrig"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "firetrig <zone> <id> [<actor>]",
            summary: "Manually fire a trigger by id.",
            long: "Builder+. Useful for testing trigger bodies. The \
                   `actor` defaults to the caster.",
        },
        run: cmd_firetrig,
    }
}


// ---- handler bodies ----

pub(crate) fn cmd_lua(world: &mut World, player: Entity, args: &str) {
    let code = args.trim();
    if code.is_empty() {
        send_to(world, player, "Usage: lua <code>\r\n");
        return;
    }
    // Take the LuaHost out of the world temporarily so we can borrow
    // both &LuaHost and &mut World at once.
    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_actor(world, player, code)
    });
    drain_lua_outbox(world);
    match result {
        Ok(out) => {
            if out.is_empty() {
                send_to(world, player, "(no output)\r\n");
            } else {
                send_to(world, player, out);
            }
        }
        Err(e) => {
            send_to(world, player, format!("{e}\r\n"));
        }
    }
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_triggers(world: &mut World, player: Entity, args: &str) {
    use mud_world::TriggerEvent;

    let arg = args.trim();
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        send_to(world, player, "You're nowhere.\r\n");
        return;
    };

    // Catalog form: `triggers <zone> <id>`. Pure two-int args
    // delegate to tstat so builders can poke a trigger by id
    // without remembering a separate command.
    let parts: Vec<&str> = arg.split_whitespace().collect();
    if parts.len() == 2
        && parts[0].parse::<i32>().is_ok()
        && parts[1].parse::<i32>().is_ok()
    {
        cmd_tstat(world, player, arg);
        return;
    }

    // `triggers full [<name>|here]` — same listing as the default,
    // but also dumps the trigger body inline so the builder doesn't
    // have to chase down (zone, id) → tstat for each one.
    let (verbose, body_arg) = if let Some(rest) = arg
        .strip_prefix("full ")
        .or_else(|| if arg.eq_ignore_ascii_case("full") { Some("") } else { None })
    {
        (true, rest.trim())
    } else {
        (false, arg)
    };

    // Targets: room itself + every mob/item/player whose Located == room,
    // unless the user named a specific keyword.
    let mut targets: Vec<Entity> = Vec::new();
    if body_arg.is_empty() || body_arg.eq_ignore_ascii_case("here") {
        targets.push(room);
        let mut q = world.query::<(Entity, &Located)>();
        for (e, l) in q.iter(world) {
            if l.0 == room {
                targets.push(e);
            }
        }
    } else if let Some(e) = find_in_room(world, body_arg, room)
        .or_else(|| find_actor_in_room(world, body_arg, room, player))
    {
        targets.push(e);
    } else {
        send_to(world, player, format!("No '{body_arg}' here.\r\n"));
        return;
    }

    let render_event = |ev: &TriggerEvent| match ev {
        TriggerEvent::Global => "GLOBAL",
        TriggerEvent::Random => "RANDOM",
        TriggerEvent::Command => "COMMAND",
        TriggerEvent::Load => "LOAD",
        TriggerEvent::Cast => "CAST",
        TriggerEvent::Leave => "LEAVE",
        TriggerEvent::Time => "TIME",
        TriggerEvent::Speech => "SPEECH",
        TriggerEvent::Act => "ACT",
        TriggerEvent::Death => "DEATH",
        TriggerEvent::Greet => "GREET",
        TriggerEvent::GreetAll => "GREET_ALL",
        TriggerEvent::Entry => "ENTRY",
        TriggerEvent::Receive => "RECEIVE",
        TriggerEvent::Fight => "FIGHT",
        TriggerEvent::HitPercent => "HIT_PERCENT",
        TriggerEvent::Bribe => "BRIBE",
        TriggerEvent::Memory => "MEMORY",
        TriggerEvent::Door => "DOOR",
        TriggerEvent::SpeechTo => "SPEECH_TO",
        TriggerEvent::Look => "LOOK",
        TriggerEvent::Auto => "AUTO",
        TriggerEvent::Attack => "ATTACK",
        TriggerEvent::Defend => "DEFEND",
        TriggerEvent::Timer => "TIMER",
        TriggerEvent::Get => "GET",
        TriggerEvent::Drop => "DROP",
        TriggerEvent::Give => "GIVE",
        TriggerEvent::Wear => "WEAR",
        TriggerEvent::Remove => "REMOVE",
        TriggerEvent::Use => "USE",
        TriggerEvent::Consume => "CONSUME",
        TriggerEvent::Reset => "RESET",
        TriggerEvent::Preentry => "PREENTRY",
        TriggerEvent::Postentry => "POSTENTRY",
    };

    let mut out = String::new();
    let mut total = 0usize;
    for &e in &targets {
        let Some(at) = world.get::<AttachedTriggers>(e) else {
            continue;
        };
        if at.0.is_empty() {
            continue;
        }
        let label = world.get::<Named>(e).map_or("(unnamed)", |n| n.name.as_str());
        let kind = if e == room {
            "room"
        } else if world.get::<Mob>(e).is_some() {
            "mob"
        } else if world.get::<Item>(e).is_some() {
            "item"
        } else if world.get::<Player>(e).is_some() {
            "player"
        } else {
            "entity"
        };
        out.push_str(&format!("{label} [{kind}]:\r\n"));
        let keys = at.0.clone();
        let catalog = world.resource::<TriggerCatalog>();
        for (zone, id) in keys {
            total += 1;
            if let Some(def) = catalog.by_key.get(&(zone, id)) {
                let flags: Vec<&'static str> = def.flags.iter().map(render_event).collect();
                let flag_str = if flags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", flags.join(" "))
                };
                out.push_str(&format!("  ({zone}, {id}) {}{flag_str}\r\n", def.name));
                if verbose {
                    // Indent the body two extra spaces for readability
                    // and prefix each line so a long body stays
                    // visually associated with its attachment.
                    for line in def.commands.lines() {
                        out.push_str("      | ");
                        out.push_str(line);
                        out.push_str("\r\n");
                    }
                }
            } else {
                out.push_str(&format!("  ({zone}, {id}) <missing>\r\n"));
            }
        }
    }

    if total == 0 {
        send_to(world, player, "No triggers attached.\r\n");
    } else {
        out.push_str(&format!("{total} trigger(s).\r\n"));
        send_to(world, player, out);
    }
}
pub(crate) fn cmd_firetrig(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        send_to(world, player, "Usage: firetrig <zone> <id> [<keyword>]\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return;
    };
    let body = world
        .resource::<TriggerCatalog>()
        .by_key
        .get(&(zone, id))
        .map(|d| d.commands.clone());
    let Some(code) = body else {
        send_to(world, player, format!("No trigger ({zone}, {id}) in catalog.\r\n"));
        return;
    };

    let actor = if parts.len() >= 3 {
        let needle = parts[2..].join(" ");
        let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        let Some(target) = find_in_room(world, &needle, room)
            .or_else(|| find_actor_in_room(world, &needle, room, player))
        else {
            send_to(world, player, format!("No '{needle}' here.\r\n"));
            return;
        };
        target
    } else {
        player
    };

    let result = world.resource_scope::<mud_script::LuaHost, _>(|world, mut host| {
        host.exec_for_actor(world, actor, &code)
    });
    drain_lua_outbox(world);
    match result {
        Ok(out) => {
            if out.is_empty() {
                send_to(world, player, "(trigger ran, no output)\r\n");
            } else {
                send_to(world, player, out);
            }
        }
        Err(e) => send_to(world, player, format!("{e}\r\n")),
    }
}

/// Resolve the `<target>` argument used by varset / varlist /
/// varclear. `here` (or empty) → the current room; `me` / `self` →
/// the caller; anything else → an actor-or-item lookup in the
/// current room. Returns `Some(entity)` on hit; `None` (after
/// sending an error) when the lookup failed.
fn resolve_var_target(world: &mut World, player: Entity, arg: &str) -> Option<Entity> {
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        send_to(world, player, "You're nowhere.\r\n");
        return None;
    };
    let arg = arg.trim();
    if arg.is_empty() || arg.eq_ignore_ascii_case("here") {
        return Some(room);
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        return Some(player);
    }
    if let Some(e) = find_in_room(world, arg, room)
        .or_else(|| find_actor_in_room(world, arg, room, player))
    {
        return Some(e);
    }
    send_to(world, player, format!("No '{arg}' here.\r\n"));
    None
}

pub(crate) fn cmd_varset(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    let mut parts = trimmed.splitn(3, char::is_whitespace);
    let Some(target_word) = parts.next().filter(|s| !s.is_empty()) else {
        send_to(
            world,
            player,
            "Usage: varset <target> <key> <value...>\r\n",
        );
        return;
    };
    let Some(key) = parts.next().filter(|s| !s.is_empty()) else {
        send_to(world, player, "varset needs a <key>.\r\n");
        return;
    };
    let value = parts.next().unwrap_or("").trim().to_string();
    let Some(target) = resolve_var_target(world, player, target_word) else {
        return;
    };
    // Insert (or upsert) the entry. Inserting the component
    // first when missing keeps the call site one-branch.
    if world.get::<mud_world::ScriptVars>(target).is_none()
        && let Ok(mut em) = world.get_entity_mut(target)
    {
        em.insert(mud_world::ScriptVars::default());
    }
    if let Some(mut vars) = world.get_mut::<mud_world::ScriptVars>(target) {
        vars.0.insert(key.to_string(), value.clone());
    }
    let target_name = name_of(world, target);
    send_to(
        world,
        player,
        format!("Set {key}={value:?} on {target_name}.\r\n"),
    );
}

pub(crate) fn cmd_varlist(world: &mut World, player: Entity, args: &str) {
    let Some(target) = resolve_var_target(world, player, args) else {
        return;
    };
    let target_name = name_of(world, target);
    let entries: Vec<(String, String)> = world
        .get::<mud_world::ScriptVars>(target)
        .map(|v| {
            v.0.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        send_to(
            world,
            player,
            format!("{target_name} has no script vars.\r\n"),
        );
        return;
    }
    let key_width = entries.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = format!("\r\n<b:cyan>{target_name}</> script vars:\r\n");
    for (k, v) in entries {
        out.push_str(&format!("  {k:<key_width$} = {v}\r\n"));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_varclear(world: &mut World, player: Entity, args: &str) {
    let trimmed = args.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let Some(target_word) = parts.next().filter(|s| !s.is_empty()) else {
        send_to(world, player, "Usage: varclear <target> [<key>]\r\n");
        return;
    };
    let key = parts.next().map(str::trim).filter(|s| !s.is_empty());
    let Some(target) = resolve_var_target(world, player, target_word) else {
        return;
    };
    let target_name = name_of(world, target);
    if let Some(key) = key {
        let removed = world
            .get_mut::<mud_world::ScriptVars>(target)
            .is_some_and(|mut v| v.0.remove(key).is_some());
        if removed {
            send_to(
                world,
                player,
                format!("Cleared {key} on {target_name}.\r\n"),
            );
        } else {
            send_to(
                world,
                player,
                format!("{target_name} has no var named {key}.\r\n"),
            );
        }
    } else {
        let count = world
            .get::<mud_world::ScriptVars>(target)
            .map_or(0, |v| v.0.len());
        if let Some(mut v) = world.get_mut::<mud_world::ScriptVars>(target) {
            v.0.clear();
        }
        if count == 0 {
            send_to(
                world,
                player,
                format!("{target_name} had no script vars to clear.\r\n"),
            );
        } else {
            let suffix = if count == 1 { "" } else { "s" };
            send_to(
                world,
                player,
                format!("Cleared {count} script var{suffix} on {target_name}.\r\n"),
            );
        }
    }
}

/// Parse `<target> <zone> <id>` for the trigattach / trigdetach
/// commands. Returns `Some((target_entity, zone, id))` on a clean
/// parse, or `None` after sending a usage message. Validates the
/// trigger exists in the catalog so an attach to a missing id
/// gets caught up front.
fn parse_trigattach_args(
    world: &mut World,
    player: Entity,
    args: &str,
    verb: &str,
) -> Option<(Entity, i32, i32)> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 3 {
        send_to(
            world,
            player,
            format!("Usage: {verb} <target> <zone> <id>\r\n"),
        );
        return None;
    }
    let (Ok(zone), Ok(id)) = (parts[1].parse::<i32>(), parts[2].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
        return None;
    };
    if !world
        .resource::<mud_world::TriggerCatalog>()
        .by_key
        .contains_key(&(zone, id))
    {
        send_to(
            world,
            player,
            format!("No trigger ({zone}, {id}) in the catalog.\r\n"),
        );
        return None;
    }
    let target = resolve_var_target(world, player, parts[0])?;
    Some((target, zone, id))
}

pub(crate) fn cmd_trigattach(world: &mut World, player: Entity, args: &str) {
    let Some((target, zone, id)) = parse_trigattach_args(world, player, args, "trigattach") else {
        return;
    };
    // Insert the AttachedTriggers component if the target has none
    // yet, then push the (zone, id) pair. Duplicates are harmless
    // (the dispatcher iterates the list) but read awkwardly in
    // `triggers <name>` output, so we de-dup.
    if world.get::<AttachedTriggers>(target).is_none()
        && let Ok(mut em) = world.get_entity_mut(target)
    {
        em.insert(AttachedTriggers::default());
    }
    let mut already_present = false;
    if let Some(mut at) = world.get_mut::<AttachedTriggers>(target) {
        if at.0.iter().any(|(z, i)| *z == zone && *i == id) {
            already_present = true;
        } else {
            at.0.push((zone, id));
        }
    }
    let target_name = name_of(world, target);
    let trig_name = world
        .resource::<mud_world::TriggerCatalog>()
        .by_key
        .get(&(zone, id))
        .map_or_else(|| String::from("?"), |d| d.name.clone());
    if already_present {
        send_to(
            world,
            player,
            format!("({zone}, {id}) {trig_name} was already attached to {target_name}.\r\n"),
        );
    } else {
        send_to(
            world,
            player,
            format!("Attached ({zone}, {id}) {trig_name} to {target_name}.\r\n"),
        );
    }
}

pub(crate) fn cmd_trighistory(world: &mut World, player: Entity, args: &str) {
    use mud_world::TriggerHistoryLog;
    let parts: Vec<&str> = args.split_whitespace().collect();
    // Parse args: any trailing integer is the limit; everything
    // before it is the target word(s). Bare `trighistory` → most
    // recent 20 fires across all entities.
    let (target_words, limit): (Vec<&str>, usize) = if let Some(last) = parts.last()
        && let Ok(n) = last.parse::<usize>()
    {
        let head: Vec<&str> = parts[..parts.len() - 1].to_vec();
        (head, n.clamp(1, 200))
    } else {
        (parts.clone(), 20)
    };
    let target: Option<Entity> = if target_words.is_empty() {
        None
    } else {
        let combined = target_words.join(" ");
        let Some(t) = resolve_var_target(world, player, &combined) else {
            return;
        };
        Some(t)
    };

    if !world.contains_resource::<TriggerHistoryLog>() {
        send_to(world, player, "No trigger fires recorded yet.\r\n");
        return;
    }
    let entries: Vec<mud_world::TriggerHistoryEntry> = {
        let log = world.resource::<TriggerHistoryLog>();
        log.entries
            .iter()
            .rev()
            .filter(|e| target.is_none_or(|t| e.listener == t))
            .take(limit)
            .cloned()
            .collect()
    };
    if entries.is_empty() {
        if let Some(t) = target {
            let n = name_of(world, t);
            send_to(
                world,
                player,
                format!("No recorded fires for {n}.\r\n"),
            );
        } else {
            send_to(world, player, "No trigger fires recorded yet.\r\n");
        }
        return;
    }
    let header_target = target.map_or_else(
        || String::from("(all entities)"),
        |t| name_of(world, t),
    );
    let mut out = format!(
        "\r\n<b:cyan>Last {} trigger fire(s) for {header_target}:</>\r\n",
        entries.len(),
    );
    let now = std::time::SystemTime::now();
    for e in &entries {
        let secs_ago = now.duration_since(e.at).map(|d| d.as_secs()).unwrap_or(0);
        let listener_name = if target.is_some() {
            String::new() // already in header
        } else {
            format!("  on {}", name_of(world, e.listener))
        };
        let status = if e.ok { "<green>ok</>" } else { "<red>err</>" };
        out.push_str(&format!(
            "  <dim>tick {tick:>6} | {ago:>5}s ago</>  ({zone}, {id}) [{event}] {status}{listener_name}\r\n",
            tick = e.tick,
            ago = secs_ago,
            zone = e.trigger_zone,
            id = e.trigger_id,
            event = e.event,
        ));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_trigdetach(world: &mut World, player: Entity, args: &str) {
    let Some((target, zone, id)) = parse_trigattach_args(world, player, args, "trigdetach") else {
        return;
    };
    let target_name = name_of(world, target);
    let removed = world
        .get_mut::<AttachedTriggers>(target)
        .is_some_and(|mut at| {
            let before = at.0.len();
            at.0.retain(|(z, i)| !(*z == zone && *i == id));
            before != at.0.len()
        });
    if removed {
        send_to(
            world,
            player,
            format!("Detached ({zone}, {id}) from {target_name}.\r\n"),
        );
    } else {
        send_to(
            world,
            player,
            format!("({zone}, {id}) wasn't attached to {target_name}.\r\n"),
        );
    }
}

pub(crate) fn cmd_zstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let zone_id = if parts.is_empty() {
        // Resolve via player's room WorldKey.
        let Some(zone) = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0))
            .map(|wk| wk.zone)
        else {
            send_to(world, player, "Can't find your zone.\r\n");
            return;
        };
        zone
    } else if parts.len() == 1 {
        let Ok(id) = parts[0].parse::<i32>() else {
            send_to(world, player, "Usage: zstat [<zone_id>]\r\n");
            return;
        };
        id
    } else {
        send_to(world, player, "Usage: zstat [<zone_id>]\r\n");
        return;
    };

    let Some(zone_entity) = world
        .resource::<WorldKeyIndex>()
        .zones
        .get(&zone_id)
        .copied()
    else {
        send_to(world, player, format!("No zone {zone_id} loaded.\r\n"));
        return;
    };
    let zone_name = name_of(world, zone_entity);
    let room_count = world
        .query_filtered::<&Located, With<mud_world::Room>>()
        .iter(world)
        .filter(|l| l.0 == zone_entity)
        .count();
    let mob_proto_count = world
        .resource::<MobPrototypes>()
        .by_key
        .keys()
        .filter(|(z, _)| *z == zone_id)
        .count();
    let obj_proto_count = world
        .resource::<ObjectPrototypes>()
        .by_key
        .keys()
        .filter(|(z, _)| *z == zone_id)
        .count();
    let live_mobs = world
        .query_filtered::<&WorldKey, With<Mob>>()
        .iter(world)
        .filter(|wk| wk.zone == zone_id)
        .count();
    let live_items = world
        .query_filtered::<&WorldKey, With<Item>>()
        .iter(world)
        .filter(|wk| wk.zone == zone_id)
        .count();

    let mut out = String::from("\r\n");
    out.push_str(&format!("entity:        {zone_entity:?}\r\n"));
    out.push_str(&format!("name:          {zone_name}\r\n"));
    out.push_str(&format!("zone_id:       {zone_id}\r\n"));
    out.push_str(&format!("rooms:         {room_count}\r\n"));
    out.push_str(&format!("mob_protos:    {mob_proto_count}\r\n"));
    out.push_str(&format!("obj_protos:    {obj_proto_count}\r\n"));
    out.push_str(&format!("live_mobs:     {live_mobs}\r\n"));
    out.push_str(&format!("live_items:    {live_items}\r\n"));
    send_to(world, player, out);
}
/// Resolve `<zone> <id>` / `<id>` / `<name>` into a `(zone, id)`
/// tuple. `<id>` alone defaults zone to the caller's current
/// zone. Names match against `name` first, then keywords —
/// case-insensitive substring. Sends an error and returns `None`
/// when the lookup fails.
fn resolve_mob_target(
    world: &mut World,
    player: Entity,
    args: &str,
) -> Option<(i32, i32)> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        send_to(world, player, "Usage: <zone> <id> | <id> | <name>\r\n");
        return None;
    }
    if parts.len() == 2
        && let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>())
    {
        return Some((zone, id));
    }
    if parts.len() == 1
        && let Ok(id) = parts[0].parse::<i32>()
    {
        let here_zone = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0).map(|k| k.zone));
        let Some(zone) = here_zone else {
            send_to(world, player, "Can't resolve current zone.\r\n");
            return None;
        };
        return Some((zone, id));
    }
    // Name lookup — case-insensitive substring against name + keywords.
    let needle = parts.join(" ").to_ascii_lowercase();
    let hit = world
        .resource::<MobPrototypes>()
        .by_key
        .iter()
        .find(|(_, p)| {
            p.name.to_ascii_lowercase().contains(&needle)
                || p.keywords.iter().any(|k| k.to_ascii_lowercase().contains(&needle))
        })
        .map(|(k, _)| *k);
    if hit.is_none() {
        send_to(
            world,
            player,
            format!("No mob proto matches '{}'.\r\n", parts.join(" ")),
        );
    }
    hit
}

fn resolve_object_target(
    world: &mut World,
    player: Entity,
    args: &str,
) -> Option<(i32, i32)> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        send_to(world, player, "Usage: <zone> <id> | <id> | <name>\r\n");
        return None;
    }
    if parts.len() == 2
        && let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>())
    {
        return Some((zone, id));
    }
    if parts.len() == 1
        && let Ok(id) = parts[0].parse::<i32>()
    {
        let here_zone = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0).map(|k| k.zone));
        let Some(zone) = here_zone else {
            send_to(world, player, "Can't resolve current zone.\r\n");
            return None;
        };
        return Some((zone, id));
    }
    let needle = parts.join(" ").to_ascii_lowercase();
    let hit = world
        .resource::<ObjectPrototypes>()
        .by_key
        .iter()
        .find(|(_, p)| {
            p.name.to_ascii_lowercase().contains(&needle)
                || p.keywords.iter().any(|k| k.to_ascii_lowercase().contains(&needle))
        })
        .map(|(k, _)| *k);
    if hit.is_none() {
        send_to(
            world,
            player,
            format!("No object proto matches '{}'.\r\n", parts.join(" ")),
        );
    }
    hit
}

/// Try `<zone> <id>` / `<id>` parsing only. Used by sstat / tstat
/// where there's no name-search fallback (shop names aren't in
/// the proto, trigger names live in the catalog body).
fn resolve_zone_id(world: &mut World, player: Entity, args: &str) -> Option<(i32, i32)> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() == 2
        && let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>())
    {
        return Some((zone, id));
    }
    if parts.len() == 1
        && let Ok(id) = parts[0].parse::<i32>()
    {
        let here_zone = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0).map(|k| k.zone));
        let Some(zone) = here_zone else {
            send_to(world, player, "Can't resolve current zone.\r\n");
            return None;
        };
        return Some((zone, id));
    }
    send_to(world, player, "Usage: <zone> <id> | <id>\r\n");
    None
}

pub(crate) fn cmd_mstat(world: &mut World, player: Entity, args: &str) {
    let Some((zone, id)) = resolve_mob_target(world, player, args) else {
        return;
    };
    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(p) = proto else {
        send_to(world, player, format!("No mob proto ({zone}, {id}).\r\n"));
        return;
    };
    let live = world
        .query_filtered::<&WorldKey, With<Mob>>()
        .iter(world)
        .filter(|wk| wk.zone == zone && wk.id == id)
        .count();
    let trig_count = world
        .resource::<mud_world::TriggerCatalog>()
        .mob_attachments
        .get(&(zone, id))
        .map_or(0, Vec::len);

    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!("name:          {}\r\n", p.name));
    out.push_str(&format!("keywords:      {}\r\n", p.keywords.join(", ")));
    out.push_str(&format!("room_desc:     {}\r\n", p.room_description));
    if !p.examine_description.trim().is_empty() {
        // Examine text can be multi-line; render as a labelled
        // block followed by an indented body for readability.
        out.push_str("examine_desc:\r\n");
        for line in p.examine_description.lines() {
            out.push_str(&format!("  {line}\r\n"));
        }
    }
    out.push_str(&format!("level:         {}\r\n", p.level));
    let align_label = mud_db::enums::Alignment::from_score(p.alignment).label();
    out.push_str(&format!(
        "alignment:     {} ({align_label})\r\n",
        p.alignment
    ));
    out.push_str(&format!("role:          {}\r\n", p.role.label()));
    out.push_str(&format!("race:          {}\r\n", p.race));
    out.push_str(&format!("gender:        {}\r\n", p.gender));
    out.push_str(&format!(
        "hp dice:       {}d{}+{}\r\n",
        p.hp_dice_num, p.hp_dice_size, p.hp_dice_bonus
    ));
    out.push_str(&format!(
        "damage dice:   {}d{}+{}\r\n",
        p.damage_dice_num, p.damage_dice_size, p.damage_dice_bonus
    ));
    out.push_str(&format!("hit_roll:      {}\r\n", p.hit_roll));
    out.push_str(&format!("armor_class:   {}\r\n", p.armor_class));
    out.push_str(&format!("wealth:        {} cp\r\n", p.wealth));
    // class_id → resolved class name when the catalog has a row;
    // raw `Some(N)` is unhelpful to a human reader.
    let class_label = p.class_id.map_or_else(
        || String::from("none"),
        |id| {
            world
                .get_resource::<mud_world::ClassCatalog>()
                .and_then(|c| c.by_id.get(&id).map(|d| d.plain_name.clone()))
                .map_or_else(|| format!("id {id} (unknown)"), |name| format!("{name} (id {id})"))
        },
    );
    out.push_str(&format!("class:         {class_label}\r\n"));
    out.push_str(&format!("triggers:      {trig_count}\r\n"));
    out.push_str(&format!("live count:    {live}\r\n"));
    send_to(world, player, out);
}

pub(crate) fn cmd_mob_ai(world: &mut World, player: Entity, args: &str) {
    let Some((zone, id)) = resolve_mob_target(world, player, args) else {
        return;
    };
    let proto = world
        .resource::<MobPrototypes>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(p) = proto else {
        send_to(world, player, format!("No mob proto ({zone}, {id}).\r\n"));
        return;
    };
    let trig_count = world
        .resource::<mud_world::TriggerCatalog>()
        .mob_attachments
        .get(&(zone, id))
        .map_or(0, Vec::len);

    let mut out = format!("\r\n=== AI brief: {} ({zone}, {id}) ===\r\n", p.name);

    // Alignment bucket — drives evil/good aggression heuristics
    // even when no explicit "AggroEvil" behavior flag exists.
    let align_bucket = mud_db::enums::Alignment::from_score(p.alignment);
    out.push_str(&format!(
        "Alignment:     {} ({})\r\n",
        p.alignment,
        align_bucket.label(),
    ));

    // Combat readout in plain English. dice → "rolls 2d6+3"; HR/AC
    // expanded so a designer doesn't need to remember which is the
    // attack stat and which is the defense.
    out.push_str(&format!(
        "Combat:        L{} — rolls {}d{}{} damage, hit roll {:+}, AC {}\r\n",
        p.level,
        p.damage_dice_num,
        p.damage_dice_size,
        if p.damage_dice_bonus == 0 {
            String::new()
        } else {
            format!("{:+}", p.damage_dice_bonus)
        },
        p.hit_roll,
        p.armor_class,
    ));

    // Class-driven AI: a mob with a class id and without NoClassAi
    // runs the class kit (warrior swings, mage casts) at runtime.
    let no_class_ai = p.behaviors.iter().any(|b| matches!(b, MobBehavior::NoClassAi));
    let class_label = p.class_id.and_then(|cid| {
        world
            .get_resource::<mud_world::ClassCatalog>()
            .and_then(|c| c.by_id.get(&cid).map(|d| d.plain_name.clone()))
    });
    match (class_label, no_class_ai) {
        (Some(cls), false) => out.push_str(&format!(
            "Class AI:      Runs the {cls} combat kit (basic attacks + class abilities).\r\n",
        )),
        (Some(cls), true) => out.push_str(&format!(
            "Class AI:      {cls} class set, but No-ClassAI flag is on — manual / scripted only.\r\n",
        )),
        (None, _) => out.push_str("Class AI:      No class — basic melee swings only.\r\n"),
    }

    // Behavior list. Empty case is worth calling out — a designer
    // checking a mob deserves a clear "this is intentional" beat.
    if p.behaviors.is_empty() {
        out.push_str("Behaviors:     <none> — vanilla wandering NPC.\r\n");
    } else {
        out.push_str("Behaviors:\r\n");
        for b in &p.behaviors {
            out.push_str(&format!("  {:<14} {}\r\n", b.label(), b.describe()));
        }
    }

    // Service professions (banker / shopkeeper / etc) drive
    // dedicated interactions; surface them so the brief is
    // complete for "what does this mob do?" questions.
    if !p.professions.is_empty() {
        let labels: Vec<String> = p
            .professions
            .iter()
            .map(|pr| format!("{pr:?}"))
            .collect();
        out.push_str(&format!("Service role:  {}\r\n", labels.join(", ")));
    }

    // Protected-kind drives alignment penalty on kill — useful
    // to know when reviewing a mob's "what happens if a player
    // kills me" surface.
    if !matches!(p.protected_kind, mud_db::enums::ProtectedKind::Normal) {
        out.push_str(&format!(
            "Protected:     {:?} — killing this mob shifts the killer's alignment by {}.\r\n",
            p.protected_kind,
            p.protected_kind.alignment_penalty(),
        ));
    }

    // Triggers override or layer on top of the inferred behavior.
    // Note: NoScript flag suppresses dispatch, so flag that
    // explicitly when both are set.
    let no_script = p.behaviors.iter().any(|b| matches!(b, MobBehavior::NoScript));
    match (trig_count, no_script) {
        (0, _) => out.push_str("Triggers:      <none> — behavior is purely from the flags above.\r\n"),
        (n, false) => out.push_str(&format!(
            "Triggers:      {n} attached — scripts run alongside the flag-driven AI. Use `tinfo` for bodies.\r\n",
        )),
        (n, true) => out.push_str(&format!(
            "Triggers:      {n} attached, but No-Script is set — dispatch is suppressed!\r\n",
        )),
    }

    send_to(world, player, out);
}

pub(crate) fn cmd_ostat(world: &mut World, player: Entity, args: &str) {
    let Some((zone, id)) = resolve_object_target(world, player, args) else {
        return;
    };
    let proto = world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(p) = proto else {
        send_to(world, player, format!("No object proto ({zone}, {id}).\r\n"));
        return;
    };
    let live = world
        .query_filtered::<&WorldKey, With<Item>>()
        .iter(world)
        .filter(|wk| wk.zone == zone && wk.id == id)
        .count();
    let trig_count = world
        .resource::<mud_world::TriggerCatalog>()
        .object_attachments
        .get(&(zone, id))
        .map_or(0, Vec::len);

    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!("name:          {}\r\n", p.name));
    out.push_str(&format!("keywords:      {}\r\n", p.keywords.join(", ")));
    if let Some(desc) = &p.examine_description {
        out.push_str(&format!("examine:       {desc}\r\n"));
    }
    out.push_str(&format!("type:          {}\r\n", p.r#type.label()));
    let wear_labels: Vec<&'static str> = p.wear_flags.iter().map(|f| f.label()).collect();
    let wear_str = if wear_labels.is_empty() {
        "<none>".to_string()
    } else {
        wear_labels.join(", ")
    };
    out.push_str(&format!("wear_flags:    {wear_str}\r\n"));
    if let Some(b) = p.board_id {
        out.push_str(&format!("board_id:      {b}\r\n"));
    }
    if let Some(liq) = &p.liquid {
        out.push_str(&format!(
            "liquid:        {} ({}/{}, poisoned={})\r\n",
            liq.liquid, liq.remaining, liq.capacity, liq.poisoned
        ));
    }
    out.push_str(&format!("triggers:      {trig_count}\r\n"));
    if p.extras.is_empty() {
        out.push_str("extras:        <none>\r\n");
    } else {
        out.push_str(&format!("extras:        {} entries\r\n", p.extras.len()));
        for (kws, _) in &p.extras {
            out.push_str(&format!("               keywords: {}\r\n", kws.join(", ")));
        }
    }
    out.push_str(&format!("live count:    {live}\r\n"));
    send_to(world, player, out);
}
pub(crate) fn cmd_setweather(world: &mut World, player: Entity, args: &str) {
    use mud_db::enums::Climate;
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        send_to(world, player, "Usage: setweather <climate> [<zone_id>]\r\n");
        return;
    }
    let climate = match parts[0].to_ascii_lowercase().as_str() {
        "none" => Climate::None,
        "semiarid" => Climate::Semiarid,
        "arid" => Climate::Arid,
        "oceanic" => Climate::Oceanic,
        "temperate" => Climate::Temperate,
        "subtropical" => Climate::Subtropical,
        "tropical" => Climate::Tropical,
        "subarctic" => Climate::Subarctic,
        "arctic" => Climate::Arctic,
        "alpine" => Climate::Alpine,
        other => {
            send_to(
                world,
                player,
                format!(
                    "Unknown climate '{other}'. Try: none, semiarid, arid, oceanic, temperate, subtropical, tropical, subarctic, arctic, alpine.\r\n"
                ),
            );
            return;
        }
    };
    let zone_id = if parts.len() >= 2 {
        let Ok(z) = parts[1].parse::<i32>() else {
            send_to(world, player, "zone_id must be an integer.\r\n");
            return;
        };
        z
    } else {
        let Some(zone) = world
            .get::<Located>(player)
            .and_then(|l| world.get::<WorldKey>(l.0))
            .map(|wk| wk.zone)
        else {
            send_to(world, player, "Can't find your zone.\r\n");
            return;
        };
        zone
    };
    let Some(zone_entity) = world
        .resource::<WorldKeyIndex>()
        .zones
        .get(&zone_id)
        .copied()
    else {
        send_to(world, player, format!("No zone {zone_id} loaded.\r\n"));
        return;
    };
    if let Some(mut zc) = world.get_mut::<ZoneClimate>(zone_entity) {
        zc.0 = climate;
    } else {
        world
            .entity_mut(zone_entity)
            .insert(ZoneClimate(climate));
    }
    let zone_name = name_of(world, zone_entity);
    send_to(
        world,
        player,
        format!("Set climate of zone {zone_id} ({zone_name}) to {climate:?}.\r\n"),
    );
}
/// Inventory of writable fields the `set` command supports.
/// `(canonical, aliases, "what it touches")`. Sourced once and
/// rendered by `set fields` so the help is always in sync with
/// the match arms below.
const SET_FIELDS: &[(&str, &[&str], &str)] = &[
    ("level", &[], "Profile.level (clamped >= 1)"),
    ("xp", &["exp", "experience"], "Profile.experience (clamped >= 0)"),
    ("hp", &[], "Health.hp (clamped 0..=max)"),
    ("maxhp", &[], "Health.max (current hp pinned to new max)"),
    ("stamina", &["stam"], "Stamina.current (clamped 0..=max)"),
    ("maxstamina", &["maxstam"], "Stamina.max (current stamina pinned)"),
    ("gold", &["copper", "wealth"], "Wealth in copper (clamped >= 0)"),
    ("alignment", &["align"], "CombatStats.alignment (signed)"),
];

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_set(world: &mut World, player: Entity, args: &str) {
    // `set fields` / `set list` — show the writable-field list.
    let trimmed = args.trim();
    if trimmed.eq_ignore_ascii_case("fields") || trimmed.eq_ignore_ascii_case("list") {
        let mut out = String::from("\r\n<b:cyan>Writable fields for `set`:</>\r\n");
        let widest_canonical = SET_FIELDS.iter().map(|(c, _, _)| c.len()).max().unwrap_or(0);
        for (canonical, aliases, desc) in SET_FIELDS {
            let alias_str = if aliases.is_empty() {
                String::new()
            } else {
                format!("  <dim>(also {})</>", aliases.join(", "))
            };
            out.push_str(&format!(
                "  <cyan>{canonical:<widest_canonical$}</>{alias_str}  <dim>—</> {desc}\r\n",
            ));
        }
        out.push_str("\r\n  <dim>Pair with `stat <player>` for the read-only component dump.</>\r\n");
        crate::commands::send_rendered(world, player, &out);
        return;
    }
    let parts: Vec<&str> = args.splitn(3, char::is_whitespace).collect();
    if parts.len() != 3 || parts[1].trim().is_empty() || parts[2].trim().is_empty() {
        send_to(
            world,
            player,
            "Usage: set <target|me> <field> <value>   (run `set fields` to list writable fields)\r\n",
        );
        return;
    }
    let target_word = parts[0].trim();
    let field = parts[1].trim().to_ascii_lowercase();
    let value_word = parts[2].trim();

    let target = if target_word.eq_ignore_ascii_case("me") || target_word == "self" {
        player
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, target_word, located.0, player) else {
            send_to(world, player, format!("No '{target_word}' here.\r\n"));
            return;
        };
        t
    };
    let target_name = name_of(world, target);

    // All supported fields are integer-typed for now.
    let Ok(value_i32) = value_word.parse::<i32>() else {
        send_to(world, player, "Value must be an integer.\r\n");
        return;
    };
    let value_i64 = i64::from(value_i32);

    let applied = match field.as_str() {
        "level" => world
            .get_mut::<Profile>(target)
            .map(|mut p| p.level = value_i32.max(1))
            .is_some(),
        "xp" | "exp" | "experience" => world
            .get_mut::<Profile>(target)
            .map(|mut p| p.experience = value_i32.max(0))
            .is_some(),
        "hp" => world
            .get_mut::<Health>(target)
            .map(|mut h| h.hp = value_i32.max(0).min(h.max))
            .is_some(),
        "maxhp" => {
            world
                .get_mut::<Health>(target)
                .map(|mut h| {
                    h.max = value_i32.max(1);
                    h.hp = h.hp.min(h.max);
                })
                .is_some()
        }
        "stamina" | "stam" => world
            .get_mut::<Stamina>(target)
            .map(|mut s| s.current = value_i32.max(0).min(s.max))
            .is_some(),
        "maxstamina" | "maxstam" => {
            world
                .get_mut::<Stamina>(target)
                .map(|mut s| {
                    s.max = value_i32.max(1);
                    s.current = s.current.min(s.max);
                })
                .is_some()
        }
        "gold" | "copper" | "wealth" => {
            if let Some(mut w) = world.get_mut::<Wealth>(target) {
                w.0 = value_i64.max(0);
                true
            } else {
                world.entity_mut(target).insert(Wealth(value_i64.max(0)));
                true
            }
        }
        "alignment" | "align" => world
            .get_mut::<CombatStats>(target)
            .map(|mut c| c.alignment = value_i32)
            .is_some(),
        other => {
            send_to(
                world,
                player,
                format!(
                    "Unknown field '{other}'. Run `set fields` to list every writable field.\r\n"
                ),
            );
            return;
        }
    };
    if applied {
        send_to(
            world,
            player,
            format!("Set {target_name}.{field} = {value_word}.\r\n"),
        );
    } else {
        send_to(
            world,
            player,
            format!("{target_name} has no component for {field}.\r\n"),
        );
    }
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_show(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim().to_ascii_lowercase();
    let mut out = String::from("\r\n");
    match arg.as_str() {
        "" => {
            out.push_str("Usage: show <subsystem>. Available:\r\n");
            out.push_str("  players   online list with role/level/room\r\n");
            out.push_str("  triggers  catalog totals and per-event tally\r\n");
            out.push_str("  effects   active EffectInstance counts\r\n");
            out.push_str("  clock     MudClock + TickCount\r\n");
            out.push_str("  resets    mob/object reset catalog counts\r\n");
            out.push_str("  corpses   active corpses + decay timers + item counts\r\n");
            out.push_str("  audit     recent admin-mutating actions\r\n");
            out.push_str("  rooms     world room totals + peaceful / light / hidden / extras counts\r\n");
        }
        "players" => {
            let mut rows: Vec<(String, String, i32, String)> = {
                let mut q = world
                    .query_filtered::<(&Named, &Account, Option<&Profile>, &Located), (With<Player>, With<Online>)>();
                q.iter(world)
                    .map(|(n, acct, prof, loc)| {
                        let role = acct.role.label().to_string();
                        let level = prof.map_or(0, |p| p.level);
                        let room = name_or(world, loc.0, "(unknown)");
                        (n.name.clone(), role, level, room)
                    })
                    .collect()
            };
            rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
            out.push_str(&format!("{} player(s) online:\r\n", rows.len()));
            for (name, role, level, room) in &rows {
                out.push_str(&format!(
                    "  {name:<24} L{level:>3} {role:<12} @ {room}\r\n"
                ));
            }
        }
        "triggers" => {
            use mud_world::TriggerEvent;
            let cat = world.resource::<TriggerCatalog>();
            out.push_str(&format!("Trigger catalog: {} rows\r\n", cat.by_key.len()));
            out.push_str(&format!(
                "  mob attachments:    {}\r\n",
                cat.mob_attachments.len()
            ));
            out.push_str(&format!(
                "  object attachments: {}\r\n",
                cat.object_attachments.len()
            ));
            out.push_str(&format!(
                "  room attachments:   {}\r\n",
                cat.room_attachments.len()
            ));
            let mut tally: HashMap<&'static str, usize> = HashMap::new();
            let label = |e: &TriggerEvent| match e {
                TriggerEvent::Global => "GLOBAL",
                TriggerEvent::Random => "RANDOM",
                TriggerEvent::Command => "COMMAND",
                TriggerEvent::Load => "LOAD",
                TriggerEvent::Cast => "CAST",
                TriggerEvent::Leave => "LEAVE",
                TriggerEvent::Time => "TIME",
                TriggerEvent::Speech => "SPEECH",
                TriggerEvent::Act => "ACT",
                TriggerEvent::Death => "DEATH",
                TriggerEvent::Greet => "GREET",
                TriggerEvent::GreetAll => "GREET_ALL",
                TriggerEvent::Entry => "ENTRY",
                TriggerEvent::Receive => "RECEIVE",
                TriggerEvent::Fight => "FIGHT",
                TriggerEvent::HitPercent => "HIT_PERCENT",
                TriggerEvent::Bribe => "BRIBE",
                TriggerEvent::Memory => "MEMORY",
                TriggerEvent::Door => "DOOR",
                TriggerEvent::SpeechTo => "SPEECH_TO",
                TriggerEvent::Look => "LOOK",
                TriggerEvent::Auto => "AUTO",
                TriggerEvent::Attack => "ATTACK",
                TriggerEvent::Defend => "DEFEND",
                TriggerEvent::Timer => "TIMER",
                TriggerEvent::Get => "GET",
                TriggerEvent::Drop => "DROP",
                TriggerEvent::Give => "GIVE",
                TriggerEvent::Wear => "WEAR",
                TriggerEvent::Remove => "REMOVE",
                TriggerEvent::Use => "USE",
                TriggerEvent::Consume => "CONSUME",
                TriggerEvent::Reset => "RESET",
                TriggerEvent::Preentry => "PREENTRY",
                TriggerEvent::Postentry => "POSTENTRY",
            };
            for def in cat.by_key.values() {
                for f in &def.flags {
                    *tally.entry(label(f)).or_insert(0) += 1;
                }
            }
            let mut entries: Vec<(&str, usize)> = tally.into_iter().collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            out.push_str("  per-event tally:\r\n");
            for (name, n) in entries {
                out.push_str(&format!("    {name:<14} {n:>5}\r\n"));
            }
        }
        "effects" => {
            let mut tally: HashMap<String, i32> = HashMap::new();
            let mut q = world.query::<&EffectInstance>();
            for e in q.iter(world) {
                *tally.entry(e.name.clone()).or_insert(0) += 1;
            }
            let total: i32 = tally.values().sum();
            out.push_str(&format!("{total} active EffectInstance(s):\r\n"));
            let mut rows: Vec<(String, i32)> = tally.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (name, n) in rows.iter().take(20) {
                out.push_str(&format!("  {name:<24} {n:>5}\r\n"));
            }
            if rows.len() > 20 {
                out.push_str(&format!("  ... ({} more)\r\n", rows.len() - 20));
            }
        }
        "clock" => {
            let tick = world.resource::<TickCount>().0;
            let clock = world.resource::<mud_world::MudClock>().clone();
            out.push_str(&format!("Tick:   {tick}\r\n"));
            out.push_str(&format!(
                "Clock:  year {}  month {}  day {}  hour {}\r\n",
                clock.year, clock.month, clock.day, clock.hour
            ));
            out.push_str(&format!("Stamp:  {} (Unix epoch seconds)\r\n", clock.stamp));
        }
        "resets" => {
            use mud_world::{MobResetCatalog, ObjectResetCatalog};
            let mob_count = world.resource::<MobResetCatalog>().entries.len();
            let obj_count = world.resource::<ObjectResetCatalog>().entries.len();
            out.push_str(&format!("Mob reset rows:    {mob_count}\r\n"));
            out.push_str(&format!("Object reset rows: {obj_count}\r\n"));
        }
        "audit" => {
            let log = world.get_resource::<AdminAuditLog>();
            match log {
                None => out.push_str("Audit log empty (no admin actions yet).\r\n"),
                Some(l) if l.entries.is_empty() => {
                    out.push_str("Audit log empty (no admin actions yet).\r\n");
                }
                Some(l) => {
                    out.push_str(&format!(
                        "Last {} admin action(s):\r\n",
                        l.entries.len(),
                    ));
                    for e in l.entries.iter().rev().take(40) {
                        let secs_ago = std::time::SystemTime::now()
                            .duration_since(e.at)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        out.push_str(&format!(
                            "  {ago:>5}s ago  {actor:<20}  {verb:<10}  {args}\r\n",
                            ago = secs_ago,
                            actor = e.actor_name,
                            verb = e.verb,
                            args = e.args,
                        ));
                    }
                }
            }
        }
        "corpses" => {
            // Collect (corpse, name, decay, room, item_count) — same shape
            // as the persistence snapshot uses, so what `show corpses`
            // surfaces matches what survives a Ctrl-C.
            let rows: Vec<(String, i32, String, usize)> = {
                let mut q = world
                    .query_filtered::<(Entity, &Named, &mud_world::CorpseDecay, &Located), With<mud_world::Corpse>>();
                let snapshot: Vec<(Entity, String, i32, Entity)> = q
                    .iter(world)
                    .map(|(e, n, d, l)| (e, n.name.clone(), d.remaining_secs, l.0))
                    .collect();
                snapshot
                    .into_iter()
                    .map(|(corpse, name, decay, room)| {
                        let room_name = name_or(world, room, "(unknown)");
                        let item_count = {
                            let mut q = world
                                .query_filtered::<&Located, With<Item>>();
                            q.iter(world).filter(|l| l.0 == corpse).count()
                        };
                        (name, decay, room_name, item_count)
                    })
                    .collect()
            };
            if rows.is_empty() {
                out.push_str("No corpses on the floor.\r\n");
            } else {
                out.push_str(&format!("{} corpse(s):\r\n", rows.len()));
                for (name, decay, room, items) in rows {
                    out.push_str(&format!(
                        "  {name} ({decay}s left, {items} item(s)) @ {room}\r\n"
                    ));
                }
            }
        }
        "rooms" => {
            let total = world
                .query_filtered::<&WorldKey, With<mud_world::Room>>()
                .iter(world)
                .count();
            let peaceful = world
                .query_filtered::<&mud_world::PeacefulRoom, With<mud_world::Room>>()
                .iter(world)
                .count();
            let (lit_override, dark_override) = {
                let mut q = world
                    .query_filtered::<&mud_world::BaseLightLevel, With<mud_world::Room>>();
                let mut lit = 0_usize;
                let mut dark = 0_usize;
                for level in q.iter(world) {
                    if level.0 > 0 {
                        lit += 1;
                    } else if level.0 < 0 {
                        dark += 1;
                    }
                }
                (lit, dark)
            };
            let (extras_rooms, extras_entries) = {
                let mut q = world
                    .query_filtered::<&mud_world::RoomExtras, With<mud_world::Room>>();
                let mut rooms_with = 0_usize;
                let mut total_entries = 0_usize;
                for extras in q.iter(world) {
                    if !extras.entries.is_empty() {
                        rooms_with += 1;
                    }
                    total_entries += extras.entries.len();
                }
                (rooms_with, total_entries)
            };
            let hidden_exits = {
                let mut q = world
                    .query_filtered::<&Exits, With<mud_world::Room>>();
                let mut count = 0_usize;
                for exits in q.iter(world) {
                    count += exits.0.values().filter(|ed| ed.is_hidden).count();
                }
                count
            };
            out.push_str(&format!("Rooms loaded:        {total}\r\n"));
            out.push_str(&format!("  peaceful:          {peaceful}\r\n"));
            out.push_str(&format!("  light overrides:   {lit_override} lit / {dark_override} dark\r\n"));
            out.push_str(&format!("  hidden exits:      {hidden_exits}\r\n"));
            out.push_str(&format!(
                "  extras:            {extras_rooms} rooms / {extras_entries} entries\r\n"
            ));
        }
        other => {
            out.push_str(&format!(
                "Unknown subsystem '{other}'. Try `show` for the list.\r\n"
            ));
        }
    }
    send_to(world, player, out);
}
pub(crate) fn cmd_scripterrors(world: &mut World, player: Entity, args: &str) {
    use mud_world::ScriptErrorLog;
    let n: usize = args
        .trim()
        .parse()
        .ok()
        .filter(|x: &usize| *x > 0)
        .unwrap_or(20);
    if !world.contains_resource::<ScriptErrorLog>() {
        send_to(world, player, "No trigger errors recorded yet.\r\n");
        return;
    }
    let log = world.resource::<ScriptErrorLog>();
    if log.entries.is_empty() {
        send_to(world, player, "No trigger errors recorded yet.\r\n");
        return;
    }
    let total = log.entries.len();
    // Number entries from the most recent (#1) so `trace N` can
    // refer back to a row by position. The numbering is stable
    // within one render call but shifts as new errors land —
    // good enough for an interactive debugging loop.
    let mut out = format!("\r\nLast {} of {total} trigger error(s):\r\n", n.min(total));
    for (idx, entry) in log.entries.iter().rev().take(n).enumerate() {
        let secs_ago = std::time::SystemTime::now()
            .duration_since(entry.at)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push_str(&format!(
            "  <dim>#{n:>3}</>  {ago:>4}s ago  ({zone}, {id}) [{event}] {name}\r\n         {msg}\r\n",
            n = idx + 1,
            ago = secs_ago,
            zone = entry.trigger_zone,
            id = entry.trigger_id,
            event = entry.event,
            name = entry.trigger_name,
            msg = entry.message,
        ));
    }
    out.push_str("  <dim>(use `trace <N>` to dump the full trigger body for entry #N.)</>\r\n");
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_trace(world: &mut World, player: Entity, args: &str) {
    use mud_world::ScriptErrorLog;
    let Ok(n) = args.trim().parse::<usize>() else {
        send_to(world, player, "Usage: trace <N>  (N is the entry number from `scripterrors`)\r\n");
        return;
    };
    if n == 0 {
        send_to(world, player, "Entry numbers start at 1.\r\n");
        return;
    }
    if !world.contains_resource::<ScriptErrorLog>() {
        send_to(world, player, "No trigger errors recorded yet.\r\n");
        return;
    }
    let entry = world
        .resource::<ScriptErrorLog>()
        .entries
        .iter()
        .rev()
        .nth(n - 1)
        .cloned();
    let Some(entry) = entry else {
        send_to(world, player, format!("No entry #{n} in the error log.\r\n"));
        return;
    };
    let secs_ago = std::time::SystemTime::now()
        .duration_since(entry.at)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = format!(
        "\r\n<b:cyan>Trigger error #{n}</>  ({zone}, {id}) [{event}] {name}\r\n",
        zone = entry.trigger_zone,
        id = entry.trigger_id,
        event = entry.event,
        name = entry.trigger_name,
    );
    out.push_str(&format!("  <dim>fired</>     {secs_ago}s ago\r\n"));
    out.push_str(&format!("  <red>error</>     {}\r\n", entry.message));
    // Pull the full body from the catalog so the builder doesn't
    // have to chase tstat. Shows the source as it was loaded —
    // if the trigger has been edited since the failure, the body
    // here is the *current* one (annotated below).
    let body = world
        .resource::<mud_world::TriggerCatalog>()
        .by_key
        .get(&(entry.trigger_zone, entry.trigger_id))
        .map(|d| d.commands.clone());
    if let Some(body) = body {
        out.push_str("  <dim>body (current — may differ from the version that failed):</>\r\n");
        for (i, line) in body.lines().enumerate() {
            out.push_str(&format!("    {n:>3} | {line}\r\n", n = i + 1));
        }
    } else {
        out.push_str("  <dim>(catalog has no current row for this id — trigger may have been deleted.)</>\r\n");
    }
    crate::commands::send_rendered(world, player, &out);
}
pub(crate) fn cmd_syslog(world: &mut World, player: Entity, args: &str) {
    let mut tokens = args.split_whitespace();
    let count: usize = tokens
        .next()
        .and_then(|s| s.parse().ok())
        .map_or(30, |n: usize| n.clamp(1, 500));
    let filter = tokens.next().map(str::to_ascii_uppercase);

    let entries = crate::syslog::snapshot();
    if entries.is_empty() {
        send_to(world, player, "Syslog buffer is empty.\r\n");
        return;
    }

    let matches = |e: &crate::syslog::SyslogEntry| -> bool {
        let Some(f) = filter.as_deref() else { return true };
        e.level.as_str().eq_ignore_ascii_case(f)
            || e.target.to_ascii_uppercase().contains(f)
            || e.message.to_ascii_uppercase().contains(f)
    };

    let mut picked: Vec<&crate::syslog::SyslogEntry> = Vec::new();
    for entry in entries.iter().rev() {
        if matches(entry) {
            picked.push(entry);
            if picked.len() >= count {
                break;
            }
        }
    }
    let total = entries.len();
    let shown = picked.len();
    picked.reverse();

    let mut out = format!("\r\nSyslog: showing {shown} of {total} entry(s)");
    if let Some(f) = filter.as_deref() {
        out.push_str(&format!(" matching '{f}'"));
    }
    out.push_str(":\r\n");
    let now = std::time::SystemTime::now();
    for e in &picked {
        let secs_ago = now.duration_since(e.at).map(|d| d.as_secs()).unwrap_or(0);
        out.push_str(&format!(
            "  {ago:>5}s  {lvl:<5}  {target:<24}  {msg}\r\n",
            ago = secs_ago,
            lvl = e.level.as_str(),
            target = e.target,
            msg = e.message,
        ));
    }
    send_to(world, player, out);
}
pub(crate) fn cmd_astat(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let target = if arg.is_empty() {
        player
    } else {
        let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
            send_to(world, player, "You're nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, room, player) else {
            send_to(world, player, format!("No '{arg}' here.\r\n"));
            return;
        };
        t
    };
    let target_name = name_of(world, target);
    let active: Vec<(String, i32, i32, mud_world::EffectSource, Option<i32>)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == target)
            .map(|(inst, _)| {
                (
                    inst.name.clone(),
                    inst.remaining_secs,
                    inst.strength,
                    inst.source.clone(),
                    inst.ability_id,
                )
            })
            .collect()
    };
    let mut out = format!("\r\nEffects on {target_name}:\r\n");
    if active.is_empty() {
        out.push_str("  (none)\r\n");
        send_to(world, player, out);
        return;
    }
    let catalog = world.resource::<AbilityCatalog>();
    for (name, remaining, strength, source, ability_id) in active {
        let from = ability_id.and_then(|id| {
            catalog
                .by_name
                .values()
                .find(|d| d.id == id)
                .map(|d| d.plain_name.clone())
        });
        let from_str = from.as_deref().map_or(String::new(), |n| format!(" from {n}"));
        let dur = if remaining < 0 {
            "permanent".to_string()
        } else {
            format!("{remaining}s left")
        };
        out.push_str(&format!(
            "  {name:<20} strength={strength:<3} {dur} source={source:?}{from_str}\r\n"
        ));
    }
    send_to(world, player, out);
}
pub(crate) fn cmd_sstat(world: &mut World, player: Entity, args: &str) {
    let Some((zone, id)) = resolve_zone_id(world, player, args) else {
        return;
    };
    let shop = world.resource::<ShopCatalog>().by_key.get(&(zone, id)).cloned();
    let Some(s) = shop else {
        send_to(world, player, format!("No shop ({zone}, {id}).\r\n"));
        return;
    };
    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!(
        "keeper:        ({}, {})\r\n",
        s.keeper_zone_id, s.keeper_id
    ));
    out.push_str(&format!("buy_profit:    {:.2}\r\n", s.buy_profit));
    out.push_str(&format!("sell_profit:   {:.2}\r\n", s.sell_profit));
    out.push_str(&format!("items:         {}\r\n", s.items.len()));
    for it in s.items.iter().take(20) {
        out.push_str(&format!(
            "  ({}, {}) amount={} price={}\r\n",
            it.object_zone_id, it.object_id, it.amount, it.price
        ));
    }
    if s.items.len() > 20 {
        out.push_str(&format!("  ... ({} more)\r\n", s.items.len() - 20));
    }
    out.push_str(&format!("accepts rules: {}\r\n", s.accepts.len()));
    out.push_str(&format!("pets:          {}\r\n", s.pets.len()));
    send_to(world, player, out);
}
pub(crate) fn cmd_tstat(world: &mut World, player: Entity, args: &str) {
    let Some((zone, id)) = resolve_zone_id(world, player, args) else {
        return;
    };
    let def = world
        .resource::<mud_world::TriggerCatalog>()
        .by_key
        .get(&(zone, id))
        .cloned();
    let Some(d) = def else {
        send_to(world, player, format!("No trigger ({zone}, {id}).\r\n"));
        return;
    };
    let flag_strs: Vec<String> = d.flags.iter().map(|f| format!("{f:?}")).collect();
    let mut out = String::from("\r\n");
    out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    out.push_str(&format!("name:          {}\r\n", d.name));
    out.push_str(&format!("attach:        {:?}\r\n", d.attach_type));
    out.push_str(&format!("flags:         [{}]\r\n", flag_strs.join(", ")));
    if !d.arg_list.is_empty() {
        out.push_str(&format!("arg_list:      [{}]\r\n", d.arg_list.join(", ")));
    }
    out.push_str(&format!("num_args:      {}\r\n", d.num_args));
    out.push_str("commands:\r\n");
    for line in d.commands.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_rstat(world: &mut World, player: Entity, args: &str) {
    type ActorRow = (Entity, String, Option<(i32, i32)>);
    type ItemRow = (Entity, String, Option<(i32, i32)>, Option<i32>);
    let trimmed = args.trim();
    let room = if trimmed.is_empty() {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        located.0
    } else {
        // Resolve via the same `<zone> <id>` / `<id>` shape as
        // mstat / sstat / etc.
        let Some((zone, id)) = resolve_zone_id(world, player, args) else {
            return;
        };
        let Some(found) = world
            .resource::<WorldKeyIndex>()
            .rooms
            .get(&(zone, id))
            .copied()
        else {
            send_to(world, player, format!("No room ({zone}, {id}) loaded.\r\n"));
            return;
        };
        found
    };

    let mut out = String::from("\r\n");
    out.push_str(&format!("entity:        {room:?}\r\n"));
    out.push_str(&format!("name:          {}\r\n", name_of(world, room)));
    if let Some(wk) = world.get::<WorldKey>(room) {
        out.push_str(&format!("world_key:     ({}, {})\r\n", wk.zone, wk.id));
    }
    if let Some(s) = world.get::<RoomSector>(room) {
        out.push_str(&format!("sector:        {:?}\r\n", s.0));
    }
    if let Some(level) = world.get::<mud_world::BaseLightLevel>(room) {
        // Builders need to confirm magical-glow / void overrides
        // are landing on the right rooms; positive = lit override,
        // negative = dark override, zero rooms don't carry the
        // component (and so this row stays absent for them).
        let direction = if level.0 > 0 { "lit" } else { "dark" };
        out.push_str(&format!(
            "light_level:   {} ({direction} override)\r\n",
            level.0
        ));
    }
    if world.get::<mud_world::PeacefulRoom>(room).is_some() {
        out.push_str("flags:         peaceful\r\n");
    }
    if let Some(extras) = world.get::<mud_world::RoomExtras>(room) {
        if extras.entries.is_empty() {
            out.push_str("extras:        <none>\r\n");
        } else {
            out.push_str(&format!("extras:        {} entries\r\n", extras.entries.len()));
            for (kws, _) in &extras.entries {
                out.push_str(&format!("               keywords: {}\r\n", kws.join(", ")));
            }
        }
    }
    if let Some(exits) = world.get::<Exits>(room).cloned() {
        if exits.0.is_empty() {
            out.push_str("exits:         <none>\r\n");
        } else {
            out.push_str(&format!("exits:         {} populated\r\n", exits.0.len()));
            let mut exit_pairs: Vec<(Direction, ExitData)> = exits.0.into_iter().collect();
            exit_pairs.sort_by_key(|(d, _)| direction_rank(*d));
            for (dir, ed) in &exit_pairs {
                let (target_name, target_label) = match ed.to {
                    Some(t) => (name_or(world, t, "(unknown)"), format!("{t:?}")),
                    None => ("(dangling)".to_string(), "None".to_string()),
                };
                let key_label = ed
                    .key
                    .map_or_else(String::new, |(z, i)| format!(" key=({z}, {i})"));
                out.push_str(&format!(
                    "               {:>9} -> {target_label} ({target_name}) [{:?}]{key_label}\r\n",
                    direction_name(*dir),
                    ed.state,
                ));
            }
        }
    }
    // Attached triggers (catalog refs).
    if let Some(trig) = world.get::<AttachedTriggers>(room).cloned() {
        if trig.0.is_empty() {
            out.push_str("triggers:      <none>\r\n");
        } else {
            out.push_str(&format!("triggers:      {} attached\r\n", trig.0.len()));
            for (z, i) in &trig.0 {
                out.push_str(&format!("               ({z}, {i})\r\n"));
            }
        }
    }
    // Occupants: mobs, players, items directly Located in this room.
    let mut mobs: Vec<ActorRow> = world
        .query_filtered::<(Entity, &Located, &Named, Option<&WorldKey>), With<Mob>>()
        .iter(world)
        .filter(|(_, l, _, _)| l.0 == room)
        .map(|(e, _, n, wk)| (e, n.name.clone(), wk.map(|w| (w.zone, w.id))))
        .collect();
    mobs.sort_by(|a, b| a.1.cmp(&b.1));
    let mut players: Vec<(Entity, String)> = world
        .query_filtered::<(Entity, &Located, &Named), With<Player>>()
        .iter(world)
        .filter(|(_, l, _)| l.0 == room)
        .map(|(e, _, n)| (e, n.name.clone()))
        .collect();
    players.sort_by(|a, b| a.1.cmp(&b.1));
    let mut items: Vec<ItemRow> = world
        .query_filtered::<(
            Entity,
            &Located,
            &Named,
            Option<&WorldKey>,
            Option<&FromObjectReset>,
        ), With<Item>>()
        .iter(world)
        .filter(|(_, l, _, _, _)| l.0 == room)
        .map(|(e, _, n, wk, fr)| (e, n.name.clone(), wk.map(|w| (w.zone, w.id)), fr.map(|f| f.0)))
        .collect();
    items.sort_by(|a, b| a.1.cmp(&b.1));
    out.push_str(&format!(
        "occupants:     {} mob(s), {} player(s), {} item(s)\r\n",
        mobs.len(),
        players.len(),
        items.len(),
    ));
    for (e, name) in &players {
        out.push_str(&format!("  player:      {e:?}  {name}\r\n"));
    }
    for (e, name, wk) in &mobs {
        let key_str = wk.map_or(String::new(), |(z, i)| format!("  ({z}, {i})"));
        out.push_str(&format!("  mob:         {e:?}  {name}{key_str}\r\n"));
    }
    for (e, name, wk, reset_id) in &items {
        let key_str = wk.map_or(String::new(), |(z, i)| format!("  ({z}, {i})"));
        let reset_str = reset_id.map_or(String::new(), |r| format!("  reset={r}"));
        out.push_str(&format!("  item:        {e:?}  {name}{key_str}{reset_str}\r\n"));
    }
    // EffectInstances applied to this room (environmental auras).
    let effects: Vec<(String, i32)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, applied)| applied.0 == room)
            .map(|(eff, _)| (eff.name.clone(), eff.remaining_secs))
            .collect()
    };
    if effects.is_empty() {
        out.push_str("effects:       <none>\r\n");
    } else {
        out.push_str(&format!("effects:       {} active\r\n", effects.len()));
        for (name, secs) in &effects {
            out.push_str(&format!("               {name} ({secs}s)\r\n"));
        }
    }
    send_to(world, player, out);
}
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_stat(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    // `stat room [<zone> <id>]` aliases through to `cmd_rstat`. Same
    // semantics: no args dumps the room you're standing in, two ids
    // resolve via WorldKeyIndex.
    let mut head = arg.split_whitespace();
    if head.next() == Some("room") {
        let rest: String = head.collect::<Vec<_>>().join(" ");
        cmd_rstat(world, player, &rest);
        return;
    }
    let target = if arg.is_empty() || arg.eq_ignore_ascii_case("me")
        || arg.eq_ignore_ascii_case("self")
    {
        player
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        // Try actor (mob/player) first, then item (room or carried).
        let needle = arg.to_ascii_lowercase();
        let actor = find_actor_in_room(world, arg, located.0, player);
        let item = actor.or_else(|| {
            let mut q = world
                .query_filtered::<(Entity, &Located, &Named, Option<&Keywords>), With<Item>>();
            q.iter(world)
                .find(|(_, l, n, kw)| {
                    (l.0 == located.0 || l.0 == player) && matches(&needle, n, *kw)
                })
                .map(|(e, _, _, _)| e)
        });
        let Some(found) = item else {
            send_to(world, player, format!("No '{arg}' here.\r\n"));
            return;
        };
        found
    };

    let mut out = String::from("\r\n");
    out.push_str(&format!("entity:        {target:?}\r\n"));
    out.push_str(&format!("name:          {}\r\n", name_of(world, target)));
    if let Some(wk) = world.get::<WorldKey>(target) {
        out.push_str(&format!("world_key:     ({}, {})\r\n", wk.zone, wk.id));
    }
    if let Some(located) = world.get::<Located>(target) {
        let in_name = name_or(world, located.0, "(unknown)");
        out.push_str(&format!("located_in:    {:?} ({})\r\n", located.0, in_name));
    }
    if let Some(kw) = world.get::<Keywords>(target) {
        out.push_str(&format!("keywords:      {:?}\r\n", kw.0));
    }
    if world.get::<Player>(target).is_some() {
        out.push_str("kind:          Player\r\n");
    } else if world.get::<Mob>(target).is_some() {
        out.push_str("kind:          Mob\r\n");
    } else if world.get::<Item>(target).is_some() {
        out.push_str("kind:          Item\r\n");
        // Lit + fuel state for Light-typed items. Lit alone is a
        // marker; LightFuel carries the burn timer.
        if world.get::<mud_world::Lit>(target).is_some() {
            out.push_str("lit:           yes\r\n");
        }
        if let Some(fuel) = world.get::<mud_world::LightFuel>(target).copied() {
            if fuel.remaining < 0 {
                out.push_str("fuel:          infinite\r\n");
            } else {
                out.push_str(&format!(
                    "fuel:          {} / {} game-hours\r\n",
                    fuel.remaining, fuel.capacity,
                ));
            }
        }
        // Resolve through the prototype catalog for weight / level /
        // type. Synthetic seed items lack a WorldKey and so fall
        // through silently.
        if let Some(wk) = world.get::<WorldKey>(target).copied() {
            if let Some(proto) = world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(wk.zone, wk.id))
                .cloned()
            {
                out.push_str(&format!(
                    "proto:         weight {:.1}, level {}, type {}\r\n",
                    proto.weight, proto.level, proto.r#type.label(),
                ));
            }
            // Bound abilities (scrolls / wands / staves).
            if let Some(abilities) = world
                .resource::<mud_world::ObjectAbilityCatalog>()
                .by_key
                .get(&(wk.zone, wk.id))
                .cloned()
            {
                let catalog = world.resource::<AbilityCatalog>();
                for b in &abilities {
                    let name = catalog
                        .by_name
                        .values()
                        .find(|d| d.id == b.ability_id)
                        .map_or_else(
                            || format!("(id {})", b.ability_id),
                            |d| d.plain_name.clone(),
                        );
                    let ch = b.charges.map_or_else(|| "∞".to_string(), |c| c.to_string());
                    out.push_str(&format!(
                        "ability:       {name} (level {}, charges {ch})\r\n",
                        b.level,
                    ));
                }
            }
        }
    } else {
        out.push_str("kind:          (other)\r\n");
    }
    if let Some(h) = world.get::<Health>(target) {
        out.push_str(&format!("health:        {}/{}\r\n", h.hp, h.max));
    }
    if let Some(s) = world.get::<Stamina>(target) {
        out.push_str(&format!("stamina:       {}/{}\r\n", s.current, s.max));
    }
    if let Some(p) = world.get::<Posture>(target) {
        out.push_str(&format!("posture:       {}\r\n", p.0.label()));
    }
    if let Some(cs) = world.get::<CombatStats>(target) {
        out.push_str(&format!(
            "combat:        hit {} / dmg {} / ac {} / align {}\r\n",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(prof) = world.get::<Profile>(target) {
        let class_label = prof
            .class_id
            .and_then(|id| {
                world
                    .get_resource::<ClassCatalog>()
                    .and_then(|c| c.by_id.get(&id).map(|d| d.plain_name.clone()))
            })
            .unwrap_or_else(|| String::from("(none)"));
        out.push_str(&format!(
            "profile:       L{} {} ({}), xp {}\r\n",
            prof.level, prof.race, class_label, prof.experience,
        ));
    }
    if let Some(f) = world.get::<Fighting>(target) {
        let n = name_or(world, f.0, "(gone)");
        out.push_str(&format!("fighting:      {:?} ({n})\r\n", f.0));
    }
    if let Some(eq) = world.get::<EquippedSlot>(target) {
        out.push_str(&format!("equipped_slot: {}\r\n", eq.0.db_label()));
    }
    if let Some(account) = world.get::<Account>(target) {
        out.push_str(&format!(
            "account:       role={} char_id={}\r\n",
            account.role.label(),
            account.character_id,
        ));
    }
    // Staff notes (Builder+ only readers — gate via min_role on the
    // Command record itself, so reaching here implies authorization).
    // Fire-and-forget DB read; the formatted block ships as a
    // follow-up message after the main `stat` body.
    let staff_notes_target: Option<(String, String)> = world
        .get::<Account>(target)
        .map(|a| (a.character_id.clone(), name_of(world, target)));
    if let Some((cid, target_name_owned)) = staff_notes_target
        && let Some(pool) = world.get_resource::<DbPool>().map(|p| p.0.clone())
        && let Some(out_chan) = world.get::<Connection>(player).map(|c| c.0.clone())
    {
        tokio::spawn(async move {
            match mud_db::characters::load_staff_notes(&pool, &cid).await {
                Ok(Some(notes)) if !notes.trim().is_empty() => {
                    let _ = out_chan.send(
                        format!(
                            "\r\n=== Staff notes for {target_name_owned} ===\r\n{notes}\r\n",
                        )
                        .into_bytes(),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "stat: load_staff_notes failed");
                }
            }
        });
    }
    if let Some(fl) = world.get::<PlayerFlags>(target) {
        let labels: Vec<&'static str> = fl.0.iter().map(|f| f.label()).collect();
        if labels.is_empty() {
            out.push_str("flags:         <none>\r\n");
        } else {
            out.push_str(&format!("flags:         {}\r\n", labels.join(", ")));
        }
    }
    // EffectInstances applied to this entity.
    let effects: Vec<(String, i32)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, applied)| applied.0 == target)
            .map(|(eff, _)| (eff.name.clone(), eff.remaining_secs))
            .collect()
    };
    if effects.is_empty() {
        out.push_str("effects:       <none>\r\n");
    } else {
        out.push_str(&format!("effects:       {} active\r\n", effects.len()));
        for (name, secs) in &effects {
            out.push_str(&format!("               {name} ({secs}s)\r\n"));
        }
    }
    send_to(world, player, out);
}

/// Hard cap on `*search` listing length so a wide query (`osearch a`)
/// doesn't flood the screen. Matches the legacy convention of paged
/// output in vsearch.cpp without porting the full pagination machinery.
const SEARCH_RESULT_LIMIT: usize = 50;

inventory::submit! {
    Command {
        names: &["slist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "slist [spell|chant|song|skill]",
            summary: "List every ability of a given kind, or all kinds.",
            long: "Builder+. Pages through `AbilityCatalog`, sorted \
                   alphabetically. With no arg, prints every ability \
                   grouped by kind. With a kind label, narrows the \
                   list. Pair with `astat <name>` for per-ability \
                   detail.",
        },
        run: cmd_slist,
    }
}

inventory::submit! {
    Command {
        names: &["snum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "snum <name>",
            summary: "Print the catalog id for an ability by name.",
            long: "Builder+. Case-insensitive whole-name match. For \
                   substring search use `ssearch`. The id surfaces \
                   on `varset` / `skillset` / formula bindings.",
        },
        run: cmd_snum,
    }
}

inventory::submit! {
    Command {
        names: &["ssearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ssearch <substring>",
            summary: "Find abilities by name substring.",
            long: "Builder+. Case-insensitive substring against the \
                   ability's plain name. Renders `(id) name (kind)` \
                   per match, capped at 50.",
        },
        run: cmd_ssearch,
    }
}

inventory::submit! {
    Command {
        names: &["vitem"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "vitem <type>",
            summary: "List object prototypes by type.",
            long: "Builder+. Filters `ObjectPrototypes` by `ObjectType` \
                   (Weapon, Armor, Container, Light, Scroll, Wand, …). \
                   Case-insensitive. Capped at 50 hits with overflow \
                   footer; pair with `ostat` to inspect any one.",
        },
        run: cmd_vitem,
    }
}

inventory::submit! {
    Command {
        names: &["vwear"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "vwear <slot>",
            summary: "List object prototypes by wear-slot.",
            long: "Builder+. Filters `ObjectPrototypes` whose \
                   `wear_flags` contain the named slot (Head, Body, \
                   Mainhand, etc.). Case-insensitive; matches the \
                   `WearFlag` enum names from the schema.",
        },
        run: cmd_vwear,
    }
}

inventory::submit! {
    Command {
        names: &["zlist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "zlist",
            summary: "List every loaded zone (id + name).",
            long: "Builder+. Sorted by zone id. Use `znum <name>` \
                   to look up the id of a zone you only know by \
                   name, or `zsearch <substring>` for a fuzzy \
                   match.",
        },
        run: cmd_zlist,
    }
}

inventory::submit! {
    Command {
        names: &["znum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "znum <name>",
            summary: "Print the zone id whose name exactly matches.",
            long: "Builder+. Case-insensitive whole-name match. For \
                   substring search use `zsearch`.",
        },
        run: cmd_znum,
    }
}

inventory::submit! {
    Command {
        names: &["zsearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "zsearch <substring>",
            summary: "Find zones by name substring.",
            long: "Builder+. Case-insensitive substring against the \
                   zone's `Named`. Capped at 50 hits.",
        },
        run: cmd_zsearch,
    }
}

inventory::submit! {
    Command {
        names: &["clist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "clist",
            summary: "List every class with its catalog id.",
            long: "Builder+. Pairs the class plain name with its \
                   numeric id (the value `Profile.class_id` carries) \
                   so you can plug it into `set` / `skillset` / \
                   `advance` paths.",
        },
        run: cmd_clist,
    }
}

inventory::submit! {
    Command {
        names: &["csearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "csearch <substring>",
            summary: "Find classes by name substring.",
            long: "Builder+. Case-insensitive substring against the \
                   class's `plain_name`. Subclass rows are flagged.",
        },
        run: cmd_csearch,
    }
}

inventory::submit! {
    Command {
        names: &["osearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "osearch <substring>",
            summary: "Find object prototypes by name or keyword substring.",
            long: "Builder+. Case-insensitive substring match against \
                   each ObjectProto's name and keyword list. Prints \
                   `(zone, id) name` per match, capped at 50 results. \
                   Pair with `ostat <zone> <id>` for full proto detail.",
        },
        run: cmd_osearch,
    }
}

inventory::submit! {
    Command {
        names: &["msearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "msearch <substring>",
            summary: "Find mob prototypes by name or keyword substring.",
            long: "Builder+. Case-insensitive substring match against \
                   each MobProto's name and keywords. Same shape as \
                   `osearch`. Pair with `mstat <zone> <id>` for full \
                   proto detail.",
        },
        run: cmd_msearch,
    }
}

inventory::submit! {
    Command {
        names: &["rsearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rsearch <substring>",
            summary: "Find rooms by name substring.",
            long: "Builder+. Case-insensitive substring match against \
                   the room's `Named` value. Prints `(zone, id) name` \
                   per match, capped at 50.",
        },
        run: cmd_rsearch,
    }
}

fn parse_ability_kind(s: &str) -> Option<mud_db::abilities::AbilityKind> {
    use mud_db::abilities::AbilityKind as K;
    match s.to_ascii_lowercase().as_str() {
        "spell" | "spells" => Some(K::Spell),
        "chant" | "chants" => Some(K::Chant),
        "song" | "songs" => Some(K::Song),
        "skill" | "skills" => Some(K::Skill),
        _ => None,
    }
}

pub(crate) fn cmd_slist(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let filter_kind = if arg.is_empty() {
        None
    } else if let Some(k) = parse_ability_kind(arg) {
        Some(k)
    } else {
        send_to(
            world,
            player,
            format!("Unknown kind '{arg}'. Try: spell / chant / song / skill.\r\n"),
        );
        return;
    };
    let mut rows: Vec<(mud_db::abilities::AbilityKind, i32, String)> = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .filter(|d| filter_kind.is_none_or(|k| d.kind == k))
        .map(|d| (d.kind, d.id, d.plain_name.clone()))
        .collect();
    rows.sort_by(|a, b| a.0.label().cmp(b.0.label()).then_with(|| a.2.cmp(&b.2)));
    if rows.is_empty() {
        send_to(world, player, "No abilities match.\r\n");
        return;
    }
    let mut out = format!(
        "\r\n<b:cyan>{} abilit{} listed:</>\r\n",
        rows.len(),
        if rows.len() == 1 { "y" } else { "ies" },
    );
    let mut current_kind: Option<&'static str> = None;
    for (kind, id, name) in &rows {
        let label = kind.label();
        if current_kind != Some(label) {
            out.push_str(&format!("\r\n<cyan>{}s:</>\r\n", capitalize_first(label)));
            current_kind = Some(label);
        }
        out.push_str(&format!("  <dim>[{id:>4}]</> {name}\r\n"));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_snum(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: snum <name>\r\n");
        return;
    }
    let lc = needle.to_ascii_lowercase();
    match world
        .resource::<AbilityCatalog>()
        .by_name
        .get(&lc)
    {
        Some(d) => {
            crate::commands::send_rendered(
                world,
                player,
                &format!(
                    "<cyan>[{}]</> {} <dim>({})</>\r\n",
                    d.id,
                    d.plain_name,
                    d.kind.label(),
                ),
            );
        }
        None => send_to(
            world,
            player,
            format!("No ability named '{needle}'. Try `ssearch`.\r\n"),
        ),
    }
}

pub(crate) fn cmd_ssearch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: ssearch <substring>\r\n");
        return;
    }
    let needle_lc = needle.to_ascii_lowercase();
    let mut hits: Vec<(i32, String, &'static str)> = world
        .resource::<AbilityCatalog>()
        .by_name
        .values()
        .filter(|d| d.plain_name.to_ascii_lowercase().contains(&needle_lc))
        .map(|d| (d.id, d.plain_name.clone(), d.kind.label()))
        .collect();
    hits.sort_by(|a, b| a.1.cmp(&b.1));
    if hits.is_empty() {
        send_to(
            world,
            player,
            format!("No abilities match '{needle}'.\r\n"),
        );
        return;
    }
    let total = hits.len();
    let shown = total.min(SEARCH_RESULT_LIMIT);
    let mut out = format!(
        "\r\n<b:cyan>{shown} of {total} abilit{} for '{needle}':</>\r\n",
        if total == 1 { "y" } else { "ies" },
    );
    for (id, name, kind) in hits.iter().take(SEARCH_RESULT_LIMIT) {
        out.push_str(&format!(
            "  <dim>[{id:>4}]</> {name}  <dim>({kind})</>\r\n",
        ));
    }
    if total > SEARCH_RESULT_LIMIT {
        out.push_str(&format!(
            "  <dim>... {} more — narrow your search.</>\r\n",
            total - SEARCH_RESULT_LIMIT,
        ));
    }
    crate::commands::send_rendered(world, player, &out);
}

/// Title-case the first character. Local helper since the global
/// `capitalize` is title-case the whole word; we only want the
/// section header.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

/// Parse a case-insensitive `ObjectType` name. Returns `None` for
/// unrecognised input — the caller renders a helpful error.
fn parse_object_type(s: &str) -> Option<mud_db::enums::ObjectType> {
    use mud_db::enums::ObjectType as T;
    match s.to_ascii_lowercase().as_str() {
        "nothing" => Some(T::Nothing),
        "light" => Some(T::Light),
        "scroll" => Some(T::Scroll),
        "wand" => Some(T::Wand),
        "staff" => Some(T::Staff),
        "weapon" => Some(T::Weapon),
        "fireweapon" => Some(T::Fireweapon),
        "missile" => Some(T::Missile),
        "treasure" => Some(T::Treasure),
        "armor" => Some(T::Armor),
        "potion" => Some(T::Potion),
        "worn" => Some(T::Worn),
        "other" => Some(T::Other),
        "trash" => Some(T::Trash),
        "trap" => Some(T::Trap),
        "container" => Some(T::Container),
        "note" => Some(T::Note),
        "drinkcontainer" | "drink" => Some(T::Drinkcontainer),
        "key" => Some(T::Key),
        "food" => Some(T::Food),
        "money" => Some(T::Money),
        "pen" => Some(T::Pen),
        "boat" => Some(T::Boat),
        "fountain" => Some(T::Fountain),
        "portal" => Some(T::Portal),
        "rope" => Some(T::Rope),
        "spellbook" => Some(T::Spellbook),
        "wall" => Some(T::Wall),
        "touchstone" => Some(T::Touchstone),
        "board" => Some(T::Board),
        "instrument" => Some(T::Instrument),
        "vehicle" => Some(T::Vehicle),
        "corpse" => Some(T::Corpse),
        "kit" => Some(T::Kit),
        "wings" => Some(T::Wings),
        "perfume" => Some(T::Perfume),
        "disguise" => Some(T::Disguise),
        "poison" => Some(T::Poison),
        _ => None,
    }
}

fn parse_wear_flag(s: &str) -> Option<mud_db::enums::WearFlag> {
    use mud_db::enums::WearFlag as W;
    match s.to_ascii_lowercase().as_str() {
        "finger" => Some(W::Finger),
        "neck" => Some(W::Neck),
        "ear" | "ears" => Some(W::Ear),
        "wrist" => Some(W::Wrist),
        "head" => Some(W::Head),
        "eyes" => Some(W::Eyes),
        "face" => Some(W::Face),
        "body" => Some(W::Body),
        "about" => Some(W::About),
        "arms" => Some(W::Arms),
        "hands" => Some(W::Hands),
        "waist" => Some(W::Waist),
        "belt" => Some(W::Belt),
        "legs" => Some(W::Legs),
        "feet" => Some(W::Feet),
        "tail" => Some(W::Tail),
        "mainhand" | "wield" => Some(W::Mainhand),
        "offhand" | "hold" => Some(W::Offhand),
        "twohand" => Some(W::Twohand),
        "badge" => Some(W::Badge),
        "hover" => Some(W::Hover),
        "disguise" => Some(W::Disguise),
        _ => None,
    }
}

pub(crate) fn cmd_vitem(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(
            world,
            player,
            "Usage: vitem <type>  (try: weapon, armor, container, light, scroll, wand, staff, food, drinkcontainer, key, portal, ...)\r\n",
        );
        return;
    }
    let Some(kind) = parse_object_type(needle) else {
        send_to(
            world,
            player,
            format!("Unknown object type '{needle}'. Run with no args for the list of valid kinds.\r\n"),
        );
        return;
    };
    let mut hits: Vec<((i32, i32), String)> = world
        .resource::<ObjectPrototypes>()
        .by_key
        .iter()
        .filter(|(_, p)| p.r#type == kind)
        .map(|((z, id), p)| ((*z, *id), p.name.clone()))
        .collect();
    hits.sort_by_key(|((z, id), _)| (*z, *id));
    let label = format!("{kind:?}");
    render_search_results(world, player, "object", &label, &hits);
}

pub(crate) fn cmd_vwear(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(
            world,
            player,
            "Usage: vwear <slot>  (try: head, body, mainhand, offhand, finger, neck, wrist, hands, feet, ...)\r\n",
        );
        return;
    }
    let Some(flag) = parse_wear_flag(needle) else {
        send_to(
            world,
            player,
            format!("Unknown wear slot '{needle}'. Run with no args for the list.\r\n"),
        );
        return;
    };
    let mut hits: Vec<((i32, i32), String)> = world
        .resource::<ObjectPrototypes>()
        .by_key
        .iter()
        .filter(|(_, p)| p.wear_flags.contains(&flag))
        .map(|((z, id), p)| ((*z, *id), p.name.clone()))
        .collect();
    hits.sort_by_key(|((z, id), _)| (*z, *id));
    let label = format!("wear-{flag:?}");
    render_search_results(world, player, "object", &label, &hits);
}

pub(crate) fn cmd_zlist(world: &mut World, player: Entity, _args: &str) {
    let mut rows: Vec<(i32, String)> = {
        let mut q = world.query_filtered::<(&WorldKey, &Named), With<mud_world::Zone>>();
        q.iter(world).map(|(k, n)| (k.zone, n.name.clone())).collect()
    };
    rows.sort_by_key(|(id, _)| *id);
    if rows.is_empty() {
        send_to(world, player, "No zones loaded.\r\n");
        return;
    }
    let mut out = format!("\r\n<b:cyan>{} zone(s) loaded:</>\r\n", rows.len());
    for (id, name) in rows {
        out.push_str(&format!("  <dim>[{id:>3}]</> {name}\r\n"));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_znum(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: znum <name>\r\n");
        return;
    }
    let hit: Option<(i32, String)> = {
        let mut q = world.query_filtered::<(&WorldKey, &Named), With<mud_world::Zone>>();
        q.iter(world)
            .find(|(_, n)| {
                let plain = crate::commands::render_color_tags(
                    &n.name,
                    crate::commands::ColorMode::Strip,
                );
                plain.eq_ignore_ascii_case(needle)
            })
            .map(|(k, n)| (k.zone, n.name.clone()))
    };
    match hit {
        Some((id, name)) => {
            crate::commands::send_rendered(
                world,
                player,
                &format!("Zone <cyan>[{id}]</> {name}\r\n"),
            );
        }
        None => send_to(
            world,
            player,
            format!("No zone named '{needle}'. Try `zsearch`.\r\n"),
        ),
    }
}

pub(crate) fn cmd_zsearch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: zsearch <substring>\r\n");
        return;
    }
    let needle_lc = needle.to_ascii_lowercase();
    let mut hits: Vec<(i32, String)> = {
        let mut q = world.query_filtered::<(&WorldKey, &Named), With<mud_world::Zone>>();
        q.iter(world)
            .filter(|(_, n)| {
                let plain = crate::commands::render_color_tags(
                    &n.name,
                    crate::commands::ColorMode::Strip,
                );
                plain.to_ascii_lowercase().contains(&needle_lc)
            })
            .map(|(k, n)| (k.zone, n.name.clone()))
            .collect()
    };
    hits.sort_by_key(|(id, _)| *id);
    if hits.is_empty() {
        send_to(
            world,
            player,
            format!("No zones match '{needle}'.\r\n"),
        );
        return;
    }
    let total = hits.len();
    let shown = total.min(SEARCH_RESULT_LIMIT);
    let mut out = format!(
        "\r\n<b:cyan>{shown} of {total} zone(s) for '{needle}':</>\r\n",
    );
    for (id, name) in hits.iter().take(SEARCH_RESULT_LIMIT) {
        out.push_str(&format!("  <dim>[{id:>3}]</> {name}\r\n"));
    }
    if total > SEARCH_RESULT_LIMIT {
        out.push_str(&format!(
            "  <dim>... {} more — narrow your search.</>\r\n",
            total - SEARCH_RESULT_LIMIT,
        ));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_clist(world: &mut World, player: Entity, _args: &str) {
    let mut rows: Vec<(i32, String, bool, Option<i32>)> = world
        .resource::<ClassCatalog>()
        .by_id
        .values()
        .map(|d| (d.id, d.plain_name.clone(), d.is_subclass, d.parent_class_id))
        .collect();
    rows.sort_by_key(|(id, _, _, _)| *id);
    if rows.is_empty() {
        send_to(world, player, "No classes loaded.\r\n");
        return;
    }
    let mut out = format!("\r\n<b:cyan>{} class(es) loaded:</>\r\n", rows.len());
    for (id, name, sub, parent) in rows {
        let suffix = if sub {
            parent.map_or_else(
                || String::from("  <dim>(subclass)</>"),
                |p| format!("  <dim>(subclass of id {p})</>"),
            )
        } else {
            String::new()
        };
        out.push_str(&format!("  <dim>[{id:>2}]</> {name}{suffix}\r\n"));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_csearch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: csearch <substring>\r\n");
        return;
    }
    let needle_lc = needle.to_ascii_lowercase();
    let mut hits: Vec<(i32, String, bool)> = world
        .resource::<ClassCatalog>()
        .by_id
        .values()
        .filter(|d| d.plain_name.to_ascii_lowercase().contains(&needle_lc))
        .map(|d| (d.id, d.plain_name.clone(), d.is_subclass))
        .collect();
    hits.sort_by_key(|(id, _, _)| *id);
    if hits.is_empty() {
        send_to(world, player, format!("No classes match '{needle}'.\r\n"));
        return;
    }
    let mut out = format!(
        "\r\n<b:cyan>{} class(es) for '{needle}':</>\r\n",
        hits.len(),
    );
    for (id, name, sub) in hits {
        let suffix = if sub { "  <dim>(subclass)</>" } else { "" };
        out.push_str(&format!("  <dim>[{id:>2}]</> {name}{suffix}\r\n"));
    }
    crate::commands::send_rendered(world, player, &out);
}

pub(crate) fn cmd_osearch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: osearch <substring>\r\n");
        return;
    }
    let needle_lc = needle.to_ascii_lowercase();
    let mut hits: Vec<((i32, i32), String)> = world
        .resource::<ObjectPrototypes>()
        .by_key
        .iter()
        .filter(|(_, p)| {
            p.name.to_ascii_lowercase().contains(&needle_lc)
                || p.keywords
                    .iter()
                    .any(|k| k.to_ascii_lowercase().contains(&needle_lc))
        })
        .map(|((z, id), p)| ((*z, *id), p.name.clone()))
        .collect();
    // Stable order by (zone, id) so consecutive runs read the
    // same; HashMap iteration order is unstable otherwise.
    hits.sort_by_key(|((z, id), _)| (*z, *id));
    render_search_results(world, player, "object", needle, &hits);
}

pub(crate) fn cmd_msearch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: msearch <substring>\r\n");
        return;
    }
    let needle_lc = needle.to_ascii_lowercase();
    let mut hits: Vec<((i32, i32), String)> = world
        .resource::<MobPrototypes>()
        .by_key
        .iter()
        .filter(|(_, p)| {
            p.name.to_ascii_lowercase().contains(&needle_lc)
                || p.keywords
                    .iter()
                    .any(|k| k.to_ascii_lowercase().contains(&needle_lc))
        })
        .map(|((z, id), p)| ((*z, *id), p.name.clone()))
        .collect();
    hits.sort_by_key(|((z, id), _)| (*z, *id));
    render_search_results(world, player, "mob", needle, &hits);
}

pub(crate) fn cmd_rsearch(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Usage: rsearch <substring>\r\n");
        return;
    }
    let needle_lc = needle.to_ascii_lowercase();
    let mut hits: Vec<((i32, i32), String)> = {
        let mut q = world.query_filtered::<(&WorldKey, &Named), With<mud_world::Room>>();
        q.iter(world)
            .filter(|(_, n)| n.name.to_ascii_lowercase().contains(&needle_lc))
            .map(|(k, n)| ((k.zone, k.id), n.name.clone()))
            .collect()
    };
    hits.sort_by_key(|((z, id), _)| (*z, *id));
    render_search_results(world, player, "room", needle, &hits);
}

fn render_search_results(
    world: &World,
    player: Entity,
    kind: &str,
    needle: &str,
    hits: &[((i32, i32), String)],
) {
    if hits.is_empty() {
        send_to(
            world,
            player,
            format!("No {kind} prototypes match '{needle}'.\r\n"),
        );
        return;
    }
    let total = hits.len();
    let shown = total.min(SEARCH_RESULT_LIMIT);
    let plural = if total == 1 { "match" } else { "matches" };
    let mut out = format!(
        "\r\n<b:cyan>{shown} of {total} {kind} {plural} for '{needle}':</>\r\n",
    );
    for ((z, id), name) in hits.iter().take(SEARCH_RESULT_LIMIT) {
        out.push_str(&format!("  <dim>({z:>3}, {id:>4})</> {name}\r\n"));
    }
    if total > SEARCH_RESULT_LIMIT {
        out.push_str(&format!(
            "  <dim>... {} more — narrow your search.</>\r\n",
            total - SEARCH_RESULT_LIMIT,
        ));
    }
    crate::commands::send_rendered(world, player, &out);
}
