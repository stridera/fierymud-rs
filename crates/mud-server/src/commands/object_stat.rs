//! Shared object-stat renderer behind `ostat` (prototype) and
//! `stat <object>` (live item). Both commands print the same block so a
//! builder never has to guess which one carries the stat bonuses, the
//! wear locks or the curse marker; `stat` only adds the live-instance
//! lines (location, current locks, charges, active effects).

use bevy_ecs::prelude::*;
use mud_db::enums::{ObjectFlag, ObjectRestriction};
use mud_world::components::WeaponDiceSizeAdjust;
use mud_world::{
    AbilityCatalog, AppliedTo, ClassCatalog, EffectInstance, EquippedSlot, Item, Keywords, Located,
    ObjectFlags, ObjectProto, ObjectRestrictions, WorldKey,
};

use crate::commands::{format_wealth, name_or};

fn signed(n: i32) -> String {
    format!("{n:+}")
}

fn title_case(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

/// Legacy-style lock marker: `!DROP` and friends.
fn restriction_tag(r: ObjectRestriction) -> &'static str {
    match r {
        ObjectRestriction::NoDrop => "!DROP",
        ObjectRestriction::NoTake => "!TAKE",
        ObjectRestriction::NoSell => "!SELL",
        ObjectRestriction::NoBurn => "!BURN",
        ObjectRestriction::NoLocate => "!LOCATE",
        ObjectRestriction::NoInvisible => "!INVIS",
    }
}

/// Render the unified object block. `item` is the live instance when
/// there is one (`stat`); `proto` is the catalog row (absent for
/// synthetic items). Returns text containing color tags: send it with
/// `send_rendered`.
#[allow(clippy::too_many_lines)]
pub(crate) fn render_object_stat(
    world: &mut World,
    proto: Option<&ObjectProto>,
    item: Option<Entity>,
) -> String {
    let mut out = String::from("\r\n");
    let key = proto.map(|p| (p.zone_id, p.id));

    if let Some((zone, id)) = key {
        out.push_str(&format!("(zone, id):    ({zone}, {id})\r\n"));
    }
    let name = match (item, proto) {
        (Some(e), _) => name_or(world, e, "(unknown)"),
        (None, Some(p)) => p.name.clone(),
        (None, None) => "(unknown)".to_string(),
    };
    out.push_str(&format!("name:          {name}\r\n"));
    let keywords: Vec<String> = match (item, proto) {
        (Some(e), _) => world
            .get::<Keywords>(e)
            .map(|k| k.0.clone())
            .or_else(|| proto.map(|p| p.keywords.clone()))
            .unwrap_or_default(),
        (None, Some(p)) => p.keywords.clone(),
        (None, None) => Vec::new(),
    };
    out.push_str(&format!("keywords:      {}\r\n", keywords.join(", ")));
    if let Some(desc) = proto.and_then(|p| p.examine_description.as_deref()) {
        out.push_str(&format!("examine:       {desc}\r\n"));
    }

    // Flags and locks: the live components when there is an instance
    // (a curse spell changes them), otherwise the proto's.
    let flags: Vec<ObjectFlag> = match item {
        Some(e) => world
            .get::<ObjectFlags>(e)
            .map(|f| f.0.clone())
            .unwrap_or_default(),
        None => proto.map(|p| p.flags.clone()).unwrap_or_default(),
    };
    let locks: Vec<ObjectRestriction> = match item {
        Some(e) => world
            .get::<ObjectRestrictions>(e)
            .map(|r| r.0.clone())
            .unwrap_or_default(),
        None => proto.map(|p| p.restrictions.clone()).unwrap_or_default(),
    };

    let Some(p) = proto else {
        out.push_str("proto:         <none> (synthetic item)\r\n");
        append_locks(&mut out, &flags, &locks);
        if let Some(e) = item {
            append_instance(world, &mut out, e);
        }
        return out;
    };

    out.push_str(&format!("type:          {}\r\n", p.r#type.label()));
    out.push_str(&format!(
        "weight:        {:.1}    level: {}    cost: {}\r\n",
        p.weight,
        p.level,
        format_wealth(i64::from(p.cost)).unwrap_or_else(|| "0".to_string()),
    ));
    let wear_labels: Vec<&'static str> = p.wear_flags.iter().map(|f| f.label()).collect();
    out.push_str(&format!(
        "wear_flags:    {}\r\n",
        if wear_labels.is_empty() {
            "<none>".to_string()
        } else {
            wear_labels.join(", ")
        }
    ));
    append_locks(&mut out, &flags, &locks);

    // Combat numbers.
    if p.weapon_dice_num > 0 {
        let bonus = match p.weapon_dice_bonus.cmp(&0) {
            std::cmp::Ordering::Equal => String::new(),
            _ => signed(p.weapon_dice_bonus),
        };
        let dtype = p
            .weapon_damage_type
            .as_deref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        // A curse spell shrinks the die on this instance only.
        let adjust = item
            .and_then(|e| world.get::<WeaponDiceSizeAdjust>(e))
            .map_or(0, |a| a.0);
        let size = (p.weapon_dice_size + adjust).max(1);
        let mut live = p.clone();
        live.weapon_dice_size = size;
        let note = if adjust == 0 {
            String::new()
        } else {
            format!(" [die {}, was d{}]", signed(adjust), p.weapon_dice_size)
        };
        out.push_str(&format!(
            "damage:        {}d{size}{bonus}{dtype}, avg {}{note}\r\n",
            p.weapon_dice_num,
            live.avg_damage(),
        ));
    }
    if p.armor_pct != 0 {
        out.push_str(&format!("armor:         {}% mitigation\r\n", p.armor_pct));
    }

    // Stat increases, resistances and granted effect flags.
    let mut applies: Vec<String> = Vec::new();
    let mut grants: Vec<String> = Vec::new();
    for g in &p.granted_effects {
        let def = world
            .get_resource::<mud_world::EffectCatalog>()
            .and_then(|c| c.by_id.get(&g.effect_id).cloned());
        let at = g
            .wear_location
            .map(|w| format!(" [worn: {}]", w.label()))
            .unwrap_or_default();
        match def {
            Some(d) if d.effect_type == "modify" => {
                let target = g
                    .modifier_data
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                let amount = g
                    .modifier_data
                    .get("amount")
                    .and_then(serde_json::Value::as_i64)
                    .map_or_else(|| "?".to_string(), |a| format!("{a:+}"));
                applies.push(format!("{target} {amount}{at}"));
            }
            Some(d) => {
                let label = g
                    .modifier_data
                    .get("flag")
                    .and_then(serde_json::Value::as_str)
                    .map_or(d.name, str::to_string);
                grants.push(format!("{label} (strength {}){at}", g.strength));
            }
            None => grants.push(format!("effect #{} (unknown){at}", g.effect_id)),
        }
    }
    for (element, value, _) in &p.resistances {
        if *value != 0 {
            applies.push(format!("resist {element:?} {}%", signed(*value)));
        }
    }
    if applies.is_empty() {
        out.push_str("applies:       <none>\r\n");
    } else {
        out.push_str(&format!("applies:       {}\r\n", applies.join(", ")));
    }
    if grants.is_empty() {
        out.push_str("effect flags:  <none>\r\n");
    } else {
        out.push_str(&format!("effect flags:  {}\r\n", grants.join(", ")));
    }

    // Bound abilities (scrolls / wands / staves).
    if let Some((zone, id)) = key {
        let bindings = world
            .resource::<mud_world::ObjectAbilityCatalog>()
            .by_key
            .get(&(zone, id))
            .cloned()
            .unwrap_or_default();
        for b in bindings {
            let name = world
                .resource::<AbilityCatalog>()
                .by_name
                .values()
                .find(|d| d.id == b.ability_id)
                .map_or_else(
                    || format!("(id {})", b.ability_id),
                    |d| d.plain_name.clone(),
                );
            let ch = b
                .charges
                .map_or_else(|| "unlimited".to_string(), |c| c.to_string());
            out.push_str(&format!(
                "ability:       {name} (level {}, charges {ch})\r\n",
                b.level
            ));
        }
    }

    if let Some(b) = p.board_id {
        out.push_str(&format!("board_id:      {b}\r\n"));
    }
    if let Some(liq) = &p.liquid {
        out.push_str(&format!(
            "liquid:        {} ({}/{}, poisoned={})\r\n",
            liq.liquid, liq.remaining, liq.capacity, liq.poisoned
        ));
    }
    if let Some(fuel) = &p.light_fuel {
        if fuel.remaining < 0 {
            out.push_str("fuel:          infinite\r\n");
        } else {
            out.push_str(&format!(
                "fuel:          {} / {} game-hours\r\n",
                fuel.remaining, fuel.capacity
            ));
        }
    }
    if p.timer_hours > 0 {
        out.push_str(&format!("timer:         {} game-hours\r\n", p.timer_hours));
    }
    if let Some(t) = p.camp_kit_tier {
        out.push_str(&format!("camp kit:      tier {t}\r\n"));
    }

    // Who can't / can wear it.
    let mut forbidden: Vec<String> = Vec::new();
    forbidden.extend(
        p.restricted_alignments
            .iter()
            .map(|a| a.label().to_string()),
    );
    forbidden.extend(p.restricted_races.iter().map(|r| title_case(r)));
    for id in &p.restricted_class_ids {
        let cname = world
            .get_resource::<ClassCatalog>()
            .and_then(|c| c.by_id.get(id).map(|d| d.plain_name.clone()))
            .unwrap_or_else(|| format!("class #{id}"));
        forbidden.push(cname);
    }
    if !forbidden.is_empty() {
        out.push_str(&format!("forbidden to:  {}\r\n", forbidden.join(", ")));
    }
    if !p.allowed_races.is_empty() {
        let names: Vec<String> = p.allowed_races.iter().map(|r| title_case(r)).collect();
        out.push_str(&format!("only races:    {}\r\n", names.join(", ")));
    }
    if let Some(min) = &p.min_size {
        out.push_str(&format!("min size:      {min}\r\n"));
    }
    if let Some(max) = &p.max_size {
        out.push_str(&format!("max size:      {max}\r\n"));
    }

    if let Some((zone, id)) = key {
        let trig_count = world
            .resource::<mud_world::TriggerCatalog>()
            .object_attachments
            .get(&(zone, id))
            .map_or(0, Vec::len);
        out.push_str(&format!("triggers:      {trig_count}\r\n"));
    }
    if p.extras.is_empty() {
        out.push_str("extras:        <none>\r\n");
    } else {
        out.push_str(&format!("extras:        {} entries\r\n", p.extras.len()));
        for (kws, _) in &p.extras {
            out.push_str(&format!("               keywords: {}\r\n", kws.join(", ")));
        }
    }

    if let Some(e) = item {
        append_instance(world, &mut out, e);
    }
    if let Some((zone, id)) = key {
        let live = world
            .query_filtered::<&WorldKey, With<Item>>()
            .iter(world)
            .filter(|wk| wk.zone == zone && wk.id == id)
            .count();
        out.push_str(&format!("live count:    {live}\r\n"));
    }
    out
}

fn append_locks(out: &mut String, flags: &[ObjectFlag], locks: &[ObjectRestriction]) {
    if flags.is_empty() {
        out.push_str("flags:         <none>\r\n");
    } else {
        let labels: Vec<&'static str> = flags.iter().map(|f| f.label()).collect();
        out.push_str(&format!("flags:         {}\r\n", labels.join(", ")));
    }
    if locks.is_empty() {
        out.push_str("locks:         <none>\r\n");
    } else {
        let tags: Vec<&'static str> = locks.iter().map(|r| restriction_tag(*r)).collect();
        out.push_str(&format!("locks:         {}\r\n", tags.join(" ")));
    }
    // Legacy ostat/stat calls an item that can't be dropped cursed.
    if locks.contains(&ObjectRestriction::NoDrop) {
        out.push_str("               <b:red>CURSED!!</> (can't be dropped or given)\r\n");
    }
}

fn append_instance(world: &mut World, out: &mut String, item: Entity) {
    out.push_str(&format!("entity:        {item:?}\r\n"));
    if let Some(located) = world.get::<Located>(item).copied() {
        let in_name = name_or(world, located.0, "(unknown)");
        out.push_str(&format!("located_in:    {:?} ({in_name})\r\n", located.0));
    }
    if let Some(eq) = world.get::<EquippedSlot>(item) {
        out.push_str(&format!("equipped_slot: {}\r\n", eq.0.db_label()));
    }
    if let Some(c) = world.get::<mud_world::Charges>(item) {
        out.push_str(&format!("charges:       {}\r\n", c.0));
    }
    if mud_world::is_lit(world, item) {
        out.push_str("lit:           yes\r\n");
    }
    if let Some(fuel) = world.get::<mud_world::LightFuel>(item).copied() {
        if fuel.remaining < 0 {
            out.push_str("fuel left:     infinite\r\n");
        } else {
            out.push_str(&format!(
                "fuel left:     {} / {} game-hours\r\n",
                fuel.remaining, fuel.capacity
            ));
        }
    }
    let effects: Vec<(String, i32)> = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .filter(|(_, a)| a.0 == item)
            .map(|(e, _)| (e.name.clone(), e.remaining_secs))
            .collect()
    };
    if effects.is_empty() {
        out.push_str("effects:       <none>\r\n");
    } else {
        out.push_str(&format!("effects:       {} active\r\n", effects.len()));
        for (n, secs) in &effects {
            out.push_str(&format!("               {n} ({secs}s)\r\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::admin_inspect::{cmd_ostat, cmd_stat};
    use crate::commands::test_support::{drain, object_proto, player_in};
    use mud_db::enums::{ObjectType, WearFlag};
    use mud_world::{
        AbilityCatalog, EffectCatalog, EffectDef, Named, ObjectAbilityCatalog, ObjectGrantedEffect,
        ObjectPrototypes, TriggerCatalog,
    };

    fn effect(id: i32, name: &str, effect_type: &str) -> EffectDef {
        EffectDef {
            id,
            name: name.into(),
            effect_type: effect_type.into(),
            description: None,
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

    /// A cursed helm (`NO_DROP`, +3 accuracy modify, armor, an
    /// effect flag) lying in the room, plus a staff player to look at it.
    fn cursed_helm_world() -> (World, Entity, Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, rx) = player_in(&mut world, room);
        let mut p = object_proto(30, 7, ObjectType::Armor);
        p.name = "Saintly Helm".to_string();
        p.keywords = vec!["saintly".to_string(), "helm".to_string()];
        p.wear_flags = vec![WearFlag::Head];
        p.armor_pct = 12;
        p.restrictions = vec![ObjectRestriction::NoDrop];
        p.granted_effects = vec![
            ObjectGrantedEffect {
                effect_id: 1,
                strength: 1,
                modifier_data: serde_json::json!({"target": "accuracy", "amount": 3}),
                wear_location: None,
            },
            ObjectGrantedEffect {
                effect_id: 2,
                strength: 1,
                modifier_data: serde_json::json!({"flag": "detect_invis"}),
                wear_location: None,
            },
        ];
        let mut protos = ObjectPrototypes::default();
        protos.by_key.insert((30, 7), p);
        world.insert_resource(protos);
        let mut effects = EffectCatalog::default();
        effects.by_id.insert(1, effect(1, "modify", "modify"));
        effects.by_id.insert(2, effect(2, "status", "status"));
        world.insert_resource(effects);
        world.insert_resource(AbilityCatalog::default());
        world.insert_resource(ObjectAbilityCatalog::default());
        world.insert_resource(TriggerCatalog::default());
        world.init_resource::<ClassCatalog>();
        world.spawn((
            Item,
            Named {
                name: "Saintly Helm".to_string(),
            },
            Keywords(vec!["saintly".to_string(), "helm".to_string()]),
            WorldKey { zone: 30, id: 7 },
            Located(room),
            ObjectRestrictions(vec![ObjectRestriction::NoDrop]),
        ));
        (world, player, rx)
    }

    type Rx = crate::commands::test_support::Rx;

    fn strip(s: &str) -> String {
        crate::commands::render_color_tags(s, crate::commands::ColorMode::Strip)
    }

    #[test]
    fn ostat_shows_locks_curse_stats_and_effect_flags() {
        let (mut world, player, mut rx) = cursed_helm_world();
        cmd_ostat(&mut world, player, "30 7");
        let out = strip(&drain(&mut rx));
        assert!(out.contains("!DROP"), "{out}");
        assert!(out.contains("CURSED!!"), "{out}");
        assert!(out.contains("armor:         12%"), "{out}");
        assert!(out.contains("accuracy +3"), "{out}");
        assert!(out.contains("detect_invis"), "{out}");
        assert!(out.contains("Head"), "{out}");
    }

    #[test]
    fn stat_on_an_item_shows_the_same_object_block_as_ostat() {
        let (mut world, player, mut rx) = cursed_helm_world();
        cmd_ostat(&mut world, player, "30 7");
        let ostat = strip(&drain(&mut rx));
        cmd_stat(&mut world, player, "helm");
        let stat = strip(&drain(&mut rx));
        // Every ostat line (bar the per-prototype live count) appears in
        // stat; stat then adds the instance lines.
        for line in ostat.lines().filter(|l| !l.starts_with("live count")) {
            assert!(
                stat.lines().any(|l| l == line),
                "stat is missing ostat line {line:?}\n--- stat ---\n{stat}"
            );
        }
        assert!(stat.contains("located_in:"), "{stat}");
        assert!(stat.contains("CURSED!!"), "{stat}");
    }

    #[test]
    fn stat_reads_the_live_locks_not_the_proto() {
        // A curse spell (or remove curse) edits the instance, so stat
        // must show the live component, not the catalog row.
        let (mut world, player, mut rx) = cursed_helm_world();
        let helm = world
            .query_filtered::<Entity, With<Item>>()
            .single(&world)
            .unwrap();
        world.entity_mut(helm).remove::<ObjectRestrictions>();
        cmd_stat(&mut world, player, "helm");
        let stat = strip(&drain(&mut rx));
        assert!(!stat.contains("CURSED!!"), "{stat}");
        assert!(stat.contains("locks:         <none>"), "{stat}");
    }
}
