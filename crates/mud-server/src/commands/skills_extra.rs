//! Thief / acrobat verbs ported from the C++ `skill_commands.cpp` and
//! `combat_commands.cpp`:
//!
//! * `palm <item>` — the DB-driven `PALM` skill (a `conceal_item`
//!   effect). The C++ command is a thin `execute_skill_command`
//!   wrapper; here the named item must be in your inventory and the
//!   skill pipeline does the proficiency / effect work.
//! * `stow` — quickly sheathe the wielded weapon into your pack (C++
//!   `cmd_stow`; it is a weapon-sheathe, not a container-put).
//! * `cartwheel` — the DB-driven `CARTWHEEL` attack skill ("acrobatic
//!   attack that evades while striking"); needs an active fight.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::UserRole;
use mud_world::{EquippedSlot, Fighting, Item, Located, Slot};

use crate::commands::{
    Category, Command, EquipFilter, Help, broadcast_room_except_players_rendered, check_stamina,
    drain_stamina, find_carried_by, invoke_ability, name_of, refresh_player_items_gmcp,
    require_alert_posture, send_to, skill_stamina_cost, try_remove,
};

const PALM_COST: i32 = 4;
const CARTWHEEL_COST: i32 = 6;

inventory::submit! {
    Command {
        names: &["palm"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "palm <item>",
            summary: "Secretly hide a small item in your hand.",
            long: "Rogue skill. Conceals an item from your inventory \
                   from casual detection for a while; others may spot \
                   it with perception. Uses the PALM ability's \
                   proficiency and effect from the database.",
        },
        run: cmd_palm,
    }
}

inventory::submit! {
    Command {
        names: &["stow"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Inventory,
        help: Help {
            usage: "stow",
            summary: "Quickly sheathe your wielded weapon.",
            long: "Moves the weapon in your wield slot into your \
                   inventory without the fuss of 'remove'.",
        },
        run: cmd_stow,
    }
}

inventory::submit! {
    Command {
        names: &["cartwheel"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Combat,
        help: Help {
            usage: "cartwheel",
            summary: "Tumble past your foe with an acrobatic strike.",
            long: "Acrobatic attack that evades while striking. \
                   Requires an active fight; strikes your current \
                   opponent using the CARTWHEEL ability from the \
                   database.",
        },
        run: cmd_cartwheel,
    }
}

fn cmd_palm(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(world, player, "Palm what?\r\n");
        return;
    }
    if !require_alert_posture(world, player, "palm") {
        return;
    }
    if find_carried_by(world, needle, player, EquipFilter::Inventory).is_none() {
        send_to(
            world,
            player,
            format!("You aren't carrying '{needle}'.\r\n"),
        );
        return;
    }
    let cost = skill_stamina_cost(world, "palm", PALM_COST);
    if !check_stamina(world, player, cost, "palm") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(world, player, "palm", AbilityKind::Skill, "use");
}

fn cmd_cartwheel(world: &mut World, player: Entity, _args: &str) {
    let Some(fighting) = world.get::<Fighting>(player).copied() else {
        send_to(world, player, "You need to be fighting to cartwheel!\r\n");
        return;
    };
    if !require_alert_posture(world, player, "cartwheel") {
        return;
    }
    if world.get_entity(fighting.0).is_err() {
        try_remove::<Fighting>(world, player);
        send_to(world, player, "Your target is gone.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "cartwheel", CARTWHEEL_COST);
    if !check_stamina(world, player, cost, "cartwheel") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, fighting.0);
    invoke_ability(
        world,
        player,
        &format!("cartwheel {target_name}"),
        AbilityKind::Skill,
        "use",
    );
}

fn cmd_stow(world: &mut World, player: Entity, _args: &str) {
    let weapon = {
        let mut q = world.query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
        q.iter(world)
            .find(|(_, l, eq)| l.0 == player && eq.0 == Slot::Wield)
            .map(|(e, _, _)| e)
    };
    let Some(weapon) = weapon else {
        send_to(world, player, "You aren't wielding anything.\r\n");
        return;
    };
    let weapon_name = name_of(world, weapon);
    crate::equip_apply::unapply_object_from_wearer(world, weapon, player);
    try_remove::<EquippedSlot>(world, weapon);
    send_to(
        world,
        player,
        format!("You quickly stow {weapon_name}.\r\n"),
    );
    let who = name_of(world, player);
    if let Some(room) = world.get::<Located>(player).map(|l| l.0) {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!("{who} quickly stows a weapon.\r\n"),
        );
    }
    crate::triggers::fire_item_event(world, weapon, player, mud_world::TriggerEvent::Remove);
    refresh_player_items_gmcp(world, player);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, drain, player_in};
    use mud_world::{Keywords, Named};

    fn setup() -> (World, Entity, Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, rx) = player_in(&mut world, room);
        (world, player, rx)
    }

    #[test]
    fn stow_moves_wielded_weapon_to_inventory() {
        let (mut world, player, mut rx) = setup();
        let sword = world
            .spawn((
                Item,
                Named {
                    name: "a rusty sword".to_string(),
                },
                Keywords(vec!["sword".to_string()]),
                Located(player),
                EquippedSlot(Slot::Wield),
            ))
            .id();
        cmd_stow(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("You quickly stow a rusty sword."), "{out}");
        assert!(world.get::<EquippedSlot>(sword).is_none());
        assert_eq!(world.get::<Located>(sword).unwrap().0, player);
    }

    #[test]
    fn stow_with_nothing_wielded() {
        let (mut world, player, mut rx) = setup();
        // A worn (non-wield) item must not be stowed.
        let ring = world
            .spawn((
                Item,
                Named {
                    name: "a ring".to_string(),
                },
                Located(player),
                EquippedSlot(Slot::LeftFinger),
            ))
            .id();
        cmd_stow(&mut world, player, "");
        assert!(drain(&mut rx).contains("You aren't wielding anything."));
        assert!(world.get::<EquippedSlot>(ring).is_some());
    }

    #[test]
    fn cartwheel_requires_a_fight() {
        let (mut world, player, mut rx) = setup();
        cmd_cartwheel(&mut world, player, "");
        assert!(drain(&mut rx).contains("You need to be fighting to cartwheel!"));
    }

    #[test]
    fn palm_needs_an_item_in_inventory() {
        let (mut world, player, mut rx) = setup();
        cmd_palm(&mut world, player, "");
        assert!(drain(&mut rx).contains("Palm what?"));
        cmd_palm(&mut world, player, "dagger");
        assert!(drain(&mut rx).contains("You aren't carrying 'dagger'."));
    }
}
