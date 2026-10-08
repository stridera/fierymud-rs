//! Per-instance item text: `nameitem` (players name their bags, issue #68).
//! Legacy `FieryMUD` had no player-facing item naming command; the name is
//! stored per instance in `CharacterItems.custom_name`.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{ItemCustomization, ObjectPrototypes, PendingSave, PlayerCorpse, WorldKey};

use crate::commands::{
    Category, Command, EquipFilter, Help, find_carried_by, name_of, send_to, try_insert,
};
use crate::item_custom::{edit, sanitize_player_name};

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
