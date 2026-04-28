use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState};
use mud_net::Outbound;
use mud_world::{CombatStats, Exits, Fighting, Health, Located, Named, Online, Player};

/// A network connection attached to an entity. Owning the Outbound here
/// keeps the channel alive for the entity's whole lifetime.
#[derive(Component)]
pub struct Connection(pub Outbound);

pub fn dispatch(world: &mut World, player: Entity, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let (cmd, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    let lower = cmd.to_ascii_lowercase();

    match lower.as_str() {
        "look" | "l" => cmd_look(world, player),
        "who" => cmd_who(world, player),
        "say" | "'" => cmd_say(world, player, rest.trim_start()),
        "attack" | "kill" | "k" => cmd_attack(world, player, rest.trim()),
        "score" | "sc" => cmd_score(world, player),
        "north" | "n" => cmd_move(world, player, Direction::North),
        "south" | "s" => cmd_move(world, player, Direction::South),
        "east" | "e" => cmd_move(world, player, Direction::East),
        "west" | "w" => cmd_move(world, player, Direction::West),
        "up" | "u" => cmd_move(world, player, Direction::Up),
        "down" | "d" => cmd_move(world, player, Direction::Down),
        "ne" | "northeast" => cmd_move(world, player, Direction::Northeast),
        "nw" | "northwest" => cmd_move(world, player, Direction::Northwest),
        "se" | "southeast" => cmd_move(world, player, Direction::Southeast),
        "sw" | "southwest" => cmd_move(world, player, Direction::Southwest),
        "in" => cmd_move(world, player, Direction::In),
        "out" => cmd_move(world, player, Direction::Out),
        "quit" => send_to(world, player, "Goodbye!\r\n"),
        _ => send_to(world, player, format!("Unknown command: {cmd}\r\n")),
    }
}

fn cmd_look(world: &mut World, player: Entity) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    let room_name = world
        .get::<Named>(room)
        .map_or_else(|| "<nowhere>".to_string(), |n| n.name.clone());
    let exits: Vec<Direction> = world
        .get::<Exits>(room)
        .map(|e| e.0.keys().copied().collect())
        .unwrap_or_default();

    // Anything Named in the same room except self (players + mobs).
    let others: Vec<String> = {
        let mut q = world.query::<(Entity, &Located, &Named)>();
        q.iter(world)
            .filter(|(e, l, _)| *e != player && l.0 == room)
            .map(|(_, _, n)| n.name.clone())
            .collect()
    };

    let mut out = String::new();
    out.push_str(&format!("\r\n{room_name}\r\n"));
    if !others.is_empty() {
        out.push_str(&format!("Also here: {}\r\n", others.join(", ")));
    }
    if exits.is_empty() {
        out.push_str("Exits: none\r\n");
    } else {
        let names: Vec<&str> = exits.iter().map(|d| direction_name(*d)).collect();
        out.push_str(&format!("Exits: {}\r\n", names.join(", ")));
    }
    send_to(world, player, out);
}

fn cmd_who(world: &mut World, player: Entity) {
    let names: Vec<String> = {
        let mut q = world.query_filtered::<&Named, (With<Player>, With<Online>)>();
        q.iter(world).map(|n| n.name.clone()).collect()
    };
    let mut out = format!("\r\n{} online:\r\n", names.len());
    for name in &names {
        out.push_str(&format!("  {name}\r\n"));
    }
    send_to(world, player, out);
}

fn cmd_say(world: &mut World, player: Entity, message: &str) {
    if message.is_empty() {
        send_to(world, player, "Say what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let speaker = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(_, l)| l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };

    for target in targets {
        let line = if target == player {
            format!("You say, \"{message}\"\r\n")
        } else {
            format!("{speaker} says, \"{message}\"\r\n")
        };
        send_to(world, target, line);
    }
}

fn cmd_move(world: &mut World, player: Entity, dir: Direction) {
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;

    let exit = world
        .get::<Exits>(from_room)
        .and_then(|e| e.0.get(&dir).copied());
    let Some(exit) = exit else {
        send_to(world, player, "You can't go that way.\r\n");
        return;
    };
    if exit.state != ExitState::Open {
        send_to(world, player, "The way is closed.\r\n");
        return;
    }
    let Some(target) = exit.to else {
        send_to(world, player, "That exit leads nowhere.\r\n");
        return;
    };

    let mover_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());
    let dir_name = direction_name(dir);

    // Notify others in the *from* room before we leave.
    let from_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == from_room)
            .map(|(e, _)| e)
            .collect()
    };
    for o in from_others {
        send_to(world, o, format!("{mover_name} leaves {dir_name}.\r\n"));
    }

    // Move.
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }

    // Notify others in the *to* room after arrival.
    let to_others: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
        q.iter(world)
            .filter(|(e, l)| *e != player && l.0 == target)
            .map(|(e, _)| e)
            .collect()
    };
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    for o in to_others {
        send_to(
            world,
            o,
            format!("{mover_name} arrives from {arrival_dir}.\r\n"),
        );
    }

    // Show the new room to the mover.
    cmd_look(world, player);
}

pub(crate) fn send_to(world: &World, target: Entity, text: impl Into<String>) {
    if let Some(conn) = world.get::<Connection>(target) {
        let _ = conn.0.send(text.into());
    }
}

fn cmd_attack(world: &mut World, player: Entity, target_name: &str) {
    if target_name.is_empty() {
        send_to(world, player, "Attack what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target_lower = target_name.to_ascii_lowercase();

    // Find first matching entity in this room (case-insensitive substring).
    let target = {
        let mut q = world.query::<(Entity, &Located, &Named)>();
        q.iter(world)
            .find(|(e, l, n)| {
                *e != player
                    && l.0 == located.0
                    && n.name.to_ascii_lowercase().contains(&target_lower)
            })
            .map(|(e, _, _)| e)
    };

    let Some(target) = target else {
        send_to(
            world,
            player,
            format!("You don't see '{target_name}' here.\r\n"),
        );
        return;
    };

    let actual_name = world
        .get::<Named>(target)
        .map_or_else(String::new, |n| n.name.clone());
    let player_name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());

    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Fighting(target));
    }
    // Mob retaliates only if it has CombatStats.
    if world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }

    send_to(world, player, format!("You attack {actual_name}!\r\n"));
    send_to(world, target, format!("{player_name} attacks you!\r\n"));
    // Bystanders see the engagement
    let bystanders: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Located)>();
        q.iter(world)
            .filter(|(e, l)| *e != player && *e != target && l.0 == located.0)
            .map(|(e, _)| e)
            .collect()
    };
    for b in bystanders {
        send_to(world, b, format!("{player_name} attacks {actual_name}.\r\n"));
    }
}

fn cmd_score(world: &mut World, player: Entity) {
    let name = world
        .get::<Named>(player)
        .map_or_else(String::new, |n| n.name.clone());
    let hp = world.get::<Health>(player).copied();
    let cs = world.get::<CombatStats>(player).copied();
    let fighting = world.get::<Fighting>(player).copied();

    let mut out = format!("\r\n{name}\r\n");
    if let Some(hp) = hp {
        out.push_str(&format!("  HP: {} / {}\r\n", hp.hp, hp.max));
    }
    if let Some(cs) = cs {
        out.push_str(&format!(
            "  Hit roll: {}    Damage roll: {}    AC: {}    Alignment: {}\r\n",
            cs.hit_roll, cs.dmg_roll, cs.ac, cs.alignment
        ));
    }
    if let Some(f) = fighting {
        let target_name = world
            .get::<Named>(f.0)
            .map_or_else(|| "<gone>".to_string(), |n| n.name.clone());
        out.push_str(&format!("  Fighting: {target_name}\r\n"));
    }
    send_to(world, player, out);
}

fn direction_name(d: Direction) -> &'static str {
    use Direction::{
        Down, East, In, North, Northeast, Northwest, Out, Portal, South, Southeast, Southwest, Up,
        West,
    };
    match d {
        North => "north",
        South => "south",
        East => "east",
        West => "west",
        Up => "up",
        Down => "down",
        Northeast => "northeast",
        Northwest => "northwest",
        Southeast => "southeast",
        Southwest => "southwest",
        In => "in",
        Out => "out",
        Portal => "portal",
        Direction::None => "<none>",
    }
}

fn opposite(d: Direction) -> Option<Direction> {
    use Direction::{
        Down, East, In, North, Northeast, Northwest, Out, South, Southeast, Southwest, Up, West,
    };
    Some(match d {
        North => South,
        South => North,
        East => West,
        West => East,
        Up => Down,
        Down => Up,
        Northeast => Southwest,
        Southwest => Northeast,
        Northwest => Southeast,
        Southeast => Northwest,
        In => Out,
        Out => In,
        _ => return None,
    })
}
