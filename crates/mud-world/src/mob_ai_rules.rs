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
//! | `self_has_debuff`  | `true`                 | `SelfHasDebuff`               |
//! | `target_lifeforce` | `LifeForce` name       | `TargetLifeforce`             |
//! | `align`            | good / neutral / evil  | `Align`                       |
//!
//! A `false` value on the `true`-only keys means "no constraint". An unknown
//! key is ignored (and logged once per load); a known key with an unusable
//! value drops the whole rule, because ignoring it would let the rule fire in
//! situations its author excluded.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

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

/// One `when` clause. All clauses of a rule must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiCondition {
    InCombat,
    OutOfCombat,
    /// Own HP below this percent.
    SelfHpBelow(u8),
    /// Chosen target's HP below this percent.
    TargetHpBelow(u8),
    /// At least this many enemies are engaged with the mob.
    GroupedTargets(u8),
    TargetCasting,
    TargetHasBuff,
    SelfHasDebuff,
    /// Some ally in the room is below this HP percent.
    AllyHpBelow(u8),
    TargetLifeforce(LifeForce),
    Align(AiAlign),
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
}

impl MobAi {
    #[must_use]
    pub fn new(rules: Arc<[AiRule]>) -> Self {
        Self {
            rules,
            last_use: SmallVec::new(),
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

/// Every parsed rule table. Built at boot by `loader::load_mob_ai_data`; empty
/// (and harmless) when the AI tables are missing or the feature is unused.
#[derive(Resource, Debug, Default)]
pub struct MobAiData {
    /// `ClassAiRules` per `Class.id`, priority order.
    pub by_class: HashMap<i32, Arc<[AiRule]>>,
    /// `MobAbilities` per mob prototype `(zone, id)`, priority order.
    pub by_mob: HashMap<(i32, i32), Arc<[AiRule]>>,
    /// `Mobs.ai_mode` for prototypes that are not `CLASS` (the default).
    pub modes: HashMap<(i32, i32), MobAiMode>,
}

impl MobAiData {
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
    /// has no spell / skill AI (mode `NONE`, `NoClassAi`, no class, or no rules).
    #[must_use]
    pub fn rules_for(&self, proto: &MobProto) -> Option<Arc<[AiRule]>> {
        let key = (proto.zone_id, proto.id);
        let rules = match self.mode(key) {
            MobAiMode::None => return None,
            MobAiMode::Custom => self.by_mob.get(&key),
            MobAiMode::Class => {
                if proto.behaviors.contains(&MobBehavior::NoClassAi) {
                    return None;
                }
                self.by_class.get(&proto.class_id?)
            }
        }?;
        (!rules.is_empty()).then(|| Arc::clone(rules))
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
            "self_hp_below" | "target_hp_below" | "ally_hp_below" => match pct(v) {
                Some(p) => out.when.push(match key.as_str() {
                    "self_hp_below" => AiCondition::SelfHpBelow(p),
                    "target_hp_below" => AiCondition::TargetHpBelow(p),
                    _ => AiCondition::AllyHpBelow(p),
                }),
                None => bad("expected a percent"),
            },
            "grouped_targets" => match (flag, v.as_i64()) {
                (Some(true), _) => out.when.push(AiCondition::GroupedTargets(2)),
                (Some(false), _) => {}
                (None, Some(n)) if n >= 1 => out.when.push(AiCondition::GroupedTargets(
                    u8::try_from(n.min(255)).unwrap_or(255),
                )),
                _ => bad("expected a count of at least 1, or true"),
            },
            "target_casting" | "target_has_buff" | "self_has_debuff" => match flag {
                Some(true) => out.when.push(match key.as_str() {
                    "target_casting" => AiCondition::TargetCasting,
                    "target_has_buff" => AiCondition::TargetHasBuff,
                    _ => AiCondition::SelfHasDebuff,
                }),
                Some(false) => {}
                None => bad("expected true or false"),
            },
            "target_lifeforce" => match v.as_str().and_then(parse_lifeforce) {
                Some(lf) => out.when.push(AiCondition::TargetLifeforce(lf)),
                None => bad("expected a LifeForce name"),
            },
            "align" => match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("good") => out.when.push(AiCondition::Align(AiAlign::Good)),
                Some("neutral") => out.when.push(AiCondition::Align(AiAlign::Neutral)),
                Some("evil") => out.when.push(AiCondition::Align(AiAlign::Evil)),
                _ => bad("expected good, neutral or evil"),
            },
            _ => out.unknown_keys.push(key.clone()),
        }
    }
    out
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
            "target_lifeforce": "undead", "align": "Evil",
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
        ] {
            assert!(p.when.contains(&c), "missing {c:?}");
        }
        assert_eq!(p.when.len(), 10);
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
