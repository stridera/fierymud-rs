//! Admin stat / show / set / scripterror / lua / trigger
//! commands. Both Command records and handler bodies live here.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, UserRole};
use mud_world::{
    AbilityCatalog, Account, AppliedTo, AttachedTriggers, ClassCatalog, CombatStats,
    EffectInstance, EquippedSlot, ExitData, Exits, Fighting, FromObjectReset, Health, Item,
    Keywords, Located, Mob, MobPrototypes, Named, ObjectPrototypes, Online, Player, PlayerFlags,
    Posture, Profile, RoomSector, ShopCatalog, Stamina, TriggerCatalog, Wealth, WorldKey,
    WorldKeyIndex, ZoneClimate,
};

use crate::TickCount;
use crate::commands::{
    AdminAuditLog, Category, Command, Help, direction_name, direction_rank, drain_lua_outbox,
    find_actor_in_room, find_in_room, matches, name_of, name_or, send_to,
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
            usage: "mstat <zone> <id>",
            summary: "Dump a mob prototype's metadata.",
            long: "Builder+. Reads `MobPrototypes[(zone, id)]` and \
                   prints the proto fields + linked behaviors / \
                   professions / abilities / triggers.",
        },
        run: cmd_mstat,
    }
}

inventory::submit! {
    Command {
        names: &["ostat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "ostat <zone> <id>",
            summary: "Dump an object prototype's metadata.",
            long: "Builder+. Mirrors `mstat` for objects: type, weight, \
                   wear flags, restrictions, special-values per type.",
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
            usage: "sstat <zone> <id>",
            summary: "Dump a shop's metadata.",
            long: "Builder+. Reads `ShopCatalog[(zone, id)]` for \
                   keeper, accept rules, items offered, pet roster.",
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
            usage: "tstat <zone> <id>",
            summary: "Dump a trigger's metadata.",
            long: "Builder+. Reads `TriggerCatalog[(zone, id)]` and \
                   prints flags, body length, last-fire stats.",
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
            usage: "rstat [<zone> <id>]",
            summary: "Dump a room's ECS state.",
            long: "Builder+. With no arg, inspects your current room. \
                   Otherwise looks up `WorldKeyIndex.rooms[(zone, id)]`.",
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
            usage: "stat <player>",
            summary: "Dump a player entity's component state.",
            long: "Builder+. Reads every component on the named \
                   player and prints a structured dump.",
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
            usage: "set <player> <field> <value>",
            summary: "Mutate a player field directly.",
            long: "Implementor-only. Field names match the writable \
                   columns on `Characters` (level, alignment, etc).",
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
            usage: "triggers [<zone> <id>]",
            summary: "List loaded triggers / inspect one.",
            long: "Builder+. With no args, lists every (zone, id) in \
                   the trigger catalog. With an id, prints body + \
                   flags + fire stats.",
        },
        run: cmd_triggers,
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

    // Targets: room itself + every mob/item/player whose Located == room,
    // unless the user named a specific keyword.
    let mut targets: Vec<Entity> = Vec::new();
    if arg.is_empty() || arg.eq_ignore_ascii_case("here") {
        targets.push(room);
        let mut q = world.query::<(Entity, &Located)>();
        for (e, l) in q.iter(world) {
            if l.0 == room {
                targets.push(e);
            }
        }
    } else if let Some(e) = find_in_room(world, arg, room)
        .or_else(|| find_actor_in_room(world, arg, room, player))
    {
        targets.push(e);
    } else {
        send_to(world, player, format!("No '{arg}' here.\r\n"));
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
pub(crate) fn cmd_mstat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: mstat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
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
    out.push_str(&format!("level:         {}\r\n", p.level));
    out.push_str(&format!("alignment:     {}\r\n", p.alignment));
    out.push_str(&format!("role:          {:?}\r\n", p.role));
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
    out.push_str(&format!("class_id:      {:?}\r\n", p.class_id));
    out.push_str(&format!("triggers:      {trig_count}\r\n"));
    out.push_str(&format!("live count:    {live}\r\n"));
    send_to(world, player, out);
}
pub(crate) fn cmd_ostat(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: ostat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
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
    out.push_str(&format!("type:          {:?}\r\n", p.r#type));
    out.push_str(&format!("wear_flags:    {:?}\r\n", p.wear_flags));
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
pub(crate) fn cmd_set(world: &mut World, player: Entity, args: &str) {
    let parts: Vec<&str> = args.splitn(3, char::is_whitespace).collect();
    if parts.len() != 3 || parts[1].trim().is_empty() || parts[2].trim().is_empty() {
        send_to(world, player, "Usage: set <target|me> <field> <value>\r\n");
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
                format!("Unknown field '{other}'. Try: level, xp, hp, maxhp, stamina, maxstamina, gold, alignment.\r\n"),
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
    let mut out = format!("\r\nLast {} of {total} trigger error(s):\r\n", n.min(total));
    for entry in log.entries.iter().rev().take(n) {
        let secs_ago = std::time::SystemTime::now()
            .duration_since(entry.at)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push_str(&format!(
            "  {ago:>4}s ago  ({zone}, {id}) [{event}] {name}\r\n         {msg}\r\n",
            ago = secs_ago,
            zone = entry.trigger_zone,
            id = entry.trigger_id,
            event = entry.event,
            name = entry.trigger_name,
            msg = entry.message,
        ));
    }
    send_to(world, player, out);
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
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: sstat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
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
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        send_to(world, player, "Usage: tstat <zone> <id>\r\n");
        return;
    }
    let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
        send_to(world, player, "Zone and id must be integers.\r\n");
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
    let parts: Vec<&str> = args.split_whitespace().collect();
    let room = if parts.is_empty() {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        located.0
    } else if parts.len() == 2 {
        let (Ok(zone), Ok(id)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) else {
            send_to(world, player, "Usage: rstat [<zone_id> <room_id>]\r\n");
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
    } else {
        send_to(world, player, "Usage: rstat [<zone_id> <room_id>]\r\n");
        return;
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
                    "proto:         weight {:.1}, level {}, type {:?}\r\n",
                    proto.weight, proto.level, proto.r#type,
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
