//! `report` (HP/stamina announcement to a player or the room) and
//! `socials` (lists every registered social verb).

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Health, MovementPoints, SocialRegistry, Stamina};

use crate::commands::{Category, Command, Help, send_to};

inventory::submit! {
    Command {
        names: &["report", "rep"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Communication,
        help: Help {
            usage: "report [<player>]",
            summary: "Announce your HP/stamina to the room or one player.",
            long: "With no argument, says your hit and movement points \
                   to everyone in the room. 'report <player>' tells \
                   that player instead (only they see it, and the \
                   usual tell refusals apply). Use 'greport' to \
                   report to your whole group.",
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
                   the matching social — 'nod', 'shrug', 'smile alice', \
                   etc.",
        },
        run: cmd_socials,
    }
}

/// Legacy `do_report` wording: "I have 80 (100) hit and 40 (50) movement
/// points." A mob's movement is its [`MovementPoints`] pool (mounts carry
/// one); players report their stamina, under the name the prompt and
/// `score` use for it.
pub(crate) fn report_body(world: &World, who: Entity) -> String {
    let hp = world.get::<Health>(who).copied();
    let mv = world
        .get::<MovementPoints>(who)
        .map(|m| (m.current, m.max, "movement"))
        .or_else(|| {
            world
                .get::<Stamina>(who)
                .map(|s| (s.current, s.max, "stamina"))
        });
    match (hp, mv) {
        (Some(h), Some((c, m, label))) => {
            format!(
                "I have {} ({}) hit and {c} ({m}) {label} points",
                h.hp, h.max
            )
        }
        (Some(h), None) => format!("I have {} ({}) hit points", h.hp, h.max),
        (None, Some((c, m, label))) => format!("I have {c} ({m}) {label} points"),
        (None, None) => "(no vital stats)".to_string(),
    }
}

/// Legacy `do_report`: `report <player>` tells that player the vitals line
/// (the `tell` path, so offline / `notell` / ignore refusals apply and only
/// the target sees it); a bare `report` says it to the room. Reporting to the
/// whole group is `greport`.
fn cmd_report(world: &mut World, player: Entity, args: &str) {
    let body = format!("{}.", report_body(world, player));
    match args.split_whitespace().next() {
        Some(target) => {
            crate::commands::tells::cmd_tell(world, player, &format!("{target} {body}"));
        }
        None => crate::commands::room_chat::cmd_say(world, player, &body),
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
    let cols = 5usize; // 2 + 5 * 14 = 72 columns, inside 80
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

#[cfg(test)]
mod socials_width_tests {
    use super::*;
    use crate::commands::test_support::{drain, player_in};

    #[test]
    fn socials_grid_fits_80_columns() {
        let mut world = World::new();
        let mut registry = SocialRegistry::default();
        for i in 0..40 {
            let name = format!("social{i:02}x");
            registry.by_name.insert(
                name.clone(),
                mud_world::SocialDef {
                    name,
                    hide: false,
                    staff_only: false,
                    char_no_arg: None,
                    others_no_arg: None,
                    char_found: None,
                    others_found: None,
                    vict_found: None,
                    not_found: None,
                    char_auto: None,
                    others_auto: None,
                },
            );
        }
        world.insert_resource(registry);
        let room = world.spawn_empty().id();
        let (p, mut rx) = player_in(&mut world, room);
        cmd_socials(&mut world, p, "");
        let out = drain(&mut rx);
        assert!(out.contains("40 socials available"), "{out}");
        for line in out.lines() {
            assert!(line.trim_end().chars().count() <= 80, "{line:?}");
        }
    }
}
