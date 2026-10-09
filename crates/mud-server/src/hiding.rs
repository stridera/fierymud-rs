//! Hiding and sneaking (legacy `GET_HIDDENNESS` / `INVIS_OK`).
//!
//! A character's [`Hiddenness`] (0..=1000) is how hard it is to spot. A
//! viewer whose perception ([`perception_of`]) is below it does not see the
//! character at all: [`hidden_from`] feeds `can_see_player`, so room
//! listings, name resolution, GMCP and per-observer messages follow
//! without a second code path. Hiding is rolled by `hide`, worn down by
//! moving (slowly while [`Sneaking`]), found by `search`, and stripped
//! by [`reveal`] whenever the character does anything noisy.
//!
//! The roll constants live on the `HIDE` / `SNEAK` ability rows
//! (`AbilityEffect.override_params`, under the `"hide"` / `"sneak"` key);
//! the defaults here are the legacy values, used when a row has none.

use bevy_ecs::prelude::*;
use mud_world::{
    AbilityCatalog, ClassCatalog, CoreStats, Hiddenness, KnownAbilities, Located, MAX_HIDDENNESS,
    Perception, Player, Profile,
};

/// Current hiddenness, 0 when not hidden.
#[must_use]
pub(crate) fn hiddenness(world: &World, e: Entity) -> i32 {
    world.get::<Hiddenness>(e).map_or(0, |h| h.0)
}

/// Legacy `IS_HIDDEN`: hiddenness above zero.
#[must_use]
pub(crate) fn is_hidden(world: &World, e: Entity) -> bool {
    hiddenness(world, e) > 0
}

/// Set hiddenness, clamped to `0..=MAX_HIDDENNESS` (legacy
/// `APPLY_HIDDENNESS`). Zero removes the component.
pub(crate) fn set_hiddenness(world: &mut World, e: Entity, value: i32) {
    let Ok(mut em) = world.get_entity_mut(e) else {
        return;
    };
    let value = value.clamp(0, MAX_HIDDENNESS);
    if value == 0 {
        em.remove::<Hiddenness>();
    } else {
        em.insert(Hiddenness(value));
    }
}

/// Add `delta` (may be negative) to hiddenness, clamped.
pub(crate) fn add_hiddenness(world: &mut World, e: Entity, delta: i32) {
    let now = hiddenness(world, e);
    set_hiddenness(world, e, now.saturating_add(delta));
}

/// Drop out of hiding (legacy `GET_HIDDENNESS(ch) = 0`). Returns the
/// hiddenness the character had, so a caller that grants a backstab bonus
/// can read it first. Observers in the room are queued for a fresh prompt
/// so their GMCP `Room.Players` picks the newly visible character up.
pub(crate) fn reveal(world: &mut World, e: Entity) -> i32 {
    let was = hiddenness(world, e);
    if was == 0 {
        return 0;
    }
    set_hiddenness(world, e, 0);
    if let Some(room) = world.get::<Located>(e).map(|l| l.0) {
        let watchers: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Player>>();
            q.iter(world)
                .filter(|(w, l)| l.0 == room && *w != e)
                .map(|(w, _)| w)
                .collect()
        };
        for w in watchers {
            crate::commands::mark_for_prompt(w);
        }
    }
    was
}

/// Legacy `GET_PERCEPTION`. A player's base is `level * ((INT + WIS) / 30)`
/// (`/ 20` for a halfling, `affect_total`), plus the [`Perception`] total
/// from gear and spells. A mob has no base: its prototype `perception`
/// is its whole value. Clamped to 0..=1000.
#[must_use]
pub(crate) fn perception_of(world: &World, e: Entity) -> i32 {
    let bonus = world.get::<Perception>(e).map_or(0, |p| p.0);
    let base = if world.get::<Player>(e).is_some() {
        let level = world.get::<Profile>(e).map_or(0, |p| p.level);
        let stats = world.get::<CoreStats>(e).copied().unwrap_or_default();
        let divisor = if is_halfling(world, e) { 20 } else { 30 };
        level.saturating_mul((stats.intelligence + stats.wisdom) / divisor)
    } else {
        0
    };
    base.saturating_add(bonus).clamp(0, MAX_HIDDENNESS)
}

#[must_use]
pub(crate) fn is_halfling(world: &World, e: Entity) -> bool {
    world
        .get::<Profile>(e)
        .is_some_and(|p| p.race.eq_ignore_ascii_case("halfling"))
}

/// Legacy `IS_IN_GROUP`: `a` and `b` follow the same leader.
#[must_use]
pub(crate) fn same_group(world: &World, a: Entity, b: Entity) -> bool {
    a == b || crate::commands::group_root(world, a) == crate::commands::group_root(world, b)
}

/// True when `target` is hiding well enough that `viewer` does not see
/// it (legacy `GET_HIDDENNESS(obj) <= GET_PERCEPTION(sub)` failing).
/// Never for the character itself, a group mate or a `HOLY_LIGHT`
/// viewer (legacy `IMM_CAN_SEE`: staff rank alone is no bypass).
#[must_use]
pub(crate) fn hidden_from(world: &World, viewer: Entity, target: Entity) -> bool {
    let hid = hiddenness(world, target);
    if hid == 0 || viewer == target {
        return false;
    }
    if crate::commands::has_flag(world, viewer, mud_db::enums::PlayerFlag::HolyLight)
        || same_group(world, viewer, target)
    {
        return false;
    }
    hid > perception_of(world, viewer)
}

/// Legacy `stat_bonus[x].rogue_skills`: a skill bonus from DEX on the
/// 0..100 scale. Linear -99..-5 up to 48, zero through 64, then 5..25 at
/// 100. The legacy table truncates each value toward zero.
#[must_use]
pub(crate) fn rogue_skill_bonus(dex: i32) -> i32 {
    // Single precision, as the legacy table is built, so the edges
    // (x = 65 gives exactly 5) truncate the same way.
    let dex = dex.clamp(0, 100);
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let x = dex as f32;
    #[allow(clippy::cast_possible_truncation)]
    match dex {
        0..=48 => (47.0_f32 / 24.0 * x - 99.0) as i32,
        49..=64 => 0,
        _ => (4.0_f32 / 7.0 * x - 225.0 / 7.0) as i32,
    }
}

/// The 0..=100 proficiency `e` has in the ability called `name`
/// (`GET_SKILL`), 0 when unknown.
#[must_use]
pub(crate) fn skill_pct(world: &World, e: Entity, name: &str) -> i32 {
    let Some(id) = world
        .get_resource::<AbilityCatalog>()
        .and_then(|c| c.by_name.get(name))
        .map(|d| d.id)
    else {
        return 0;
    };
    world
        .get::<KnownAbilities>(e)
        .and_then(|k| k.entries.iter().find(|(a, _, _)| *a == id))
        .map_or(0, |(_, raw, _)| (raw / 10).clamp(0, 100))
}

/// Constants of the `hide` roll, from the `HIDE` ability row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HideParams {
    /// `lower = cubic*s^3 + quadratic*s^2 + linear*s` for skill `s`.
    pub lower_cubic: f64,
    pub lower_quadratic: f64,
    pub lower_linear: f64,
    /// `upper = s * (dex_weight*DEX + int_weight*INT) / divisor`.
    pub dex_weight: f64,
    pub int_weight: f64,
    pub divisor: f64,
    /// Lag after a hide, in ticks (10 per second); a thief gets the second.
    pub wait_ticks: u64,
    pub thief_wait_ticks: u64,
    /// A halfling hiding in a group multiplies its DEX bonus by
    /// `level / this + 1`.
    pub halfling_group_level_divisor: i32,
    /// The stealth skill must beat `random(0, this)` to set `Stealth`.
    pub stealth_roll_max: i32,
}

impl Default for HideParams {
    fn default() -> Self {
        Self {
            lower_cubic: -0.0008,
            lower_quadratic: 0.1668,
            lower_linear: -3.225,
            dex_weight: 3.0,
            int_weight: 1.0,
            divisor: 40.0,
            wait_ticks: 40,
            thief_wait_ticks: 20,
            halfling_group_level_divisor: 30,
            stealth_roll_max: 101,
        }
    }
}

/// Constants of the per-move hiddenness decay, from the `SNEAK` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SneakParams {
    /// A sneaking mover loses `random(decay_min, decay_max)` per move.
    pub decay_min: i32,
    pub decay_max: i32,
    /// Otherwise a mover loses `level / level_divisor` when
    /// `random(1, 101)` beats `sneak skill + DEX bonus + fail_base`.
    pub fail_base: i32,
    pub level_divisor: i32,
}

impl Default for SneakParams {
    fn default() -> Self {
        Self {
            decay_min: 2,
            decay_max: 5,
            fail_base: 15,
            level_divisor: 2,
        }
    }
}

/// `override_params[key]` object of the first effect row of ability `name`
/// that carries one.
fn ability_params(world: &World, name: &str, key: &str) -> Option<serde_json::Value> {
    let catalog = world.get_resource::<AbilityCatalog>()?;
    let id = catalog.by_name.get(name)?.id;
    catalog
        .effects_for
        .get(&id)?
        .iter()
        .filter_map(|(_, params)| params.as_ref())
        .find_map(|p| p.get(key).filter(|v| v.is_object()).cloned())
}

fn param_f64(obj: &serde_json::Value, key: &str, default: f64) -> f64 {
    obj.get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(default)
}

fn param_u64(obj: &serde_json::Value, key: &str, default: u64) -> u64 {
    obj.get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(default)
}

fn param_i32(obj: &serde_json::Value, key: &str, default: i32) -> i32 {
    obj.get(key)
        .and_then(serde_json::Value::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or(default)
}

/// `HideParams` from the `HIDE` ability row, legacy defaults for any
/// key it leaves out.
#[must_use]
pub(crate) fn hide_params(world: &World) -> HideParams {
    let mut out = HideParams::default();
    let Some(obj) = ability_params(world, "hide", "hide") else {
        return out;
    };
    out.lower_cubic = param_f64(&obj, "lowerCubic", out.lower_cubic);
    out.lower_quadratic = param_f64(&obj, "lowerQuadratic", out.lower_quadratic);
    out.lower_linear = param_f64(&obj, "lowerLinear", out.lower_linear);
    out.dex_weight = param_f64(&obj, "dexWeight", out.dex_weight);
    out.int_weight = param_f64(&obj, "intWeight", out.int_weight);
    out.divisor = param_f64(&obj, "divisor", out.divisor).max(1.0);
    out.wait_ticks = param_u64(&obj, "waitTicks", out.wait_ticks);
    out.thief_wait_ticks = param_u64(&obj, "thiefWaitTicks", out.thief_wait_ticks);
    out.halfling_group_level_divisor = param_i32(
        &obj,
        "halflingGroupLevelDivisor",
        out.halfling_group_level_divisor,
    )
    .max(1);
    out.stealth_roll_max = param_i32(&obj, "stealthRollMax", out.stealth_roll_max);
    out
}

/// `SneakParams` from the `SNEAK` ability row, legacy defaults otherwise.
#[must_use]
pub(crate) fn sneak_params(world: &World) -> SneakParams {
    let mut out = SneakParams::default();
    let Some(obj) = ability_params(world, "sneak", "sneak") else {
        return out;
    };
    out.decay_min = param_i32(&obj, "decayMin", out.decay_min);
    out.decay_max = param_i32(&obj, "decayMax", out.decay_max).max(out.decay_min);
    out.fail_base = param_i32(&obj, "failBase", out.fail_base);
    out.level_divisor = param_i32(&obj, "levelDivisor", out.level_divisor).max(1);
    out
}

/// The `hide` roll: `random(lower, upper) + DEX bonus`, floored at 0.
/// `bonus` is the already-scaled DEX bonus. `roll(lo, hi)` draws an
/// integer in `lo..=hi`.
#[must_use]
pub(crate) fn roll_hiddenness(
    p: &HideParams,
    skill: i32,
    dex: i32,
    int: i32,
    bonus: i32,
    roll: &mut dyn FnMut(i32, i32) -> i32,
) -> i32 {
    let s = f64::from(skill);
    #[allow(clippy::cast_possible_truncation)]
    let (lo, hi) = (
        (p.lower_cubic * s.powi(3) + p.lower_quadratic * s.powi(2) + p.lower_linear * s) as i32,
        (s * (p.dex_weight * f64::from(dex) + p.int_weight * f64::from(int)) / p.divisor) as i32,
    );
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    (roll(lo, hi) + bonus).max(0)
}

/// Wear hiddenness down for one step (legacy `do_simple_move`): a sneaker
/// loses `random(2, 5)`; anyone else hiding loses `level / 2` when
/// `random(1, 101)` beats `sneak skill + DEX bonus + 15`. Staying hidden
/// as you walk takes skill. Returns the hiddenness left.
pub(crate) fn decay_on_move(
    world: &mut World,
    mover: Entity,
    roll: &mut dyn FnMut(i32, i32) -> i32,
) -> i32 {
    let hid = hiddenness(world, mover);
    let sneaking = world.get::<mud_world::Sneaking>(mover).is_some();
    if hid == 0 {
        return 0;
    }
    let p = sneak_params(world);
    if sneaking {
        // Legacy only wears a player's hiddenness down this way.
        if world.get::<Player>(mover).is_some() {
            add_hiddenness(world, mover, -roll(p.decay_min, p.decay_max));
        }
    } else {
        let dex = world.get::<CoreStats>(mover).map_or(0, |s| s.dexterity);
        let keep = skill_pct(world, mover, "sneak") + rogue_skill_bonus(dex) + p.fail_base;
        if roll(1, 101) > keep {
            let level = mud_world::effective_level(world, mover);
            add_hiddenness(world, mover, -(level / p.level_divisor));
        }
    }
    hiddenness(world, mover)
}

/// Lowercase name of the class `e` belongs to (not its parents).
#[must_use]
pub(crate) fn class_plain_name(world: &World, e: Entity) -> Option<String> {
    let id = world.get::<Profile>(e)?.class_id?;
    world
        .get_resource::<ClassCatalog>()?
        .by_id
        .get(&id)
        .map(|c| c.plain_name.to_ascii_lowercase())
}

/// Commands that do not give a hiding character away: the legacy
/// `CMD_HIDE` entries of `cmd_info` (movement, `look`, `examine`,
/// `hide`, `visible`, `backstab`, `steal`, ...) plus every `CMD_ANY`
/// entry (`score`, `inventory`, channels such as `tell`, the info and
/// staff commands), under their Rust names. `sneak` and `conceal`
/// belong with `hide`. Everything else reveals the caller before it
/// runs ([`reveals_on_command`]).
const HIDE_SAFE: &[&str] = &[
    // movement
    "north",
    "south",
    "east",
    "west",
    "up",
    "down",
    "northeast",
    "northwest",
    "southeast",
    "southwest",
    "in",
    "out",
    "walk",
    "fly",
    "go",
    "stay",
    // hiding and the rogue kit
    "hide",
    "visible",
    "sneak",
    "conceal",
    "backstab",
    "cartwheel",
    "lure",
    "palm",
    "pick",
    "steal",
    "stow",
    "scan",
    // looking and reading
    "look",
    "examine",
    "glance",
    "diagnose",
    "exits",
    "identify",
    "value",
    "score",
    "inventory",
    "equipment",
    "experience",
    "level",
    "skills",
    "spells",
    "songs",
    "innate",
    "socials",
    "help",
    "wizhelp",
    "commands",
    "credits",
    "motd",
    "news",
    "policies",
    "time",
    "date",
    "uptime",
    "weather",
    "version",
    "trophy",
    "world",
    "who",
    "whoami",
    "where",
    "users",
    "game",
    "title",
    "display",
    "prompt",
    "toggle",
    "color",
    "alias",
    "unalias",
    "consent",
    "subclass",
    "group",
    "quit",
    "save",
    "bug",
    "idea",
    "typo",
    "petition",
    "abort",
    "clear",
    "cls",
    "ignore",
    "unignore",
    "effects",
    "cooldowns",
    "achievements",
    "flags",
    "wealth",
    "practice",
    // channels and mail (legacy `CMD_ANY` plus `gossip`)
    "tell",
    "reply",
    "lasttells",
    "gsay",
    "ctell",
    "qsay",
    "qecho",
    "music",
    "gossip",
    "wiznet",
    "mail",
    "mailbox",
    "readmail",
    "note",
    "pnote",
    "lastgossips",
    "lastshouts",
];

/// True when running `command` (typed as `typed`) leaves a hiding
/// character hidden. Staff commands (above Player) never reveal: a
/// hiding god stays hidden while working. `shadow` is the legacy
/// hide-safe spelling of `follow`.
#[must_use]
pub(crate) fn is_hide_safe(command: &str, typed: &str, staff_only: bool) -> bool {
    staff_only || HIDE_SAFE.contains(&command) || typed == "shadow"
}

/// Socials that keep a hiding character hidden (legacy `eyebrow`,
/// `frown`); every other social reveals.
#[must_use]
pub(crate) fn social_reveals(word: &str) -> bool {
    !matches!(word, "eyebrow" | "frown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rogue_skill_bonus_follows_the_legacy_table() {
        assert_eq!(rogue_skill_bonus(0), -99);
        assert_eq!(rogue_skill_bonus(48), -5);
        assert_eq!(rogue_skill_bonus(49), 0);
        assert_eq!(rogue_skill_bonus(64), 0);
        assert_eq!(rogue_skill_bonus(65), 5);
        assert_eq!(rogue_skill_bonus(100), 25);
        assert_eq!(rogue_skill_bonus(80), 13);
    }

    #[test]
    fn hide_roll_spans_the_legacy_bounds() {
        let p = HideParams::default();
        // skill 100, DEX 80, INT 60: lower 545, upper 750.
        let mut seen = Vec::new();
        let v = roll_hiddenness(&p, 100, 80, 60, 13, &mut |lo, hi| {
            seen.push((lo, hi));
            lo
        });
        assert_eq!(seen, vec![(545, 750)]);
        assert_eq!(v, 558);
        // No skill rolls 0..0; a negative result floors at 0.
        assert_eq!(roll_hiddenness(&p, 0, 80, 60, -5, &mut |lo, _| lo), 0);
    }

    #[test]
    fn hide_safe_list_is_the_legacy_cmd_hide_set() {
        assert!(is_hide_safe("look", "l", false));
        assert!(is_hide_safe("score", "sc", false));
        assert!(is_hide_safe("hide", "hide", false));
        assert!(is_hide_safe("sneak", "sneak", false));
        assert!(is_hide_safe("backstab", "bs", false));
        assert!(is_hide_safe("follow", "shadow", false));
        assert!(!is_hide_safe("follow", "follow", false));
        assert!(!is_hide_safe("say", "say", false));
        assert!(!is_hide_safe("get", "get", false));
        assert!(!is_hide_safe("search", "search", false));
        assert!(is_hide_safe("restore", "restore", true));
        assert!(!social_reveals("eyebrow") && social_reveals("smile"));
    }
}
