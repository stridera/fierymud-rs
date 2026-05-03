//! `report` (HP/stamina announcement to group or room) and
//! `socials` (lists every registered social verb).

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Health, Located, Player, SocialRegistry, Stamina};

use crate::commands::{
    Category, Command, Help, group_members, group_root, name_of, send_rendered, send_to,
};

inventory::submit! {
    Command {
        names: &["report", "rep"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "report",
            summary: "Announce your HP/stamina to your group or room.",
            long: "If you're grouped, the report goes to every group \
                   member regardless of room (useful for healers in \
                   adjacent rooms). Solo players announce to the \
                   room only.",
        },
        run: cmd_report,
    }
}

inventory::submit! {
    Command {
        names: &["socials"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "socials",
            summary: "List every available social verb.",
            long: "Sorted alphabetically, columnar layout. Use any \
                   listed verb on its own (or with a target) to fire \
                   the matching social — `nod`, `shrug`, `smile alice`, \
                   etc.",
        },
        run: cmd_socials,
    }
}

fn cmd_report(world: &mut World, player: Entity, _args: &str) {
    let hp = world.get::<Health>(player).copied();
    let stamina = world.get::<Stamina>(player).copied();
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let speaker = name_of(world, player);
    let body = match (hp, stamina) {
        (Some(h), Some(s)) => format!(
            "HP {}/{}, stamina {}/{}",
            h.hp, h.max, s.current, s.max
        ),
        (Some(h), None) => format!("HP {}/{}", h.hp, h.max),
        (None, Some(s)) => format!("stamina {}/{}", s.current, s.max),
        (None, None) => "(no vital stats)".to_string(),
    };
    let root = group_root(world, player);
    let group = group_members(world, root);
    let (targets, self_label, third_label) = if group.len() > 1 {
        (group, "your group", "the group")
    } else {
        let in_room: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == located.0)
                .map(|(e, _)| e)
                .collect()
        };
        (in_room, "the room", "the room")
    };
    for target in targets {
        let line = if target == player {
            format!("You report to {self_label}: {body}.\r\n")
        } else {
            format!("{speaker} reports to {third_label}: {body}.\r\n")
        };
        send_rendered(world, target, &line);
    }
}

fn cmd_socials(world: &mut World, player: Entity, _args: &str) {
    let mut names: Vec<String> = world
        .resource::<SocialRegistry>()
        .by_name
        .keys()
        .cloned()
        .collect();
    names.sort_unstable();
    let mut out = format!("\r\n{} socials available:\r\n", names.len());
    let cols = 6usize;
    let col_width = 14usize;
    for (i, name) in names.iter().enumerate() {
        if i % cols == 0 {
            out.push_str("  ");
        }
        out.push_str(&format!("{name:<col_width$}"));
        if i % cols == cols - 1 {
            out.push_str("\r\n");
        }
    }
    if !names.len().is_multiple_of(cols) {
        out.push_str("\r\n");
    }
    send_to(world, player, out);
}
