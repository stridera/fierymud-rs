//! Data model for mob spellcasting / skill AI (WP-A of `ops/plans/mob-ai.md`).
//!
//! Rules come from two tables: `ClassAiRules` (shared by every mob of a class)
//! and `MobAbilities` (one mob prototype's own list, used when its
//! `Mobs.ai_mode` is `CUSTOM`). They are parsed once at boot into [`AiRule`]s
//! and held in the [`MobAiData`] resource as shared `Arc<[AiRule]>` tables, so
//! spawning a mob is a pointer clone. The whole feature sits behind the
//! `mob_ai.enabled` `GameConfig` flag (default off): when it is off nothing here
//! is attached to a mob and the combat tick never sees a [`MobAi`].
//!
//! `conditions` JSON is an object whose keys must all hold. Keys:
//!
//! | key                | value                  | [`AiCondition`]               |
//! |--------------------|------------------------|-------------------------------|
//! | `in_combat`        | bool                   | `InCombat` / `OutOfCombat`    |
//! | `out_of_combat`    | bool                   | `OutOfCombat` / `InCombat`    |
//! | `self_hp_below`    | percent 0..=100        | `SelfHpBelow`                 |
//! | `target_hp_below`  | percent 0..=100        | `TargetHpBelow`               |
//! | `ally_hp_below`    | percent 0..=100        | `AllyHpBelow`                 |
//! | `grouped_targets`  | count, or `true` (= 2) | `GroupedTargets`              |
//! | `target_casting`   | `true`                 | `TargetCasting`               |
//! | `target_has_buff`  | `true`                 | `TargetHasBuff`               |
//! | `target_hp_above`  | percent 0..=100        | `TargetHpAbove`               |
//! | `self_has_debuff`  | `true`, a name, or names | `SelfHasDebuff` / `SelfHasDebuffNamed` |
//! | `target_lifeforce` | `LifeForce` name       | `TargetLifeforce`             |
//! | `not_lifeforce`    | `LifeForce` name       | `NotLifeforce`                |
//! | `align`            | good / neutral / evil  | `Align` (the TARGET's band)   |
//! | `not_align`        | good / neutral / evil  | `NotAlign` (target is not in the band) |
//! | `self_align`       | band, or a list of bands | `SelfAlign` (the caster's own band) |
//! | `target_level_max` | integer offset         | `TargetLevelMax`              |
//! | `level_scaled_chance` | `true` or a factor  | `LevelScaledChance`           |
//! | `level_roll`       | `{"min": n, "max": m}` | `LevelRoll`                   |
//! | `composition`      | `Composition` name(s)  | `Composition`                 |
//! | `not_composition`  | `Composition` name(s)  | `NotComposition`              |
//! | `requires_shield`  | `true`                 | `RequiresShield`              |
//! | `requires_weapon_type` | damage type, or `piercing` / `slashing` / `bludgeoning` | `RequiresWeaponType` |
//! | `outdoors`         | `true`                 | `Outdoors`                    |
//! | `telegraph`        | `true`                 | `Telegraph`                   |
//!
//! Target-side conditions (`target_*`, `align`, `not_align`, `*_lifeforce`)
//! describe the rule's chosen target; for a rule that targets the mob itself
//! they describe the mob's current opponent instead (legacy `victim`), so
//! "buff while the victim is still healthy" reads naturally.
//!
//! `target_level_max: n` holds when the target's level is at most the mob's
//! level plus `n` (legacy "victim level < mob level + 20" is `19`).
//!
//! `level_scaled_chance` is legacy `random_number(0, 101) < GET_LEVEL(ch)`:
//! the rule passes with probability `level%` times the factor (`true` = 1),
//! times `mob_ai.scale`. The roll is made once per mob per round and shared
//! by every rule that asks for it, as legacy rolled once per class AI call.
//!
//! `level_roll` is legacy `dragonlike_attack`'s `roll = random(0, 150 - level)`
//! with the rule passing when `min <= roll < max`. One roll per mob per round
//! is shared by all rules, so the breath / sweep / roar bands exclude each
//! other exactly as the legacy `else if` chain does.
//!
//! `telegraph` is a flag, not a test: the engine warns the room one combat
//! round before the rule's cast and fires it the round after (breath, area
//! attacks). Off unless the rule sets it.
//!
//! A `false` value on the `true`-only keys means "no constraint". An unknown
//! key is ignored (and logged once per load); a known key with an unusable
//! value drops the whole rule, because ignoring it would let the rule fire in
//! situations its author excluded.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::*;
use mud_db::enums::{LifeForce, MobAiMode, MobBehavior};
use smallvec::SmallVec;

use crate::resources::MobProto;

/// What an [`AiRule`] invokes. Mirrors `Ability.ability_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiKind {
    Spell,
    Skill,
    Song,
    Chant,
}

impl AiKind {
    /// Parse `Ability.ability_type` (case-insensitive).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "SPELL" => Some(Self::Spell),
            "SKILL" => Some(Self::Skill),
            "SONG" => Some(Self::Song),
            "CHANT" => Some(Self::Chant),
            _ => None,
        }
    }
}

/// Who a rule aims at. `Auto` leaves the choice to the engine (the ability's
/// own targeting); it is what a `MobAbilities` row with a null `target` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTarget {
    Auto,
    SelfTarget,
    Tank,
    WeakestAttacker,
    Caster,
    MostBuffed,
    LowestAlly,
    Room,
}

impl AiTarget {
    /// Parse a `target` column value (case-insensitive, `_` or `-` or space).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let norm: String = s
            .trim()
            .chars()
            .map(|c| {
                if c == '-' || c == ' ' {
                    '_'
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        match norm.as_str() {
            "AUTO" => Some(Self::Auto),
            "SELF" => Some(Self::SelfTarget),
            "TANK" => Some(Self::Tank),
            "WEAKEST_ATTACKER" => Some(Self::WeakestAttacker),
            "CASTER" => Some(Self::Caster),
            "MOST_BUFFED" => Some(Self::MostBuffed),
            "LOWEST_ALLY" => Some(Self::LowestAlly),
            "ROOM" => Some(Self::Room),
            _ => None,
        }
    }
}

/// Alignment band for [`AiCondition::Align`], on the usual +-350 thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAlign {
    Good,
    Neutral,
    Evil,
}

impl AiAlign {
    /// The band an alignment scalar (-1000..=1000) falls in.
    #[must_use]
    pub const fn of(alignment: i32) -> Self {
        if alignment >= 350 {
            Self::Good
        } else if alignment <= -350 {
            Self::Evil
        } else {
            Self::Neutral
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Good => 1,
            Self::Neutral => 2,
            Self::Evil => 4,
        }
    }
}

/// A set of alignment bands (`self_align: ["good", "neutral"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignMask(u8);

impl AlignMask {
    /// The set holding exactly `bands`.
    #[must_use]
    pub fn of(bands: &[AiAlign]) -> Self {
        Self(bands.iter().fold(0, |m, b| m | b.bit()))
    }

    #[must_use]
    pub const fn contains(self, band: AiAlign) -> bool {
        self.0 & band.bit() != 0
    }
}

/// One `when` clause. All clauses of a rule must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiCondition {
    InCombat,
    OutOfCombat,
    /// Own HP below this percent.
    SelfHpBelow(u8),
    /// Chosen target's HP below this percent.
    TargetHpBelow(u8),
    /// Chosen target's HP above this percent.
    TargetHpAbove(u8),
    /// At least this many enemies are engaged with the mob.
    GroupedTargets(u8),
    TargetCasting,
    TargetHasBuff,
    SelfHasDebuff,
    /// Some ally in the room is below this HP percent.
    AllyHpBelow(u8),
    /// The mob carries a debuff matching one of these names (an effect name,
    /// or the plain name of the ability that applied it; `blind` answers to
    /// every blindness flag). Same matching as the cleansing spells use.
    SelfHasDebuffNamed(Vec<String>),
    TargetLifeforce(LifeForce),
    NotLifeforce(LifeForce),
    /// The chosen target's alignment band.
    Align(AiAlign),
    /// The chosen target is NOT in this band.
    NotAlign(AiAlign),
    /// The caster's own alignment band is one of these.
    SelfAlign(AlignMask),
    /// Target level <= mob level + n.
    TargetLevelMax(i32),
    /// Pass with probability `level% * factor / 1000` (factor in thousandths).
    LevelScaledChance(u32),
    /// Shared per-round roll `random(0, 150 - level)` is in `min..max`.
    LevelRoll {
        min: i32,
        max: i32,
    },
    /// The mob's composition is one of these.
    Composition(Vec<mud_db::enums::Composition>),
    /// The mob's composition is none of these.
    NotComposition(Vec<mud_db::enums::Composition>),
    /// The mob wears a shield (an armor piece held in the off hand).
    RequiresShield,
    /// The mob wields a weapon of this damage type (lowercase; `piercing`,
    /// `slashing` and `bludgeoning` stand for the legacy families).
    RequiresWeaponType(String),
    /// The mob's room is not indoors.
    Outdoors,
    /// The rule warns the room one round before it fires (not a test the
    /// engine evaluates; it changes how the rule is carried out).
    Telegraph,
}

/// One mob AI rule, parsed and validated. Lists are kept in priority order
/// (lower `priority` first); the engine walks them top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiRule {
    pub ability_id: i32,
    pub kind: AiKind,
    pub priority: i32,
    /// Roll to pass per evaluation, 0..=100.
    pub chance_pct: u8,
    /// Minimum seconds between uses of this rule by one mob.
    pub cooldown_s: u32,
    pub when: Vec<AiCondition>,
    pub target: AiTarget,
    /// Mob level gate (`ClassAiRules.min_level`); `None` = any level.
    pub min_level: Option<i32>,
}

/// A mob's live AI state: its (shared) rule list and when it last used each
/// rule. Attached at spawn only when `mob_ai.enabled` is on and the mob has
/// rules; absent means the mob has no spell / skill AI.
#[derive(Component, Debug, Clone)]
pub struct MobAi {
    pub rules: Arc<[AiRule]>,
    /// `(rule index in `rules`, tick of last use)`. Only rules that have fired
    /// appear, so a mob that never casts carries no heap data.
    pub last_use: SmallVec<[(u16, u64); 4]>,
    /// `(rule index, tick)` of a `Telegraph` warning already given. It holds
    /// only for the very next combat round; an older entry is stale.
    pub warned: SmallVec<[(u16, u64); 2]>,
    /// `(rule index, tick)` of the last attempt that did not take effect
    /// (refused, resisted, no target). The rule rests for a while instead of
    /// being retried every round.
    pub failed: SmallVec<[(u16, u64); 2]>,
}

impl MobAi {
    #[must_use]
    pub fn new(rules: Arc<[AiRule]>) -> Self {
        Self {
            rules,
            last_use: SmallVec::new(),
            warned: SmallVec::new(),
            failed: SmallVec::new(),
        }
    }

    /// Tick at which rule `idx` last failed to take effect.
    #[must_use]
    pub fn failed_at(&self, idx: usize) -> Option<u64> {
        let idx = u16::try_from(idx).ok()?;
        self.failed.iter().find(|(i, _)| *i == idx).map(|(_, t)| *t)
    }

    /// Record that rule `idx` was tried at `tick` and did not take effect.
    pub fn mark_failed(&mut self, idx: usize, tick: u64) {
        let Ok(idx) = u16::try_from(idx) else {
            return;
        };
        match self.failed.iter_mut().find(|(i, _)| *i == idx) {
            Some(slot) => slot.1 = tick,
            None => self.failed.push((idx, tick)),
        }
    }

    /// Tick at which rule `idx` last gave its `Telegraph` warning.
    #[must_use]
    pub fn warned_at(&self, idx: usize) -> Option<u64> {
        let idx = u16::try_from(idx).ok()?;
        self.warned.iter().find(|(i, _)| *i == idx).map(|(_, t)| *t)
    }

    /// Record a `Telegraph` warning for rule `idx` at `tick`.
    pub fn mark_warned(&mut self, idx: usize, tick: u64) {
        let Ok(idx) = u16::try_from(idx) else {
            return;
        };
        match self.warned.iter_mut().find(|(i, _)| *i == idx) {
            Some(slot) => slot.1 = tick,
            None => self.warned.push((idx, tick)),
        }
    }

    /// Forget rule `idx`'s warning (the cast it announced happened or fizzled).
    pub fn clear_warned(&mut self, idx: usize) {
        if let Ok(idx) = u16::try_from(idx) {
            self.warned.retain(|(i, _)| *i != idx);
        }
    }

    /// Tick at which rule `idx` last fired, if it ever did.
    #[must_use]
    pub fn last_used(&self, idx: usize) -> Option<u64> {
        let idx = u16::try_from(idx).ok()?;
        self.last_use
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, t)| *t)
    }

    /// Record that rule `idx` fired at `tick`.
    pub fn mark_used(&mut self, idx: usize, tick: u64) {
        let Ok(idx) = u16::try_from(idx) else {
            return;
        };
        match self.last_use.iter_mut().find(|(i, _)| *i == idx) {
            Some(slot) => slot.1 = tick,
            None => self.last_use.push((idx, tick)),
        }
    }
}

/// Race + class merges, by `(class id, lowercase race)`.
type MergedRules = HashMap<(Option<i32>, String), Arc<[AiRule]>>;

/// Every parsed rule table. Built at boot by `loader::load_mob_ai_data`; empty
/// (and harmless) when the AI tables are missing or the feature is unused.
#[derive(Resource, Debug, Default)]
pub struct MobAiData {
    /// `ClassAiRules` per `Class.id`, priority order.
    pub by_class: HashMap<i32, Arc<[AiRule]>>,
    /// `RaceAiRules` per race (lowercase enum text), priority order.
    pub by_race: HashMap<String, Arc<[AiRule]>>,
    /// `MobAbilities` per mob prototype `(zone, id)`, priority order.
    pub by_mob: HashMap<(i32, i32), Arc<[AiRule]>>,
    /// `Mobs.ai_mode` for prototypes that are not `CLASS` (the default).
    pub modes: HashMap<(i32, i32), MobAiMode>,
    /// Race + class merges already built, so a thousand goblins share one
    /// list instead of allocating a thousand.
    merged: Mutex<MergedRules>,
}

impl MobAiData {
    /// Build the tables from the parsed rule lists.
    #[must_use]
    pub fn new(
        by_class: HashMap<i32, Arc<[AiRule]>>,
        by_race: HashMap<String, Arc<[AiRule]>>,
        by_mob: HashMap<(i32, i32), Arc<[AiRule]>>,
        modes: HashMap<(i32, i32), MobAiMode>,
    ) -> Self {
        Self {
            by_class,
            by_race,
            by_mob,
            modes,
            merged: Mutex::default(),
        }
    }

    /// `Mobs.ai_mode` of the prototype at `key`.
    #[must_use]
    pub fn mode(&self, key: (i32, i32)) -> MobAiMode {
        self.modes.get(&key).copied().unwrap_or_default()
    }

    /// Whether `proto` runs spell / skill AI at all: not `ai_mode = NONE`, and
    /// not flagged `NoClassAi` unless it brings its own `CUSTOM` rules (that
    /// flag switches off the *class-driven* AI only).
    #[must_use]
    pub fn ai_active(&self, proto: &MobProto) -> bool {
        match self.mode((proto.zone_id, proto.id)) {
            MobAiMode::None => false,
            MobAiMode::Custom => true,
            MobAiMode::Class => !proto.behaviors.contains(&MobBehavior::NoClassAi),
        }
    }

    /// The rule list a fresh instance of `proto` should carry, `None` when it
    /// has no spell / skill AI (mode `NONE`, `NoClassAi`, or no rules).
    ///
    /// A `CLASS` mob runs its race's rules (breath, sweep, roar) first and its
    /// class rules after them: a race rule is a rare roll that rarely fires,
    /// so the class kit behind it is reached on almost every round. A `CUSTOM`
    /// mob runs exactly its own `MobAbilities` rows.
    #[must_use]
    pub fn rules_for(&self, proto: &MobProto) -> Option<Arc<[AiRule]>> {
        let key = (proto.zone_id, proto.id);
        match self.mode(key) {
            MobAiMode::None => None,
            MobAiMode::Custom => self
                .by_mob
                .get(&key)
                .filter(|r| !r.is_empty())
                .map(Arc::clone),
            MobAiMode::Class => {
                if proto.behaviors.contains(&MobBehavior::NoClassAi) {
                    return None;
                }
                let class = proto.class_id.and_then(|c| self.by_class.get(&c));
                let race_key = proto.race.to_ascii_lowercase();
                let race = self.by_race.get(&race_key);
                match (race, class) {
                    (None, None) => None,
                    (None, Some(c)) => (!c.is_empty()).then(|| Arc::clone(c)),
                    (Some(r), None) => (!r.is_empty()).then(|| Arc::clone(r)),
                    (Some(r), Some(c)) => {
                        let mut cache = self
                            .merged
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let merged = cache
                            .entry((proto.class_id, race_key))
                            .or_insert_with(|| r.iter().chain(c.iter()).cloned().collect());
                        (!merged.is_empty()).then(|| Arc::clone(merged))
                    }
                }
            }
        }
    }
}

/// Result of parsing one `conditions` value.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedConditions {
    pub when: Vec<AiCondition>,
    /// Keys this build does not know (ignored).
    pub unknown_keys: Vec<String>,
    /// Known keys with an unusable value, as `key: reason`. Any entry means
    /// the rule must not be used.
    pub bad_values: Vec<String>,
}

fn pct(v: &serde_json::Value) -> Option<u8> {
    let n = v.as_i64()?;
    u8::try_from(n.clamp(0, 100)).ok()
}

/// Parse a `conditions` JSON value (see the module docs for the grammar).
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per key of the grammar
pub fn parse_conditions(value: Option<&serde_json::Value>) -> ParsedConditions {
    let mut out = ParsedConditions::default();
    let Some(value) = value else {
        return out;
    };
    let map = match value {
        serde_json::Value::Null => return out,
        serde_json::Value::Object(m) => m,
        other => {
            out.bad_values
                .push(format!("conditions: expected an object, got {other}"));
            return out;
        }
    };
    for (key, v) in map {
        let mut bad = |why: &str| out.bad_values.push(format!("{key}: {why} ({v})"));
        let flag = v.as_bool();
        match key.as_str() {
            "in_combat" | "out_of_combat" => match flag {
                Some(b) => {
                    let combat = b == (key == "in_combat");
                    out.when.push(if combat {
                        AiCondition::InCombat
                    } else {
                        AiCondition::OutOfCombat
                    });
                }
                None => bad("expected true or false"),
            },
            "self_hp_below" | "target_hp_below" | "target_hp_above" | "ally_hp_below" => {
                match pct(v) {
                    Some(p) => out.when.push(match key.as_str() {
                        "self_hp_below" => AiCondition::SelfHpBelow(p),
                        "target_hp_below" => AiCondition::TargetHpBelow(p),
                        "target_hp_above" => AiCondition::TargetHpAbove(p),
                        _ => AiCondition::AllyHpBelow(p),
                    }),
                    None => bad("expected a percent"),
                }
            }
            "grouped_targets" => match (flag, v.as_i64()) {
                (Some(true), _) => out.when.push(AiCondition::GroupedTargets(2)),
                (Some(false), _) => {}
                (None, Some(n)) if n >= 1 => out.when.push(AiCondition::GroupedTargets(
                    u8::try_from(n.min(255)).unwrap_or(255),
                )),
                _ => bad("expected a count of at least 1, or true"),
            },
            "target_casting" | "target_has_buff" | "telegraph" | "requires_shield" | "outdoors" => {
                match flag {
                    Some(true) => out.when.push(match key.as_str() {
                        "target_casting" => AiCondition::TargetCasting,
                        "target_has_buff" => AiCondition::TargetHasBuff,
                        "telegraph" => AiCondition::Telegraph,
                        "requires_shield" => AiCondition::RequiresShield,
                        _ => AiCondition::Outdoors,
                    }),
                    Some(false) => {}
                    None => bad("expected true or false"),
                }
            }
            "self_has_debuff" => match (flag, string_list(v)) {
                (Some(true), _) => out.when.push(AiCondition::SelfHasDebuff),
                (Some(false), _) => {}
                (None, Some(names)) => out.when.push(AiCondition::SelfHasDebuffNamed(
                    names.into_iter().map(|n| n.to_ascii_lowercase()).collect(),
                )),
                (None, None) => bad("expected true, a debuff name, or a list of names"),
            },
            "target_lifeforce" => match v.as_str().and_then(parse_lifeforce) {
                Some(lf) => out.when.push(AiCondition::TargetLifeforce(lf)),
                None => bad("expected a LifeForce name"),
            },
            "not_lifeforce" => {
                let forces: Option<Vec<LifeForce>> = string_list(v)
                    .filter(|l| !l.is_empty())
                    .and_then(|l| l.iter().map(|s| parse_lifeforce(s)).collect());
                match forces {
                    Some(list) => out
                        .when
                        .extend(list.into_iter().map(AiCondition::NotLifeforce)),
                    None => bad("expected a LifeForce name or a list of them"),
                }
            }
            "align" | "not_align" => match v.as_str().and_then(parse_align) {
                Some(which) => out.when.push(if key == "align" {
                    AiCondition::Align(which)
                } else {
                    AiCondition::NotAlign(which)
                }),
                None => bad("expected good, neutral or evil"),
            },
            "self_align" => {
                let bands: Option<Vec<AiAlign>> = string_list(v)
                    .filter(|l| !l.is_empty())
                    .and_then(|l| l.iter().map(|s| parse_align(s)).collect());
                match bands {
                    Some(b) => out.when.push(AiCondition::SelfAlign(AlignMask::of(&b))),
                    None => bad("expected good, neutral, evil, or a list of them"),
                }
            }
            "target_level_max" => match v.as_i64().and_then(|n| i32::try_from(n).ok()) {
                Some(n) => out.when.push(AiCondition::TargetLevelMax(n)),
                None => bad("expected an integer"),
            },
            "level_scaled_chance" => {
                let factor = match (flag, v.as_f64()) {
                    (Some(true), _) => Some(1.0),
                    (Some(false), _) => None,
                    (None, Some(f)) if f > 0.0 && f <= 100.0 => Some(f),
                    _ => {
                        bad("expected true or a positive factor");
                        None
                    }
                };
                if let Some(f) = factor {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    out.when
                        .push(AiCondition::LevelScaledChance((f * 1000.0).round() as u32));
                }
            }
            "level_roll" => {
                let min = v.get("min").and_then(serde_json::Value::as_i64);
                let max = v.get("max").and_then(serde_json::Value::as_i64);
                match (min, max) {
                    (Some(lo), Some(hi)) if (0..=hi).contains(&lo) && hi <= 1000 => {
                        out.when.push(AiCondition::LevelRoll {
                            min: i32::try_from(lo).unwrap_or(0),
                            max: i32::try_from(hi).unwrap_or(0),
                        });
                    }
                    _ => bad("expected {\"min\": n, \"max\": m} with 0 <= min <= max"),
                }
            }
            "composition" | "not_composition" => {
                let comps: Option<Vec<mud_db::enums::Composition>> = string_list(v)
                    .filter(|l| !l.is_empty())
                    .and_then(|l| l.iter().map(|s| parse_composition(s)).collect());
                match comps {
                    Some(c) => out.when.push(if key == "composition" {
                        AiCondition::Composition(c)
                    } else {
                        AiCondition::NotComposition(c)
                    }),
                    None => bad("expected a Composition name or a list of them"),
                }
            }
            "requires_weapon_type" => match v.as_str().map(|s| s.trim().to_ascii_lowercase()) {
                Some(t) if !t.is_empty() => out.when.push(AiCondition::RequiresWeaponType(t)),
                _ => bad("expected a weapon damage type"),
            },
            _ => out.unknown_keys.push(key.clone()),
        }
    }
    out
}

/// A JSON string, or an array of strings, as a list.
fn string_list(v: &serde_json::Value) -> Option<Vec<String>> {
    match v {
        serde_json::Value::String(s) => Some(vec![s.clone()]),
        serde_json::Value::Array(a) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>(),
        _ => None,
    }
}

fn parse_align(s: &str) -> Option<AiAlign> {
    match s.trim().to_ascii_lowercase().as_str() {
        "good" => Some(AiAlign::Good),
        "neutral" => Some(AiAlign::Neutral),
        "evil" => Some(AiAlign::Evil),
        _ => None,
    }
}

fn parse_composition(s: &str) -> Option<mud_db::enums::Composition> {
    use mud_db::enums::Composition as C;
    match s.trim().to_ascii_uppercase().as_str() {
        "FLESH" => Some(C::Flesh),
        "EARTH" => Some(C::Earth),
        "AIR" => Some(C::Air),
        "FIRE" => Some(C::Fire),
        "WATER" => Some(C::Water),
        "ICE" => Some(C::Ice),
        "MIST" => Some(C::Mist),
        "ETHER" => Some(C::Ether),
        "METAL" => Some(C::Metal),
        "STONE" => Some(C::Stone),
        "BONE" => Some(C::Bone),
        "LAVA" => Some(C::Lava),
        "PLANT" => Some(C::Plant),
        _ => None,
    }
}

fn parse_lifeforce(s: &str) -> Option<LifeForce> {
    match s.trim().to_ascii_uppercase().as_str() {
        "LIFE" => Some(LifeForce::Life),
        "UNDEAD" => Some(LifeForce::Undead),
        "MAGIC" => Some(LifeForce::Magic),
        "CELESTIAL" => Some(LifeForce::Celestial),
        "DEMONIC" => Some(LifeForce::Demonic),
        "ELEMENTAL" => Some(LifeForce::Elemental),
        _ => None,
    }
}

/// Raw inputs of one rule, before validation. Both tables map onto this.
pub struct RawRule<'a> {
    pub ability_id: i32,
    pub ability_type: &'a str,
    pub priority: i32,
    pub chance_pct: i32,
    pub cooldown_s: i32,
    pub conditions: Option<&'a serde_json::Value>,
    pub target: Option<&'a str>,
    pub min_level: Option<i32>,
}

/// Problems met while building rules, kept so the loader can log each distinct
/// one once instead of once per row.
#[derive(Debug, Default)]
pub struct RuleDiagnostics {
    /// Unknown `conditions` keys (each reported once).
    pub unknown_keys: BTreeSet<String>,
    /// Rules dropped, as `ability <id>: <reason>`.
    pub dropped: Vec<String>,
}

/// Validate one raw row into an [`AiRule`]; `None` (with a `diag` entry) when
/// it cannot be used.
pub fn build_rule(raw: &RawRule<'_>, diag: &mut RuleDiagnostics) -> Option<AiRule> {
    let id = raw.ability_id;
    let Some(kind) = AiKind::parse(raw.ability_type) else {
        diag.dropped.push(format!(
            "ability {id}: unknown ability type {:?}",
            raw.ability_type
        ));
        return None;
    };
    let target = match raw.target {
        None => AiTarget::Auto,
        Some(t) => {
            if let Some(t) = AiTarget::parse(t) {
                t
            } else {
                diag.dropped
                    .push(format!("ability {id}: unknown target {t:?}"));
                return None;
            }
        }
    };
    let parsed = parse_conditions(raw.conditions);
    diag.unknown_keys.extend(parsed.unknown_keys);
    if !parsed.bad_values.is_empty() {
        diag.dropped
            .push(format!("ability {id}: {}", parsed.bad_values.join("; ")));
        return None;
    }
    Some(AiRule {
        ability_id: id,
        kind,
        priority: raw.priority,
        chance_pct: u8::try_from(raw.chance_pct.clamp(0, 100)).unwrap_or(100),
        cooldown_s: u32::try_from(raw.cooldown_s.max(0)).unwrap_or(0),
        when: parsed.when,
        target,
        min_level: raw.min_level,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw<'a>(conditions: Option<&'a serde_json::Value>, target: Option<&'a str>) -> RawRule<'a> {
        RawRule {
            ability_id: 7,
            ability_type: "SPELL",
            priority: 10,
            chance_pct: 150,
            cooldown_s: -3,
            conditions,
            target,
            min_level: Some(5),
        }
    }

    #[test]
    fn parses_every_known_key() {
        let v = json!({
            "in_combat": true, "self_hp_below": 60, "target_hp_below": 30,
            "ally_hp_below": 40, "grouped_targets": true, "target_casting": true,
            "target_has_buff": true, "self_has_debuff": true,
            "target_lifeforce": "undead", "align": "Evil", "telegraph": true,
        });
        let p = parse_conditions(Some(&v));
        assert!(p.unknown_keys.is_empty() && p.bad_values.is_empty());
        for c in [
            AiCondition::InCombat,
            AiCondition::SelfHpBelow(60),
            AiCondition::TargetHpBelow(30),
            AiCondition::AllyHpBelow(40),
            AiCondition::GroupedTargets(2),
            AiCondition::TargetCasting,
            AiCondition::TargetHasBuff,
            AiCondition::SelfHasDebuff,
            AiCondition::TargetLifeforce(LifeForce::Undead),
            AiCondition::Align(AiAlign::Evil),
            AiCondition::Telegraph,
        ] {
            assert!(p.when.contains(&c), "missing {c:?}");
        }
        assert_eq!(p.when.len(), 11);
    }

    #[test]
    fn target_hp_above_parses_next_to_below() {
        let p = parse_conditions(Some(&json!({"target_hp_above": 75, "target_hp_below": 90})));
        assert!(p.bad_values.is_empty() && p.unknown_keys.is_empty());
        assert!(p.when.contains(&AiCondition::TargetHpAbove(75)));
        assert!(p.when.contains(&AiCondition::TargetHpBelow(90)));
        let p = parse_conditions(Some(&json!({"target_hp_above": "most"})));
        assert_eq!(p.bad_values.len(), 1);
    }

    #[test]
    fn alignment_conditions_parse_bands_and_band_lists() {
        let p = parse_conditions(Some(&json!({
            "align": "evil", "not_align": "Good", "self_align": ["good", "neutral"],
        })));
        assert!(p.bad_values.is_empty(), "{:?}", p.bad_values);
        assert!(p.when.contains(&AiCondition::Align(AiAlign::Evil)));
        assert!(p.when.contains(&AiCondition::NotAlign(AiAlign::Good)));
        let mask = AlignMask::of(&[AiAlign::Good, AiAlign::Neutral]);
        assert!(p.when.contains(&AiCondition::SelfAlign(mask)));
        assert!(mask.contains(AiAlign::Good) && !mask.contains(AiAlign::Evil));
        // A single band is accepted for self_align too; junk is not.
        let p = parse_conditions(Some(&json!({"self_align": "evil"})));
        assert_eq!(
            p.when,
            vec![AiCondition::SelfAlign(AlignMask::of(&[AiAlign::Evil]))]
        );
        for bad in [
            json!({"self_align": []}),
            json!({"self_align": ["lawful"]}),
            json!({"not_align": 3}),
        ] {
            assert_eq!(parse_conditions(Some(&bad)).bad_values.len(), 1, "{bad}");
        }
    }

    #[test]
    fn alignment_bands_follow_the_350_thresholds() {
        assert_eq!(AiAlign::of(350), AiAlign::Good);
        assert_eq!(AiAlign::of(349), AiAlign::Neutral);
        assert_eq!(AiAlign::of(-349), AiAlign::Neutral);
        assert_eq!(AiAlign::of(-350), AiAlign::Evil);
    }

    #[test]
    fn not_lifeforce_takes_one_name_or_a_list() {
        let p = parse_conditions(Some(&json!({"not_lifeforce": "undead"})));
        assert_eq!(p.when, vec![AiCondition::NotLifeforce(LifeForce::Undead)]);
        let p = parse_conditions(Some(&json!({"not_lifeforce": ["UNDEAD", "magic"]})));
        assert_eq!(
            p.when,
            vec![
                AiCondition::NotLifeforce(LifeForce::Undead),
                AiCondition::NotLifeforce(LifeForce::Magic)
            ]
        );
        assert_eq!(
            parse_conditions(Some(&json!({"not_lifeforce": "ghosty"})))
                .bad_values
                .len(),
            1
        );
    }

    #[test]
    fn level_and_roll_conditions_parse() {
        let p = parse_conditions(Some(&json!({
            "target_level_max": 19, "level_scaled_chance": true,
            "level_roll": {"min": 5, "max": 10},
        })));
        assert!(p.bad_values.is_empty(), "{:?}", p.bad_values);
        assert!(p.when.contains(&AiCondition::TargetLevelMax(19)));
        assert!(p.when.contains(&AiCondition::LevelScaledChance(1000)));
        assert!(p.when.contains(&AiCondition::LevelRoll { min: 5, max: 10 }));
        let p = parse_conditions(Some(&json!({"level_scaled_chance": 0.5})));
        assert_eq!(p.when, vec![AiCondition::LevelScaledChance(500)]);
        assert!(
            parse_conditions(Some(&json!({"level_scaled_chance": false})))
                .when
                .is_empty()
        );
        for bad in [
            json!({"level_scaled_chance": "lots"}),
            json!({"level_scaled_chance": -1}),
            json!({"level_roll": {"min": 9, "max": 2}}),
            json!({"level_roll": 5}),
            json!({"target_level_max": "high"}),
        ] {
            assert_eq!(parse_conditions(Some(&bad)).bad_values.len(), 1, "{bad}");
        }
    }

    #[test]
    fn debuff_condition_is_a_flag_or_names() {
        let p = parse_conditions(Some(&json!({"self_has_debuff": true})));
        assert_eq!(p.when, vec![AiCondition::SelfHasDebuff]);
        let p = parse_conditions(Some(&json!({"self_has_debuff": "Poison"})));
        assert_eq!(
            p.when,
            vec![AiCondition::SelfHasDebuffNamed(vec!["poison".into()])]
        );
        let p = parse_conditions(Some(&json!({"self_has_debuff": ["web", "entangle"]})));
        assert_eq!(
            p.when,
            vec![AiCondition::SelfHasDebuffNamed(vec![
                "web".into(),
                "entangle".into()
            ])]
        );
        assert_eq!(
            parse_conditions(Some(&json!({"self_has_debuff": 3})))
                .bad_values
                .len(),
            1
        );
    }

    #[test]
    fn equipment_room_and_composition_conditions_parse() {
        use mud_db::enums::Composition as C;
        let p = parse_conditions(Some(&json!({
            "requires_shield": true, "requires_weapon_type": "Piercing", "outdoors": true,
            "composition": ["fire", "LAVA"], "not_composition": "flesh",
        })));
        assert!(p.bad_values.is_empty(), "{:?}", p.bad_values);
        assert!(p.when.contains(&AiCondition::RequiresShield));
        assert!(
            p.when
                .contains(&AiCondition::RequiresWeaponType("piercing".into()))
        );
        assert!(p.when.contains(&AiCondition::Outdoors));
        assert!(
            p.when
                .contains(&AiCondition::Composition(vec![C::Fire, C::Lava]))
        );
        assert!(
            p.when
                .contains(&AiCondition::NotComposition(vec![C::Flesh]))
        );
        assert!(
            parse_conditions(Some(&json!({"requires_shield": false, "outdoors": false})))
                .when
                .is_empty()
        );
        for bad in [
            json!({"requires_weapon_type": ""}),
            json!({"composition": ["jelly"]}),
            json!({"outdoors": "yes"}),
        ] {
            assert_eq!(parse_conditions(Some(&bad)).bad_values.len(), 1, "{bad}");
        }
    }

    #[test]
    fn mob_ai_remembers_failed_attempts_per_rule() {
        let mut ai = MobAi::new(Arc::from(Vec::new()));
        assert_eq!(ai.failed_at(1), None);
        ai.mark_failed(1, 40);
        ai.mark_failed(1, 80);
        ai.mark_failed(0, 5);
        assert_eq!(ai.failed_at(1), Some(80));
        assert_eq!(ai.failed_at(0), Some(5));
        assert_eq!(ai.failed.len(), 2);
    }

    #[test]
    fn combat_booleans_pick_the_matching_clause() {
        let p = parse_conditions(Some(&json!({"in_combat": false})));
        assert_eq!(p.when, vec![AiCondition::OutOfCombat]);
        let p = parse_conditions(Some(&json!({"out_of_combat": true})));
        assert_eq!(p.when, vec![AiCondition::OutOfCombat]);
        let p = parse_conditions(Some(&json!({"out_of_combat": false})));
        assert_eq!(p.when, vec![AiCondition::InCombat]);
    }

    #[test]
    fn unknown_keys_are_reported_and_ignored() {
        let p = parse_conditions(Some(&json!({"self_hp_below": 50, "moon_phase": "full"})));
        assert_eq!(p.when, vec![AiCondition::SelfHpBelow(50)]);
        assert_eq!(p.unknown_keys, vec!["moon_phase".to_string()]);
        assert!(p.bad_values.is_empty());
    }

    #[test]
    fn bad_values_are_reported() {
        let p = parse_conditions(Some(&json!({"self_hp_below": "lots", "align": "chaotic"})));
        assert!(p.when.is_empty());
        assert_eq!(p.bad_values.len(), 2);
        let p = parse_conditions(Some(&json!([1, 2])));
        assert_eq!(p.bad_values.len(), 1);
        assert_eq!(parse_conditions(None), ParsedConditions::default());
        assert_eq!(
            parse_conditions(Some(&serde_json::Value::Null)),
            ParsedConditions::default()
        );
    }

    #[test]
    fn false_on_true_only_keys_means_no_constraint() {
        let p = parse_conditions(Some(
            &json!({"target_casting": false, "grouped_targets": false}),
        ));
        assert!(p.when.is_empty() && p.bad_values.is_empty());
    }

    #[test]
    fn build_rule_clamps_and_collects_diagnostics() {
        let mut diag = RuleDiagnostics::default();
        let cond = json!({"in_combat": true, "phase": 1});
        let rule = build_rule(&raw(Some(&cond), Some("weakest-attacker")), &mut diag).unwrap();
        assert_eq!(rule.kind, AiKind::Spell);
        assert_eq!(rule.target, AiTarget::WeakestAttacker);
        assert_eq!(rule.chance_pct, 100);
        assert_eq!(rule.cooldown_s, 0);
        assert_eq!(rule.min_level, Some(5));
        assert_eq!(rule.when, vec![AiCondition::InCombat]);
        assert_eq!(diag.unknown_keys.len(), 1);
        // A repeat of the same unknown key is still a single entry.
        build_rule(&raw(Some(&cond), None), &mut diag).unwrap();
        assert_eq!(diag.unknown_keys.len(), 1);
        assert_eq!(
            build_rule(&raw(None, None), &mut diag).unwrap().target,
            AiTarget::Auto
        );
        assert!(build_rule(&raw(None, Some("nobody")), &mut diag).is_none());
        let bad = json!({"align": 3});
        assert!(build_rule(&raw(Some(&bad), None), &mut diag).is_none());
        assert_eq!(diag.dropped.len(), 2);
    }

    #[test]
    fn mob_ai_tracks_last_use_per_rule() {
        let mut ai = MobAi::new(Arc::from(Vec::new()));
        assert_eq!(ai.last_used(2), None);
        ai.mark_used(2, 40);
        ai.mark_used(2, 90);
        ai.mark_used(0, 5);
        assert_eq!(ai.last_used(2), Some(90));
        assert_eq!(ai.last_used(0), Some(5));
        assert_eq!(ai.last_use.len(), 2);
    }
}
