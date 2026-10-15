//! Issue #158: `stat` on a live player or mob prints the legacy
//! `do_stat_character` sheet. Golden-style snapshots. Test-only.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    Account, BankWealth, ClassCatalog, ClassDef, CombatStats, CoreStats, Drunkenness,
    EffectInstance, EffectSource, Exits, Focus, Health, Hunger, ModifyDelta, Named, PlayerFlags,
    Posture, PostureKind, Profile, RaceCatalog, RaceDef, Room, SavingThrows, Stamina, Thirst,
    TimePlayed, Title, Wealth, WorldKey,
};

use super::actor_stat::render_actor_stat;
use super::test_support::{drain, mob_proto, player_in};
use crate::commands::{ColorMode, render_color_tags};

fn room(world: &mut World, zone: i32, id: i32) -> Entity {
    world
        .spawn((
            Room,
            Named {
                name: "A quiet hall".into(),
            },
            Exits::default(),
            WorldKey { zone, id },
        ))
        .id()
}

/// The sheet with colour tags removed and CRLF folded, for comparison.
fn plain(text: &str) -> String {
    render_color_tags(text, ColorMode::Strip).replace("\r\n", "\n")
}

fn catalogs(world: &mut World) {
    let mut races = RaceCatalog::default();
    races.by_race.insert(
        "HUMAN".into(),
        RaceDef {
            race: "HUMAN".into(),
            name: "Human".into(),
            plain_name: "Human".into(),
            race_align: "GOOD".into(),
            default_size: "MEDIUM".into(),
            default_lifeforce: "LIFE".into(),
            ..RaceDef::default()
        },
    );
    world.insert_resource(races);
    let mut classes = ClassCatalog::default();
    classes.by_id.insert(
        3,
        ClassDef {
            id: 3,
            name: "<blue>Bard</>".into(),
            plain_name: "Bard".into(),
            is_subclass: false,
            parent_class_id: None,
            description: None,
            hit_dice: "1d8".into(),
            primary_stat: None,
            hp_per_level: 0,
            exp_gain_factor: 1.0,
            alignment_bias: 0,
            campcraft_bonus: false,
            tags: Vec::new(),
            combat_rates: mud_db::characters::ClassCombatRates::default(),
            resistances: HashMap::new(),
        },
    );
    world.insert_resource(classes);
}

fn effect(world: &mut World, bearer: Entity, name: &str, secs: i32, delta: Option<(&str, i32)>) {
    let mut em = world.spawn((
        EffectInstance {
            kind: 1,
            name: name.into(),
            strength: 1,
            remaining_secs: secs,
            source: EffectSource::Spell,
            ability_id: None,
        },
        mud_world::AppliedTo(bearer),
    ));
    if let Some((target, amount)) = delta {
        em.insert(ModifyDelta {
            target: target.into(),
            amount,
        });
    }
}

#[test]
fn player_sheet_follows_the_legacy_layout() {
    let mut world = World::new();
    catalogs(&mut world);
    let home = room(&mut world, 30, 54);
    let (p, mut rx) = player_in(&mut world, home);
    world.entity_mut(p).insert((
        Named {
            name: "Knak".into(),
        },
        Account {
            user_id: "u1".into(),
            character_id: "c-20911".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: Vec::new(),
        },
        Profile {
            level: 67,
            class_id: Some(3),
            race: "HUMAN".into(),
            experience: 44_844_000,
            gender: "male".into(),
        },
        Title("the Bold".into()),
    ));
    world.entity_mut(p).insert((
        // Natural 62, +10 from worn gear (permanent), +8 from a spell.
        CoreStats {
            strength: 80,
            intelligence: 95,
            wisdom: 91,
            dexterity: 73,
            constitution: 100,
            charisma: 98,
        },
        Health { hp: 700, max: 752 },
        Stamina {
            current: 403,
            max: 403,
        },
        Focus(59),
        Wealth(1_535_404),
        BankWealth(250),
        CombatStats {
            accuracy: 70,
            evasion: 55,
            attack_power: 15,
            crit_chance: 5,
            armor_pct: 30,
            alignment: 992,
            ..CombatStats::default()
        },
        SavingThrows {
            para: 0,
            rod: 0,
            petri: 0,
            breath: -8,
            spell: -43,
        },
        Posture(PostureKind::Resting),
        PlayerFlags(vec![
            mud_db::enums::PlayerFlag::AutoExit,
            mud_db::enums::PlayerFlag::AutoSplit,
        ]),
        Hunger(3),
        Thirst(1),
        Drunkenness(0),
        TimePlayed(94 * 3600 + 34 * 60),
    ));
    effect(&mut world, p, "strength", -1, Some(("strength", 10)));
    effect(&mut world, p, "strength", 750, Some(("strength", 8)));
    effect(&mut world, p, "bless", 150, None);
    world.entity_mut(p).insert(mud_world::Bless);

    let out = plain(&render_actor_stat(&mut world, p));
    assert_eq!(out, PLAYER_SHEET, "\n{out}");
    assert!(
        out.lines().all(|l| l.chars().count() <= 80),
        "a line is wider than 80 columns:\n{out}"
    );

    // `stat me` sends the same sheet to the staff member.
    super::admin_inspect::cmd_stat(&mut world, p, "me");
    let sent = drain(&mut rx);
    assert!(sent.contains("IDNum: [c-20911]"), "{sent}");
    assert!(sent.contains("ACTUAL"), "{sent}");
}

const PLAYER_SHEET: &str = r"
PC 'Knak'  IDNum: [c-20911], In room (30, 54)
Title: the Bold
Race: Human, Race Align: GOOD, Size: Medium, Gender: Male
Life force: Life, Composition: Flesh
Class: Bard, Lev: [67], XP: [44844000], Align: [ 992] (good)
Last Logon: [never], Played: [94h 34m]
Age: [87 years, 9 months], Homeroom: [<none>]
         STR   INT   WIS   DEX   CON   CHA
ACTUAL    72    95    91    73   100    98
NATURAL   62    95    91    73   100    98
AFFECTED  80    95    91    73   100    98
HP: [700/752]  HP Gain: [2/s] HP Regen Bonus: [0]
MV: [403/403]  MV Gain: [4/s] MV Regen Bonus: [0]
Focus: [59]
Coins: [1535p / 4g / 0s / 4c], Bank: [0p / 2g / 5s / 0c]
Armor: [30%/0], Accuracy: [70], Evasion: [55], Attack Power: [+15]
Spell Power: [+0], Crit: [5%], Pen: [0%/0], Ward: [0%], Hardness: [0]
Saving throws: [0/0/0/-8/-43], Weapon Hitroll: [+0], Unarmed Damage: [+0]
Perception: [ 402], Hiddenness: [   0]
Pos: resting (resting), Fighting: <none>
Idle: [0 tics], Connected: Playing
PLR: NO FLAGS
PRF: AUTO_EXIT AUTO_SPLIT
PRV: ROLE=PLAYER
Max Carry: 435 weight; Carried: 0 (0.00 weight); Worn: 0 (0 weight)
Hunger: 3, Thirst: 1, Drunk: 0
Master is: <none>, Followers are:
Group Master is: <none>, groupees are:
EFF: BLESS
SPL: (perma) Strength              +10 to strength
SPL: ( 10hr) Strength              +8 to strength
SPL: (  2hr) Bless
";
const MOB_SHEET: &str = r"
MOB 'a brown fox', In room (30, 1)
Alias: fox brown, VNum: (30, 7)
L-Des: A brown fox sniffs about here.
Desc: Its coat is russet.
Role: Boss, Class: Bard
Race: Human, Race Align: GOOD, Size: Medium, Gender: Female
Life force: Undead, Composition: Flesh
Lev: [20], Align: [-400] (evil)
HP: [104/104]  HP Gain: [0/s] HP Regen Bonus: [0]
Coins: [1p / 2g / 3s / 4c], Bank: [0p / 0g / 0s / 0c]
Armor: [25%/0], Accuracy: [60], Evasion: [45], Attack Power: [+10]
Spell Power: [+0], Crit: [5%], Pen: [0%/0], Ward: [0%], Hardness: [0]
Saving throws: [0/0/0/0/0], Weapon Hitroll: [+0], Unarmed Damage: [+0]
Perception: [  20], Hiddenness: [   0]
Pos: standing (alert), Default Pos: standing, Fighting: <none>
Mob Spec-Proc: None, NPC Bare Hand Dam: 2d6+3, Attack type: bite
NPC flags: SENTINEL WIMPY BANK
Max Carry: 200 weight; Carried: 0 (0.00 weight); Worn: 0 (0 weight)
Hunger: 0, Thirst: 0, Drunk: 0
Master is: <none>, Followers are:
Group Master is: <none>, groupees are:
EFF: SANCT
SPL: (  4hr) Sanctuary
Script information:
  Trigger 'fox yips' (30, 3), types: RANDOM
Reset: row 9, loads in room (30, 1) (max 2)
";

#[test]
fn mob_sheet_follows_the_legacy_layout() {
    use mud_db::enums::{MobBehavior, MobProfession, MobRole};
    use mud_world::{
        MobPrototypes, MobResetCatalog, MobResetEntry, TriggerAttach, TriggerCatalog, TriggerDef,
        TriggerEvent,
    };

    let mut world = World::new();
    catalogs(&mut world);
    let hall = room(&mut world, 30, 1);
    let (staff, _rx) = player_in(&mut world, hall);
    let mut proto = mob_proto(30, 7, MobProfession::Banker);
    proto.name = "a brown fox".into();
    proto.keywords = vec!["fox".into(), "brown".into()];
    proto.room_description = "A brown fox sniffs about here.".into();
    proto.examine_description = "Its coat is russet.".into();
    proto.gender = "female".into();
    proto.race = "human".into();
    proto.level = 20;
    proto.alignment = -400;
    proto.role = MobRole::Boss;
    proto.class_id = Some(3);
    proto.hp_dice_num = 4;
    proto.hp_dice_size = 1;
    proto.hp_dice_bonus = 100;
    proto.damage_dice_num = 2;
    proto.damage_dice_size = 6;
    proto.damage_dice_bonus = 3;
    proto.accuracy = 60;
    proto.evasion = 45;
    proto.attack_power = 10;
    proto.armor_rating = 25;
    proto.perception = 20;
    proto.wealth = 1_234;
    proto.behaviors = vec![MobBehavior::Sentinel, MobBehavior::Wimpy];
    proto.life_force = mud_db::enums::LifeForce::Undead;
    proto.damage_type = mud_db::enums::DamageType::Bite;
    let mut protos = MobPrototypes::default();
    protos.by_key.insert((30, 7), proto.clone());
    world.insert_resource(protos);
    world.insert_resource(MobResetCatalog {
        entries: vec![MobResetEntry {
            reset_id: 9,
            mob_zone_id: 30,
            mob_id: 7,
            room_entity: hall,
            max_instances: 2,
        }],
    });
    let mut triggers = TriggerCatalog::default();
    triggers.by_key.insert(
        (30, 3),
        TriggerDef {
            zone_id: 30,
            id: 3,
            name: "fox yips".into(),
            attach_type: TriggerAttach::Mob,
            commands: String::new(),
            flags: vec![TriggerEvent::Random],
            arg_list: Vec::new(),
            num_args: 0,
        },
    );
    triggers.mob_attachments.insert((30, 7), vec![(30, 3)]);
    world.insert_resource(triggers);

    let fox = mud_world::mob_spawn::spawn_mob_from_proto(&mut world, &proto, hall, Some(9));
    world.entity_mut(staff).insert(Named {
        name: "Gandalf".into(),
    });
    effect(&mut world, fox, "sanctuary", 300, None);
    world.entity_mut(fox).insert(mud_world::Sanctuary);

    let out = plain(&render_actor_stat(&mut world, fox));
    assert_eq!(out, MOB_SHEET, "\n{out}");
}
