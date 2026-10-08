//! Output spacing (issue #53): a blank line sets the prompt off from the
//! preceding block unless COMPACT, unsolicited output breaks past an open
//! prompt line, and `look` separates the description from the listings.

use bevy_ecs::prelude::*;
use mud_db::enums::{PlayerFlag, Sector};
use mud_world::{Description, Item, Located, Named, PlayerFlags, Room, RoomSector};

use super::info::cmd_look;
use super::test_support::{Rx, drain, player_in};
use super::{PromptState, flush_prompts, note_player_input, send_prompt, send_to};

fn world_with_player() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.init_resource::<mud_world::MudClock>();
    world.insert_resource(PromptState::default());
    let room = world
        .spawn((
            Room,
            RoomSector(Sector::Structure),
            Named {
                name: "A Quiet Hall".to_string(),
            },
            Description("Stone walls rise on every side.".to_string()),
        ))
        .id();
    let (player, rx) = player_in(&mut world, room);
    (world, room, player, rx)
}

fn set_compact(world: &mut World, player: Entity) {
    let mut flags = PlayerFlags::default();
    flags.toggle(PlayerFlag::Compact);
    world.entity_mut(player).insert(flags);
}

/// Everything sent so far with the `%h/%H` default prompt reduced to `P>`.
fn plain(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c.is_ascii() {
            out.push(c);
        }
    }
    out
}

#[test]
fn prompt_after_output_is_preceded_by_a_blank_line() {
    let (mut world, _room, player, mut rx) = world_with_player();
    note_player_input(&world, player);
    send_to(&world, player, "You hit the hedge.\r\n");
    send_prompt(&mut world, player);
    let out = plain(&drain(&mut rx));
    assert!(
        out.starts_with("You hit the hedge.\r\n\r\n<"),
        "blank line before prompt: {out:?}"
    );
}

#[test]
fn compact_removes_the_blank_line_before_the_prompt() {
    let (mut world, _room, player, mut rx) = world_with_player();
    set_compact(&mut world, player);
    note_player_input(&world, player);
    send_to(&world, player, "You hit the hedge.\r\n");
    send_prompt(&mut world, player);
    let out = plain(&drain(&mut rx));
    assert!(
        out.starts_with("You hit the hedge.\r\n<"),
        "no blank line in compact: {out:?}"
    );
    assert!(!out.contains("\r\n\r\n"), "{out:?}");
}

#[test]
fn bare_enter_gets_one_blank_line_before_the_prompt() {
    let (mut world, _room, player, mut rx) = world_with_player();
    send_prompt(&mut world, player);
    drain(&mut rx);
    note_player_input(&world, player);
    send_prompt(&mut world, player);
    let out = plain(&drain(&mut rx));
    assert!(out.starts_with("\r\n<"), "one blank line: {out:?}");
    assert!(!out.starts_with("\r\n\r\n"), "{out:?}");
}

#[test]
fn bare_enter_in_compact_redraws_with_no_blank_line() {
    let (mut world, _room, player, mut rx) = world_with_player();
    set_compact(&mut world, player);
    send_prompt(&mut world, player);
    drain(&mut rx);
    note_player_input(&world, player);
    send_prompt(&mut world, player);
    let out = plain(&drain(&mut rx));
    assert!(out.starts_with('<'), "no leading newline: {out:?}");
}

#[test]
fn exactly_one_blank_line_whatever_the_output_ends_with() {
    for (text, expect) in [
        ("Ok.", "Ok.\r\n\r\n<"),
        ("Ok.\r\n", "Ok.\r\n\r\n<"),
        ("Ok.\r\n\r\n", "Ok.\r\n\r\n<"),
        ("Ok.\r\n\r\n\r\n", "Ok.\r\n\r\n\r\n<"),
        ("<red>Ok.\r\n</>", "Ok.\r\n\r\n<"),
        ("<red>Ok.\r\n\r\n</>", "Ok.\r\n\r\n<"),
    ] {
        let (mut world, _room, player, mut rx) = world_with_player();
        note_player_input(&world, player);
        send_to(&world, player, text);
        send_prompt(&mut world, player);
        let out = plain(&drain(&mut rx));
        assert!(out.starts_with(expect), "{text:?} -> {out:?}");
    }
}

#[test]
fn compact_never_adds_a_blank_line_but_still_starts_a_fresh_line() {
    for (text, expect) in [("Ok.", "Ok.\r\n<"), ("Ok.\r\n", "Ok.\r\n<")] {
        let (mut world, _room, player, mut rx) = world_with_player();
        set_compact(&mut world, player);
        note_player_input(&world, player);
        send_to(&world, player, text);
        send_prompt(&mut world, player);
        let out = plain(&drain(&mut rx));
        assert!(out.starts_with(expect), "{text:?} -> {out:?}");
    }
}

#[test]
fn unsolicited_output_breaks_past_the_open_prompt_line() {
    let (mut world, _room, player, mut rx) = world_with_player();
    send_prompt(&mut world, player);
    drain(&mut rx);
    // No player input in between: combat round / arrival.
    send_to(&world, player, "The hedge swings at you but misses.\r\n");
    send_to(&world, player, "A gardener leaves east.\r\n");
    send_prompt(&mut world, player);
    let out = plain(&drain(&mut rx));
    assert!(
        out.starts_with(
            "\r\nThe hedge swings at you but misses.\r\nA gardener leaves east.\r\n\r\n<"
        ),
        "{out:?}"
    );
}

#[test]
fn reply_to_typed_input_is_not_prefixed() {
    let (mut world, _room, player, mut rx) = world_with_player();
    send_prompt(&mut world, player);
    drain(&mut rx);
    note_player_input(&world, player);
    send_to(&world, player, "Ok.\r\n");
    flush_prompts(&mut world);
    let out = plain(&drain(&mut rx));
    assert!(out.starts_with("Ok.\r\n\r\n<"), "{out:?}");
}

#[test]
fn look_separates_description_from_the_ground_listing() {
    let (mut world, room, player, mut rx) = world_with_player();
    world.spawn((
        Item,
        Named {
            name: "a rusty key".to_string(),
        },
        Located(room),
    ));
    cmd_look(&mut world, player, "");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("Stone walls rise on every side.\r\n\r\na rusty key\r\n"),
        "{out:?}"
    );
}

#[test]
fn look_in_an_empty_room_adds_no_trailing_blank_line() {
    let (mut world, _room, player, mut rx) = world_with_player();
    cmd_look(&mut world, player, "");
    let out = plain(&drain(&mut rx));
    assert!(
        out.contains("Stone walls rise on every side.\r\nRoom.Players"),
        "{out:?}"
    );
}

/// Issue #88: login sends its own prompt after the enter-game output; the
/// next `flush_prompts` must not send a second one for that same output.
#[test]
fn an_explicit_prompt_is_not_repeated_by_the_next_flush() {
    let (mut world, _room, player, mut rx) = world_with_player();
    world
        .entity_mut(player)
        .insert(mud_world::Prompt("PROMPT> ".to_string()));
    send_to(&world, player, "Welcome, Tester.\r\n");
    send_prompt(&mut world, player);
    flush_prompts(&mut world);
    flush_prompts(&mut world);
    let out = plain(&drain(&mut rx));
    assert_eq!(out.matches("PROMPT>").count(), 1, "{out:?}");
}

#[test]
fn output_after_an_explicit_prompt_still_earns_a_new_prompt() {
    let (mut world, _room, player, mut rx) = world_with_player();
    world
        .entity_mut(player)
        .insert(mud_world::Prompt("PROMPT> ".to_string()));
    send_prompt(&mut world, player);
    send_to(&world, player, "A gnat bites you.\r\n");
    flush_prompts(&mut world);
    let out = plain(&drain(&mut rx));
    assert_eq!(out.matches("PROMPT>").count(), 2, "{out:?}");
}
