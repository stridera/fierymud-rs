//! `stat` on a live player or mob (issue #158): the legacy `do_stat_character`
//! sheet (`act.wizinfo.cpp`) rendered from the ECS state.
//!
//! The layout and colours follow legacy: identity, race / life force /
//! composition / class, the STR..CHA table, HP / MV, coins, the combat
//! numbers, position, flags, weight, conditions, followers / group, effect
//! flags and every active effect, cooldowns, scripts, memory. Wherever
//! legacy printed a `CircleMUD` value this world no longer has (AC, hitroll,
//! damroll, rage), the modern equivalents are printed instead (armor,
//! accuracy, evasion, attack power, ...). A few legacy fields have no
//! equivalent here and are left out: `RNum`, creation date, rage, stance
//! beyond posture, the OLC zone list, queued events, quest stages,
//! permanent titles and the NPC spell-circle line.
//!
//! The text carries XML-Lite colour tags: send it through `send_to`.

use bevy_ecs::prelude::*;
use mud_db::enums::PlayerFlag;
use mud_world::{
    Account, AttachedTriggers, BankWealth, BodyMetrics, ClanMembership, ClassCatalog, CombatStats,
    CoreStats, Drunkenness, EffectInstance, EquippedSlot, Fighting, Focus, Follower, FromMobReset,
    Frozen, Ghost, GroupMember, Guarding, Health, Hiddenness, Hunger, Item, Keywords, LastInputAt,
    Located, Mob, MobPrototypes, ModifyDelta, Named, NaturalAttackType, NaturalDamage, Player,
    PlayerFlags, Posture, PreviousLogin, Profile, RaceCatalog, RaceDefaults, RecallPoint,
    RegenBonus, Resistances, SavingThrows, Shopkeeper, Sized, Stamina, Stunned, SwitchedFrom,
    SwitchedInto, Thirst, TimePlayed, Title, UnarmedDamage, Wealth, WeaponHitroll, WimpyThreshold,
    WizInvis, WorldKey,
};

use super::identify_actor::{composition_tag, life_force_of, lifeforce_style};
use crate::commands::{capitalize, name_of};

/// One game tic of the legacy idle timer: 75 real seconds (a MUD hour).
const IDLE_TIC_SECS: u64 = 75;

/// Legacy `FCYN` / `FGRN` / `FYEL` / `HYEL` / `FWHT` as XML-Lite tags.
const CYAN: &str = "<cyan>";
const GREEN: &str = "<green>";
const YELLOW: &str = "<yellow>";
const BOLD_YELLOW: &str = "<b:yellow>";
const WHITE: &str = "<white>";
const END: &str = "</>";

/// `<tag>text</>`.
fn paint(tag: &str, text: impl std::fmt::Display) -> String {
    format!("{tag}{text}{END}")
}

/// Legacy `sprintflag`: the names space-separated, or `NO FLAGS`.
fn flag_list<I: IntoIterator<Item = String>>(names: I) -> String {
    let joined = names.into_iter().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        "NO FLAGS".to_string()
    } else {
        joined
    }
}

/// A flag label as legacy spells flags: upper case, underscores.
fn flag_name(label: &str) -> String {
    label.to_ascii_uppercase().replace(['-', ' '], "_")
}

/// Legacy coin colours: platinum cyan, gold bright yellow, silver white,
/// copper yellow. A copper total splits 1000 / 100 / 10 / 1.
fn coin_line(total: i64) -> String {
    let total = total.max(0);
    format!(
        "[{}p / {}g / {}s / {}c]",
        paint(CYAN, total / 1000),
        paint(BOLD_YELLOW, (total % 1000) / 100),
        paint(WHITE, (total % 100) / 10),
        paint(YELLOW, total % 10),
    )
}

/// `prefix` then `names` comma-separated, broken into lines at 78 columns
/// (legacy wrapped the follower and group lists the same way).
fn wrapped(prefix: &str, plain_prefix_len: usize, names: &[String]) -> String {
    let mut out = String::new();
    let mut line = prefix.to_string();
    let mut width = plain_prefix_len;
    let mut first_on_line = true;
    for (i, name) in names.iter().enumerate() {
        let visible = name.chars().count();
        if width + visible >= 78 {
            out.push_str(&line);
            out.push_str(",\r\n");
            line = String::new();
            width = 0;
            first_on_line = true;
        }
        let sep = if i > 0 && !first_on_line { "," } else { "" };
        line.push_str(&format!("{sep} {name}"));
        width += sep.len() + 1 + visible;
        first_on_line = false;
    }
    out.push_str(&line);
    out.push_str("\r\n");
    out
}

/// `(zone, id)` of the entity's own prototype row, when it has one.
fn key_of(world: &World, e: Entity) -> Option<(i32, i32)> {
    world.get::<WorldKey>(e).map(|k| (k.zone, k.id))
}

/// `"(zone, id)"` of a room entity, `(none)` if it has no key.
fn room_label(world: &World, room: Entity) -> String {
    key_of(world, room).map_or_else(|| "(none)".to_string(), |(z, i)| format!("({z}, {i})"))
}

/// A race's catalog row; mob protos spell the race lower-case.
fn race_row<'a>(world: &'a World, race: &str) -> Option<&'a mud_world::RaceDef> {
    let cat = world.get_resource::<RaceCatalog>()?;
    cat.get(race)
        .or_else(|| cat.get(&race.to_ascii_uppercase()))
}

/// The stat's slot in STR / INT / WIS / DEX / CON / CHA order.
fn stat_slot(target: &str) -> Option<usize> {
    match target {
        "str" | "strength" | "str_bonus" => Some(0),
        "int" | "intelligence" | "int_bonus" => Some(1),
        "wis" | "wisdom" | "wis_bonus" => Some(2),
        "dex" | "dexterity" | "dex_bonus" => Some(3),
        "con" | "constitution" | "con_bonus" => Some(4),
        "cha" | "charisma" | "cha_bonus" => Some(5),
        _ => None,
    }
}

/// `(total, permanent)` modifier sums per stat slot, from every effect on
/// `target` that records a stat delta. Permanent means no expiry (gear,
/// innate and mob-default effects).
fn stat_modifiers(world: &mut World, target: Entity) -> ([i32; 6], [i32; 6]) {
    let mut total = [0; 6];
    let mut perm = [0; 6];
    for e in mud_world::effects_on(world, target) {
        let (Some(d), Some(inst)) = (world.get::<ModifyDelta>(e), world.get::<EffectInstance>(e))
        else {
            continue;
        };
        if let Some(slot) = stat_slot(&d.target.to_ascii_lowercase()) {
            total[slot] += d.amount;
            if inst.remaining_secs < 0 {
                perm[slot] += d.amount;
            }
        }
    }
    (total, perm)
}

/// The effect flags legacy printed on the `EFF:` line, from the marker
/// components the effect system maintains.
fn effect_flags(world: &World, e: Entity) -> Vec<&'static str> {
    use mud_world::{
        Blinded, Blur, DetectAlign, DetectInvis, Empowered, Familiar, Farsee, Flying, Glory, Haste,
        Infravision, Invisible, Misdirection, ProtectFromEvil, ProtectFromGood, Sanctuary,
        SenseLife, Sneaking, WaterWalk,
    };
    fn has<T: Component>(world: &World, e: Entity) -> bool {
        world.get::<T>(e).is_some()
    }
    let mut out = Vec::new();
    let table: [(bool, &'static str); 21] = [
        (has::<Invisible>(world, e), "INVIS"),
        (has::<DetectInvis>(world, e), "DET_INVIS"),
        (has::<DetectAlign>(world, e), "DET_ALIGN"),
        (has::<SenseLife>(world, e), "SENSE_LIFE"),
        (has::<Infravision>(world, e), "INFRA"),
        (has::<Blinded>(world, e), "BLIND"),
        (has::<Flying>(world, e), "FLY"),
        (has::<WaterWalk>(world, e), "WATERWALK"),
        (has::<Farsee>(world, e), "FARSEE"),
        (has::<Familiar>(world, e), "FAMILIARITY"),
        (has::<mud_world::Bless>(world, e), "BLESS"),
        (has::<Sanctuary>(world, e), "SANCT"),
        (has::<Haste>(world, e), "HASTE"),
        (has::<Blur>(world, e), "BLUR"),
        (has::<Empowered>(world, e), "EMPOWERED"),
        (has::<Glory>(world, e), "GLORY"),
        (has::<Misdirection>(world, e), "MISDIRECTION"),
        (has::<ProtectFromEvil>(world, e), "PROT_EVIL"),
        (has::<ProtectFromGood>(world, e), "PROT_GOOD"),
        (has::<Sneaking>(world, e), "SNEAK"),
        (has::<mud_world::Feared>(world, e), "FEAR"),
    ];
    for (on, name) in table {
        if on {
            out.push(name);
        }
    }
    out
}

/// Names of every entity satisfying `pred`, in a stable order.
fn names_where(
    world: &mut World,
    mut pred: impl FnMut(&World, Entity) -> bool,
    include: impl Fn(&World, Entity) -> bool,
) -> Vec<String> {
    let mut found: Vec<Entity> = world
        .query_filtered::<Entity, With<Named>>()
        .iter(world)
        .collect();
    found.retain(|e| include(world, *e) && pred(world, *e));
    found.sort();
    found.into_iter().map(|e| name_of(world, e)).collect()
}

/// Legacy stance label for the `Pos:` line.
fn stance_label(world: &World, e: Entity) -> &'static str {
    if world.get::<Ghost>(e).is_some() {
        "dead"
    } else if world.get::<Stunned>(e).is_some() {
        "stunned"
    } else if world.get::<Fighting>(e).is_some() {
        "fighting"
    } else {
        match world.get::<Posture>(e).map(|p| p.0) {
            Some(mud_world::PostureKind::Sleeping) => "sleeping",
            Some(mud_world::PostureKind::Resting) => "resting",
            _ => "alert",
        }
    }
}

/// Per-second HP regen the regen tick would grant `e` right now.
fn hp_gain(world: &World, e: Entity) -> i32 {
    if world.get::<Player>(e).is_none()
        || world.get::<Fighting>(e).is_some()
        || world.get::<Ghost>(e).is_some()
    {
        return 0;
    }
    let posture = world
        .get::<Posture>(e)
        .map_or(mud_world::PostureKind::Standing, |p| p.0);
    let bonus = world.get::<RegenBonus>(e).map_or(0, |b| b.hp);
    crate::regen::health_per_tick(posture)
        .saturating_add(bonus)
        .max(0)
}

/// Per-second stamina regen the regen tick would grant `e` right now.
fn mv_gain(world: &World, e: Entity) -> i32 {
    if world.get::<Player>(e).is_none()
        || world.get::<Fighting>(e).is_some()
        || world.get::<Ghost>(e).is_some()
    {
        return 0;
    }
    let posture = world
        .get::<Posture>(e)
        .map_or(mud_world::PostureKind::Standing, |p| p.0);
    let bonus = world.get::<RegenBonus>(e).map_or(0, |b| b.stamina);
    crate::regen::stamina_per_tick(posture)
        .saturating_add(bonus)
        .max(0)
}

/// The full legacy-style stat sheet for a live player or mob.
#[allow(clippy::too_many_lines)]
pub(crate) fn render_actor_stat(world: &mut World, target: Entity) -> String {
    let is_player = world.get::<Player>(target).is_some();
    let is_mob = world.get::<Mob>(target).is_some();
    let key = key_of(world, target);
    let proto = if is_mob {
        key.and_then(|k| {
            world
                .get_resource::<MobPrototypes>()
                .and_then(|p| p.by_key.get(&k))
                .cloned()
        })
    } else {
        None
    };
    let profile = world.get::<Profile>(target).cloned();
    let name = name_of(world, target);
    let mut out = String::from("\r\n");

    // ---- Vital data ----------------------------------------------------
    let room = world
        .get::<Located>(target)
        .map_or_else(|| "(none)".to_string(), |l| room_label(world, l.0));
    if is_player {
        let id = world
            .get::<Account>(target)
            .map_or_else(|| "?".to_string(), |a| a.character_id.clone());
        out.push_str(&format!("PC '{name}'  IDNum: [{id}], In room {room}\r\n"));
    } else {
        let kind = if proto.is_some() { "MOB" } else { "NPC" };
        out.push_str(&format!(
            "{kind} '{name}'  Entity: [{target:?}], In room {room}\r\n"
        ));
    }

    // ---- Strings --------------------------------------------------------
    if is_mob {
        let alias = world
            .get::<Keywords>(target)
            .map_or_else(String::new, |k| k.0.join(" "));
        match key {
            Some((z, i)) => out.push_str(&format!("Alias: {alias}, VNum: ({z}, {i})\r\n")),
            None => out.push_str(&format!("Alias: {alias}\r\n")),
        }
        let ldesc = world
            .get::<mud_world::Description>(target)
            .map(|d| d.0.trim().to_string())
            .filter(|d| !d.is_empty());
        out.push_str(&format!(
            "L-Des: {}\r\n",
            ldesc.as_deref().unwrap_or("<None>")
        ));
        if let Some(desc) = world.get::<mud_world::ExamineText>(target) {
            let desc = desc.0.trim_end();
            if !desc.is_empty() {
                out.push_str(&format!("Desc: {desc}\r\n"));
            }
        }
    } else {
        let title = world
            .get::<Title>(target)
            .map(|t| t.0.clone())
            .filter(|t| !t.is_empty());
        out.push_str(&format!(
            "Title: {}\r\n",
            title.as_deref().unwrap_or("<None>")
        ));
    }

    // ---- Role / class / race -------------------------------------------
    let class_id = profile
        .as_ref()
        .and_then(|p| p.class_id)
        .or_else(|| proto.as_ref().and_then(|p| p.class_id));
    let class_name = class_id
        .and_then(|id| {
            world
                .get_resource::<ClassCatalog>()
                .and_then(|c| c.by_id.get(&id).map(|d| d.name.clone()))
        })
        .unwrap_or_else(|| "Classless".to_string());
    if let Some(p) = &proto {
        out.push_str(&format!(
            "Role: {}, Class: {class_name}\r\n",
            p.role.label()
        ));
    }
    let race_raw = profile
        .as_ref()
        .map(|p| p.race.clone())
        .or_else(|| proto.as_ref().map(|p| p.race.clone()))
        .unwrap_or_default();
    let row = race_row(world, &race_raw);
    let race_name = row.map_or_else(
        || {
            if race_raw.is_empty() {
                "Unknown".to_string()
            } else {
                capitalize(&race_raw.to_ascii_lowercase())
            }
        },
        |r| r.name.clone(),
    );
    let race_align = match row.map(|r| r.race_align.to_ascii_uppercase()).as_deref() {
        Some("GOOD") => paint(BOLD_YELLOW, "GOOD"),
        Some("EVIL") => paint("<b:red>", "EVIL"),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    };
    let size_label = world
        .get::<Sized>(target)
        .map(|s| s.0.label().to_string())
        .or_else(|| {
            world
                .get_resource::<RaceDefaults>()
                .and_then(|r| r.size_by_race.get(&race_raw.to_ascii_uppercase()).cloned())
                .map(|s| capitalize(&s.to_ascii_lowercase()))
        })
        .unwrap_or_else(|| "Medium".to_string());
    let gender = profile
        .as_ref()
        .map(|p| p.gender.clone())
        .or_else(|| proto.as_ref().map(|p| p.gender.clone()))
        .unwrap_or_else(|| "neutral".to_string());
    out.push_str(&format!(
        "Race: {race_name}, Race Align: {race_align}, Size: {}, Gender: {}\r\n",
        capitalize(&size_label.to_ascii_lowercase()),
        capitalize(&gender),
    ));
    let (lf_name, lf_tag) = lifeforce_style(life_force_of(world, target));
    let comp = mud_world::actor_composition(world, target);
    let comp_tag = composition_tag(comp);
    let comp_name = capitalize(&format!("{comp:?}").to_ascii_lowercase());
    let comp_text = if comp_tag.is_empty() {
        comp_name
    } else {
        paint(comp_tag, comp_name)
    };
    out.push_str(&format!(
        "Life force: {}, Composition: {comp_text}\r\n",
        paint(lf_tag, capitalize(lf_name)),
    ));
    let level = mud_world::effective_level(world, target);
    let alignment = world.get::<CombatStats>(target).map_or(0, |c| c.alignment);
    let align_word = mud_db::enums::Alignment::from_score(alignment).label();
    if let Some(prof) = &profile {
        let to_level = crate::commands::level_progress_for(world, target, prof)
            .map(|lp| {
                format!(
                    ", To level: [{}]",
                    paint(YELLOW, (lp.next_level_xp - lp.current_xp).max(0))
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "Class: {class_name}, Lev: [{}], XP: [{}]{to_level}, Align: [{alignment:4}] ({align_word})\r\n",
            paint(YELLOW, format!("{level:2}")),
            paint(YELLOW, format!("{:7}", prof.experience)),
        ));
    } else {
        out.push_str(&format!(
            "Lev: [{}], Align: [{alignment:4}] ({align_word})\r\n",
            paint(YELLOW, format!("{level:2}")),
        ));
    }

    // ---- Player-only history -------------------------------------------
    if is_player {
        let last_logon = world
            .get::<PreviousLogin>(target)
            .and_then(|p| chrono::DateTime::from_timestamp(p.0, 0))
            .map_or_else(|| "never".to_string(), |t| t.format("%b %d %Y").to_string());
        let played = world.get::<TimePlayed>(target).map_or(0, |t| t.0.max(0));
        let age = profile
            .as_ref()
            .and_then(|p| crate::commands::format_age(p.level))
            .unwrap_or_else(|| "?".to_string());
        out.push_str(&format!(
            "Last Logon: [{last_logon}], Played: [{}h {}m]\r\n",
            played / 3600,
            (played / 60) % 60,
        ));
        let home = world
            .get::<RecallPoint>(target)
            .filter(|r| world.get_entity(r.0).is_ok())
            .map_or_else(|| "<none>".to_string(), |r| room_label(world, r.0));
        let mut line = format!("Age: [{age}], Homeroom: [{home}]");
        if let Some(c) = world.get::<ClanMembership>(target) {
            line.push_str(&format!(", Clan: [{}], Rank: [{}]", c.clan_abbrev, c.rank));
        }
        if let Some(bm) = world.get::<BodyMetrics>(target) {
            line.push_str(&format!(
                ", Height: [{}in], Weight: [{}lb]",
                bm.height, bm.weight
            ));
        }
        out.push_str(&line);
        out.push_str("\r\n");
    }

    // ---- Attributes ------------------------------------------------------
    if let Some(cs) = world.get::<CoreStats>(target).copied() {
        let (total, perm) = stat_modifiers(world, target);
        let affected = [
            cs.strength,
            cs.intelligence,
            cs.wisdom,
            cs.dexterity,
            cs.constitution,
            cs.charisma,
        ];
        let natural: Vec<i32> = affected.iter().zip(total).map(|(a, t)| a - t).collect();
        let actual: Vec<i32> = natural.iter().zip(perm).map(|(n, p)| n + p).collect();
        let row = |label: &str, v: &[i32]| {
            format!(
                "{label}{}{:3}   {:3}   {:3}   {:3}   {:3}   {:3}{END}\r\n",
                CYAN, v[0], v[1], v[2], v[3], v[4], v[5]
            )
        };
        out.push_str("         STR   INT   WIS   DEX   CON   CHA\r\n");
        out.push_str(&row("ACTUAL   ", &actual));
        out.push_str(&row("NATURAL  ", &natural));
        out.push_str(&row("AFFECTED ", &affected));
    }

    // ---- Points ----------------------------------------------------------
    let hp = world.get::<Health>(target).copied();
    let regen_bonus = world.get::<RegenBonus>(target).copied().unwrap_or_default();
    if let Some(h) = hp {
        out.push_str(&format!(
            "HP: [{}]  HP Gain: [{}/s] HP Regen Bonus: [{}]\r\n",
            paint(GREEN, format!("{}/{}", h.hp, h.max)),
            paint(GREEN, hp_gain(world, target)),
            paint(GREEN, regen_bonus.hp),
        ));
    }
    let mv = world
        .get::<Stamina>(target)
        .map(|s| (s.current, s.max))
        .or_else(|| {
            world
                .get::<mud_world::MovementPoints>(target)
                .map(|m| (m.current, m.max))
        });
    if let Some((cur, max)) = mv {
        out.push_str(&format!(
            "MV: [{}]  MV Gain: [{}/s] MV Regen Bonus: [{}]\r\n",
            paint(GREEN, format!("{cur}/{max}")),
            paint(GREEN, mv_gain(world, target)),
            paint(GREEN, regen_bonus.stamina),
        ));
    }
    if is_player || world.get::<Focus>(target).is_some() {
        out.push_str(&format!(
            "Focus: [{}]\r\n",
            paint(GREEN, world.get::<Focus>(target).map_or(0, |f| f.0))
        ));
    }
    let coins = world
        .get::<Wealth>(target)
        .map(|w| w.0)
        .or_else(|| proto.as_ref().map(|p| p.wealth))
        .unwrap_or(0);
    let bank = world.get::<BankWealth>(target).map_or(0, |b| b.0);
    out.push_str(&format!(
        "Coins: {}, Bank: {}\r\n",
        coin_line(coins),
        coin_line(bank)
    ));

    // ---- Combat numbers --------------------------------------------------
    if let Some(c) = world.get::<CombatStats>(target).copied() {
        out.push_str(&format!(
            "Armor: [{}%/{}], Accuracy: [{}], Evasion: [{}], Attack Power: [{:+}]\r\n",
            c.armor_pct, c.armor_flat, c.accuracy, c.evasion, c.attack_power
        ));
        out.push_str(&format!(
            "Spell Power: [{:+}], Crit: [{}%], Pen: [{}%/{}], Ward: [{}%], Hardness: [{}]\r\n",
            c.spell_power, c.crit_chance, c.pen_pct, c.pen_flat, c.ward_pct, c.hardness
        ));
    }
    let saves = world
        .get::<SavingThrows>(target)
        .copied()
        .unwrap_or_default();
    out.push_str(&format!(
        "Saving throws: [{}/{}/{}/{}/{}], Weapon Hitroll: [{:+}], Unarmed Damage: [{:+}]\r\n",
        saves.para,
        saves.rod,
        saves.petri,
        saves.breath,
        saves.spell,
        world.get::<WeaponHitroll>(target).map_or(0, |w| w.0),
        world.get::<UnarmedDamage>(target).map_or(0, |u| u.0),
    ));
    if let Some(r) = world.get::<Resistances>(target) {
        let mut parts: Vec<String> =
            r.0.iter()
                .filter(|(_, v)| **v != 0)
                .map(|(k, v)| format!("{k:?} {v:+}"))
                .collect();
        parts.sort();
        if !parts.is_empty() {
            out.push_str(&format!("Resistances: {}\r\n", parts.join(", ")));
        }
    }
    out.push_str(&format!(
        "Perception: [{:4}], Hiddenness: [{:4}]\r\n",
        crate::hiding::perception_of(world, target),
        world.get::<Hiddenness>(target).map_or(0, |h| h.0),
    ));

    // ---- Status ----------------------------------------------------------
    let posture = world
        .get::<Posture>(target)
        .map_or("standing", |p| p.0.label());
    let mut pos = format!("Pos: {posture} ({})", stance_label(world, target));
    if let Some(p) = &proto {
        pos.push_str(&format!(
            ", Default Pos: {}",
            p.default_position.as_str().to_ascii_lowercase()
        ));
    }
    let fighting = world
        .get::<Fighting>(target)
        .map_or_else(|| "<none>".to_string(), |f| name_of(world, f.0));
    pos.push_str(&format!(", Fighting: {fighting}"));
    if let Some(s) = world.get::<SwitchedInto>(target) {
        pos.push_str(&format!(", Switched into: {}", name_of(world, s.0)));
    }
    out.push_str(&pos);
    out.push_str("\r\n");

    let mut conn: Vec<String> = Vec::new();
    if is_player {
        let idle = world
            .get::<LastInputAt>(target)
            .map_or(0, |l| l.0.elapsed().as_secs() / IDLE_TIC_SECS);
        conn.push(format!(
            "Idle: [{idle} tic{}]",
            if idle == 1 { "" } else { "s" }
        ));
        let state = if world.get::<crate::commands::Linkdead>(target).is_some() {
            "Linkdead"
        } else if world.get::<crate::commands::Connection>(target).is_some() {
            "Playing"
        } else {
            "Disconnected"
        };
        conn.push(format!("Connected: {state}"));
    }
    if let Some(s) = world.get::<SwitchedFrom>(target) {
        conn.push(format!("Switched into by: {}", name_of(world, s.0)));
    }
    if !conn.is_empty() {
        out.push_str(&conn.join(", "));
        out.push_str("\r\n");
    }

    if is_mob {
        let spec = match world.get::<Shopkeeper>(target) {
            Some(s) => format!("Shopkeeper ({}, {})", s.shop_zone_id, s.shop_id),
            None => "None".to_string(),
        };
        let nd = world
            .get::<NaturalDamage>(target)
            .copied()
            .unwrap_or_default();
        let attack = world.get::<NaturalAttackType>(target).map_or_else(
            || "<INVALID>".to_string(),
            |a| a.0.label().to_ascii_lowercase(),
        );
        out.push_str(&format!(
            "Mob Spec-Proc: {spec}, NPC Bare Hand Dam: {}d{}+{}, Attack type: {attack}\r\n",
            nd.num, nd.size, nd.bonus
        ));
    }

    // ---- Flags -----------------------------------------------------------
    if is_mob {
        let mut names: Vec<String> = Vec::new();
        if let Some(b) = world.get::<mud_world::MobBehaviors>(target) {
            names.extend(b.0.iter().map(|b| flag_name(b.label())));
        }
        if let Some(t) = world.get::<mud_world::MobTraits>(target) {
            names.extend(t.0.iter().map(|t| flag_name(t.label())));
        }
        if let Some(p) = &proto {
            names.extend(p.professions.iter().map(|p| flag_name(p.label())));
        }
        out.push_str(&format!("NPC flags: {}\r\n", paint(CYAN, flag_list(names))));
    }
    if is_player {
        let flags = world.get::<PlayerFlags>(target).cloned();
        let mut plr: Vec<String> = Vec::new();
        if world.get::<Frozen>(target).is_some() {
            plr.push("FROZEN".into());
        }
        if world.get::<Ghost>(target).is_some() {
            plr.push("GHOST".into());
        }
        if flags.as_ref().is_some_and(|f| f.has(PlayerFlag::Muted)) {
            plr.push("MUTED".into());
        }
        let prf: Vec<String> = flags
            .iter()
            .flat_map(|f| f.0.iter())
            .filter(|f| !matches!(f, PlayerFlag::Muted))
            .map(|f| f.label().to_string())
            .collect();
        let prv: Vec<String> = world
            .get::<Account>(target)
            .map(|a| {
                let mut v = vec![format!("ROLE={}", a.role.label().to_ascii_uppercase())];
                v.extend(a.perms.iter().map(|p| flag_name(p.label())));
                v
            })
            .unwrap_or_default();
        out.push_str(&format!("PLR: {}\r\n", paint(CYAN, flag_list(plr))));
        out.push_str(&format!("PRF: {}\r\n", paint(GREEN, flag_list(prf))));
        out.push_str(&format!("PRV: {}\r\n", paint(GREEN, flag_list(prv))));
    }

    // ---- Weight and objects ----------------------------------------------
    {
        let mut worn_n = 0;
        let mut worn_w = 0.0_f64;
        let mut held_n = 0;
        let mut q = world.query_filtered::<(Entity, &Located, Option<&EquippedSlot>), With<Item>>();
        let carried: Vec<(Entity, bool)> = q
            .iter(world)
            .filter(|(_, l, _)| l.0 == target)
            .map(|(e, _, slot)| (e, slot.is_some()))
            .collect();
        for (item, worn) in carried {
            if worn {
                worn_n += 1;
                worn_w += crate::commands::item_weight(world, item);
            } else {
                held_n += 1;
            }
        }
        let total = crate::commands::carried_weight(world, target);
        out.push_str(&format!(
            "Max Carry: {:.0} weight; Carried: {held_n} ({:.2} weight); Worn: {worn_n} ({worn_w:.0} weight)\r\n",
            crate::commands::carry_capacity(world, target),
            (total - worn_w).max(0.0),
        ));
    }

    // ---- Conditions --------------------------------------------------------
    out.push_str(&format!(
        "Hunger: {}, Thirst: {}, Drunk: {}\r\n",
        world.get::<Hunger>(target).map_or(0, |h| h.0),
        world.get::<Thirst>(target).map_or(0, |t| t.0),
        world.get::<Drunkenness>(target).map_or(0, |d| d.0),
    ));

    // ---- Followers / group / guard -------------------------------------
    let master = world
        .get::<Follower>(target)
        .map_or_else(|| "<none>".to_string(), |f| name_of(world, f.0));
    let followers = names_where(
        world,
        |w, e| w.get::<Follower>(e).is_some_and(|f| f.0 == target),
        |_, _| true,
    );
    let prefix = format!("Master is: {master}, Followers are:");
    out.push_str(&wrapped(&prefix, prefix.chars().count(), &followers));

    let leader_e = mud_world::group_root(world, target);
    let leader = if world.get::<GroupMember>(target).is_some() && leader_e != target {
        name_of(world, leader_e)
    } else {
        "<none>".to_string()
    };
    let groupees = if leader_e == target {
        names_where(
            world,
            |w, e| w.get::<GroupMember>(e).is_some_and(|g| g.0 == target),
            |_, _| true,
        )
    } else {
        mud_world::group_members(world, leader_e)
            .into_iter()
            .filter(|e| *e != target)
            .map(|e| name_of(world, e))
            .collect()
    };
    let group_prefix = format!(
        "{} is: {leader}, {} are:",
        paint("<b:green>", "Group Master"),
        paint("<b:green>", "groupees"),
    );
    out.push_str(&wrapped(
        &group_prefix,
        "Group Master is: , groupees are:".len() + leader.chars().count(),
        &groupees,
    ));

    let guarding = world.get::<Guarding>(target).map(|g| name_of(world, g.0));
    let guarded_by = names_where(
        world,
        |w, e| w.get::<Guarding>(e).is_some_and(|g| g.0 == target),
        |_, _| true,
    );
    if guarding.is_some() || !guarded_by.is_empty() {
        out.push_str(&format!(
            "Guarding: {}, Guarded by: {}\r\n",
            guarding.as_deref().unwrap_or("<none>"),
            if guarded_by.is_empty() {
                "<none>".to_string()
            } else {
                guarded_by.join(", ")
            },
        ));
    }

    // ---- Effects -------------------------------------------------------------
    let flags = effect_flags(world, target);
    out.push_str(&format!(
        "EFF: {}\r\n",
        paint(YELLOW, flag_list(flags.into_iter().map(str::to_string)))
    ));
    let effects: Vec<Entity> = mud_world::effects_on(world, target);
    for e in effects {
        let Some(inst) = world.get::<EffectInstance>(e).cloned() else {
            continue;
        };
        let label = crate::commands::info::effect_label(world, &inst);
        let when = if inst.remaining_secs < 0 {
            "(perma)".to_string()
        } else {
            format!(
                "({:3}hr)",
                crate::commands::info::effect_hours_left(inst.remaining_secs)
            )
        };
        let modifier = world
            .get::<ModifyDelta>(e)
            .filter(|d| d.amount != 0)
            .map(|d| format!("{:+} to {}", d.amount, d.target));
        let shown = match modifier {
            Some(_) => format!("{label:<21}"),
            None => label,
        };
        out.push_str(&format!("SPL: {when} {}", paint(CYAN, shown)));
        if let Some(m) = modifier {
            out.push_str(&format!(" {m}"));
        }
        out.push_str("\r\n");
    }

    // ---- Cooldowns -----------------------------------------------------------
    if let Some(cd) = world.get::<mud_world::Cooldowns>(target) {
        let now = std::time::Instant::now();
        let mut active: Vec<(i32, u64)> = cd
            .ready_at
            .iter()
            .filter(|(_, at)| **at > now)
            .map(|(id, at)| (*id, (*at - now).as_secs() + 1))
            .collect();
        active.sort_unstable();
        if !active.is_empty() {
            out.push_str("Cooldowns:\r\n");
            for (id, secs) in active {
                let label = crate::commands::info::ability_label_by_id(world, id.abs())
                    .unwrap_or_else(|| format!("ability {}", id.abs()));
                out.push_str(&format!("{label:<25}: {secs} sec\r\n"));
            }
        }
    }

    // ---- Scripts, reset origin, memory ------------------------------------
    if is_mob {
        out.push_str("Script information:\r\n");
        let attached = world
            .get::<AttachedTriggers>(target)
            .map(|a| a.0.clone())
            .unwrap_or_default();
        if attached.is_empty() {
            out.push_str("  None.\r\n");
        }
        for (z, i) in attached {
            let def = world
                .get_resource::<mud_world::TriggerCatalog>()
                .and_then(|c| c.by_key.get(&(z, i)));
            match def {
                Some(d) => {
                    let events: Vec<String> = d
                        .flags
                        .iter()
                        .map(|f| format!("{f:?}").to_ascii_uppercase())
                        .collect();
                    out.push_str(&format!(
                        "  Trigger '{}' ({z}, {i}), types: {}\r\n",
                        d.name,
                        events.join(" ")
                    ));
                }
                None => out.push_str(&format!("  Trigger ({z}, {i}) (not loaded)\r\n")),
            }
        }
        if let Some(reset) = world.get::<FromMobReset>(target).map(|r| r.0) {
            let row = world
                .get_resource::<mud_world::MobResetCatalog>()
                .and_then(|c| c.entries.iter().find(|e| e.reset_id == reset))
                .map(|e| (e.room_entity, e.max_instances));
            match row {
                Some((room, max)) => out.push_str(&format!(
                    "Reset: row {reset}, loads in room {} (max {max})\r\n",
                    room_label(world, room)
                )),
                None => out.push_str(&format!("Reset: row {reset}\r\n")),
            }
        } else {
            out.push_str("Reset: none (spawned outside a zone reset)\r\n");
        }
    }
    if let Some(mem) = world.get::<crate::combat::MobMemory>(target) {
        let mut enemies: Vec<Entity> = mem.0.iter().copied().collect();
        enemies.sort();
        let names: Vec<String> = enemies
            .into_iter()
            .filter(|e| world.get_entity(*e).is_ok())
            .map(|e| paint(GREEN, name_of(world, e)))
            .collect();
        if !names.is_empty() {
            out.push_str(&format!("Remembered enemies: {}\r\n", names.join(", ")));
        }
    }
    if is_mob && let Some(f) = proto.as_ref().and_then(|p| p.aggression_formula.as_ref()) {
        out.push_str(&format!("Aggression: {f}\r\n"));
    }

    // ---- Staff-facing extras this world adds -----------------------------
    let overflow = crate::commands::stat_overflow_entries(world, target);
    if !overflow.is_empty() {
        let parts: Vec<String> = overflow.iter().map(|(k, n)| format!("{k} {n:+}")).collect();
        out.push_str(&format!("stat_overflow: {}\r\n", parts.join(", ")));
    }
    if let Some(w) = world.get::<WizInvis>(target) {
        out.push_str(&format!("WizInvis: level {}\r\n", w.0));
    }
    if let Some(w) = world.get::<WimpyThreshold>(target)
        && w.0 > 0
    {
        out.push_str(&format!("Wimpy: {}%\r\n", w.0));
    }
    out
}
