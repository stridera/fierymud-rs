//! Per-instance item text: `nameitem` (players name their bags, issue #68)
//! and `iedit` (staff edit one item instance online, issue #67).
//!
//! Legacy `FieryMUD`'s `iedit` (`oedit.cpp` `do_iedit`) pulled the object out
//! of the world and opened the full OLC menu on a copy. Here it is typed
//! lines only, and limited to what an item instance really persists in
//! `CharacterItems`: short description (`custom_name`), examine text
//! (`custom_examine_description`), the keyword override (`custom_values`)
//! and charges. Legacy had no player-facing item naming command.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{
    Charges, Item, ItemCustomization, Keywords, Located, Named, ObjectPrototypes, PendingSave,
    PlayerCorpse, WorldKey,
};

use crate::commands::{
    Category, Command, EquipFilter, Help, find_carried_by, find_in_room, name_of,
    record_admin_action, send_to, try_insert,
};
use crate::item_custom::{
    MAX_EXAMINE_LEN, MAX_STAFF_NAME_LEN, edit, holder_of, sanitize_keywords, sanitize_player_name,
    sanitize_staff_text,
};

inventory::submit! {
    Command {
        names: &["nameitem", "label"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "nameitem <container> <new name> | nameitem <container> clear",
            summary: "Give a bag or other container a name of your own.",
            long: "Renames a container you carry, e.g. 'nameitem sack \
                   Daedela's cloth sack'. The new name is what inventory \
                   and look show, and its words also work for targeting \
                   ('get gems daedela'). Names are 3-40 characters; colour \
                   codes and unusual symbols are removed. 'nameitem sack \
                   clear' restores the original name. Containers only.",
        },
        run: cmd_nameitem,
    }
}

inventory::submit! {
    Command {
        names: &["iedit"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "iedit <item> [name|examine|keywords|charges <value|clear>]",
            summary: "Edit one item instance online (Builder+).",
            long: "Edits the specific item you carry, wear or see in the \
                   room; the prototype is untouched. 'iedit <item>' shows \
                   the editable fields. 'iedit <item> name <text>' sets the \
                   short description, 'examine <text>' the examine text, \
                   'keywords <w1 w2 ...>' the keyword list, 'charges <n>' \
                   (-1 = unlimited). 'clear' on name/examine/keywords \
                   restores the prototype's. Colour tags are allowed. \
                   Changes apply at once and save with the holder.",
        },
        run: cmd_iedit,
    }
}

fn item_proto_type(world: &World, item: Entity) -> Option<ObjectType> {
    let key = world.get::<WorldKey>(item)?;
    world
        .resource::<ObjectPrototypes>()
        .by_key
        .get(&(key.zone, key.id))
        .map(|p| p.r#type)
}

fn cmd_nameitem(world: &mut World, player: Entity, args: &str) {
    let args = args.trim();
    let Some((needle, rest)) = args.split_once(char::is_whitespace) else {
        send_to(
            world,
            player,
            "Usage: nameitem <container> <new name>   (or 'clear' to restore)\r\n",
        );
        return;
    };
    let rest = rest.trim();
    let Some(item) = find_carried_by(world, needle, player, EquipFilter::Anywhere) else {
        send_to(
            world,
            player,
            format!("You aren't carrying '{needle}'.\r\n"),
        );
        return;
    };
    let old_name = name_of(world, item);
    if world.get::<PlayerCorpse>(item).is_some()
        || item_proto_type(world, item) != Some(ObjectType::Container)
    {
        send_to(
            world,
            player,
            format!("You can only name containers, and {old_name} isn't one.\r\n"),
        );
        return;
    }
    if rest.eq_ignore_ascii_case("clear") {
        if world
            .get::<ItemCustomization>(item)
            .is_none_or(|c| c.name.is_none())
        {
            send_to(
                world,
                player,
                format!("{old_name} has no custom name to clear.\r\n"),
            );
            return;
        }
        edit(world, item, |c| c.name = None);
        let new_name = name_of(world, item);
        try_insert(world, player, PendingSave);
        send_to(
            world,
            player,
            format!("You restore {old_name}'s original name: {new_name}.\r\n"),
        );
        return;
    }
    let new_name = match sanitize_player_name(rest) {
        Ok(n) => n,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    edit(world, item, |c| c.name = Some(new_name.clone()));
    try_insert(world, player, PendingSave);
    send_to(
        world,
        player,
        format!("You name {old_name} \"{new_name}\".\r\n"),
    );
}

fn cmd_iedit(world: &mut World, player: Entity, args: &str) {
    let args = args.trim();
    if args.is_empty() {
        send_to(
            world,
            player,
            "Usage: iedit <item> [name|examine|keywords|charges <value|clear>]\r\n",
        );
        return;
    }
    let (needle, rest) = args
        .split_once(char::is_whitespace)
        .map_or((args, ""), |(n, r)| (n, r.trim()));
    let item = find_carried_by(world, needle, player, EquipFilter::Anywhere).or_else(|| {
        let room = world.get::<Located>(player)?.0;
        find_in_room(world, needle, room)
    });
    let Some(item) = item else {
        send_to(world, player, "Item not found.\r\n");
        return;
    };
    if world.get::<Item>(item).is_none() {
        send_to(world, player, "Item not found.\r\n");
        return;
    }
    if rest.is_empty() {
        show_item(world, player, item);
        return;
    }
    let (field, value) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(f, v)| (f, v.trim()));
    let clear = value.eq_ignore_ascii_case("clear");
    let summary: Result<String, String> = match field.to_ascii_lowercase().as_str() {
        "name" | "short" => {
            if clear {
                edit(world, item, |c| c.name = None);
                Ok("name cleared".to_string())
            } else {
                sanitize_staff_text(value, MAX_STAFF_NAME_LEN).map(|text| {
                    edit(world, item, |c| c.name = Some(text.clone()));
                    format!("name set to '{text}'")
                })
            }
        }
        "examine" | "desc" | "description" => {
            if clear {
                edit(world, item, |c| c.examine = None);
                Ok("examine text cleared".to_string())
            } else {
                sanitize_staff_text(value, MAX_EXAMINE_LEN).map(|text| {
                    edit(world, item, |c| c.examine = Some(text.clone()));
                    format!("examine text set ({} chars)", text.chars().count())
                })
            }
        }
        "keywords" | "keyword" | "alias" => {
            if clear {
                edit(world, item, |c| c.keywords = None);
                Ok("keywords cleared".to_string())
            } else {
                sanitize_keywords(value).map(|words| {
                    let shown = words.join(" ");
                    edit(world, item, |c| c.keywords = Some(words));
                    format!("keywords set to '{shown}'")
                })
            }
        }
        "charges" => match value.parse::<i32>() {
            Ok(n) if (-1..=9999).contains(&n) => {
                try_insert(world, item, Charges(n));
                Ok(format!("charges set to {n}"))
            }
            _ => Err("Charges must be a number from -1 (unlimited) to 9999.\r\n".to_string()),
        },
        _ => Err(format!(
            "Unknown field '{field}'. Fields: name, examine, keywords, charges.\r\n"
        )),
    };
    let summary = match summary {
        Ok(s) => s,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    let label = item_label(world, item);
    record_admin_action(world, player, "iedit", &format!("{label}: {summary}"));
    let persisted = if let Some(holder) = holder_of(world, item) {
        try_insert(world, holder, PendingSave);
        "saves with its holder"
    } else {
        "not persisted: nobody is carrying it"
    };
    let name = name_of(world, item);
    send_to(
        world,
        player,
        format!("Edited {name}: {summary} ({persisted}).\r\n"),
    );
}

fn item_label(world: &World, item: Entity) -> String {
    let key = world.get::<WorldKey>(item).map_or_else(
        || "no-proto".to_string(),
        |k| format!("{}:{}", k.zone, k.id),
    );
    format!("{} [{key}]", name_of(world, item))
}

fn show_item(world: &mut World, player: Entity, item: Entity) {
    let custom = world
        .get::<ItemCustomization>(item)
        .cloned()
        .unwrap_or_default();
    let keywords = world
        .get::<Keywords>(item)
        .map_or_else(String::new, |k| k.0.join(" "));
    let charges = world
        .get::<Charges>(item)
        .map_or_else(|| "none".to_string(), |c| c.0.to_string());
    let examine = world
        .get::<mud_world::Description>(item)
        .map_or("(none)", |d| d.0.as_str())
        .to_string();
    let mark = |custom: bool| if custom { " (edited)" } else { "" };
    let named = world
        .get::<Named>(item)
        .map_or("?", |n| n.name.as_str())
        .to_string();
    let out = format!(
        "Item {}\r\n  name:     {named}{}\r\n  keywords: {keywords}{}\r\n  examine:  {examine}{}\r\n  charges:  {charges}\r\n",
        item_label(world, item),
        mark(custom.name.is_some()),
        mark(custom.keywords.is_some()),
        mark(custom.examine.is_some()),
    );
    send_to(world, player, out);
}
