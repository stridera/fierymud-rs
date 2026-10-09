//! Player-facing command parity with legacy: `use`, `play`, `alert`,
//! `emote's`, `ki` kicking, and socials taking part in abbreviation.
//! Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{ObjectType, UserRole};
use mud_world::{
    AbilityCatalog, Account, Charges, EffectCatalog, EffectDef, EquippedSlot, Item, Keywords,
    Located, Mob, Named, ObjectAbilityBinding, ObjectAbilityCatalog, ObjectPrototypes, Profile,
    Slot, SocialDef, SocialRegistry, SpellSlotData, WorldKey,
};

use super::test_support::{Rx, ability_def, drain, object_proto, player_in};
use crate::commands::{Abbrev, all_commands, dispatch, longest_prefix_match, resolve_abbrev};

const IDENTIFY: i32 = 195;
const INSPECT: i32 = 50;

fn account(role: UserRole) -> Account {
    Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role,
        account_role: role,
        perms: vec![],
    }
}

fn base_world() -> (World, Entity, Entity, Rx) {
    let mut world = World::new();
    world.insert_resource(SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    world.insert_resource(SocialRegistry::default());
    let room = world.spawn_empty().id();
    let (player, rx) = player_in(&mut world, room);
    world.entity_mut(player).insert(account(UserRole::Player));
    (world, room, player, rx)
}

fn say_social(name: &str) -> SocialDef {
    SocialDef {
        name: name.into(),
        hide: false,
        char_no_arg: Some(format!("You {name}.")),
        others_no_arg: Some(format!("{{actor.name}} {name}s.")),
        char_found: None,
        others_found: None,
        vict_found: None,
        not_found: None,
        char_auto: None,
        others_auto: None,
    }
}

// ----- ki / kic ---------------------------------------------------------

#[test]
fn ki_and_kic_run_the_kick_skill_not_the_type_it_out_refusal() {
    for typed in ["ki", "kic", "kick"] {
        let (mut world, _room, player, mut rx) = base_world();
        dispatch(&mut world, player, typed);
        let out = drain(&mut rx);
        assert!(!out.contains("Type the whole command"), "{typed}: {out}");
        assert!(out.contains("You aren't fighting anyone"), "{typed}: {out}");
    }
}

#[test]
fn bare_k_stays_unknown() {
    let (mut world, _room, player, mut rx) = base_world();
    dispatch(&mut world, player, "k");
    let out = drain(&mut rx);
    assert!(out.contains("Unknown command"), "{out}");
}

// ----- socials in abbreviation -----------------------------------------

/// The `do_action` rows of legacy `cmd_info[]` (level 0), in table order.
const LEGACY_SOCIALS: &[&str] = &[
    "ack",
    "accuse",
    "afk",
    "agree",
    "amaze",
    "apologize",
    "applaud",
    "ayt",
    "bang",
    "bark",
    "beckon",
    "beer",
    "beg",
    "bite",
    "bird",
    "blink",
    "bleed",
    "blush",
    "boggle",
    "bonk",
    "bored",
    "bounce",
    "bow",
    "brb",
    "burp",
    "bye",
    "cackle",
    "chuckle",
    "cheer",
    "choke",
    "clap",
    "comfort",
    "comb",
    "cough",
    "cringe",
    "cry",
    "cuddle",
    "curse",
    "curtsey",
    "dance",
    "daydream",
    "dream",
    "drool",
    "duck",
    "duh",
    "embrace",
    "envy",
    "eyebrow",
    "fart",
    "flanic",
    "flex",
    "flip",
    "flirt",
    "fool",
    "fondle",
    "french",
    "frown",
    "fume",
    "gag",
    "gape",
    "gasp",
    "giggle",
    "glare",
    "glomp",
    "glower",
    "groan",
    "greet",
    "grin",
    "grope",
    "grovel",
    "growl",
    "grumble",
    "halo",
    "hi5",
    "hiccup",
    "hiss",
    "hop",
    "hug",
    "hunger",
    "imitate",
    "impale",
    "kiss",
    "lag",
    "laugh",
    "lean",
    "lick",
    "love",
    "moan",
    "massage",
    "moon",
    "mosh",
    "mourn",
    "mumble",
    "mutter",
    "nap",
    "nibble",
    "nod",
    "nog",
    "noogie",
    "nudge",
    "nuzzle",
    "panic",
    "pant",
    "pat",
    "peer",
    "pet",
    "poke",
    "ponder",
    "pounce",
    "pout",
    "protect",
    "puke",
    "punch",
    "purr",
    "raise",
    "rofl",
    "roll",
    "ready",
    "ruffle",
    "salute",
    "scare",
    "scold",
    "scratch",
    "scream",
    "screw",
    "seduce",
    "shake",
    "shiver",
    "shrug",
    "shudder",
    "sigh",
    "sing",
    "slap",
    "slobber",
    "smell",
    "smile",
    "smirk",
    "smoke",
    "snicker",
    "snap",
    "snarl",
    "sneeze",
    "sniff",
    "snoogie",
    "snore",
    "snort",
    "snuggle",
    "spam",
    "spank",
    "spit",
    "squeeze",
    "stare",
    "steam",
    "stroke",
    "strut",
    "sulk",
    "swat",
    "sweat",
    "tackle",
    "tango",
    "tap",
    "tarzan",
    "taunt",
    "tease",
    "thank",
    "think",
    "thirst",
    "throw",
    "tip",
    "tickle",
    "tongue",
    "tug",
    "twibble",
    "twiddle",
    "twitch",
    "veto",
    "wave",
    "wait",
    "wet",
    "whap",
    "whatever",
    "whine",
    "whistle",
    "wiggle",
    "wince",
    "wink",
    "worship",
    "yawn",
    "yodel",
    "zone",
];

/// A `Social` table holding the legacy socials that are not also one of
/// our commands.
fn legacy_socials() -> SocialRegistry {
    let mut reg = SocialRegistry::default();
    for &n in LEGACY_SOCIALS {
        if !all_commands().any(|c| c.names.contains(&n)) {
            reg.by_name.insert(n.to_string(), say_social(n));
        }
    }
    reg
}

fn abbrev_name(typed: &str, role: UserRole, reg: &SocialRegistry) -> Option<String> {
    if longest_prefix_match(&[typed]).is_some() || reg.get(typed).is_some() {
        return Some(typed.to_string());
    }
    match resolve_abbrev(typed, role, &[], Some(reg))? {
        Abbrev::Command(c) => Some(c.names[0].to_string()),
        Abbrev::Social(n) => Some(n.to_string()),
    }
}

#[test]
fn socials_abbreviate_by_legacy_table_order() {
    let reg = legacy_socials();
    for (typed, want) in [
        ("gig", "giggle"),
        ("smi", "smile"),
        ("gr", "groan"),
        ("be", "beckon"),
        ("ta", "tackle"),
        ("ti", "tip"),
        ("sm", "smell"),
        ("ha", "halo"),
    ] {
        assert!(reg.get(want).is_some(), "{want} missing from fixture");
        assert_eq!(
            abbrev_name(typed, UserRole::Player, &reg).as_deref(),
            Some(want),
            "'{typed}' should resolve like legacy"
        );
    }
}

#[test]
fn abbreviated_social_runs_as_the_full_social() {
    let (mut world, _room, player, mut rx) = base_world();
    world.insert_resource(legacy_socials());
    dispatch(&mut world, player, "gig");
    let out = drain(&mut rx);
    assert!(out.contains("You giggle."), "{out}");
    dispatch(&mut world, player, "smi");
    let out = drain(&mut rx);
    assert!(out.contains("You smile."), "{out}");
}

#[test]
fn exact_command_name_beats_a_social_of_the_same_name() {
    let (mut world, _room, player, mut rx) = base_world();
    let mut reg = legacy_socials();
    reg.by_name.insert("look".into(), say_social("look"));
    reg.by_name
        .insert("inventory".into(), say_social("inventory"));
    world.insert_resource(reg);
    dispatch(&mut world, player, "inventory");
    let out = drain(&mut rx);
    assert!(!out.contains("You inventory."), "{out}");
}

#[test]
fn commands_still_win_prefixes_the_legacy_table_gives_them() {
    let reg = legacy_socials();
    // Legacy `l` is look, `i` inventory, `n` north: no social outranks them.
    for (typed, want) in [("l", "look"), ("i", "inventory"), ("n", "north")] {
        let got = abbrev_name(typed, UserRole::Player, &reg).unwrap();
        let canonical = all_commands()
            .find(|c| c.names.contains(&got.as_str()))
            .map(|c| c.names[0]);
        assert_eq!(canonical, Some(want), "'{typed}'");
    }
    // `gr` is groan, not an alias of group (legacy table order).
    assert_eq!(
        abbrev_name("gr", UserRole::Player, &reg).as_deref(),
        Some("groan")
    );
    assert_eq!(
        abbrev_name("grou", UserRole::Player, &reg).as_deref(),
        Some("group")
    );
}

#[test]
fn socials_the_legacy_table_never_had_rank_after_commands() {
    let mut reg = legacy_socials();
    reg.by_name.insert("wibble".into(), say_social("wibble"));
    reg.by_name
        .insert("zzsocial".into(), say_social("zzsocial"));
    // `wib` has no command or legacy social; the new social answers it.
    assert_eq!(
        abbrev_name("wib", UserRole::Player, &reg).as_deref(),
        Some("wibble")
    );
    // `who` is a command and a prefix of nothing a later social can steal.
    assert_eq!(
        abbrev_name("wh", UserRole::Player, &reg).as_deref(),
        Some("who")
    );
}

#[test]
fn denylisted_commands_still_refuse_abbreviation_with_socials_loaded() {
    let (mut world, _room, player, mut rx) = base_world();
    world.insert_resource(legacy_socials());
    dispatch(&mut world, player, "qui");
    let out = drain(&mut rx);
    assert!(out.contains("you must type out 'quit'"), "{out}");
}

#[test]
fn staff_only_snowball_abbreviates_for_staff_only() {
    let mut reg = legacy_socials();
    reg.by_name
        .insert("snowball".into(), say_social("snowball"));
    assert_ne!(
        abbrev_name("snowb", UserRole::Player, &reg).as_deref(),
        Some("snowball")
    );
    assert_eq!(
        abbrev_name("snowb", UserRole::Implementor, &reg).as_deref(),
        Some("snowball")
    );
}

// ----- alert ------------------------------------------------------------

fn set_posture(world: &mut World, e: Entity, p: mud_world::PostureKind) {
    world.entity_mut(e).insert(mud_world::Posture(p));
}

#[test]
fn alert_from_resting_sits_up_without_standing() {
    use mud_world::PostureKind;
    let (mut world, _room, player, mut rx) = base_world();
    set_posture(&mut world, player, PostureKind::Resting);
    dispatch(&mut world, player, "alert");
    let out = drain(&mut rx);
    assert!(
        out.contains("You sit up straight and start to pay attention"),
        "{out}"
    );
    assert_eq!(
        world.get::<mud_world::Posture>(player).map(|p| p.0),
        Some(PostureKind::Sitting)
    );
}

#[test]
fn alert_refusals_follow_legacy() {
    use mud_world::PostureKind;
    let (mut world, _room, player, mut rx) = base_world();
    set_posture(&mut world, player, PostureKind::Sleeping);
    dispatch(&mut world, player, "alert");
    assert!(drain(&mut rx).contains("How about waking up first"));
    set_posture(&mut world, player, PostureKind::Standing);
    dispatch(&mut world, player, "alert");
    assert!(drain(&mut rx).contains("already about as tense as you can get"));
    set_posture(&mut world, player, PostureKind::Resting);
    let foe = world.spawn_empty().id();
    world.entity_mut(player).insert(mud_world::Fighting(foe));
    dispatch(&mut world, player, "alert");
    assert!(drain(&mut rx).contains("you're pretty alert already"));
    assert_eq!(
        world.get::<mud_world::Posture>(player).map(|p| p.0),
        Some(PostureKind::Resting)
    );
}

// ----- emote's ----------------------------------------------------------

#[test]
fn emotes_prepends_name_and_possessive() {
    let (mut world, _room, player, mut rx) = base_world();
    dispatch(&mut world, player, "emote's eyes widen.");
    let out = drain(&mut rx);
    assert!(out.contains("Tester's eyes widen."), "{out}");
    dispatch(&mut world, player, "emote smiles.");
    let out = drain(&mut rx);
    assert!(out.contains("Tester smiles."), "{out}");
    assert!(!out.contains("Tester's"), "{out}");
}

// ----- use / play -------------------------------------------------------

fn world_with_items() -> (World, Entity, Entity, Rx) {
    let (mut world, room, player, rx) = base_world();
    let mut catalog = AbilityCatalog::default();
    let mut def = ability_def(IDENTIFY, "Identify", AbilityKind::Spell);
    def.cast_time_rounds = 0;
    catalog.by_name.insert("identify".into(), def);
    catalog.effects_for.insert(IDENTIFY, vec![(INSPECT, None)]);
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    effects.by_id.insert(
        INSPECT,
        EffectDef {
            id: INSPECT,
            name: "inspect".into(),
            description: None,
            effect_type: "inspect".into(),
            tags: Vec::new(),
            presence_override: None,
            default_params: serde_json::json!({}),
            prevents_speaking: false,
            prevents_casting: false,
            prevents_movement: false,
            on_apply: None,
            on_tick: None,
            on_remove: None,
        },
    );
    world.insert_resource(effects);
    let mut protos = ObjectPrototypes::default();
    protos
        .by_key
        .insert((30, 1), object_proto(30, 1, ObjectType::Wand));
    protos
        .by_key
        .insert((30, 2), object_proto(30, 2, ObjectType::Staff));
    protos
        .by_key
        .insert((30, 3), object_proto(30, 3, ObjectType::Instrument));
    protos
        .by_key
        .insert((30, 4), object_proto(30, 4, ObjectType::Light));
    let mut strong = object_proto(30, 5, ObjectType::Wand);
    strong.level = 50;
    protos.by_key.insert((30, 5), strong);
    world.insert_resource(protos);
    let mut bindings = ObjectAbilityCatalog::default();
    for key in [(30, 1), (30, 2), (30, 3), (30, 5)] {
        bindings.by_key.insert(
            key,
            vec![ObjectAbilityBinding {
                ability_id: IDENTIFY,
                level: 10,
                charges: Some(3),
            }],
        );
    }
    world.insert_resource(bindings);
    world.entity_mut(player).insert(Profile {
        level: 10,
        class_id: None,
        race: "HUMAN".into(),
        experience: 0,
        gender: "neutral".into(),
    });
    let _ = room;
    (world, room, player, rx)
}

fn hold(world: &mut World, holder: Entity, name: &str, kw: &str, key: (i32, i32)) -> Entity {
    world
        .spawn((
            Item,
            Named { name: name.into() },
            Keywords(vec![kw.into()]),
            Located(holder),
            EquippedSlot(Slot::Hold),
            WorldKey {
                zone: key.0,
                id: key.1,
            },
            Charges(3),
        ))
        .id()
}

fn goblin(world: &mut World, room: Entity) -> Entity {
    world
        .spawn((
            Mob,
            Named {
                name: "a snarling goblin".into(),
            },
            Keywords(vec!["goblin".into()]),
            Located(room),
        ))
        .id()
}

#[test]
fn use_without_an_argument_asks_what() {
    let (mut world, _room, player, mut rx) = world_with_items();
    dispatch(&mut world, player, "use");
    assert!(drain(&mut rx).contains("What do you want to use?"));
    dispatch(&mut world, player, "play");
    assert!(drain(&mut rx).contains("What do you want to play?"));
}

#[test]
fn use_needs_the_item_held_not_carried() {
    let (mut world, _room, player, mut rx) = world_with_items();
    // Carried in the pack, not held.
    world.spawn((
        Item,
        Named {
            name: "a wand of identify".into(),
        },
        Keywords(vec!["wand".into()]),
        Located(player),
        WorldKey { zone: 30, id: 1 },
        Charges(3),
    ));
    dispatch(&mut world, player, "use wand");
    let out = drain(&mut rx);
    assert!(
        out.contains("You don't seem to be holding a wand."),
        "{out}"
    );
    dispatch(&mut world, player, "use orb");
    assert!(drain(&mut rx).contains("holding an orb."));
}

#[test]
fn use_wand_at_a_target_spends_a_charge() {
    let (mut world, room, player, mut rx) = world_with_items();
    goblin(&mut world, room);
    let wand = hold(&mut world, player, "a wand of identify", "wand", (30, 1));
    dispatch(&mut world, player, "use wand goblin");
    let out = drain(&mut rx);
    assert!(
        out.contains("You point a wand of identify at a snarling goblin."),
        "{out}"
    );
    assert!(out.contains("Name: a snarling goblin"), "{out}");
    assert_eq!(world.get::<Charges>(wand).map(|c| c.0), Some(2));
}

#[test]
fn use_wand_with_no_or_missing_target_keeps_the_charge() {
    let (mut world, _room, player, mut rx) = world_with_items();
    let wand = hold(&mut world, player, "a wand of identify", "wand", (30, 1));
    dispatch(&mut world, player, "use wand");
    assert!(drain(&mut rx).contains("At what should"));
    dispatch(&mut world, player, "use wand nobody");
    assert!(drain(&mut rx).contains("You can't see any nobody here"));
    assert_eq!(world.get::<Charges>(wand).map(|c| c.0), Some(3));
}

#[test]
fn use_staff_taps_it_on_the_ground() {
    let (mut world, _room, player, mut rx) = world_with_items();
    hold(&mut world, player, "a staff of identify", "staff", (30, 2));
    dispatch(&mut world, player, "use staff");
    let out = drain(&mut rx);
    assert!(
        out.contains("You tap a staff of identify three times on the ground."),
        "{out}"
    );
}

#[test]
fn use_refuses_things_that_are_not_wands_or_staves() {
    let (mut world, _room, player, mut rx) = world_with_items();
    hold(&mut world, player, "a lantern", "lantern", (30, 4));
    dispatch(&mut world, player, "use lantern");
    assert!(drain(&mut rx).contains("You can't seem to figure out how to use it."));
    dispatch(&mut world, player, "play lantern");
    assert!(drain(&mut rx).contains("figure out how to make sound with it."));
}

#[test]
fn use_refuses_an_item_too_powerful_for_the_user() {
    let (mut world, _room, player, mut rx) = world_with_items();
    let wand = hold(&mut world, player, "a mighty wand", "mighty", (30, 5));
    dispatch(&mut world, player, "use mighty goblin");
    assert!(drain(&mut rx).contains("That item is too powerful for you to use."));
    assert_eq!(world.get::<Charges>(wand).map(|c| c.0), Some(3));
}

#[test]
fn play_plays_a_held_instrument() {
    let (mut world, _room, player, mut rx) = world_with_items();
    hold(&mut world, player, "a silver flute", "flute", (30, 3));
    dispatch(&mut world, player, "play flute");
    let out = drain(&mut rx);
    assert!(out.contains("You play a silver flute."), "{out}");
}

/// Legacy `cmd_info[]` had no `sneak` and no `accept`, so `sn`/`sne` were
/// snicker/sneeze and `ac`/`acc` ack/accuse there. Here those two verbs
/// exist, and a command the legacy table never had outranks every social
/// that shares its first letters; the socials keep their longer prefixes.
#[test]
fn rust_only_commands_beat_the_socials_that_share_their_first_letters() {
    let reg = legacy_socials();
    for (typed, want) in [
        ("sn", "sneak"),
        ("sne", "sneak"),
        ("snea", "sneak"),
        ("ac", "accept"),
        ("acc", "accept"),
        ("acce", "accept"),
        // The longer social prefixes are still the socials.
        ("snic", "snicker"),
        ("snee", "sneeze"),
        ("ack", "ack"),
        ("accu", "accuse"),
        // Legacy order between two legacy entries is untouched: sulk (835)
        // precedes summon (836) and subclass (843) in `cmd_info[]`.
        ("su", "sulk"),
        ("sul", "sulk"),
        ("sub", "subclass"),
    ] {
        assert_eq!(
            abbrev_name(typed, UserRole::Player, &reg).as_deref(),
            Some(want),
            "'{typed}'"
        );
    }
}

#[test]
fn abbreviated_sneak_and_accept_run_the_commands() {
    let (mut world, _room, player, mut rx) = base_world();
    world.insert_resource(legacy_socials());
    dispatch(&mut world, player, "ac");
    let out = drain(&mut rx);
    assert!(out.contains("no pending group invites"), "{out}");
    assert!(!out.contains("You ack"), "{out}");
}
