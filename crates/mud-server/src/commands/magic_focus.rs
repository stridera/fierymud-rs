//! `concentrate` and `scribe`, ported from the C++ `magic_commands.cpp`.
//!
//! `concentrate` is `meditate` with a temporary Focus boost; the boost
//! lives in [`Concentrating`] and only counts while [`Meditating`] is
//! also set (the memorize tick reads both). `scribe` mirrors the C++
//! command, which validates the request and emits the flavor text but
//! does not write to a persistent spellbook (no such model exists in
//! either server).

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{
    AbilityCatalog, CoreStats, EquippedSlot, Fighting, Item, KnownAbilities, Located, Meditating,
    ObjectPrototypes, Posture, PostureKind, WorldKey,
};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, name_of, send_to, set_posture,
    try_insert, try_remove,
};

/// Temporary Focus bonus granted by `concentrate`. Cleared whenever
/// `Meditating` is cleared (toggle off, posture change).
#[derive(Component, Debug, Clone, Copy)]
pub struct Concentrating(pub i32);

/// Minimum proficiency (percent) required to scribe a spell.
const SCRIBE_MIN_PROFICIENCY: i32 = 50;

inventory::submit! {
    Command {
        names: &["concentrate"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "concentrate",
            summary: "Concentrate deeply to speed up spell memorization.",
            long: "Like `meditate`, but also grants a temporary Focus \
                   bonus (Intelligence / 4, at least 1) while it lasts. \
                   You sit down if standing. Cannot be used while \
                   fighting. Re-running `concentrate` (or standing, \
                   or being drawn into combat) breaks your \
                   concentration.",
        },
        run: cmd_concentrate,
    }
}

inventory::submit! {
    Command {
        names: &["scribe"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Magic,
        help: Help {
            usage: "scribe '<spell name>'",
            summary: "Scribe a known spell onto a blank scroll.",
            long: "You must be sitting, out of combat, carrying a \
                   scroll, and know the spell at 50% proficiency or \
                   better.",
        },
        run: cmd_scribe,
    }
}

fn cmd_concentrate(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't concentrate while fighting!\r\n");
        return;
    }
    let player_name = name_of(world, player);
    let room = world.get::<Located>(player).map(|l| l.0);
    if world.get::<Meditating>(player).is_some() {
        try_remove::<Meditating>(world, player);
        try_remove::<Concentrating>(world, player);
        send_to(world, player, "You break your concentration.\r\n");
        if let Some(room) = room {
            broadcast_room_except_players_rendered(
                world,
                room,
                &[player],
                &format!("{player_name} breaks their concentration.\r\n"),
            );
        }
        return;
    }
    match world.get::<Posture>(player).map(|p| p.0) {
        Some(PostureKind::Standing) => {
            set_posture(world, player, PostureKind::Sitting);
        }
        Some(PostureKind::Sitting | PostureKind::Resting | PostureKind::Kneeling) | None => {}
        Some(PostureKind::Sleeping) => {
            send_to(
                world,
                player,
                "You need to be sitting or standing to concentrate.\r\n",
            );
            return;
        }
    }
    let intelligence = world.get::<CoreStats>(player).map_or(0, |s| s.intelligence);
    let bonus = (intelligence / 4).max(1);
    try_insert(world, player, Meditating);
    try_insert(world, player, Concentrating(bonus));
    send_to(
        world,
        player,
        format!("You enter a state of deep concentration. (Focus +{bonus})\r\n"),
    );
    if let Some(room) = room {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} enters a state of deep concentration.\r\n"),
        );
    }
}

/// True when `player` carries (not wears) a Scroll-type item.
fn carries_scroll(world: &mut World, player: Entity) -> bool {
    let keys: Vec<(i32, i32)> = {
        let mut q =
            world.query_filtered::<(&Located, &WorldKey, Option<&EquippedSlot>), With<Item>>();
        q.iter(world)
            .filter(|(l, _, eq)| l.0 == player && eq.is_none())
            .map(|(_, k, _)| (k.zone, k.id))
            .collect()
    };
    let Some(protos) = world.get_resource::<ObjectPrototypes>() else {
        return false;
    };
    keys.iter().any(|k| {
        protos
            .by_key
            .get(k)
            .is_some_and(|p| p.r#type == ObjectType::Scroll)
    })
}

fn cmd_scribe(world: &mut World, player: Entity, args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(
            world,
            player,
            "If you wanna commit suicide just say so!\r\n",
        );
        return;
    }
    if world.get::<Posture>(player).map(|p| p.0) != Some(PostureKind::Sitting) {
        send_to(world, player, "You have to be sitting to scribe.\r\n");
        return;
    }
    let raw = args.trim();
    let raw = raw
        .strip_prefix(['\'', '"'])
        .and_then(|r| r.strip_suffix(['\'', '"']))
        .unwrap_or(raw)
        .trim();
    if raw.is_empty() {
        send_to(
            world,
            player,
            "What spell do you want to scribe?\r\nUsage: scribe '<spell name>'\r\n",
        );
        return;
    }
    let needle = raw.to_ascii_lowercase().replace(' ', "_");
    let def = {
        let catalog = world.resource::<AbilityCatalog>();
        catalog
            .find_by_prefix(
                &needle,
                None,
                world.get::<mud_world::KnownAbilities>(player),
            )
            .cloned()
    };
    let Some(def) = def else {
        send_to(
            world,
            player,
            format!("You don't know any spell called '{raw}'.\r\n"),
        );
        return;
    };
    if def.kind != AbilityKind::Spell {
        send_to(world, player, "You can only scribe spells, not skills.\r\n");
        return;
    }
    if !carries_scroll(world, player) {
        send_to(world, player, "You need a blank scroll to scribe on.\r\n");
        return;
    }
    let proficiency = world
        .get::<KnownAbilities>(player)
        .and_then(|k| k.entries.iter().find(|(id, _, _)| *id == def.id).copied())
        .map_or(0, |(_, prof, _)| prof);
    if proficiency < SCRIBE_MIN_PROFICIENCY {
        send_to(
            world,
            player,
            "You don't know that spell well enough to scribe it (need 50% proficiency).\r\n",
        );
        return;
    }
    send_to(
        world,
        player,
        format!("You carefully scribe the words for '{}'.\r\n", def.name),
    );
    let player_name = name_of(world, player);
    if let Some(room) = world.get::<Located>(player).map(|l| l.0) {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} carefully scribes a spell onto a scroll.\r\n"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, ability_def, drain, object_proto, player_in};
    use mud_world::{Keywords, Named};

    fn caster(posture: PostureKind, intelligence: i32) -> (World, Entity, Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert((
            Posture(posture),
            CoreStats {
                strength: 50,
                dexterity: 50,
                constitution: 50,
                intelligence,
                wisdom: 50,
                charisma: 50,
            },
        ));
        (world, player, rx)
    }

    #[test]
    fn concentrate_sits_up_and_grants_focus_bonus() {
        let (mut world, player, mut rx) = caster(PostureKind::Standing, 80);
        cmd_concentrate(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("Focus +20"), "{out}");
        assert!(world.get::<Meditating>(player).is_some());
        assert_eq!(world.get::<Concentrating>(player).unwrap().0, 20);
        assert_eq!(
            world.get::<Posture>(player).unwrap().0,
            PostureKind::Sitting
        );
    }

    #[test]
    fn concentrate_toggles_off_and_clears_bonus() {
        let (mut world, player, mut rx) = caster(PostureKind::Sitting, 4);
        cmd_concentrate(&mut world, player, "");
        // Minimum bonus is 1 even for tiny Intelligence.
        assert_eq!(world.get::<Concentrating>(player).unwrap().0, 1);
        cmd_concentrate(&mut world, player, "");
        assert!(drain(&mut rx).contains("You break your concentration."));
        assert!(world.get::<Meditating>(player).is_none());
        assert!(world.get::<Concentrating>(player).is_none());
    }

    #[test]
    fn concentrate_refused_in_combat() {
        let (mut world, player, mut rx) = caster(PostureKind::Standing, 50);
        let foe = world.spawn_empty().id();
        world.entity_mut(player).insert(Fighting(foe));
        cmd_concentrate(&mut world, player, "");
        assert!(drain(&mut rx).contains("while fighting"));
        assert!(world.get::<Meditating>(player).is_none());
    }

    #[test]
    fn scribe_requires_sitting() {
        let (mut world, player, mut rx) = caster(PostureKind::Standing, 50);
        cmd_scribe(&mut world, player, "'fireball'");
        assert!(drain(&mut rx).contains("You have to be sitting to scribe."));
    }

    #[test]
    fn scribe_full_path() {
        let (mut world, player, mut rx) = caster(PostureKind::Sitting, 50);
        let mut catalog = AbilityCatalog::default();
        catalog.by_name.insert(
            "fireball".to_string(),
            ability_def(7, "Fireball", AbilityKind::Spell),
        );
        catalog.by_name.insert(
            "kick".to_string(),
            ability_def(8, "Kick", AbilityKind::Skill),
        );
        world.insert_resource(catalog);
        let mut protos = ObjectPrototypes::default();
        protos
            .by_key
            .insert((1, 1), object_proto(1, 1, ObjectType::Scroll));
        world.insert_resource(protos);

        // No scroll yet.
        cmd_scribe(&mut world, player, "'fireball'");
        assert!(drain(&mut rx).contains("You need a blank scroll"));

        world.spawn((
            Item,
            Named {
                name: "a blank scroll".to_string(),
            },
            Keywords(vec!["scroll".to_string()]),
            WorldKey { zone: 1, id: 1 },
            Located(player),
        ));
        // Scroll but spell not known -> proficiency refusal.
        cmd_scribe(&mut world, player, "'fireball'");
        assert!(drain(&mut rx).contains("need 50% proficiency"));

        world.entity_mut(player).insert(KnownAbilities {
            entries: vec![(7, 60, true)],
        });
        cmd_scribe(&mut world, player, "'fireball'");
        assert!(drain(&mut rx).contains("You carefully scribe the words for 'Fireball'."));

        cmd_scribe(&mut world, player, "kick");
        assert!(drain(&mut rx).contains("only scribe spells"));
        cmd_scribe(&mut world, player, "nonsense");
        assert!(drain(&mut rx).contains("You don't know any spell called 'nonsense'."));
    }
}
