//! Playtest 2026-10-11, batch 1: aggro banner, social pronouns, death
//! line, slot refusal, peaceful-room cast order, `look <dir>` wrapping,
//! `wield`, drink label, bank amounts, rest line. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Direction, ExitState, UserRole};
use mud_world::{
    AbilityCatalog, Account, CombatStats, Exits, Fighting, Health, Item, Keywords, KnownAbilities,
    Located, Mob, Named, Profile, Room, SpellSlotData, WorldKey,
};

use super::test_support::{Rx, ability_def, drain, player_in};
use super::{
    Pronouns, Viewer, bank_transfer, disengage_attackers_of, engage_combat, invoke_ability_with,
    parse_bank_amount, run_social, spent_slots_message, substitute_for,
};

fn account() -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: UserRole::Player,
        account_role: UserRole::Player,
        perms: vec![],
    }
}

fn profile(gender: &str) -> Profile {
    Profile {
        level: 10,
        class_id: None,
        race: "HUMAN".into(),
        experience: 0,
        gender: gender.into(),
    }
}

fn room(world: &mut World) -> Entity {
    world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            Exits::default(),
        ))
        .id()
}

fn named_player(world: &mut World, room: Entity, name: &str, gender: &str) -> (Entity, Rx) {
    let (e, rx) = player_in(world, room);
    world.entity_mut(e).insert((
        Named { name: name.into() },
        CombatStats::default(),
        Health { hp: 100, max: 100 },
        account(),
        profile(gender),
    ));
    (e, rx)
}

// ---- #2: the attacker is not told "X sees Y and attacks!" ----

#[test]
fn aggro_banner_goes_to_victim_and_bystanders_only() {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    let r = room(&mut world);
    let (attacker, mut arx) = named_player(&mut world, r, "Alice", "female");
    let (defender, mut drx) = named_player(&mut world, r, "Bob", "male");
    let (_watcher, mut wrx) = named_player(&mut world, r, "Carol", "female");
    engage_combat(&mut world, attacker, defender, r);
    let a = drain(&mut arx);
    assert!(!a.contains("attacks!"), "attacker saw: {a}");
    assert!(drain(&mut drx).contains("Alice sees you and attacks!"));
    assert!(drain(&mut wrx).contains("Alice sees Bob and attacks!"));
}

// ---- #3: social pronouns follow the real sex, per viewer ----

#[test]
fn social_templates_use_real_pronouns_per_viewer() {
    let alice = ("Alice", Pronouns::FEMALE);
    let bob = ("Bob", Pronouns::MALE);
    let t = "{actor.name} hugs {target.pronoun.objective}; \
             {target.pronoun.Subjective} is {target.pronoun.possessive} own.";
    assert_eq!(
        substitute_for(t, Viewer::Actor, alice, Some(bob)),
        "Alice hugs him; He is his own."
    );
    assert_eq!(
        substitute_for(t, Viewer::Target, alice, Some(bob)),
        "Alice hugs you; You is your own."
    );
    assert_eq!(
        substitute_for(t, Viewer::Bystander, alice, Some(bob)),
        "Alice hugs him; He is his own."
    );
    let mob = ("a rat", Pronouns::IT);
    assert_eq!(
        substitute_for(
            "Hug {target.pronoun.objective}",
            Viewer::Actor,
            alice,
            Some(mob)
        ),
        "Hug it"
    );
    // The actor reads themself in the second person.
    assert_eq!(
        substitute_for("Hit {actor.pronoun.reflexive}", Viewer::Actor, alice, None),
        "Hit yourself"
    );
    assert_eq!(
        substitute_for(
            "Hit {actor.pronoun.reflexive}",
            Viewer::Bystander,
            alice,
            None
        ),
        "Hit herself"
    );
}

#[test]
fn hug_a_player_reads_with_their_real_pronoun_and_you() {
    let mut world = World::new();
    let r = room(&mut world);
    let (alice, mut arx) = named_player(&mut world, r, "Alice", "female");
    let (_bob, mut brx) = named_player(&mut world, r, "Bob", "male");
    let (_carol, mut crx) = named_player(&mut world, r, "Carol", "female");
    let social = mud_world::SocialDef {
        name: "hug".into(),
        hide: false,
        staff_only: false,
        char_no_arg: None,
        others_no_arg: None,
        char_found: Some("You give {target.pronoun.objective} a hug.".into()),
        others_found: Some("{actor.name} hugs {target.name}.".into()),
        vict_found: Some("{actor.name} hugs {target.pronoun.reflexive}.".into()),
        not_found: None,
        char_auto: None,
        others_auto: None,
    };
    run_social(&mut world, alice, &social, "bob");
    assert!(drain(&mut arx).contains("You give him a hug."));
    assert!(drain(&mut brx).contains("Alice hugs yourself."));
    assert!(drain(&mut crx).contains("Alice hugs Bob."));
}

// ---- #4: no "Your target falls." ----

#[test]
fn disengaging_attackers_is_silent() {
    let mut world = World::new();
    let r = room(&mut world);
    let (p, mut rx) = named_player(&mut world, r, "Alice", "female");
    let mob = world
        .spawn((
            Mob,
            Named {
                name: "a rat".into(),
            },
            Located(r),
        ))
        .id();
    world.entity_mut(p).insert(Fighting(mob));
    disengage_attackers_of(&mut world, mob);
    assert!(world.get::<Fighting>(p).is_none());
    assert!(drain(&mut rx).is_empty());
}

// ---- #11: a circle with no slots at all ----

#[test]
fn zero_slot_circle_is_not_yet_castable() {
    assert_eq!(
        spent_slots_message(4, 0, 0),
        "You can't cast circle 4 spells yet.\r\n"
    );
    let spent = spent_slots_message(2, 2, 2);
    assert!(spent.contains("Wait for one to recover"), "{spent}");
}

// ---- #12: the peaceful-room refusal waits for target resolution ----

fn peaceful_caster() -> (World, Entity, Rx) {
    let mut world = World::new();
    let r = room(&mut world);
    world.entity_mut(r).insert(mud_world::PeacefulRoom);
    let mut catalog = AbilityCatalog::default();
    for (id, name, violent) in [(1, "ray", true), (2, "buff", false)] {
        let mut def = ability_def(id, name, AbilityKind::Spell);
        def.violent = violent;
        def.cast_time_rounds = 0;
        catalog.by_name.insert(name.into(), def);
    }
    world.insert_resource(catalog);
    world.insert_resource(mud_world::EffectCatalog::default());
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    let (p, rx) = named_player(&mut world, r, "Alice", "female");
    world.entity_mut(p).insert(KnownAbilities {
        entries: vec![(1, 500, true), (2, 500, true)],
    });
    let goblin = world
        .spawn((
            Mob,
            Named {
                name: "a goblin".into(),
            },
            Keywords(vec!["goblin".into()]),
            Located(r),
            Health { hp: 50, max: 50 },
        ))
        .id();
    let _ = goblin;
    (world, p, rx)
}

fn cast(world: &mut World, p: Entity, args: &str) {
    invoke_ability_with(
        world,
        p,
        args,
        AbilityKind::Spell,
        "cast",
        false,
        false,
        false,
        None,
    );
}

#[test]
fn peaceful_room_reports_a_missing_target_before_the_peace_lecture() {
    let (mut world, p, mut rx) = peaceful_caster();
    cast(&mut world, p, "'ray' nobody");
    let out = drain(&mut rx);
    assert!(out.contains("You don't see 'nobody' here"), "{out}");
    assert!(!out.contains("peaceful aura"), "{out}");
}

#[test]
fn peaceful_room_still_refuses_a_resolved_hostile_cast() {
    let (mut world, p, mut rx) = peaceful_caster();
    cast(&mut world, p, "'ray' goblin");
    assert!(drain(&mut rx).contains("A peaceful aura forbids hostile magic here."));
    // Dispel-class spells are violent (legacy), even aimed at oneself.
    cast(&mut world, p, "'ray' me");
    assert!(drain(&mut rx).contains("A peaceful aura forbids hostile magic here."));
}

#[test]
fn peaceful_room_allows_a_beneficial_self_cast() {
    let (mut world, p, mut rx) = peaceful_caster();
    cast(&mut world, p, "'buff' me");
    let out = drain(&mut rx);
    assert!(!out.contains("peaceful aura"), "{out}");
}

// ---- #17: `look <dir>` wraps ----

#[test]
fn look_direction_wraps_the_destination_description() {
    let mut fx = super::gmcp_tests::fixture();
    let (a, b) = (fx.a, fx.b);
    fx.world.get_mut::<mud_world::Description>(b).unwrap().0 =
        "word ".repeat(120).trim_end().to_string();
    fx.world.get_mut::<Exits>(a).unwrap().0.insert(
        Direction::North,
        super::gmcp_tests::exit(b, ExitState::Open, false, &[]),
    );
    let (p, mut rx) = super::gmcp_tests::player(&mut fx.world, a, "Alice");
    super::dispatch(&mut fx.world, p, "look north");
    let out = drain(&mut rx);
    assert!(out.contains("You peer north."), "{out}");
    for line in out.lines() {
        assert!(line.trim_end_matches('\r').chars().count() <= 80, "{line}");
    }
    assert!(out.lines().count() >= 8, "{out}");
}

// ---- #23: bare `wield` ----

#[test]
fn bare_wield_and_hold_ask_what_to_wield_or_hold() {
    let mut world = World::new();
    let r = room(&mut world);
    let (p, mut rx) = player_in(&mut world, r);
    super::wear_into(&mut world, p, "", Some(mud_world::Slot::Wield));
    assert!(drain(&mut rx).contains("Wield what?"));
    super::wear_into(&mut world, p, "", Some(mud_world::Slot::Hold));
    assert!(drain(&mut rx).contains("Hold what?"));
    super::wear_into(&mut world, p, "", None);
    assert!(drain(&mut rx).contains("Wear what?"));
}

// ---- #25: drinking names the liquid ----

#[test]
fn drinking_names_the_liquid_not_its_colour() {
    let mut world = World::new();
    world.insert_resource(mud_world::ObjectPrototypes::default());
    world.init_resource::<mud_world::LiquidIndex>();
    world.init_resource::<mud_world::ConsumableEffectCatalog>();
    world.init_resource::<mud_world::EffectCatalog>();
    let mut liquids = mud_world::LiquidCatalog::default();
    liquids.insert(mud_world::LiquidDef {
        id: 1,
        name: "Water".into(),
        alias: "water".into(),
        color_desc: "clear".into(),
        drunk_effect: 0,
        hunger_effect: 0,
        thirst_effect: 0,
        description: None,
    });
    world.insert_resource(liquids);
    let r = room(&mut world);
    let (p, mut rx) = player_in(&mut world, r);
    world.spawn((
        Item,
        Named {
            name: "a leather water skin".into(),
        },
        Keywords(vec!["skin".into()]),
        Located(p),
        mud_world::LiquidContainer {
            liquid: "WATER".into(),
            capacity: 10,
            remaining: 10,
            poisoned: false,
        },
    ));
    super::drink_amount(&mut world, p, "skin", 1, "drink");
    let out = drain(&mut rx);
    assert!(
        out.contains("You drink some water from a leather water skin."),
        "{out}"
    );
    assert!(!out.contains("clear"), "{out}");
}

// ---- #30: bank amounts take units and `all` ----

#[test]
fn bank_amounts_parse_units_and_all() {
    assert_eq!(parse_bank_amount("50", 0), Some(50));
    assert_eq!(parse_bank_amount("5 gold", 0), Some(500));
    assert_eq!(parse_bank_amount("5 g", 0), Some(500));
    assert_eq!(parse_bank_amount("2 platinum", 0), Some(2000));
    assert_eq!(parse_bank_amount("3 silver", 0), Some(30));
    assert_eq!(parse_bank_amount("7c", 0), Some(7));
    assert_eq!(parse_bank_amount("all", 1234), Some(1234));
    assert_eq!(parse_bank_amount("all", 0), Some(0));
    for bad in ["", "gold", "0", "-5", "5 widgets", "5 gold extra"] {
        assert_eq!(parse_bank_amount(bad, 99), None, "{bad:?}");
    }
}

fn banker_world() -> (World, Entity, Rx) {
    let mut world = World::new();
    let r = room(&mut world);
    let mut protos = mud_world::MobPrototypes::default();
    protos.by_key.insert(
        (7, 9),
        super::test_support::mob_proto(7, 9, mud_db::enums::MobProfession::Banker),
    );
    world.insert_resource(protos);
    world.spawn((Mob, WorldKey { zone: 7, id: 9 }, Located(r)));
    let (p, rx) = player_in(&mut world, r);
    (world, p, rx)
}

#[test]
fn deposit_five_gold_moves_five_hundred_copper() {
    let (mut world, p, mut rx) = banker_world();
    world.entity_mut(p).insert(mud_world::Wealth(1000));
    bank_transfer(&mut world, p, "5 gold", "deposit");
    assert_eq!(world.get::<mud_world::Wealth>(p).unwrap().0, 500);
    assert_eq!(world.get::<mud_world::BankWealth>(p).unwrap().0, 500);
    assert!(drain(&mut rx).contains("Deposited"));
    bank_transfer(&mut world, p, "all", "deposit");
    assert_eq!(world.get::<mud_world::Wealth>(p).unwrap().0, 0);
    assert_eq!(world.get::<mud_world::BankWealth>(p).unwrap().0, 1000);
    drain(&mut rx);
    bank_transfer(&mut world, p, "all", "deposit");
    assert!(drain(&mut rx).contains("nothing to deposit"));
    bank_transfer(&mut world, p, "1 platinum", "withdraw");
    assert_eq!(world.get::<mud_world::Wealth>(p).unwrap().0, 1000);
    bank_transfer(&mut world, p, "lots", "deposit");
    assert!(drain(&mut rx).contains("Usage: deposit"));
}

// ---- #38: the rest line has no debug text ----

#[test]
fn rest_line_for_a_logged_out_character_reads_naturally() {
    use super::{RestStateDisplay, format_rest_line};
    let line = format_rest_line(RestStateDisplay {
        source: mud_db::enums::RestSource::Quit,
        tier: 0,
        repose: 0,
    });
    assert_eq!(line, "Rested while logged out");
    assert!(!line.contains("no pool yet"));
}
