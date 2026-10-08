//! `drag <target> <direction>`: haul a corpse, a movable object or a
//! consenting body along behind you. Mirrors legacy `do_drag`.

use bevy_ecs::prelude::{Entity, World};
use mud_db::enums::{Direction, ObjectRestriction, PlayerFlag, UserRole};
use mud_world::{
    Account, Corpse, Exits, Fighting, Located, Mob, Player, PlayerCorpse, PlayerFlags, Posture,
    PostureKind, Stamina,
};

use crate::commands::{
    Category, Command, Help, broadcast_room_except_players_rendered, broadcast_room_player_diff,
    carry_capacity, cmd_look, cmd_move, direction_name, find_actor_in_room, find_in_room,
    has_restriction, is_staff, item_weight, name_of, parse_direction, send_room_players_snapshot,
    send_to,
};
use crate::room_access::{entry_allowed_following, room_visible_to};

inventory::submit! {
    Command {
        names: &["drag"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Movement,
        help: Help {
            usage: "drag <target> <direction>",
            summary: "Drag a corpse, object or consenting body with you.",
            long: "Takes the target (an object or corpse in the room, or a \
                   sitting/resting player who has used 'consent') along when \
                   you walk in the given direction. You must be standing. \
                   Anything you could pick up (or any corpse) can be dragged, \
                   up to three times what you can carry, and heavy loads cost \
                   extra stamina. Only staff can drag mobs. Player corpses \
                   are only draggable by their owner.",
        },
        run: cmd_drag,
    }
}

/// What `drag` resolved its first argument to.
#[derive(Clone, Copy)]
enum Dragged {
    Object(Entity),
    Body(Entity),
}

/// Extra stamina per drag on top of the destination sector's move cost:
/// one point per 50 weight, capped at four (legacy `min(4, weight/50)`).
#[allow(clippy::cast_possible_truncation)]
fn drag_extra_cost(weight: f64) -> i32 {
    ((weight / 50.0).floor() as i32).clamp(0, 4)
}

/// The legacy refusals for dragging `dragged`; `Some((entity, weight))`
/// when `player` may haul it, otherwise the refusal has been sent.
fn check_draggable(
    world: &mut World,
    player: Entity,
    dragged: Dragged,
    staff: bool,
) -> Option<(Entity, f64)> {
    let result = match dragged {
        Dragged::Object(item) => {
            if crate::corpses::is_unsettled(world, item) {
                send_to(world, player, "That body is still settling.\r\n");
                return None;
            }
            if !staff {
                let is_corpse = world.get::<Corpse>(item).is_some();
                if !is_corpse && has_restriction(world, item, ObjectRestriction::NoTake) {
                    send_to(world, player, "You cant drag that!\r\n");
                    return None;
                }
                if item_weight(world, item) > 3.0 * carry_capacity(world, player) {
                    send_to(world, player, "It is too heavy for you to drag.\r\n");
                    return None;
                }
                if world.get::<PlayerCorpse>(item).is_some() {
                    let owner_ok = name_of(world, item)
                        .strip_prefix("the corpse of ")
                        .is_some_and(|o| o.trim().eq_ignore_ascii_case(&name_of(world, player)));
                    if !owner_ok {
                        send_to(
                            world,
                            player,
                            "Not without consent you don't! That body is another adventurer's.\r\n",
                        );
                        return None;
                    }
                }
            }
            (item, item_weight(world, item))
        }
        Dragged::Body(body) => {
            if world.get::<Fighting>(body).is_some() {
                let name = name_of(world, body);
                send_to(world, player, format!("{name} is fighting!\r\n"));
                return None;
            }
            let consents = world
                .get::<PlayerFlags>(body)
                .is_some_and(|f| f.has(PlayerFlag::Consent));
            if staff {
                let outranks = |w: &World, e| w.get::<Account>(e).map_or(0, |a| a.role.rank());
                if outranks(world, body) >= outranks(world, player) && !consents {
                    send_to(
                        world,
                        player,
                        "You can't drag someone a higher level than you.\r\n",
                    );
                    return None;
                }
            } else {
                if world.get::<Mob>(body).is_some() {
                    send_to(world, player, "You can't drag NPC's!\r\n");
                    return None;
                }
                if !consents {
                    send_to(world, player, "Not without consent you don't!\r\n");
                    return None;
                }
                let relaxed = world
                    .get::<Posture>(body)
                    .is_none_or(|p| p.0 != PostureKind::Standing && p.0 != PostureKind::Kneeling);
                if !relaxed {
                    let name = name_of(world, body);
                    send_to(
                        world,
                        player,
                        format!("{name} isn't quite relaxed enough to be dragged.\r\n"),
                    );
                    return None;
                }
            }
            (body, 0.0)
        }
    };
    Some(result)
}

/// May `body` be hauled through `dir` out of `from_room`? `cmd_move` only
/// applies room entry rules to the dragger, so check the body here: the
/// destination must not be hidden from it (god zones) and its entry
/// restriction must admit it as the dragger's follower. A missing exit is
/// left for `cmd_move` to refuse.
fn body_may_enter(
    world: &mut World,
    player: Entity,
    body: Entity,
    from_room: Entity,
    dir: Direction,
) -> bool {
    let Some(dest) = world
        .get::<Exits>(from_room)
        .and_then(|e| e.0.get(&dir))
        .and_then(|ed| ed.to)
    else {
        return true;
    };
    room_visible_to(world, body, dest)
        && entry_allowed_following(world, body, dest, Some(player), true)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_drag(world: &mut World, player: Entity, args: &str) {
    let args = args.trim();
    let (target_word, dest_word) = args
        .split_once(char::is_whitespace)
        .map_or((args, ""), |(t, d)| (t, d.trim()));
    if target_word.is_empty() || dest_word.is_empty() {
        send_to(world, player, "Drag what? Where?\r\n");
        return;
    }
    let posture = world.get::<Posture>(player).map(|p| p.0);
    if posture.is_some_and(|p| p != PostureKind::Standing) {
        send_to(
            world,
            player,
            "You don't have the proper leverage to do that.  Try standing.\r\n",
        );
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "No way!  You're fighting for your life!\r\n");
        return;
    }
    let Some(from_room) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };

    // Never the dragger themself: `find_actor_in_room` excludes `player`,
    // and the self words get their own legacy line.
    if matches!(target_word.to_ascii_lowercase().as_str(), "me" | "self") {
        send_to(world, player, "One foot in front of the other, now...\r\n");
        return;
    }
    let staff = is_staff(world, player);
    let dragged = if let Some(actor) = find_actor_in_room(world, target_word, from_room, player) {
        Dragged::Body(actor)
    } else if let Some(item) = find_in_room(world, target_word, from_room) {
        Dragged::Object(item)
    } else {
        send_to(world, player, "Can't find that!\r\n");
        return;
    };

    let Some((subject, weight)) = check_draggable(world, player, dragged, staff) else {
        return;
    };

    let Some(dir) = parse_direction(dest_word) else {
        send_to(
            world,
            player,
            format!("Can't find a '{dest_word}' to drag into!\r\n"),
        );
        return;
    };

    if let Dragged::Body(body) = dragged
        && !body_may_enter(world, player, body, from_room, dir)
    {
        let name = name_of(world, body);
        send_to(
            world,
            player,
            format!("{name} can't be dragged that way.\r\n"),
        );
        return;
    }

    let extra = drag_extra_cost(weight);
    if !staff
        && world
            .get::<Stamina>(player)
            .is_some_and(|s| s.current < extra + 6)
    {
        send_to(world, player, "You are too exhausted!\r\n");
        return;
    }

    let player_name = name_of(world, player);
    let subject_name = name_of(world, subject);
    let dir_label = direction_name(dir);

    // `cmd_move` does every real movement check (exit, door, wall,
    // stamina, followers); if it left us in place the drag failed too.
    cmd_move(world, player, dir);
    let Some(to_room) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    if to_room == from_room {
        broadcast_room_except_players_rendered(
            world,
            from_room,
            &[player, subject],
            &format!(
                "Looking confused, {player_name} tried to drag {subject_name} {dir_label}.\r\n"
            ),
        );
        return;
    }

    if extra > 0
        && !staff
        && let Some(mut s) = world.get_mut::<Stamina>(player)
    {
        s.current = (s.current - extra).max(0);
    }
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player, subject],
        &format!("{player_name} drags {subject_name} behind them.\r\n"),
    );
    // A standing follower of the dragger already walked through `cmd_move`
    // (with its own GMCP diffs and look); don't move or announce it twice.
    let already_there = world
        .get::<Located>(subject)
        .is_some_and(|l| l.0 == to_room);
    if !already_there && world.get::<Located>(subject).is_some() {
        crate::combat::relocate(world, subject, to_room);
        if matches!(dragged, Dragged::Object(_)) {
            crate::corpses::queue_set_room(world, subject, to_room);
        }
    }
    let is_player_body =
        !already_there && matches!(dragged, Dragged::Body(b) if world.get::<Player>(b).is_some());
    if is_player_body {
        broadcast_room_player_diff(world, from_room, subject, "RemovePlayer");
        broadcast_room_player_diff(world, to_room, subject, "AddPlayer");
        send_room_players_snapshot(world, subject);
        let asleep = world
            .get::<Posture>(subject)
            .is_some_and(|p| p.0 == PostureKind::Sleeping);
        send_to(
            world,
            subject,
            format!("{player_name} drags you {dir_label} behind them.\r\n"),
        );
        if asleep {
            send_to(
                world,
                subject,
                "Your dreams grow bumpy, as if someone were dragging you...\r\n",
            );
        } else {
            cmd_look(world, subject, "");
        }
    }
    send_to(
        world,
        player,
        format!("You drag {subject_name} behind you.\r\n"),
    );
    broadcast_room_except_players_rendered(
        world,
        to_room,
        &[player, subject],
        &format!("{player_name} drags {subject_name} behind them.\r\n"),
    );
}

#[cfg(test)]
mod tests {
    use bevy_ecs::prelude::*;
    use mud_db::enums::{Direction, ExitState, ObjectRestriction, UserRole, effective_rank};
    use mud_world::*;

    use crate::commands::dispatch;
    use crate::commands::test_support::{Rx, drain, player_in};

    fn setup() -> (World, Entity, Entity, Entity, Rx) {
        let mut world = World::new();
        world.insert_resource(mud_script::LuaHost::default());
        world.insert_resource(WorldKeyIndex::default());
        world.insert_resource(WeatherCatalog::default());
        world.insert_resource(AbilityCatalog::default());
        world.insert_resource(EffectCatalog::default());
        world.insert_resource(RaceCatalog::default());
        world.insert_resource(RuntimeConfig::default());
        let zone = world.spawn((Zone, WorldKey { zone: 30, id: 0 })).id();
        let mut room = |id: i32| {
            world
                .spawn((
                    Room,
                    WorldKey { zone: 30, id },
                    Named {
                        name: format!("Room {id}"),
                    },
                    Located(zone),
                    Exits::default(),
                ))
                .id()
        };
        let a = room(1);
        let b = room(2);
        world.get_mut::<Exits>(a).unwrap().0.insert(
            Direction::South,
            ExitData {
                to: Some(b),
                state: ExitState::Open,
                key: None,
                description: None,
                keywords: Vec::new(),
                is_hidden: false,
                is_pickproof: false,
                is_bashable: false,
                hit_points: None,
            },
        );
        let (p, rx) = player_in(&mut world, a);
        world.entity_mut(p).insert((
            Online,
            Account {
                user_id: String::new(),
                character_id: "c-tester".into(),
                role: effective_rank(20, UserRole::Player),
                account_role: UserRole::Player,
                perms: vec![],
            },
            Profile {
                level: 20,
                class_id: None,
                race: "Human".into(),
                experience: 0,
                gender: "neutral".into(),
            },
        ));
        (world, a, b, p, rx)
    }

    fn corpse(world: &mut World, room: Entity, name: &str) -> Entity {
        world
            .spawn((
                Item,
                Corpse,
                Named { name: name.into() },
                Keywords(vec!["corpse".into()]),
                Located(room),
            ))
            .id()
    }

    fn room_of(world: &World, e: Entity) -> Entity {
        world.get::<Located>(e).unwrap().0
    }

    #[test]
    fn drag_corpse_moves_it_with_you() {
        let (mut world, a, b, p, mut rx) = setup();
        let c = corpse(&mut world, a, "the corpse of a goblin");
        dispatch(&mut world, p, "drag corpse s");
        let out = drain(&mut rx);
        assert!(
            out.contains("You drag the corpse of a goblin behind you."),
            "{out}"
        );
        assert!(!out.contains("grab yourself"), "{out}");
        assert_eq!(room_of(&world, p), b);
        assert_eq!(room_of(&world, c), b);
    }

    #[test]
    fn drag_object_by_name_and_full_direction_word() {
        let (mut world, a, b, p, mut rx) = setup();
        let crate_e = world
            .spawn((
                Item,
                Named {
                    name: "a wooden crate".into(),
                },
                Keywords(vec!["crate".into(), "wooden".into()]),
                Located(a),
            ))
            .id();
        dispatch(&mut world, p, "drag crate south");
        drain(&mut rx);
        assert_eq!(room_of(&world, p), b);
        assert_eq!(room_of(&world, crate_e), b);
    }

    #[test]
    fn drag_without_args_asks_what_and_where() {
        let (mut world, a, _b, p, mut rx) = setup();
        let c = corpse(&mut world, a, "the corpse of a goblin");
        dispatch(&mut world, p, "drag");
        assert!(drain(&mut rx).contains("Drag what? Where?"));
        dispatch(&mut world, p, "drag corpse");
        assert!(drain(&mut rx).contains("Drag what? Where?"));
        assert_eq!(room_of(&world, c), a);
        assert_eq!(room_of(&world, p), a);
    }

    #[test]
    fn drag_self_or_unknown_target_does_nothing() {
        let (mut world, a, _b, p, mut rx) = setup();
        dispatch(&mut world, p, "drag self s");
        assert!(drain(&mut rx).contains("One foot in front of the other"));
        dispatch(&mut world, p, "drag tester s");
        assert!(drain(&mut rx).contains("Can't find that!"));
        assert_eq!(room_of(&world, p), a);
    }

    #[test]
    fn drag_blocked_direction_leaves_object_and_player_put() {
        let (mut world, a, _b, p, mut rx) = setup();
        let c = corpse(&mut world, a, "the corpse of a goblin");
        dispatch(&mut world, p, "drag corpse n");
        drain(&mut rx);
        assert_eq!(room_of(&world, p), a);
        assert_eq!(room_of(&world, c), a);
    }

    #[test]
    fn drag_refuses_fixed_objects_and_other_players_corpses() {
        let (mut world, a, _b, p, mut rx) = setup();
        let fixture = world
            .spawn((
                Item,
                Named {
                    name: "a stone altar".into(),
                },
                Keywords(vec!["altar".into()]),
                ObjectRestrictions(vec![ObjectRestriction::NoTake]),
                Located(a),
            ))
            .id();
        dispatch(&mut world, p, "drag altar s");
        assert!(drain(&mut rx).contains("You cant drag that!"));
        assert_eq!(room_of(&world, fixture), a);

        let pc = corpse(&mut world, a, "the corpse of Bob");
        world
            .entity_mut(pc)
            .insert((PlayerCorpse, PlayerCorpseId(1)));
        dispatch(&mut world, p, "drag corpse s");
        assert!(drain(&mut rx).contains("Not without consent"));
        assert_eq!(room_of(&world, p), a);
        assert_eq!(room_of(&world, pc), a);
    }

    #[test]
    fn drag_refuses_a_player_corpse_whose_death_is_not_committed() {
        let (mut world, a, _b, p, mut rx) = setup();
        let c = corpse(&mut world, a, "the corpse of Tester");
        world.entity_mut(c).insert(PlayerCorpse);
        dispatch(&mut world, p, "drag corpse s");
        assert!(drain(&mut rx).contains("still settling"));
        assert_eq!(room_of(&world, c), a);
        assert_eq!(room_of(&world, p), a);
    }

    #[tokio::test]
    async fn dragging_your_corpse_updates_its_row_room() {
        let _lock = crate::commands::test_support::db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let Ok(Ok(pool)) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            mud_db::connect_with(&url, crate::commands::test_support::db_test_pool_settings()),
        )
        .await
        else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        if mud_db::sqlx::query("SELECT 1 FROM \"PlayerCorpses\" LIMIT 1")
            .execute(&pool)
            .await
            .is_err()
        {
            eprintln!("skipping: PlayerCorpses table missing");
            return;
        }
        let cid = format!("zd-{}", std::process::id());
        mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW()) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&cid)
        .bind(format!("Zd{}", std::process::id()))
        .execute(&pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let corpse_id = mud_db::player_corpses::insert(&mut conn, &cid, 30, 45, 0, 600)
            .await
            .unwrap();
        drop(conn);

        let (mut world, a, b, p, _rx) = setup();
        world.entity_mut(b).insert(WorldKey { zone: 30, id: 46 });
        let c = corpse(&mut world, a, "the corpse of Tester");
        world
            .entity_mut(c)
            .insert((PlayerCorpse, PlayerCorpseId(corpse_id)));
        world.insert_resource(crate::corpses::CorpseDb::spawn(pool.clone()));

        dispatch(&mut world, p, "drag corpse s");
        assert_eq!(room_of(&world, c), b);
        let mut room = (0, 0);
        for _ in 0..100 {
            room = mud_db::sqlx::query_as(
                "SELECT room_zone_id, room_id FROM \"PlayerCorpses\" WHERE id = $1",
            )
            .bind(corpse_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            if room == (30, 46) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(room, (30, 46));
        mud_db::sqlx::query("DELETE FROM \"Characters\" WHERE id = $1")
            .bind(&cid)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[test]
    fn drag_refuses_npcs_for_mortals() {
        let (mut world, a, _b, p, mut rx) = setup();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "a goblin".into(),
                },
                Keywords(vec!["goblin".into()]),
                Located(a),
            ))
            .id();
        dispatch(&mut world, p, "drag goblin s");
        assert!(drain(&mut rx).contains("You can't drag NPC's!"));
        assert_eq!(room_of(&world, mob), a);
        assert_eq!(room_of(&world, p), a);
    }

    #[test]
    fn drag_requires_standing() {
        let (mut world, a, _b, p, mut rx) = setup();
        let c = corpse(&mut world, a, "the corpse of a goblin");
        world.entity_mut(p).insert(Posture(PostureKind::Sitting));
        dispatch(&mut world, p, "drag corpse s");
        assert!(drain(&mut rx).contains("proper leverage"));
        assert_eq!(room_of(&world, c), a);
    }

    fn body_in(world: &mut World, room: Entity, name: &str) -> (Entity, Rx) {
        let (b, rx) = player_in(world, room);
        world.entity_mut(b).insert((
            Named { name: name.into() },
            Keywords(vec![name.to_ascii_lowercase()]),
            PlayerFlags(vec![mud_db::enums::PlayerFlag::Consent]),
            Posture(PostureKind::Sitting),
        ));
        (b, rx)
    }

    #[test]
    fn drag_body_into_room_that_refuses_it_is_blocked() {
        let (mut world, a, b, p, mut rx) = setup();
        let (body, _brx) = body_in(&mut world, a, "Bob");
        // Admits the level-20 dragger but not the level-5 body.
        world.entity_mut(body).insert(Profile {
            level: 5,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "neutral".into(),
        });
        world
            .entity_mut(b)
            .insert(EntryRestriction("return actor.level >= 20".into()));
        dispatch(&mut world, p, "drag bob s");
        let out = drain(&mut rx);
        assert!(out.contains("can't be dragged that way"), "{out}");
        assert_eq!(room_of(&world, p), a);
        assert_eq!(room_of(&world, body), a);
    }

    #[test]
    fn drag_body_into_god_zone_is_blocked() {
        let (mut world, a, b, p, mut rx) = setup();
        let (body, _brx) = body_in(&mut world, a, "Bob");
        let zone = room_of(&world, b);
        world.entity_mut(zone).insert(GodZone);
        // An immortal dragger may enter; the mortal body still may not.
        world.entity_mut(p).insert(Account {
            user_id: String::new(),
            character_id: "c-god".into(),
            role: effective_rank(100, UserRole::Player),
            account_role: UserRole::Player,
            perms: vec![],
        });
        dispatch(&mut world, p, "drag bob s");
        drain(&mut rx);
        assert_eq!(room_of(&world, body), a);
    }

    #[test]
    fn drag_body_into_unrestricted_room_works() {
        let (mut world, a, b, p, mut rx) = setup();
        let (body, _brx) = body_in(&mut world, a, "Bob");
        dispatch(&mut world, p, "drag bob s");
        drain(&mut rx);
        assert_eq!(room_of(&world, p), b);
        assert_eq!(room_of(&world, body), b);
    }

    #[test]
    fn drag_refuses_a_body_that_is_fighting() {
        let (mut world, a, _b, p, mut rx) = setup();
        let (body, _brx) = body_in(&mut world, a, "Bob");
        let foe = world.spawn((Mob, Located(a))).id();
        world.entity_mut(body).insert(Fighting(foe));
        dispatch(&mut world, p, "drag bob s");
        let out = drain(&mut rx);
        assert!(out.contains("Bob is fighting!"), "{out}");
        assert_eq!(room_of(&world, p), a);
        assert_eq!(room_of(&world, body), a);
    }
}
