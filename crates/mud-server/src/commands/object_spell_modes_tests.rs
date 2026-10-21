//! The object-target uses of thirteen spells (legacy `mag_alter_obj`,
//! `spell_*` object branches), wired the way the data patch wires them:
//! one `alter_object` row per spell plus the targeting row, next to the
//! spell's existing character effect. Each spell gets object-case tests and
//! a regression that its character case still works. Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{Alignment, MobProfession, ObjectFlag, ObjectRestriction, ObjectType};
use mud_world::components::{FoodPoisoned, ItemAlterDirty, ItemBarredAlignments, ItemRemovedFlags};
use mud_world::{
    AbilityCatalog, Account, AppliedTo, Corpse, CorpseDecay, EffectCatalog, EffectDef,
    EffectInstance, EquippedSlot, Follower, Health, Hunger, Item, Keywords, KnownAbilities,
    LightFuel, LiquidContainer, Lit, Located, Mob, MobPrototypes, Named, ObjectFlags,
    ObjectPrototypes, ObjectRestrictions, Profile, Room, Thirst, WorldKey, WorldKeyIndex,
};

use super::invoke_ability_with;
use super::test_support::{Rx, ability_def, drain, mob_proto, object_proto, player_in};

const ALTER: i32 = 7;
const STATUS: i32 = 10;
const MODIFY: i32 = 11;
const ROOM_FX: i32 = 12;
const CLEANSE: i32 = 13;
const DISPEL: i32 = 14;
const HEAL: i32 = 15;
const SUMMON: i32 = 16;
const CREATE: i32 = 17;

const NOTHING: &str = "Nothing seems to happen.";

struct Spell {
    id: i32,
    name: &'static str,
    violent: bool,
    targets: &'static [&'static str],
    /// (effect id, params), the character effect first then the alter row.
    effects: Vec<(i32, serde_json::Value)>,
}

fn alter(params: serde_json::Value) -> (i32, serde_json::Value) {
    (ALTER, params)
}

/// The spells exactly as `fierylib/data/abilities.json` carries them.
#[allow(clippy::too_many_lines)]
fn spells() -> Vec<Spell> {
    vec![
        Spell {
            id: 100,
            name: "Bless",
            violent: false,
            targets: &["SELF", "ALLY_PC", "ALLY_NPC", "OBJECT_INV"],
            effects: vec![
                (
                    STATUS,
                    serde_json::json!({"flag": "bless", "duration": "10 + (skill / 7)", "durationUnit": "hours"}),
                ),
                alter(serde_json::json!({
                    "mode": "flags", "staffBypass": true,
                    "requireTypes": ["WEAPON"],
                    "wrongTypeMessage": "This spell is only effective on weapons.",
                    "maxObjectLevel": "skill",
                    "tooPowerfulMessage": "{item} is too powerful for you to bless.",
                    "maxObjectWeight": "5 * skill",
                    "tooLargeMessage": "{item} is too large for you to bless.",
                    "blockedByFlags": ["GLOW", "HUM", "INVISIBLE", "MAGIC"],
                    "blockedByRestrictions": ["NO_DROP"],
                    "blockedByAlignments": ["GOOD"],
                    "blockedMessage": "{item} doesn't seem receptive to the blessing.",
                    "repelledIfMagical": true,
                    "repelledMessage": "The blessing is repelled from {item}.",
                    "barAlignment": "EVIL",
                    "messageToCaster": "{item} glows briefly.",
                    "messageToRoom": "{item} glows briefly.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 101,
            name: "Invisibility",
            violent: false,
            targets: &["SELF", "ALLY_PC", "ALLY_NPC", "OBJECT_INV", "OBJECT_WORLD"],
            effects: vec![
                (
                    MODIFY,
                    serde_json::json!({"amount": "40", "duration": "9 + (skill / 9)", "target": "evasion", "durationUnit": "hours"}),
                ),
                alter(serde_json::json!({
                    "mode": "flags",
                    "blockedByFlags": ["INVISIBLE"],
                    "blockedByRestrictions": ["NO_INVISIBLE"],
                    "setFlags": ["INVISIBLE"],
                    "messageToCaster": "{item} vanishes.",
                    "messageToRoom": "{item} vanishes.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 102,
            name: "Dispel Magic",
            violent: true,
            targets: &[
                "SELF",
                "ENEMY_PC",
                "ENEMY_NPC",
                "ALLY_PC",
                "ALLY_NPC",
                "OBJECT_INV",
                "OBJECT_WORLD",
            ],
            effects: vec![
                (
                    DISPEL,
                    serde_json::json!({"filter": "magic", "scope": "all", "power": "skill"}),
                ),
                alter(serde_json::json!({
                    "mode": "flags",
                    "requireAnyFlag": ["INVISIBLE"],
                    "powerRoll": "random(1, 50) + skill",
                    "resistMessageToCaster": "You fail.",
                    "clearFlags": ["INVISIBLE"],
                    "messageToCaster": "{item} fades into existence.",
                    "messageToRoom": "{item} fades into existence.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 103,
            name: "Poison",
            violent: true,
            targets: &["ENEMY_PC", "ENEMY_NPC", "OBJECT_INV"],
            effects: vec![
                (
                    MODIFY,
                    serde_json::json!({"amount": "-5", "duration": "5 + (skill / 10)", "target": "str", "durationUnit": "hours"}),
                ),
                alter(serde_json::json!({
                    "mode": "poison", "poisoned": true,
                    "messageToCaster": "{item} steams briefly.",
                    "messageToRoom": "{item} steams briefly.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 104,
            name: "Remove Poison",
            violent: false,
            targets: &["SELF", "ALLY_PC", "ALLY_NPC", "OBJECT_INV", "OBJECT_WORLD"],
            effects: vec![
                (
                    CLEANSE,
                    serde_json::json!({"condition": "poison", "scope": "all"}),
                ),
                alter(serde_json::json!({
                    "mode": "poison", "poisoned": false,
                    "messageToCaster": "{item} steams briefly.",
                    "messageToRoom": "{item} steams briefly.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 105,
            name: "Create Water",
            violent: false,
            targets: &["OBJECT_INV"],
            effects: vec![alter(serde_json::json!({
                "mode": "liquid", "liquid": "water", "mixedLiquid": "slime-mold-juice",
                "amount": "1 + 15 * skill / 2",
                "fullMessage": "{item} seems to be full already.",
                "messageToCaster": "Water wells up within {item}."
            }))],
        },
        Spell {
            id: 106,
            name: "Preserve",
            violent: false,
            targets: &["SELF", "OBJECT_WORLD"],
            effects: vec![
                (
                    STATUS,
                    serde_json::json!({"flag": "sanctuary", "duration": "skill / 5", "durationUnit": "hours"}),
                ),
                alter(serde_json::json!({
                    "mode": "corpse_timer", "hours": "level",
                    "notCorpseMessageToCaster": "You can only preserve a corpse!",
                    "messageToCaster": ["{item} glows blue briefly.", "{item} is preserved for {hours} more hours."],
                    "messageToRoom": "{item} glows blue briefly."
                })),
            ],
        },
        Spell {
            id: 107,
            name: "Dark Feast",
            violent: false,
            targets: &["SELF", "OBJECT_WORLD"],
            effects: vec![
                (
                    HEAL,
                    serde_json::json!({"resource": "hp", "amount": "skill / 2"}),
                ),
                alter(serde_json::json!({
                    "mode": "corpse_feed", "hunger": 24, "thirst": 24,
                    "notCorpseMessageToCaster": "That is not a corpse!",
                    "messageToCaster": "You gorge yourself on {item}.",
                    "messageToRoom": "{actor.name} gorges {actor.ref} on {item}."
                })),
            ],
        },
        Spell {
            id: 108,
            name: "Darkness",
            violent: false,
            targets: &["SELF", "ALLY_PC", "ALLY_NPC", "OBJECT_INV"],
            effects: vec![
                (
                    ROOM_FX,
                    serde_json::json!({"type": "darkness", "duration": "skill / 10", "subtype": "effect", "durationUnit": "hours"}),
                ),
                alter(serde_json::json!({
                    "mode": "light", "lightAction": "drain",
                    "messageToCaster": "You drain {item}'s energy.",
                    "extinguishMessageToCaster": "{item} quickly sputters out.",
                    "extinguishMessageToRoom": "{item} quickly sputters out.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 109,
            name: "Illumination",
            violent: false,
            targets: &["SELF", "ALLY_PC", "ALLY_NPC", "OBJECT_INV"],
            effects: vec![
                (
                    ROOM_FX,
                    serde_json::json!({"type": "light", "duration": "5 + (skill / 2)", "subtype": "effect", "durationUnit": "hours"}),
                ),
                alter(serde_json::json!({
                    "mode": "light", "lightAction": "ignite",
                    "capacityMultiplier": 2, "defaultFuel": 50,
                    "permanentMessageToCaster": "The power of your magic washes uselessly over the surface of {item}.",
                    "messageToCaster": "{item} begins glowing with a bright yellow light.",
                    "messageToRoom": "{item} begins glowing with a bright yellow light.",
                    "noopMessageToCaster": NOTHING
                })),
            ],
        },
        Spell {
            id: 110,
            name: "Animate Dead",
            violent: false,
            targets: &["SELF", "OBJECT_WORLD"],
            effects: vec![(
                SUMMON,
                serde_json::json!({"type": "creature", "mobZone": 54, "mobId": 20}),
            )],
        },
        Spell {
            id: 111,
            name: "Summon Dracolich",
            violent: false,
            targets: &["SELF", "OBJECT_WORLD"],
            effects: vec![(
                SUMMON,
                serde_json::json!({"type": "creature", "mobType": "dracolich", "mobZone": 533, "mobId": 11}),
            )],
        },
    ]
}

fn effect_def(id: i32, effect_type: &str) -> EffectDef {
    EffectDef {
        id,
        name: effect_type.to_string(),
        description: None,
        effect_type: effect_type.to_string(),
        tags: Vec::new(),
        presence_override: None,
        default_params: serde_json::json!({}),
        prevents_speaking: false,
        prevents_casting: false,
        prevents_movement: false,
        on_apply: None,
        on_tick: None,
        on_remove: None,
    }
}

struct Fx {
    world: World,
    room: Entity,
    caster: Entity,
    rx: Rx,
    next_obj: i32,
}

fn fx() -> Fx {
    let mut world = World::new();
    world.insert_resource(WorldKeyIndex::default());
    world.insert_resource(MobPrototypes::default());
    world.insert_resource(ObjectPrototypes::default());
    world.insert_resource(mud_script::LuaHost::default());
    world.insert_resource(mud_world::WeatherCatalog::default());
    world.insert_resource(mud_world::RaceCatalog::default());
    world.insert_resource(mud_world::SpellSlotData::default());
    world.insert_resource(mud_world::ClassSkillsData::default());
    let room = world
        .spawn((
            Room,
            WorldKey { zone: 30, id: 1 },
            Named {
                name: "Here".into(),
            },
        ))
        .id();
    world
        .resource_mut::<WorldKeyIndex>()
        .rooms
        .insert((30, 1), room);
    let mut catalog = AbilityCatalog::default();
    let mut known = Vec::new();
    for s in spells() {
        let mut def = ability_def(s.id, s.name, AbilityKind::Spell);
        def.violent = s.violent;
        def.plain_name = s.name.to_ascii_uppercase().replace(' ', "_");
        def.cast_time_rounds = 0;
        catalog
            .by_name
            .insert(s.name.to_ascii_lowercase().replace(' ', "_"), def);
        catalog.effects_for.insert(
            s.id,
            s.effects
                .iter()
                .map(|(id, params)| (*id, Some(params.clone())))
                .collect(),
        );
        catalog.targeting.insert(
            s.id,
            mud_world::TargetingRule {
                valid_targets: s.targets.iter().map(|t| (*t).to_string()).collect(),
                scope: "SINGLE".to_string(),
                max_targets: 1,
                require_los: false,
            },
        );
        known.push((s.id, 1000, true));
    }
    world.insert_resource(catalog);
    let mut effects = EffectCatalog::default();
    for (id, ty) in [
        (ALTER, "alter_object"),
        (STATUS, "status"),
        (MODIFY, "modify"),
        (ROOM_FX, "room"),
        (CLEANSE, "cleanse"),
        (DISPEL, "dispel"),
        (HEAL, "heal"),
        (SUMMON, "summon"),
        (CREATE, "create"),
    ] {
        effects.by_id.insert(id, effect_def(id, ty));
    }
    world.insert_resource(effects);
    let (caster, rx) = player_in(&mut world, room);
    world.entity_mut(caster).insert((
        Profile {
            level: 20,
            class_id: None,
            race: "Human".into(),
            experience: 0,
            gender: "neutral".into(),
        },
        KnownAbilities { entries: known },
        Health { hp: 10, max: 100 },
        Hunger(40),
        Thirst(30),
    ));
    Fx {
        world,
        room,
        caster,
        rx,
        next_obj: 1,
    }
}

impl Fx {
    fn cast(&mut self, spell: &str, target: &str) -> String {
        invoke_ability_with(
            &mut self.world,
            self.caster,
            &format!("'{spell}' {target}"),
            AbilityKind::Spell,
            "cast",
            false,
            false,
            false,
            None,
        );
        drain(&mut self.rx)
    }

    /// A registered object of `kind` named "a test <kind>" with keyword
    /// `word`, held by `holder`; `tweak` edits the prototype.
    fn object(
        &mut self,
        holder: Entity,
        word: &str,
        kind: ObjectType,
        tweak: impl FnOnce(&mut mud_world::ObjectProto),
    ) -> Entity {
        let id = self.next_obj;
        self.next_obj += 1;
        let mut p = object_proto(40, id, kind);
        p.level = 1;
        tweak(&mut p);
        let flags = p.flags.clone();
        let restrictions = p.restrictions.clone();
        let liquid = p.liquid.clone();
        let fuel = p.light_fuel;
        self.world
            .resource_mut::<ObjectPrototypes>()
            .by_key
            .insert((40, id), p);
        let e = self
            .world
            .spawn((
                Item,
                Named {
                    name: format!("a test {word}"),
                },
                Keywords(vec![word.to_string()]),
                WorldKey { zone: 40, id },
                Located(holder),
            ))
            .id();
        let mut em = self.world.entity_mut(e);
        if !flags.is_empty() {
            em.insert(ObjectFlags(flags));
        }
        if !restrictions.is_empty() {
            em.insert(ObjectRestrictions(restrictions));
        }
        if let Some(l) = liquid {
            em.insert(LiquidContainer {
                liquid: l.liquid,
                capacity: l.capacity,
                remaining: l.remaining,
                poisoned: l.poisoned,
            });
        }
        if let Some(f) = fuel {
            em.insert(LightFuel {
                capacity: f.capacity,
                remaining: f.remaining,
            });
        }
        e
    }

    fn weapon(&mut self, holder: Entity) -> Entity {
        self.object(holder, "sword", ObjectType::Weapon, |_| {})
    }

    fn corpse(&mut self, holder: Entity) -> Entity {
        let c = self.object(holder, "corpse", ObjectType::Corpse, |_| {});
        self.world.entity_mut(c).insert((
            Corpse,
            CorpseDecay {
                remaining_secs: 600,
            },
        ));
        c
    }

    fn drink(&mut self, holder: Entity, liquid: &str, capacity: i32, remaining: i32) -> Entity {
        let liquid = liquid.to_string();
        self.object(holder, "flask", ObjectType::Drinkcontainer, |p| {
            p.liquid = Some(mud_world::LiquidProto {
                liquid,
                capacity,
                remaining,
                poisoned: false,
            });
        })
    }

    fn light(&mut self, holder: Entity, capacity: i32, remaining: i32) -> Entity {
        self.object(holder, "lamp", ObjectType::Light, |p| {
            p.light_fuel = Some(mud_world::LightFuelProto {
                capacity,
                remaining,
            });
        })
    }

    fn has_flag(&self, e: Entity, f: ObjectFlag) -> bool {
        self.world
            .get::<ObjectFlags>(e)
            .is_some_and(|flags| flags.has(f))
    }

    fn effects_on(&mut self, target: Entity) -> Vec<String> {
        let mut q = self.world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(&self.world)
            .filter(|(_, a)| a.0 == target)
            .map(|(e, _)| e.name.clone())
            .collect()
    }

    fn followers(&mut self) -> Vec<(i32, i32)> {
        let caster = self.caster;
        let mut q = self
            .world
            .query_filtered::<(&Follower, &WorldKey), With<Mob>>();
        q.iter(&self.world)
            .filter(|(f, _)| f.0 == caster)
            .map(|(_, k)| (k.zone, k.id))
            .collect()
    }

    fn goblin(&mut self) -> Entity {
        self.world
            .spawn((
                Mob,
                Named {
                    name: "a goblin".into(),
                },
                Keywords(vec!["goblin".into()]),
                Located(self.room),
                Health { hp: 50, max: 50 },
            ))
            .id()
    }
}

// -- Bless -----------------------------------------------------------------

#[test]
fn bless_a_weapon_bars_evil_and_says_it_glows() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    let out = f.cast("bless", "sword");
    assert!(out.contains("A test sword glows briefly."), "{out}");
    assert_eq!(
        f.world.get::<ItemBarredAlignments>(sword).unwrap().0,
        vec![Alignment::Evil]
    );
    assert!(f.world.get::<ItemAlterDirty>(sword).is_some());
    assert!(
        f.effects_on(f.caster).is_empty(),
        "the object took the cast"
    );
}

#[test]
fn bless_refuses_what_legacy_refused() {
    let mut f = fx();
    let cases: Vec<(Entity, &str)> = vec![
        (
            f.object(f.caster, "robe", ObjectType::Armor, |_| {}),
            "only effective on weapons",
        ),
        (
            f.object(f.caster, "titan", ObjectType::Weapon, |p| p.level = 200),
            "too powerful for you to bless",
        ),
        (
            f.object(f.caster, "anvil", ObjectType::Weapon, |p| p.weight = 900.0),
            "too large for you to bless",
        ),
        (
            f.object(f.caster, "torch", ObjectType::Weapon, |p| {
                p.flags = vec![ObjectFlag::Glow];
            }),
            "doesn't seem receptive",
        ),
        (
            f.object(f.caster, "brand", ObjectType::Weapon, |p| {
                p.restrictions = vec![ObjectRestriction::NoDrop];
            }),
            "doesn't seem receptive",
        ),
        (
            f.object(f.caster, "halo", ObjectType::Weapon, |p| {
                p.restricted_alignments = vec![Alignment::Good];
            }),
            "doesn't seem receptive",
        ),
    ];
    for (item, expect) in cases {
        let word = f.world.get::<Keywords>(item).unwrap().0[0].clone();
        let out = f.cast("bless", &word);
        assert!(out.contains(expect), "{word}: {out}");
        assert!(
            f.world.get::<ItemBarredAlignments>(item).is_none(),
            "{word}"
        );
    }
}

#[test]
fn a_second_bless_is_repelled_and_a_magical_weapon_refuses() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    f.cast("bless", "sword");
    let out = f.cast("bless", "sword");
    assert!(
        out.contains("The blessing is repelled from a test sword."),
        "{out}"
    );
    let ring = f.object(f.caster, "wand", ObjectType::Weapon, |_| {});
    f.world
        .entity_mut(ring)
        .insert(mud_world::components::ItemApplies(vec![(
            "accuracy".into(),
            2,
        )]));
    let out = f.cast("bless", "wand");
    assert!(out.contains("The blessing is repelled"), "{out}");
    assert_eq!(
        f.world.get::<ItemBarredAlignments>(sword).unwrap().0.len(),
        1
    );
}

#[test]
fn staff_bless_skips_the_gates() {
    let mut f = fx();
    f.world.entity_mut(f.caster).insert(Account {
        user_id: "u".into(),
        character_id: "c".into(),
        role: mud_db::enums::UserRole::Immortal,
        account_role: mud_db::enums::UserRole::Immortal,
        perms: vec![],
    });
    let shirt = f.object(f.caster, "shirt", ObjectType::Armor, |_| {});
    let out = f.cast("bless", "shirt");
    assert!(out.contains("glows briefly"), "{out}");
    assert!(f.world.get::<ItemBarredAlignments>(shirt).is_some());
}

#[test]
fn bless_on_a_person_still_grants_the_status() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    f.cast("bless", "");
    assert_eq!(f.effects_on(f.caster), vec!["bless".to_string()]);
    assert!(f.world.get::<ItemBarredAlignments>(sword).is_none());
}

// -- Invisibility ----------------------------------------------------------

#[test]
fn invisibility_makes_an_item_invisible_once() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    let out = f.cast("invisibility", "sword");
    assert!(out.contains("A test sword vanishes."), "{out}");
    assert!(f.has_flag(sword, ObjectFlag::Invisible));
    // Recorded as the spell's own flag, so it is saved with the instance.
    assert_eq!(
        crate::item_alter::snapshot(&f.world, sword).flags_added,
        vec![ObjectFlag::Invisible]
    );
    let out = f.cast("invisibility", "sword");
    assert!(out.contains(NOTHING), "{out}");
}

#[test]
fn invisibility_works_on_the_floor_and_respects_no_invisible() {
    let mut f = fx();
    let rug = f.object(f.room, "rug", ObjectType::Other, |_| {});
    f.cast("invisibility", "rug");
    assert!(f.has_flag(rug, ObjectFlag::Invisible));
    let idol = f.object(f.caster, "idol", ObjectType::Other, |p| {
        p.restrictions = vec![ObjectRestriction::NoInvisible];
    });
    let out = f.cast("invisibility", "idol");
    assert!(out.contains(NOTHING), "{out}");
    assert!(!f.has_flag(idol, ObjectFlag::Invisible));
}

#[test]
fn invisibility_on_a_person_still_adds_evasion() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    f.cast("invisibility", "");
    assert_eq!(f.effects_on(f.caster), vec!["evasion".to_string()]);
    assert!(!f.has_flag(sword, ObjectFlag::Invisible));
}

// -- Dispel Magic ----------------------------------------------------------

#[test]
fn dispel_magic_makes_an_invisible_item_visible() {
    let mut f = fx();
    let sword = f.object(f.caster, "sword", ObjectType::Weapon, |p| {
        p.flags = vec![ObjectFlag::Invisible];
    });
    let out = f.cast("dispel magic", "sword");
    assert!(out.contains("A test sword fades into existence."), "{out}");
    assert!(!f.has_flag(sword, ObjectFlag::Invisible));
    // The prototype's own flag is recorded as lost so a reload keeps it off.
    let delta = crate::item_alter::snapshot(&f.world, sword);
    assert_eq!(delta.flags_removed, vec![ObjectFlag::Invisible]);
    assert!(f.world.get::<ItemRemovedFlags>(sword).is_some());
}

#[test]
fn dispel_magic_fails_against_a_powerful_object_and_ignores_plain_ones() {
    let mut f = fx();
    let relic = f.object(f.caster, "relic", ObjectType::Other, |p| {
        p.level = 500;
        p.flags = vec![ObjectFlag::Invisible];
    });
    let out = f.cast("dispel magic", "relic");
    assert!(out.contains("You fail."), "{out}");
    assert!(f.has_flag(relic, ObjectFlag::Invisible));
    let plain = f.weapon(f.caster);
    let out = f.cast("dispel magic", "sword");
    assert!(out.contains(NOTHING), "{out}");
    assert!(f.world.get::<ItemAlterDirty>(plain).is_none());
}

#[test]
fn dispel_magic_on_a_floor_item_and_the_spell_flag_forgotten() {
    let mut f = fx();
    let rug = f.object(f.room, "rug", ObjectType::Other, |_| {});
    f.cast("invisibility", "rug");
    f.cast("dispel magic", "rug");
    assert!(!f.has_flag(rug, ObjectFlag::Invisible));
    assert!(crate::item_alter::snapshot(&f.world, rug).is_empty());
}

#[test]
fn dispel_magic_on_a_person_still_runs_the_dispel_path() {
    let mut f = fx();
    let hidden = f.object(f.caster, "sword", ObjectType::Weapon, |p| {
        p.flags = vec![ObjectFlag::Invisible];
    });
    let goblin = f.goblin();
    let out = f.cast("dispel magic", "goblin");
    assert!(!out.contains("fades into existence"), "{out}");
    assert!(
        f.has_flag(hidden, ObjectFlag::Invisible),
        "the mob took the cast"
    );
    assert!(f.world.get_entity(goblin).is_ok());
}

// -- Poison / Remove Poison ------------------------------------------------

#[test]
fn poison_and_remove_poison_toggle_food() {
    let mut f = fx();
    let pie = f.object(f.caster, "pie", ObjectType::Food, |_| {});
    let out = f.cast("poison", "pie");
    assert!(out.contains("A test pie steams briefly."), "{out}");
    assert_eq!(f.world.get::<FoodPoisoned>(pie).map(|p| p.0), Some(true));
    assert_eq!(
        crate::item_alter::snapshot(&f.world, pie).poisoned,
        Some(true)
    );
    let out = f.cast("poison", "pie");
    assert!(out.contains(NOTHING), "{out}");
    let out = f.cast("remove poison", "pie");
    assert!(out.contains("steams briefly"), "{out}");
    assert_eq!(f.world.get::<FoodPoisoned>(pie).map(|p| p.0), Some(false));
    assert_eq!(crate::item_alter::snapshot(&f.world, pie).poisoned, None);
    let out = f.cast("remove poison", "pie");
    assert!(out.contains(NOTHING), "{out}");
}

#[test]
fn poison_a_drink_and_cure_a_prototype_poisoned_food() {
    let mut f = fx();
    let flask = f.drink(f.caster, "water", 10, 5);
    f.cast("poison", "flask");
    assert!(f.world.get::<LiquidContainer>(flask).unwrap().poisoned);
    f.cast("remove poison", "flask");
    assert!(!f.world.get::<LiquidContainer>(flask).unwrap().poisoned);
    let stew = f.object(f.room, "stew", ObjectType::Food, |p| p.food_poisoned = true);
    f.cast("remove poison", "stew");
    assert_eq!(f.world.get::<FoodPoisoned>(stew).map(|p| p.0), Some(false));
    assert_eq!(
        crate::item_alter::snapshot(&f.world, stew).poisoned,
        Some(false)
    );
}

#[test]
fn poison_ignores_things_that_are_not_food_or_drink() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    let out = f.cast("poison", "sword");
    assert!(out.contains(NOTHING), "{out}");
    assert!(f.world.get::<FoodPoisoned>(sword).is_none());
}

#[test]
fn poison_on_a_mob_still_weakens_it_and_remove_poison_on_self_cleanses() {
    let mut f = fx();
    let pie = f.object(f.caster, "pie", ObjectType::Food, |_| {});
    let goblin = f.goblin();
    f.cast("poison", "goblin");
    assert_eq!(f.effects_on(goblin), vec!["str".to_string()]);
    assert!(f.world.get::<FoodPoisoned>(pie).is_none());
    f.world.spawn((
        EffectInstance {
            kind: 99,
            name: "poison".into(),
            strength: 1,
            remaining_secs: 100,
            source: mud_world::EffectSource::Spell,
            ability_id: None,
        },
        AppliedTo(f.caster),
    ));
    f.world.resource_mut::<EffectCatalog>().by_id.insert(99, {
        let mut d = effect_def(99, "modify");
        d.name = "poison".into();
        d.tags = vec!["poison".into()];
        d
    });
    f.cast("remove poison", "");
    assert!(f.effects_on(f.caster).iter().all(|n| n != "poison"));
}

// -- Create Water ----------------------------------------------------------

#[test]
fn create_water_fills_a_container() {
    let mut f = fx();
    let flask = f.drink(f.caster, "water", 10, 2);
    let out = f.cast("create water", "flask");
    assert!(out.contains("Water wells up within a test flask."), "{out}");
    let lc = f.world.get::<LiquidContainer>(flask).unwrap();
    assert_eq!((lc.liquid.as_str(), lc.remaining), ("water", 10));
}

#[test]
fn create_water_without_a_container_says_it_needs_one() {
    let mut f = fx();
    let flask = f.drink(f.caster, "water", 10, 2);
    let out = f.cast("create water", "");
    assert!(out.contains("not a valid target"), "{out}");
    assert_eq!(f.world.get::<LiquidContainer>(flask).unwrap().remaining, 2);
}

#[test]
fn create_water_into_something_else_makes_slime_and_a_full_flask_says_so() {
    let mut f = fx();
    let wine = f.drink(f.caster, "wine", 10, 3);
    f.cast("create water", "flask");
    let lc = f.world.get::<LiquidContainer>(wine).unwrap();
    assert_eq!((lc.liquid.as_str(), lc.remaining), ("slime-mold-juice", 10));
    let out = f.cast("create water", "flask");
    assert!(
        out.contains("A test flask seems to be full already."),
        "{out}"
    );
    // An empty container takes plain water whatever it held.
    let empty = f.drink(f.room, "wine", 10, 0);
    f.world
        .entity_mut(empty)
        .insert(Keywords(vec!["jug".into()]));
    f.world.entity_mut(empty).insert(Located(f.caster));
    f.cast("create water", "jug");
    assert_eq!(
        f.world.get::<LiquidContainer>(empty).unwrap().liquid,
        "water"
    );
}

#[test]
fn create_water_reaches_a_worn_container_and_skips_other_items() {
    let mut f = fx();
    let skin = f.drink(f.caster, "water", 8, 0);
    f.world.entity_mut(skin).insert((
        EquippedSlot(mud_world::Slot::Hold),
        Keywords(vec!["skin".into()]),
    ));
    f.cast("create water", "skin");
    assert_eq!(f.world.get::<LiquidContainer>(skin).unwrap().remaining, 8);
    let sword = f.weapon(f.caster);
    let out = f.cast("create water", "sword");
    assert!(!out.contains("wells up"), "{out}");
    assert!(f.world.get::<LiquidContainer>(sword).is_none());
}

// -- Preserve --------------------------------------------------------------

#[test]
fn preserve_extends_a_corpse_by_the_casters_level_in_hours() {
    let mut f = fx();
    let corpse = f.corpse(f.room);
    let out = f.cast("preserve", "corpse");
    assert!(out.contains("A test corpse glows blue briefly."), "{out}");
    assert!(out.contains("is preserved for 20 more hours."), "{out}");
    assert_eq!(
        f.world.get::<CorpseDecay>(corpse).unwrap().remaining_secs,
        600 + 20 * 75
    );
}

#[test]
fn preserve_refuses_what_is_not_a_corpse_and_still_shields_a_person() {
    let mut f = fx();
    let sword = f.object(f.room, "sword", ObjectType::Weapon, |_| {});
    let out = f.cast("preserve", "sword");
    assert!(out.contains("You can only preserve a corpse!"), "{out}");
    assert!(f.world.get::<CorpseDecay>(sword).is_none());
    f.cast("preserve", "");
    assert_eq!(f.effects_on(f.caster), vec!["sanctuary".to_string()]);
}

// -- Dark Feast ------------------------------------------------------------

#[test]
fn dark_feast_on_a_corpse_feeds_the_caster() {
    let mut f = fx();
    let corpse = f.corpse(f.room);
    let out = f.cast("dark feast", "corpse");
    assert!(
        out.contains("You gorge yourself on a test corpse."),
        "{out}"
    );
    assert_eq!(f.world.get::<Hunger>(f.caster).unwrap().0, 16);
    assert_eq!(f.world.get::<Thirst>(f.caster).unwrap().0, 6);
    assert!(f.world.get_entity(corpse).is_ok(), "the corpse stays");
    f.cast("dark feast", "corpse");
    assert_eq!(f.world.get::<Hunger>(f.caster).unwrap().0, 0);
    assert_eq!(f.world.get::<Thirst>(f.caster).unwrap().0, 0);
}

#[test]
fn dark_feast_refuses_a_non_corpse_and_still_heals_the_caster() {
    let mut f = fx();
    f.object(f.room, "sword", ObjectType::Weapon, |_| {});
    let out = f.cast("dark feast", "sword");
    assert!(out.contains("That is not a corpse!"), "{out}");
    assert_eq!(f.world.get::<Hunger>(f.caster).unwrap().0, 40);
    f.cast("dark feast", "");
    assert_eq!(f.world.get::<Health>(f.caster).unwrap().hp, 60);
}

// -- Illumination / Darkness -----------------------------------------------

#[test]
fn illumination_relights_a_burnt_out_lamp_scaled_by_skill() {
    let mut f = fx();
    let lamp = f.light(f.caster, 24, 0);
    let out = f.cast("illumination", "lamp");
    assert!(
        out.contains("begins glowing with a bright yellow light"),
        "{out}"
    );
    // base 48: 24 + 48 * 100% = 72 hours at full proficiency.
    let fuel = f.world.get::<LightFuel>(lamp).unwrap();
    assert_eq!((fuel.capacity, fuel.remaining), (24, 72));
    assert!(f.world.get::<Lit>(lamp).is_some());
}

#[test]
fn illumination_leaves_a_permanent_light_and_a_non_light_alone() {
    let mut f = fx();
    let beacon = f.light(f.caster, -1, -1);
    let out = f.cast("illumination", "lamp");
    assert!(
        out.contains("washes uselessly over the surface of a test lamp"),
        "{out}"
    );
    assert_eq!(f.world.get::<LightFuel>(beacon).unwrap().remaining, -1);
    let sword = f.weapon(f.caster);
    let out = f.cast("illumination", "sword");
    assert!(out.contains(NOTHING), "{out}");
    assert!(f.world.get::<Lit>(sword).is_none());
}

#[test]
fn darkness_drains_and_snuffs_a_lit_lamp_and_illumination_restores_it() {
    let mut f = fx();
    let lamp = f.light(f.caster, 0, 30);
    f.world.entity_mut(lamp).insert(Lit);
    let out = f.cast("darkness", "lamp");
    assert!(out.contains("You drain a test lamp's energy."), "{out}");
    assert!(out.contains("quickly sputters out"), "{out}");
    let fuel = f.world.get::<LightFuel>(lamp).unwrap();
    assert_eq!((fuel.capacity, fuel.remaining), (30, 0));
    assert!(f.world.get::<Lit>(lamp).is_none());
    let out = f.cast("darkness", "lamp");
    assert!(out.contains(NOTHING), "{out}");
    f.cast("illumination", "lamp");
    assert!(f.world.get::<LightFuel>(lamp).unwrap().remaining > 0);
}

#[test]
fn darkness_on_a_permanent_light_burns_it_out_but_illumination_makes_it_permanent_again() {
    let mut f = fx();
    let beacon = f.light(f.caster, -1, -1);
    f.cast("darkness", "lamp");
    let fuel = f.world.get::<LightFuel>(beacon).unwrap();
    assert_eq!((fuel.capacity, fuel.remaining), (-1, 0));
    assert!(!mud_world::is_lit(&f.world, beacon));
    f.cast("illumination", "lamp");
    assert_eq!(f.world.get::<LightFuel>(beacon).unwrap().remaining, -1);
    assert!(mud_world::is_lit(&f.world, beacon));
}

#[test]
fn darkness_and_illumination_without_an_object_still_cast_on_the_room() {
    let mut f = fx();
    let lamp = f.light(f.caster, 10, 0);
    let out = f.cast("darkness", "");
    assert!(!out.contains("drain"), "{out}");
    let out = f.cast("illumination", "");
    assert!(!out.contains("begins glowing"), "{out}");
    assert_eq!(f.world.get::<LightFuel>(lamp).unwrap().remaining, 0);
}

// -- Animate Dead / Summon Dracolich / Earth Blessing -----------------------

fn with_skeleton(f: &mut Fx) {
    f.world
        .resource_mut::<MobPrototypes>()
        .by_key
        .insert((54, 20), mob_proto(54, 20, MobProfession::Banker));
}

#[test]
fn animate_dead_raises_the_named_corpse_in_the_room() {
    let mut f = fx();
    with_skeleton(&mut f);
    let corpse = f.corpse(f.room);
    let out = f.cast("animate dead", "corpse");
    assert_eq!(f.followers(), vec![(54, 20)], "{out}");
    assert!(f.world.get_entity(corpse).is_err(), "the corpse is used up");
}

#[test]
fn animate_dead_without_a_corpse_still_raises_the_default_and_a_wrong_word_refuses() {
    let mut f = fx();
    with_skeleton(&mut f);
    f.cast("animate dead", "");
    assert_eq!(f.followers(), vec![(54, 20)]);
    let before = f.followers().len();
    let out = f.cast("animate dead", "ogre");
    assert!(out.contains("no corpse"), "{out}");
    assert_eq!(f.followers().len(), before);
}

#[test]
fn summon_dracolich_still_summons_when_an_object_is_named() {
    let mut f = fx();
    f.world
        .resource_mut::<MobPrototypes>()
        .by_key
        .insert((533, 11), mob_proto(533, 11, MobProfession::Banker));
    f.object(f.room, "bones", ObjectType::Other, |_| {});
    let out = f.cast("summon dracolich", "bones");
    assert_eq!(f.followers(), vec![(533, 11)], "{out}");
    let mut g = fx();
    g.world
        .resource_mut::<MobPrototypes>()
        .by_key
        .insert((533, 11), mob_proto(533, 11, MobProfession::Banker));
    g.cast("summon dracolich", "");
    assert_eq!(g.followers(), vec![(533, 11)]);
}

// -- Target order: characters before objects (legacy find_spell_target) -----

impl Fx {
    fn rat(&mut self) -> Entity {
        self.world
            .spawn((
                Mob,
                Named {
                    name: "a rat".into(),
                },
                Keywords(vec!["rat".into()]),
                Located(self.room),
                Health { hp: 50, max: 50 },
            ))
            .id()
    }
}

#[test]
fn cast_poison_rat_hits_the_rat_not_the_rat_tail_in_the_pack() {
    let mut f = fx();
    let tail = f.object(f.caster, "rat", ObjectType::Food, |_| {});
    let rat = f.rat();
    f.cast("poison", "rat");
    assert_eq!(f.effects_on(rat), vec!["str".to_string()]);
    assert!(f.world.get::<FoodPoisoned>(tail).is_none());
    assert!(f.world.get::<ItemAlterDirty>(tail).is_none());
}

#[test]
fn cast_poison_rat_still_poisons_the_tail_when_no_rat_is_here() {
    let mut f = fx();
    let tail = f.object(f.caster, "rat", ObjectType::Food, |_| {});
    let out = f.cast("poison", "rat");
    assert_eq!(
        f.world.get::<FoodPoisoned>(tail).map(|p| p.0),
        Some(true),
        "{out}"
    );
}

#[test]
fn cast_bless_sword_targets_the_sword_when_no_character_matches() {
    let mut f = fx();
    let sword = f.weapon(f.caster);
    let rat = f.rat();
    let out = f.cast("bless", "sword");
    assert!(out.contains("A test sword glows briefly."), "{out}");
    assert!(f.world.get::<ItemAlterDirty>(sword).is_some());
    assert!(f.effects_on(rat).is_empty());
}

#[test]
fn cast_bless_rat_blesses_the_rat_before_a_carried_rat_item() {
    let mut f = fx();
    let sword = f.object(f.caster, "rat", ObjectType::Weapon, |_| {});
    let rat = f.rat();
    f.cast("bless", "rat");
    assert!(f.world.get::<ItemAlterDirty>(sword).is_none());
    assert!(!f.effects_on(rat).is_empty());
}
