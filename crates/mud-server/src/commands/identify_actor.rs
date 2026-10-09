//! Identify cast on a character (issue #90). Legacy `spell_identify`'s
//! victim branch: name, age (players), height and weight, then what the
//! creature is composed of and the nature of its life force. Legacy shows
//! no active effects here, so neither does this.

use bevy_ecs::prelude::{Entity, World};
use mud_db::enums::Composition;
use mud_world::{BodyMetrics, LifeForceTag, Mob, MobPrototypes, Profile, RaceCatalog};

use crate::commands::{format_age, send_rendered};

/// Legacy `compositions[].color` as an XML-Lite open tag.
const fn composition_tag(c: Composition) -> &'static str {
    match c {
        Composition::Flesh | Composition::Lava => "<red>",
        Composition::Earth => "<yellow>",
        Composition::Air => "<cyan>",
        Composition::Fire => "<b:red>",
        Composition::Water => "<b:blue>",
        Composition::Ice => "<blue>",
        Composition::Mist => "<b:cyan>",
        Composition::Ether => "<magenta>",
        Composition::Metal => "<b:black>",
        Composition::Stone => "",
        Composition::Bone => "<b:white>",
        Composition::Plant => "<green>",
    }
}

/// Legacy `lifeforces[]` name and color tag.
const fn lifeforce_style(lf: mud_db::enums::LifeForce) -> (&'static str, &'static str) {
    use mud_db::enums::LifeForce;
    match lf {
        LifeForce::Life => ("life", "<b:green>"),
        LifeForce::Undead => ("undead", "<b:black>"),
        LifeForce::Magic => ("magic", "<b:blue>"),
        LifeForce::Celestial => ("celestial", "<cyan>"),
        LifeForce::Demonic => ("demonic", "<b:red>"),
        LifeForce::Elemental => ("elemental", "<yellow>"),
    }
}

/// Legacy `statelength`.
fn state_length(inches: i32) -> String {
    let plural = |n: i32, one: &'static str, many: &'static str| if n == 1 { one } else { many };
    if inches < 12 {
        format!("{inches} {}", plural(inches, "inch", "inches"))
    } else if inches < 1200 && inches % 12 != 0 {
        format!(
            "{} {}, {} {}",
            inches / 12,
            plural(inches / 12, "foot", "feet"),
            inches % 12,
            plural(inches % 12, "inch", "inches"),
        )
    } else {
        format!("{} {}", inches / 12, plural(inches / 12, "foot", "feet"))
    }
}

/// Legacy `stateweight`.
fn state_weight(pounds: i32) -> String {
    if pounds < 2000 {
        format!("{pounds}.00 pound{}", if pounds == 1 { "" } else { "s" })
    } else if pounds < 2100 {
        "1 ton".to_string()
    } else {
        let tons = format!("{:.1}", f64::from(pounds) / 2000.0);
        format!("{} tons", tons.strip_suffix(".0").unwrap_or(&tons))
    }
}

fn composition_of(world: &World, target: Entity) -> Composition {
    if world.get::<Mob>(target).is_some()
        && let Some(key) = world.get::<mud_world::WorldKey>(target).copied()
        && let Some(proto) = world
            .get_resource::<MobPrototypes>()
            .and_then(|p| p.by_key.get(&(key.zone, key.id)))
    {
        return crate::commands::info::mob_composition(world, proto);
    }
    world
        .get::<Profile>(target)
        .and_then(|p| world.get_resource::<RaceCatalog>()?.get(&p.race))
        .map_or(Composition::Flesh, |r| r.default_composition)
}

fn life_force_of(world: &World, target: Entity) -> mud_db::enums::LifeForce {
    use mud_db::enums::LifeForce;
    if let Some(LifeForceTag(lf)) = world.get::<LifeForceTag>(target).copied() {
        return lf;
    }
    let race_default = world.get::<Profile>(target).and_then(|p| {
        world
            .get_resource::<RaceCatalog>()?
            .get(&p.race)
            .map(|r| r.default_lifeforce.to_ascii_uppercase())
    });
    match race_default.as_deref() {
        Some("UNDEAD") => LifeForce::Undead,
        Some("MAGIC") => LifeForce::Magic,
        Some("CELESTIAL") => LifeForce::Celestial,
        Some("DEMONIC") => LifeForce::Demonic,
        Some("ELEMENTAL") => LifeForce::Elemental,
        _ => LifeForce::Life,
    }
}

/// Tell `caster` what the identify spell reveals about `target`.
pub(crate) fn identify_actor(world: &mut World, caster: Entity, target: Entity) {
    let name = crate::commands::name_of(world, target);
    let mut out = format!("Name: {name}\r\n");
    if let Some(prof) = world.get::<Profile>(target)
        && let Some(age) = format_age(prof.level)
    {
        out.push_str(&format!("{name} is {age} old.\r\n"));
    }
    if let Some(bm) = world.get::<BodyMetrics>(target).copied() {
        out.push_str(&format!(
            "Height {}; Weight {}\r\n",
            state_length(bm.height),
            state_weight(bm.weight),
        ));
    }
    let (he, his, _) =
        super::look_auras::pronouns(&super::look_auras::target_gender(world, target));
    let comp = composition_of(world, target);
    let (lf_name, lf_tag) = lifeforce_style(life_force_of(world, target));
    let comp_tag = composition_tag(comp);
    let comp_close = if comp_tag.is_empty() { "" } else { "</>" };
    out.push_str(&format!(
        "{} is composed of {comp_tag}{}{comp_close}, and {his} nature is {lf_tag}{lf_name}</>.\r\n",
        crate::commands::capitalize(he),
        format!("{comp:?}").to_ascii_lowercase(),
    ));
    send_rendered(world, caster, &out);
}
