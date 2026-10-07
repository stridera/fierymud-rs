//! Parser-cluster tests: abbreviation dispatch, `quit`, alias expansion,
//! the `toggle` list and the item-targeting syntax testers reported in #21.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectType, PlayerFlag, Sector, UserRole};
use mud_world::{
    Account, Aliases, Corpse, Fighting, Item, Keywords, Located, Mob, Named, ObjectPrototypes,
    PlayerFlags, Room, RoomSector, WorldKey,
};

use crate::commands::info::cmd_get;
use crate::commands::test_support::{Rx, drain, object_proto, player_in};
use crate::commands::{CommandOrigin, Quitting, dispatch, with_command_origin};

fn account(role: UserRole) -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role,
        account_role: role,
        perms: vec![],
    }
}

/// A player with an account, empty flags, and a connection to read from.
fn setup(role: UserRole) -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(ObjectPrototypes::default());
    world.insert_resource(mud_world::SocialRegistry::default());
    world.insert_resource(mud_world::WorldKeyIndex::default());
    let room = world.spawn((Room, RoomSector(Sector::Field))).id();
    let (player, rx) = player_in(&mut world, room);
    world
        .entity_mut(player)
        .insert((account(role), PlayerFlags(Vec::new())));
    (world, room, player, rx)
}

fn has(world: &World, player: Entity, flag: PlayerFlag) -> bool {
    world
        .get::<PlayerFlags>(player)
        .is_some_and(|f| f.has(flag))
}

fn set_aliases(world: &mut World, player: Entity, entries: &[(&str, &str)]) {
    world.entity_mut(player).insert(Aliases {
        entries: entries
            .iter()
            .map(|(a, c)| ((*a).to_string(), (*c).to_string()))
            .collect(),
    });
}

// ---- #1 abbreviation priority through the dispatcher ----

#[test]
fn abbreviations_of_any_length_dispatch_by_legacy_priority() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    // `to` is "toggle" (legacy order: toggle before touch/track).
    dispatch(&mut world, p, "to brief");
    assert!(has(&world, p, PlayerFlag::Brief));
    dispatch(&mut world, p, "tog compact");
    assert!(has(&world, p, PlayerFlag::Compact));
    drain(&mut rx);
    // Gibberish still reports an unknown command.
    dispatch(&mut world, p, "zzzzq");
    assert!(drain(&mut rx).contains("Unknown command"));
}

#[test]
fn destructive_commands_do_not_run_from_an_abbreviation() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    dispatch(&mut world, p, "ju");
    let out = drain(&mut rx);
    assert!(out.contains("Type the whole command"), "{out}");
}

// ---- #7 quit ----

#[test]
fn quit_says_goodbye_and_flags_the_session() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    dispatch(&mut world, p, "quit");
    assert!(drain(&mut rx).contains("Goodbye, friend.  Come back soon!"));
    assert!(world.get::<Quitting>(p).is_some());
}

#[test]
fn quit_yes_is_accepted_and_junk_is_not() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    dispatch(&mut world, p, "quit later");
    assert!(world.get::<Quitting>(p).is_none());
    assert!(drain(&mut rx).contains("Just type 'quit'"));
    dispatch(&mut world, p, "quit yes");
    assert!(world.get::<Quitting>(p).is_some());
}

#[test]
fn qui_gets_the_safety_reply_and_shorter_forms_are_not_quit() {
    // Legacy hidden `qui` entry: it refuses, `q`/`qu` are `quaff`.
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    dispatch(&mut world, p, "qui");
    assert!(drain(&mut rx).contains("For safety purposes, you must type out 'quit'"));
    assert!(world.get::<Quitting>(p).is_none());
    for typed in ["q", "qu"] {
        dispatch(&mut world, p, typed);
        let out = drain(&mut rx);
        assert!(out.contains("Quaff what?"), "`{typed}`: {out}");
        assert!(world.get::<Quitting>(p).is_none(), "`{typed}` quit");
    }
}

#[test]
fn quit_is_refused_mid_fight_for_mortals_only() {
    let (mut world, room, p, mut rx) = setup(UserRole::Player);
    let foe = world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Located(room),
        ))
        .id();
    world.entity_mut(p).insert(Fighting(foe));
    dispatch(&mut world, p, "quit");
    assert!(drain(&mut rx).contains("No way!  You're fighting for your life!"));
    assert!(world.get::<Quitting>(p).is_none());

    // Staff may leave regardless (legacy: immortals skip the check).
    let (mut world, room, god, mut rx) = setup(UserRole::Immortal);
    let foe = world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Located(room),
        ))
        .id();
    world.entity_mut(god).insert(Fighting(foe));
    dispatch(&mut world, god, "quit");
    assert!(world.get::<Quitting>(god).is_some());
    assert!(drain(&mut rx).contains("Goodbye"));
}

#[test]
fn quit_is_refused_while_casting() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    world.entity_mut(p).insert(mud_world::Casting {
        ability_id: 1,
        ability_name: "Mend".into(),
        args: String::new(),
        kind_label: "spell".into(),
        verb: "cast".into(),
        ticks_remaining: 8,
        ticks_total: 8,
        target: mud_world::CastTarget::Caster,
        recognized_by: Vec::new(),
        slot_reservation: None,
    });
    dispatch(&mut world, p, "quit");
    assert!(drain(&mut rx).contains("You are busy spellcasting"));
    assert!(world.get::<Quitting>(p).is_none());
}

#[test]
fn switched_mob_cannot_quit() {
    let mut world = World::new();
    let room = world.spawn((Room, RoomSector(Sector::Field))).id();
    let (mob, mut rx) = player_in(&mut world, room);
    // A connection-bearing entity that is not a Player (switched mob).
    world.entity_mut(mob).remove::<mud_world::Player>();
    world.entity_mut(mob).insert(Mob);
    dispatch(&mut world, mob, "quit");
    assert!(drain(&mut rx).contains("You can't quit while shapechanged!"));
    assert!(world.get::<Quitting>(mob).is_none());
}

// ---- #19 aliases ----

#[test]
fn simple_alias_replaces_the_line_and_drops_typed_arguments() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    set_aliases(&mut world, p, &[("b", "toggle brief")]);
    dispatch(&mut world, p, "b compact");
    assert!(has(&world, p, PlayerFlag::Brief));
    assert!(
        !has(&world, p, PlayerFlag::Compact),
        "typed args are dropped"
    );
}

#[test]
fn star_passes_everything_typed_after_the_alias() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    set_aliases(&mut world, p, &[("tt", "toggle $*")]);
    dispatch(&mut world, p, "tt compact");
    assert!(has(&world, p, PlayerFlag::Compact));
    // `$*` is the whole tail, multi-word included.
    dispatch(&mut world, p, "tt auto loot");
    // "autoloot" after normalising separators? No: two words stay two words.
    assert!(!has(&world, p, PlayerFlag::AutoLoot));
    set_aliases(&mut world, p, &[("tt", "toggle $*")]);
    dispatch(&mut world, p, "tt auto_loot");
    assert!(has(&world, p, PlayerFlag::AutoLoot));
}

#[test]
fn positional_arguments_substitute_one_to_nine() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    set_aliases(&mut world, p, &[("two", "toggle $2;toggle $1")]);
    dispatch(&mut world, p, "two brief compact");
    assert!(has(&world, p, PlayerFlag::Brief));
    assert!(has(&world, p, PlayerFlag::Compact));
    // Extra words beyond the placeholders are ignored.
    dispatch(&mut world, p, "two brief compact afk");
    assert!(!has(&world, p, PlayerFlag::Brief));
    assert!(!has(&world, p, PlayerFlag::Compact));
    assert!(!has(&world, p, PlayerFlag::Afk));
}

#[test]
fn missing_positional_argument_expands_to_nothing() {
    let apply = |r: &str, a: &str| crate::commands::apply_alias(r, a, 16 * 1024).unwrap();
    assert_eq!(apply("say $1 and $3", "a b"), vec!["say a and"]);
    assert_eq!(apply("say $$ $x", "a"), vec!["say $ x"]);
    assert_eq!(apply("look", "ignored"), vec!["look"]);
}

#[test]
fn semicolon_chains_commands_in_order() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    set_aliases(
        &mut world,
        p,
        &[("both", "toggle brief ; toggle compact;; toggle afk")],
    );
    dispatch(&mut world, p, "both");
    for f in [PlayerFlag::Brief, PlayerFlag::Compact, PlayerFlag::Afk] {
        assert!(has(&world, p, f), "{f:?}");
    }
}

#[test]
fn alias_does_not_reexpand_itself() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    // Wrapping the real command: `toggle` -> `toggle brief` runs the
    // command once instead of recursing.
    set_aliases(&mut world, p, &[("toggle", "toggle brief")]);
    dispatch(&mut world, p, "toggle");
    assert!(has(&world, p, PlayerFlag::Brief));
}

#[test]
fn mutually_recursive_aliases_terminate() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    set_aliases(&mut world, p, &[("ping", "pong"), ("pong", "ping")]);
    dispatch(&mut world, p, "ping");
    // Ends as an ordinary (unknown) command rather than looping.
    assert!(drain(&mut rx).contains("Unknown command"));
}

#[test]
fn alias_expansion_is_capped() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    let body = vec!["toggle brief"; 50].join(";");
    set_aliases(&mut world, p, &[("flood", body.as_str())]);
    dispatch(&mut world, p, "flood");
    let out = drain(&mut rx);
    assert_eq!(out.matches("Brief is now").count(), 32, "{out}");
    assert!(out.contains("too many commands"));
}

#[test]
fn nested_alias_chains_expand_and_are_depth_limited() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    set_aliases(
        &mut world,
        p,
        &[("outer", "inner"), ("inner", "toggle brief")],
    );
    dispatch(&mut world, p, "outer");
    assert!(has(&world, p, PlayerFlag::Brief));
    // A 12-deep chain trips the depth guard instead of running.
    let names: Vec<String> = (0..12).map(|i| format!("a{i}")).collect();
    let mut entries: Vec<(String, String)> = names
        .windows(2)
        .map(|w| (w[0].clone(), w[1].clone()))
        .collect();
    entries.push(("a11".into(), "toggle compact".into()));
    world.entity_mut(p).insert(Aliases { entries });
    drain(&mut rx);
    dispatch(&mut world, p, "a0");
    assert!(drain(&mut rx).contains("nested too deeply"));
    assert!(!has(&world, p, PlayerFlag::Compact));
}

#[test]
fn alias_cannot_reach_staff_commands_from_a_script() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Implementor);
    set_aliases(&mut world, p, &[("gg", "goto 30 5")]);
    // Typed by the (staff) player: permitted, so no refusal.
    dispatch(&mut world, p, "gg");
    assert!(!drain(&mut rx).contains("You can't do that."));
    // Queued by a script: the alias keeps the script origin and is refused.
    with_command_origin(CommandOrigin::Script, || dispatch(&mut world, p, "gg"));
    assert!(drain(&mut rx).contains("You can't do that."));
}

#[test]
fn quit_in_an_alias_chain_stops_the_rest() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    set_aliases(
        &mut world,
        p,
        &[("bye", "toggle brief;quit;toggle compact")],
    );
    dispatch(&mut world, p, "bye");
    assert!(has(&world, p, PlayerFlag::Brief));
    assert!(world.get::<Quitting>(p).is_some());
    assert!(!has(&world, p, PlayerFlag::Compact));
}

// ---- #15 toggle ----

#[test]
fn bare_toggle_lists_every_toggle_with_state() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    dispatch(&mut world, p, "toggle brief");
    drain(&mut rx);
    dispatch(&mut world, p, "toggle");
    let out = drain(&mut rx);
    assert!(out.contains("TOGGLES"), "{out}");
    for name in [
        "NoSummon",
        "Brief",
        "Compact",
        "AFK",
        "AutoLoot",
        "AutoGold",
        "AutoExit",
        "ShowDiceRolls",
        "Wimpy",
    ] {
        assert!(out.contains(name), "missing {name}: {out}");
    }
    // States: Brief is on, the rest off.
    assert!(out.contains("ON"));
    assert!(out.contains("OFF"));
    // God-only toggles are hidden from mortals.
    assert!(!out.contains("HolyLight"));
    assert!(!out.contains("Muted"));
}

#[test]
fn toggle_list_shows_staff_toggles_to_staff() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Builder);
    dispatch(&mut world, p, "toggle");
    let out = drain(&mut rx);
    assert!(
        out.contains("HolyLight") && out.contains("ShowIds"),
        "{out}"
    );
}

#[test]
fn toggle_accepts_names_without_underscores_and_abbreviations() {
    let (mut world, _room, p, _rx) = setup(UserRole::Player);
    for typed in [
        "showdicerolls",
        "ShowDiceRolls",
        "show_dice_rolls",
        "showd",
        "dice",
    ] {
        dispatch(&mut world, p, &format!("toggle {typed}"));
        assert!(has(&world, p, PlayerFlag::ShowDiceRolls), "{typed} on");
        dispatch(&mut world, p, &format!("toggle {typed}"));
        assert!(!has(&world, p, PlayerFlag::ShowDiceRolls), "{typed} off");
    }
    // First match in list order wins an ambiguous abbreviation (`auto`
    // reaches AutoExit before AutoLoot).
    dispatch(&mut world, p, "toggle auto");
    assert!(has(&world, p, PlayerFlag::AutoExit));
}

#[test]
fn toggle_unknown_and_god_only_flags_are_refused_for_mortals() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    dispatch(&mut world, p, "toggle nonsense");
    assert!(drain(&mut rx).contains("Toggle what"));
    dispatch(&mut world, p, "toggle holylight");
    assert!(!has(&world, p, PlayerFlag::HolyLight));
}

// ---- #21 targeting prefixes and optional prepositions ----

struct Gear {
    world: World,
    room: Entity,
    player: Entity,
}

fn gear() -> Gear {
    let (mut world, room, player, _rx) = setup(UserRole::Player);
    // Keep the receiver alive-free: tests assert on locations, not text.
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((30, 1), object_proto(30, 1, ObjectType::Container));
    world.insert_resource(protos);
    Gear {
        world,
        room,
        player,
    }
}

fn item(world: &mut World, name: &str, kws: &[&str], at: Entity) -> Entity {
    world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(kws.iter().map(|k| (*k).to_string()).collect()),
            Located(at),
        ))
        .id()
}

fn belt(g: &mut Gear, at: Entity) -> Entity {
    let b = item(&mut g.world, "a belt of potions", &["belt", "potions"], at);
    g.world.entity_mut(b).insert(WorldKey { zone: 30, id: 1 });
    b
}

fn loc(world: &World, e: Entity) -> Entity {
    world.get::<Located>(e).unwrap().0
}

#[test]
fn get_all_dot_filter_from_a_carried_belt_by_keyword() {
    let mut g = gear();
    let at = g.player;
    let belt = belt(&mut g, at);
    let id1 = item(
        &mut g.world,
        "a scroll of identify",
        &["scroll", "identify"],
        belt,
    );
    let id2 = item(
        &mut g.world,
        "a scroll of identify",
        &["scroll", "identify"],
        belt,
    );
    let fire = item(
        &mut g.world,
        "a scroll of fireball",
        &["scroll", "fireball"],
        belt,
    );
    // Drop the belt to the floor first: the tester's case is a belt on
    // the ground; carried is covered below.
    g.world.entity_mut(belt).insert(Located(g.room));
    cmd_get(&mut g.world, g.player, "all.id from potions");
    assert_eq!(loc(&g.world, id1), g.player);
    assert_eq!(loc(&g.world, id2), g.player);
    assert_eq!(loc(&g.world, fire), belt);
    // Prepositionless and carried-container forms behave the same.
    let id3 = item(
        &mut g.world,
        "a scroll of identify",
        &["scroll", "identify"],
        belt,
    );
    g.world.entity_mut(belt).insert(Located(g.player));
    cmd_get(&mut g.world, g.player, "all.id potions");
    assert_eq!(loc(&g.world, id3), g.player);
    assert_eq!(loc(&g.world, fire), belt);
}

#[test]
fn get_indexed_item_with_and_without_from() {
    let mut g = gear();
    let corpse = g
        .world
        .spawn((
            Item,
            Corpse,
            Named {
                name: "the corpse of a goblin".into(),
            },
            Keywords(vec!["corpse".into()]),
            Located(g.room),
        ))
        .id();
    // Newest arrival is listed first, so `2.sword` is the older one: put
    // `b` in first.
    let b = item(&mut g.world, "a bent sword", &["sword"], corpse);
    let a = item(&mut g.world, "a rusty sword", &["sword"], corpse);
    cmd_get(&mut g.world, g.player, "2.sword from corpse");
    assert_eq!(loc(&g.world, a), corpse);
    assert_eq!(loc(&g.world, b), g.player);
    // `b` goes back in as the newest arrival, so `2.sword` is now `a`.
    g.world.entity_mut(b).insert(Located(corpse));
    cmd_get(&mut g.world, g.player, "2.sword corpse");
    assert_eq!(loc(&g.world, a), g.player);
}

#[test]
fn get_from_an_indexed_container() {
    let mut g = gear();
    let mk = |g: &mut Gear| {
        g.world
            .spawn((
                Item,
                Corpse,
                Named {
                    name: "the corpse of a goblin".into(),
                },
                Keywords(vec!["corpse".into()]),
                Located(g.room),
            ))
            .id()
    };
    // Newest arrival is listed first, so `2.corpse` is the older one.
    let second = mk(&mut g);
    let first = mk(&mut g);
    let in_first = item(&mut g.world, "a coin", &["coin"], first);
    let in_second = item(&mut g.world, "a gem", &["gem"], second);
    cmd_get(&mut g.world, g.player, "all from 2.corpse");
    assert_eq!(loc(&g.world, in_first), first);
    assert_eq!(loc(&g.world, in_second), g.player);
    // And without the preposition.
    let in_second_b = item(&mut g.world, "a ruby", &["ruby"], second);
    cmd_get(&mut g.world, g.player, "all 2.corpse");
    assert_eq!(loc(&g.world, in_second_b), g.player);
}

#[test]
fn articles_are_skipped_like_legacy_fill_words() {
    let mut g = gear();
    let sword = item(&mut g.world, "a rusty sword", &["sword"], g.room);
    dispatch(&mut g.world, g.player, "get the sword");
    assert_eq!(loc(&g.world, sword), g.player);
    let bag = item(&mut g.world, "a leather bag", &["bag"], g.room);
    dispatch(&mut g.world, g.player, "put the sword in the bag");
    assert_eq!(loc(&g.world, sword), bag);
    dispatch(&mut g.world, g.player, "get the sword from the bag");
    assert_eq!(loc(&g.world, sword), g.player);
    dispatch(&mut g.world, g.player, "drop the sword");
    assert_eq!(loc(&g.world, sword), g.room);
}

#[test]
fn give_skips_articles_and_accepts_indexes() {
    let mut g = gear();
    let bob = g
        .world
        .spawn((
            Mob,
            Named { name: "Bob".into() },
            Keywords(vec!["bob".into()]),
            Located(g.room),
        ))
        .id();
    let sword = item(&mut g.world, "a rusty sword", &["sword"], g.player);
    dispatch(&mut g.world, g.player, "give the sword to the bob");
    assert_eq!(loc(&g.world, sword), bob);
}

#[test]
fn index_and_all_prefixes_work_across_item_verbs() {
    let mut g = gear();
    let bag = item(&mut g.world, "a leather bag", &["bag"], g.room);
    let swords: Vec<Entity> = ["rusty", "bent", "dull"]
        .iter()
        .map(|adj| {
            item(
                &mut g.world,
                &format!("a {adj} sword"),
                &["sword"],
                g.player,
            )
        })
        .collect();
    let held = |g: &Gear| {
        swords
            .iter()
            .filter(|&&s| loc(&g.world, s) == g.player)
            .count()
    };
    let in_bag = |g: &Gear| swords.iter().filter(|&&s| loc(&g.world, s) == bag).count();
    let on_floor = |g: &Gear| {
        swords
            .iter()
            .filter(|&&s| loc(&g.world, s) == g.room)
            .count()
    };
    // `2.x` takes exactly one match through each verb.
    dispatch(&mut g.world, g.player, "drop 2.sword");
    assert_eq!((held(&g), on_floor(&g)), (2, 1));
    dispatch(&mut g.world, g.player, "put 2.sword in bag");
    assert_eq!((held(&g), in_bag(&g)), (1, 1));
    // `all.x` sweeps every match, with or without a preposition.
    dispatch(&mut g.world, g.player, "get all.sword");
    assert_eq!(held(&g), 2);
    dispatch(&mut g.world, g.player, "put all.sword bag");
    assert_eq!(in_bag(&g), 3);
    dispatch(&mut g.world, g.player, "get 2.sword from bag");
    assert_eq!((held(&g), in_bag(&g)), (1, 2));
}

#[test]
fn look_and_examine_take_an_index() {
    let (mut world, room, p, mut rx) = setup(UserRole::Player);
    world.entity_mut(room).insert(RoomSector(Sector::City));
    // Newest arrival is listed first, so `2.sword` is the older (bent) one.
    item(&mut world, "a bent sword", &["sword"], room);
    item(&mut world, "a rusty sword", &["sword"], room);
    for verb in ["look", "look at", "examine", "look at the"] {
        dispatch(&mut world, p, &format!("{verb} 2.sword"));
        let out = drain(&mut rx);
        assert!(out.contains("bent"), "`{verb} 2.sword`: {out}");
        assert!(!out.contains("rusty"), "`{verb} 2.sword`: {out}");
    }
}

// ---- alias expansion bounds ----

#[test]
fn alias_dos_attack_is_rejected_with_bounded_work() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    // `$*` 2000 times per alias would copy the argument 2000x per level.
    let body = "$*".repeat(128);
    set_aliases(
        &mut world,
        p,
        &[("a", body.as_str()), ("b", "a $*"), ("c", "b $*;b $*;b $*")],
    );
    let args = "x".repeat(200);
    let start = std::time::Instant::now();
    dispatch(&mut world, p, &format!("c {args}"));
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    let out = drain(&mut rx);
    assert!(out.contains("Alias expansion refused"), "{out}");
    // Direct apply: the same 2000x `$*` is refused before allocating it.
    let huge = "$*".repeat(2000);
    let args = "y".repeat(1000);
    assert!(crate::commands::apply_alias(&huge, &args, 16 * 1024).is_err());
}

#[test]
fn alias_run_budget_spans_nested_levels() {
    use crate::commands::{AliasLimit, apply_alias};
    // Each line fits (<4096) but together they pass the budget.
    let args = "z".repeat(3000);
    let r = apply_alias("say $*;say $*", &args, 4000);
    assert_eq!(r, Err(AliasLimit::BudgetExceeded));
    let r = apply_alias("say $*$*", &"z".repeat(3000), 16 * 1024);
    assert_eq!(r, Err(AliasLimit::LineTooLong));
    assert!(apply_alias("say $*;say $*", &"z".repeat(1000), 16 * 1024).is_ok());
}

#[test]
fn alias_definition_limits_are_enforced_and_normal_aliases_work() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Player);
    let long = "x".repeat(257);
    dispatch(&mut world, p, &format!("alias big say {long}"));
    assert!(drain(&mut rx).contains("limited to 256"));
    assert!(
        world
            .get::<Aliases>(p)
            .is_none_or(|a| a.get("big").is_none())
    );
    dispatch(&mut world, p, "alias tb toggle brief");
    assert!(drain(&mut rx).contains("Alias 'tb' set"));
    dispatch(&mut world, p, "tb");
    assert!(has(&world, p, PlayerFlag::Brief));
    // 50 aliases max; redefining an existing one is still fine.
    for i in 0..49 {
        dispatch(&mut world, p, &format!("alias n{i} look"));
    }
    assert_eq!(world.get::<Aliases>(p).unwrap().entries.len(), 50);
    drain(&mut rx);
    dispatch(&mut world, p, "alias extra look");
    assert!(drain(&mut rx).contains("more than 50"));
    dispatch(&mut world, p, "alias tb toggle compact");
    assert!(drain(&mut rx).contains("updated"));
}

#[test]
fn staff_verbs_need_the_full_word() {
    let (mut world, _room, p, mut rx) = setup(UserRole::Implementor);
    for typed in ["zr", "ad", "du", "rer", "sla", "dev", "rena", "areloa"] {
        dispatch(&mut world, p, typed);
        let out = drain(&mut rx);
        assert!(out.contains("Type the whole command"), "`{typed}`: {out}");
    }
}

#[test]
fn ability_lookup_is_prefix_anchored_and_exact_wins() {
    use crate::commands::test_support::ability_def;
    use mud_db::abilities::AbilityKind::Spell;
    use mud_world::AbilityCatalog;

    let mut catalog = AbilityCatalog::default();
    for (id, name) in [
        (1, "invisibility"),
        (2, "mass_invisibility"),
        (3, "cure_light"),
        (4, "cure_critic"),
        (5, "fire"),
        (6, "fireball"),
    ] {
        catalog
            .by_name
            .insert(name.to_string(), ability_def(id, name, Spell));
    }
    let find = |n: &str| catalog.find_by_prefix(n, Some(Spell), None).map(|d| d.id);
    assert_eq!(
        find("invis"),
        Some(1),
        "invis must not reach mass_invisibility"
    );
    assert_eq!(find("mass inv"), Some(2));
    assert_eq!(find("sibility"), None, "substring no longer matches");
    assert_eq!(find("cure l"), Some(3), "multi-word prefix");
    assert_eq!(find("fire"), Some(5), "exact beats prefix");
    assert_eq!(find("fireb"), Some(6));
    assert_eq!(find("FIRE"), Some(5));
    assert_eq!(find("c l"), Some(3), "word-by-word abbreviation");
    assert_eq!(find("c c"), Some(4));
}

#[test]
fn ability_prefix_prefers_spells_the_caster_knows() {
    use crate::commands::test_support::ability_def;
    use mud_db::abilities::AbilityKind::Spell;
    use mud_world::{AbilityCatalog, KnownAbilities};

    let mut catalog = AbilityCatalog::default();
    for (id, name) in [(1, "fire_breath"), (2, "fireball"), (3, "fire_shield")] {
        catalog
            .by_name
            .insert(name.to_string(), ability_def(id, name, Spell));
    }
    // Fireball-only caster: `fire` is not an exact name, so the known
    // prefix match beats the alphabetically-first catalog entry.
    let known = KnownAbilities {
        entries: vec![(2, 100, true)],
    };
    let id =
        |k: Option<&KnownAbilities>| catalog.find_by_prefix("fire", Some(Spell), k).map(|d| d.id);
    assert_eq!(id(Some(&known)), Some(2));
    // Nothing known: alphabetical catalog fallback.
    assert_eq!(id(None), Some(1));
    assert_eq!(id(Some(&KnownAbilities::default())), Some(1));
}

#[test]
fn online_player_lookup_exact_name_beats_longer_prefix() {
    use crate::commands::find_online_player_anywhere;
    use mud_world::{Online, Player};
    let mut world = World::new();
    let me = world
        .spawn((Player, Online, Named { name: "Me".into() }))
        .id();
    let sam = world
        .spawn((Player, Online, Named { name: "Sam".into() }))
        .id();
    let samui = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Samui".into(),
            },
        ))
        .id();
    assert_eq!(
        find_online_player_anywhere(&mut world, "sam", me),
        Some(sam)
    );
    assert_eq!(
        find_online_player_anywhere(&mut world, "samu", me),
        Some(samui)
    );
    assert_eq!(find_online_player_anywhere(&mut world, "amui", me), None);
    // Without the exact `Sam`, the prefix is ambiguous and refused.
    world.despawn(sam);
    let _samantha = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Samantha".into(),
            },
        ))
        .id();
    assert_eq!(find_online_player_anywhere(&mut world, "sam", me), None);
}
