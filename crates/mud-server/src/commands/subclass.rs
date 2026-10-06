//! `subclass` — specialize a base class into one of its subclasses
//! (Cleric -> Priest / Druid / ..., Warrior -> Paladin / ..., etc.).
//! Subclass relations come straight from the DB-driven `ClassCatalog`
//! (`Class.is_subclass` / `Class.parent_class_id`); the minimum level
//! is the `character.subclass_min_level` `GameConfig` tunable
//! (default 20, the C++ value).

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, ClassCatalog, ClassDef, Located, Profile, RuntimeConfig};

use crate::commands::{
    Category, Command, DbPool, Help, broadcast_room_except_players_rendered, name_of, send_to,
};

const DEFAULT_SUBCLASS_MIN_LEVEL: i32 = 20;

inventory::submit! {
    Command {
        names: &["subclass"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "subclass [specialization]",
            summary: "List or choose a subclass specialization.",
            long: "With no argument, lists the subclasses available to \
                   your class. With a name (or unique prefix), \
                   specializes into that subclass. Requires level 20 \
                   (the `character.subclass_min_level` config) and is \
                   permanent once chosen.",
        },
        run: cmd_subclass,
    }
}

/// Subclasses of `parent_id`, sorted by id for stable listings.
fn subclasses_of(catalog: &ClassCatalog, parent_id: i32) -> Vec<ClassDef> {
    let mut v: Vec<ClassDef> = catalog
        .by_id
        .values()
        .filter(|c| c.is_subclass && c.parent_class_id == Some(parent_id))
        .cloned()
        .collect();
    v.sort_by_key(|c| c.id);
    v
}

#[allow(clippy::too_many_lines)]
fn cmd_subclass(world: &mut World, player: Entity, args: &str) {
    let Some(profile) = world.get::<Profile>(player).cloned() else {
        send_to(world, player, "Only players can have subclasses.\r\n");
        return;
    };
    let Some(class_id) = profile.class_id else {
        send_to(world, player, "Your class is not configured correctly.\r\n");
        return;
    };
    let Some(current) = world
        .get_resource::<ClassCatalog>()
        .and_then(|c| c.by_id.get(&class_id))
        .cloned()
    else {
        send_to(world, player, "Your class is not configured correctly.\r\n");
        return;
    };
    if current.is_subclass {
        send_to(
            world,
            player,
            format!("You are already specialized as a {}.\r\n", current.name),
        );
        return;
    }
    let options = world
        .get_resource::<ClassCatalog>()
        .map(|c| subclasses_of(c, class_id))
        .unwrap_or_default();
    if options.is_empty() {
        send_to(
            world,
            player,
            "There are no subclass specializations available for your class.\r\n",
        );
        return;
    }
    let min_level =
        world
            .get_resource::<RuntimeConfig>()
            .map_or(DEFAULT_SUBCLASS_MIN_LEVEL, |cfg| {
                cfg.get_i32(
                    "character",
                    "subclass_min_level",
                    DEFAULT_SUBCLASS_MIN_LEVEL,
                )
            });
    let wanted = args.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        let mut out = String::from(
            "Available Subclass Specializations:\r\n\
             -----------------------------------\r\n",
        );
        for sc in &options {
            match sc.description.as_deref().filter(|d| !d.trim().is_empty()) {
                Some(d) => out.push_str(&format!("  {} ({}) - {}\r\n", sc.name, sc.plain_name, d)),
                None => out.push_str(&format!("  {} ({})\r\n", sc.name, sc.plain_name)),
            }
        }
        out.push_str(&format!(
            "\r\nUsage: subclass <specialization>\r\n\
             You must be at least level {min_level} to specialize.\r\n"
        ));
        send_to(world, player, out);
        return;
    }
    if profile.level < min_level {
        send_to(
            world,
            player,
            format!("You must be at least level {min_level} to choose a subclass.\r\n"),
        );
        return;
    }
    let Some(chosen) = options
        .iter()
        .find(|c| c.plain_name.to_ascii_lowercase().starts_with(&wanted))
        .cloned()
    else {
        send_to(
            world,
            player,
            format!(
                "'{}' is not an available subclass for your class.\r\n\
                 Type 'subclass' to see available options.\r\n",
                args.trim()
            ),
        );
        return;
    };

    if let Some(mut p) = world.get_mut::<Profile>(player) {
        p.class_id = Some(chosen.id);
    }
    if let (Some(pool), Some(account)) = (
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
        world.get::<Account>(player).map(|a| a.character_id.clone()),
    ) {
        let new_class = chosen.id;
        tokio::spawn(async move {
            if let Err(e) = mud_db::classes::set_character_class(&pool, &account, new_class).await {
                tracing::warn!(error = %e, "subclass persist failed");
            }
        });
    }
    send_to(
        world,
        player,
        format!(
            "You have chosen to specialize as a {}!\r\n\
             Your abilities and growth will now reflect your new specialization.\r\n",
            chosen.name
        ),
    );
    let who = name_of(world, player);
    if let Some(room) = world.get::<Located>(player).map(|l| l.0) {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{who} has specialized as a {}!\r\n", chosen.name),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, drain, player_in};
    use std::collections::HashMap;

    fn class(id: i32, name: &str, sub: bool, parent: Option<i32>) -> ClassDef {
        ClassDef {
            id,
            name: name.to_string(),
            plain_name: name.to_string(),
            is_subclass: sub,
            parent_class_id: parent,
            description: None,
            hit_dice: "1d8".to_string(),
            primary_stat: None,
            hp_per_level: 10,
            resistances: HashMap::new(),
        }
    }

    fn world_for(class_id: i32, level: i32) -> (World, Entity, Rx) {
        let mut world = World::new();
        let mut catalog = ClassCatalog::default();
        for c in [
            class(2, "Cleric", false, None),
            class(8, "Druid", true, Some(2)),
            class(16, "Priest", true, Some(2)),
            class(4, "Warrior", false, None),
        ] {
            catalog.by_id.insert(c.id, c);
        }
        world.insert_resource(catalog);
        let room = world.spawn_empty().id();
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert(Profile {
            level,
            class_id: Some(class_id),
            race: "HUMAN".to_string(),
            experience: 0,
            gender: "male".to_string(),
        });
        (world, player, rx)
    }

    #[test]
    fn lists_only_children_of_current_class() {
        let (mut world, player, mut rx) = world_for(2, 5);
        cmd_subclass(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("Druid") && out.contains("Priest"), "{out}");
        assert!(!out.contains("Warrior"), "{out}");
        assert!(out.contains("level 20"), "{out}");
    }

    #[test]
    fn warrior_without_subclasses_in_catalog() {
        let (mut world, player, mut rx) = world_for(4, 50);
        cmd_subclass(&mut world, player, "");
        assert!(drain(&mut rx).contains("no subclass specializations"));
    }

    #[test]
    fn level_gate_blocks_choice() {
        let (mut world, player, mut rx) = world_for(2, 19);
        cmd_subclass(&mut world, player, "druid");
        assert!(drain(&mut rx).contains("at least level 20"));
        assert_eq!(world.get::<Profile>(player).unwrap().class_id, Some(2));
    }

    #[test]
    fn choosing_by_prefix_changes_class_once() {
        let (mut world, player, mut rx) = world_for(2, 25);
        cmd_subclass(&mut world, player, "dru");
        assert!(drain(&mut rx).contains("specialize as a Druid"));
        assert_eq!(world.get::<Profile>(player).unwrap().class_id, Some(8));
        // Second attempt: already a subclass.
        cmd_subclass(&mut world, player, "priest");
        assert!(drain(&mut rx).contains("already specialized as a Druid"));
        assert_eq!(world.get::<Profile>(player).unwrap().class_id, Some(8));
    }

    #[test]
    fn unknown_subclass_rejected() {
        let (mut world, player, mut rx) = world_for(2, 25);
        cmd_subclass(&mut world, player, "paladin");
        assert!(drain(&mut rx).contains("not an available subclass"));
    }
}
