//! Character-creation pick lists: the race and class menus and the matching
//! of what the player types against them.
//!
//! Everything shown comes from the loaded catalogs (`Races`, `Class`, and the
//! class help articles), never from a hard-coded list. A menu fits in 80
//! columns. A choice may be typed as its number, its full name, or the start
//! of its name when that is unambiguous.

use bevy_ecs::prelude::*;
use mud_world::{ClassCatalog, HelpCatalog, RaceCatalog};

/// Widest line a menu row may take.
const MENU_WIDTH: usize = 79;
/// A race counts as strong in a stat when its cap reaches this (the
/// stat scale tops out at 100; a Human's caps are 76).
const STRONG_STAT_CAP: i32 = 80;
/// Class blurbs are cut to this many characters at a word boundary.
const BLURB_MAX_CHARS: usize = 170;

/// One row of a pick list.
#[derive(Debug, Clone)]
pub(crate) struct MenuOption<T> {
    /// What a successful pick returns.
    pub value: T,
    /// Name shown in the list and in ambiguity messages.
    pub label: String,
    /// Normalized strings the player may type (full name or a prefix of one).
    pub keys: Vec<String>,
}

/// Outcome of matching typed input against a pick list.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pick<T> {
    Found(T),
    /// Several rows fit; the labels of the candidates.
    Ambiguous(Vec<String>),
    None,
}

/// `text` without `<...>` colour tags.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Lower-case alphanumerics only, so `Half-Elf`, `half elf` and `HALF_ELF`
/// compare equal.
pub(crate) fn normalize(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Match `input` against `options`: a row number, then an exact name, then a
/// unique prefix of a name.
pub(crate) fn pick<T: Clone>(options: &[MenuOption<T>], input: &str) -> Pick<T> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Pick::None;
    }
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return match trimmed.parse::<usize>() {
            Ok(n) if (1..=options.len()).contains(&n) => Pick::Found(options[n - 1].value.clone()),
            _ => Pick::None,
        };
    }
    let needle = normalize(trimmed);
    if needle.is_empty() {
        return Pick::None;
    }
    let exact: Vec<&MenuOption<T>> = options
        .iter()
        .filter(|o| o.keys.contains(&needle))
        .collect();
    let hits = if exact.is_empty() {
        options
            .iter()
            .filter(|o| o.keys.iter().any(|k| k.starts_with(&needle)))
            .collect()
    } else {
        exact
    };
    match hits.as_slice() {
        [] => Pick::None,
        [one] => Pick::Found(one.value.clone()),
        many => Pick::Ambiguous(many.iter().map(|o| o.label.clone()).collect()),
    }
}

/// The one-line "did you mean" reply for an ambiguous pick.
pub(crate) fn ambiguity_message(typed: &str, labels: &[String]) -> String {
    format!(
        "'{}' could be {}. Type more of the name, or its number.\r\n",
        typed.trim(),
        join_or(labels)
    )
}

fn join_or(labels: &[String]) -> String {
    match labels {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// True when the player asked to see the list again.
pub(crate) fn wants_relist(input: &str) -> bool {
    matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "?" | "list" | "help" | "menu"
    )
}

/// Playable races as pick rows: the `Races` display name, then everything a
/// player might type for it. Sorted by name, then by enum text.
pub(crate) fn race_options(world: &World) -> Vec<MenuOption<String>> {
    let Some(catalog) = world.get_resource::<RaceCatalog>() else {
        return Vec::new();
    };
    let mut rows: Vec<(&mud_world::RaceDef, String)> = catalog
        .by_race
        .values()
        .filter(|r| r.playable)
        .map(|r| {
            let label = if r.plain_name.trim().is_empty() {
                r.race.clone()
            } else {
                r.plain_name.clone()
            };
            (r, label)
        })
        .collect();
    rows.sort_by(|a, b| {
        a.1.to_ascii_lowercase()
            .cmp(&b.1.to_ascii_lowercase())
            .then_with(|| a.0.race.cmp(&b.0.race))
    });
    rows.into_iter()
        .map(|(r, label)| {
            // `name` carries colour tags (`<b:red>Dr</>...`); only its text is a key.
            let mut keys = vec![
                normalize(&label),
                normalize(&r.race),
                normalize(&strip_tags(&r.name)),
            ];
            keys.retain(|k| !k.is_empty());
            keys.dedup();
            MenuOption {
                value: r.race.clone(),
                label,
                keys,
            }
        })
        .collect()
}

/// Stat abbreviations a race's caps put at or above [`STRONG_STAT_CAP`].
fn strong_stats(def: &mud_world::RaceDef) -> Vec<&'static str> {
    let caps = &def.stat_caps;
    [
        ("STR", caps.strength),
        ("INT", caps.intelligence),
        ("WIS", caps.wisdom),
        ("DEX", caps.dexterity),
        ("CON", caps.constitution),
        ("CHA", caps.charisma),
    ]
    .into_iter()
    .filter(|(_, cap)| *cap >= STRONG_STAT_CAP)
    .map(|(name, _)| name)
    .collect()
}

/// `size, alignment, strong stats` for a race row, from the `Races` columns.
fn race_detail(def: &mud_world::RaceDef) -> String {
    let size = def.default_size.to_ascii_lowercase();
    let align = def.race_align.to_ascii_lowercase();
    let strong = strong_stats(def);
    let strong = if strong.is_empty() {
        "balanced".to_string()
    } else {
        format!("strong: {}", strong.join(" "))
    };
    format!("{size}, {align}; {strong}")
}

/// The race menu, one race per line.
pub(crate) fn render_race_menu(world: &World) -> String {
    let options = race_options(world);
    let catalog = world.get_resource::<RaceCatalog>();
    let mut out = String::from(
        "\r\nChoose your race. Type its name, the start of its name, or its number.\r\n\r\n",
    );
    let label_width = options
        .iter()
        .map(|o| o.label.chars().count())
        .max()
        .unwrap_or(0);
    for (i, option) in options.iter().enumerate() {
        let detail = catalog
            .and_then(|c| c.get(&option.value))
            .map(race_detail)
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:>2}) {:<label_width$}  {detail}\r\n",
            i + 1,
            option.label
        ));
    }
    out.push_str("\r\nRace: ");
    out
}

/// Base classes as pick rows, sorted by name.
pub(crate) fn class_options(world: &World) -> Vec<MenuOption<(i32, String)>> {
    let mut bases: Vec<(i32, String)> = world
        .resource::<ClassCatalog>()
        .by_id
        .values()
        .filter(|c| !c.is_subclass)
        .map(|c| (c.id, c.plain_name.clone()))
        .collect();
    bases.sort_by(|a, b| {
        a.1.to_ascii_lowercase()
            .cmp(&b.1.to_ascii_lowercase())
            .then(a.0.cmp(&b.0))
    });
    bases
        .into_iter()
        .map(|(id, name)| MenuOption {
            keys: vec![normalize(&name)],
            label: name.clone(),
            value: (id, name),
        })
        .collect()
}

/// First sentence of `text`, whitespace collapsed, cut to
/// [`BLURB_MAX_CHARS`] at a word boundary.
fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .match_indices(". ")
        .map(|(i, _)| i + 1)
        .next()
        .unwrap_or(flat.len());
    let sentence = &flat[..end];
    if sentence.chars().count() <= BLURB_MAX_CHARS {
        return sentence.to_string();
    }
    let cut: String = sentence.chars().take(BLURB_MAX_CHARS).collect();
    let trimmed = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}...", trimmed.trim_end_matches([',', ';', ':']))
}

/// One-line description of a base class: the `Class.description` column when a
/// builder wrote one, else the first sentence of the class's help article
/// (category `class`, keyword the class name or its plural).
pub(crate) fn class_blurb(world: &World, class_id: i32, plain_name: &str) -> Option<String> {
    if let Some(desc) = world
        .resource::<ClassCatalog>()
        .by_id
        .get(&class_id)
        .and_then(|c| c.description.as_deref())
        .filter(|d| !d.trim().is_empty())
    {
        return Some(first_sentence(desc));
    }
    let help = world.get_resource::<HelpCatalog>()?;
    let lower = plain_name.to_ascii_lowercase();
    let plural = format!("{lower}s");
    let mut hits: Vec<&mud_world::HelpEntry> = help
        .entries
        .values()
        .filter(|e| e.category.as_deref() == Some("class") && e.min_level == 0)
        .filter(|e| {
            e.keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case(&lower) || k.eq_ignore_ascii_case(&plural))
        })
        .collect();
    hits.sort_by_key(|e| e.id);
    hits.first().map(|e| first_sentence(&e.content))
}

/// Full attribute name for a `Class.primary_stat` abbreviation.
pub(crate) fn stat_word(abbrev: &str) -> &'static str {
    match abbrev.trim().to_ascii_uppercase().as_str() {
        "STR" => "strength",
        "INT" => "intelligence",
        "WIS" => "wisdom",
        "DEX" => "dexterity",
        "CON" => "constitution",
        "CHA" => "charisma",
        _ => "",
    }
}

/// A class's `primary_stat` abbreviation, when the class names one.
pub(crate) fn class_key_stat(world: &World, class_id: i32) -> Option<String> {
    world
        .get_resource::<ClassCatalog>()?
        .by_id
        .get(&class_id)?
        .primary_stat
        .clone()
        .filter(|s| !stat_word(s).is_empty())
        .map(|s| s.trim().to_ascii_uppercase())
}

/// The class menu: name, key stat, and a wrapped one-sentence description.
pub(crate) fn render_class_menu(world: &World) -> String {
    let mut out = String::from("\r\nChoose your class. Type its name or its number.\r\n");
    for (i, option) in class_options(world).iter().enumerate() {
        let (id, name) = &option.value;
        let key = class_key_stat(world, *id)
            .map(|s| format!("  (key stat: {s})"))
            .unwrap_or_default();
        out.push_str(&format!("\r\n  {:>2}) {name}{key}\r\n", i + 1));
        if let Some(blurb) = class_blurb(world, *id, name) {
            for line in crate::layout::wrap_paragraph(&blurb, MENU_WIDTH, 6, 6) {
                out.push_str(&line);
                out.push_str("\r\n");
            }
        }
    }
    out.push_str("\r\nClass: ");
    out
}

/// What the rolled numbers mean, shown under the stat block: the bonus in
/// brackets, and which stat the chosen class cares about.
pub(crate) fn stat_explainer(class_name: &str, key_stat: Option<&str>) -> String {
    let mut out = String::from("The number in brackets is the bonus that score gives you.\r\n");
    if let Some(stat) = key_stat {
        out.push_str(&format!(
            "For a {class_name}, {stat} ({}) matters most.\r\nReroll until you like the numbers.\r\n",
            stat_word(stat)
        ));
    } else {
        out.push_str("Reroll until you like the numbers.\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{ClassDef, HelpEntry, RaceDef};

    fn race(race: &str, plain: &str, name: &str, playable: bool) -> RaceDef {
        RaceDef {
            race: race.to_string(),
            plain_name: plain.to_string(),
            name: name.to_string(),
            playable,
            default_size: "MEDIUM".to_string(),
            race_align: "GOOD".to_string(),
            ..Default::default()
        }
    }

    fn world_with_races(rows: &[(&str, &str, &str, bool)]) -> World {
        let mut world = World::new();
        let mut catalog = RaceCatalog::default();
        for (r, plain, name, playable) in rows {
            catalog
                .by_race
                .insert((*r).to_string(), race(r, plain, name, *playable));
        }
        world.insert_resource(catalog);
        world
    }

    fn sample_races() -> World {
        world_with_races(&[
            ("HUMAN", "Human", "Human", true),
            ("ELF", "Elf", "Elf", true),
            ("HALF_ELF", "Half-Elf", "Half-Elf", true),
            ("HALFLING", "Halfling", "Halfling", true),
            ("DRAGONBORN_ACID", "Acid Dragonborn", "Dragonborn", true),
            ("DRAGONBORN_FIRE", "Fire Dragonborn", "Dragonborn", true),
            ("DEMON", "Demon", "Demon", false),
        ])
    }

    fn pick_race(world: &World, input: &str) -> Pick<String> {
        pick(&race_options(world), input)
    }

    #[test]
    fn race_menu_lists_names_not_enum_ids_and_fits_80_columns() {
        let world = sample_races();
        let menu = render_race_menu(&world);
        assert!(menu.contains("Half-Elf"), "{menu}");
        assert!(menu.contains("Acid Dragonborn"), "{menu}");
        assert!(!menu.contains("DRAGONBORN_ACID"), "{menu}");
        assert!(!menu.contains("Demon"), "non-playable race listed: {menu}");
        for line in menu.lines() {
            assert!(line.chars().count() <= 79, "{} cols: {line}", line.len());
        }
        assert!(menu.ends_with("Race: "), "{menu}");
    }

    #[test]
    fn race_menu_is_numbered_in_name_order() {
        let world = sample_races();
        let menu = render_race_menu(&world);
        let acid = menu.find("Acid Dragonborn").unwrap();
        let human = menu.find("Human").unwrap();
        assert!(acid < human, "{menu}");
        assert!(menu.contains(" 1) Acid Dragonborn"), "{menu}");
    }

    #[test]
    fn race_rows_describe_size_alignment_and_strengths() {
        let mut world = sample_races();
        let mut catalog = world.resource_mut::<RaceCatalog>();
        let elf = catalog.by_race.get_mut("ELF").unwrap();
        elf.stat_caps.intelligence = 88;
        elf.default_size = "SMALL".to_string();
        let menu = render_race_menu(&world);
        let line = menu.lines().find(|l| l.contains(") Elf")).unwrap();
        assert!(line.contains("small, good; strong: INT"), "{line}");
        let human = menu.lines().find(|l| l.contains(") Human")).unwrap();
        assert!(
            human.contains("balanced") || human.contains("strong"),
            "{human}"
        );
    }

    #[test]
    fn race_pick_accepts_name_prefix_and_number() {
        let world = sample_races();
        assert_eq!(pick_race(&world, "human"), Pick::Found("HUMAN".into()));
        assert_eq!(pick_race(&world, "hum"), Pick::Found("HUMAN".into()));
        assert_eq!(
            pick_race(&world, "Half-Elf"),
            Pick::Found("HALF_ELF".into())
        );
        assert_eq!(
            pick_race(&world, "half elf"),
            Pick::Found("HALF_ELF".into())
        );
        assert_eq!(
            pick_race(&world, "HALF_ELF"),
            Pick::Found("HALF_ELF".into())
        );
        assert_eq!(pick_race(&world, "elf"), Pick::Found("ELF".into()));
        assert_eq!(
            pick_race(&world, "1"),
            Pick::Found("DRAGONBORN_ACID".into())
        );
        assert_eq!(
            pick_race(&world, "fire"),
            Pick::Found("DRAGONBORN_FIRE".into())
        );
    }

    #[test]
    fn race_pick_reports_ambiguity_and_unknowns() {
        let world = sample_races();
        match pick_race(&world, "half") {
            Pick::Ambiguous(labels) => assert_eq!(labels, vec!["Half-Elf", "Halfling"]),
            other => panic!("{other:?}"),
        }
        match pick_race(&world, "dragonborn") {
            Pick::Ambiguous(labels) => assert_eq!(labels.len(), 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(pick_race(&world, "human elf"), Pick::None);
        assert_eq!(pick_race(&world, "demon"), Pick::None);
        assert_eq!(pick_race(&world, "99"), Pick::None);
        assert_eq!(pick_race(&world, ""), Pick::None);
    }

    #[test]
    fn ambiguity_message_names_the_candidates() {
        let msg = ambiguity_message(" half ", &["Half-Elf".to_string(), "Halfling".to_string()]);
        assert_eq!(
            msg,
            "'half' could be Half-Elf or Halfling. Type more of the name, or its number.\r\n"
        );
    }

    fn class_world() -> World {
        let mut world = World::new();
        let mut classes = ClassCatalog::default();
        for (id, name, sub, stat) in [
            (1, "Sorcerer", false, Some("INT")),
            (4, "Warrior", false, Some("STR")),
            (7, "Paladin", true, None),
        ] {
            classes.by_id.insert(
                id,
                ClassDef {
                    id,
                    name: name.to_string(),
                    plain_name: name.to_string(),
                    is_subclass: sub,
                    parent_class_id: None,
                    description: None,
                    hit_dice: "1d8".to_string(),
                    primary_stat: stat.map(str::to_string),
                    hp_per_level: 0,
                    exp_gain_factor: 1.0,
                    alignment_bias: 0,
                    campcraft_bonus: false,
                    tags: Vec::new(),
                    combat_rates: mud_db::characters::ClassCombatRates::default(),
                    resistances: std::collections::HashMap::new(),
                },
            );
        }
        world.insert_resource(classes);
        let mut help = HelpCatalog::default();
        help.entries.insert(
            378,
            HelpEntry {
                id: 378,
                title: "Sorcerer".into(),
                content: "Sorcerers are dabblers in the arcane realms.  They reach into various\n\
                          aspects of magic, taking and leaving knowledge as they see fit."
                    .into(),
                min_level: 0,
                category: Some("class".into()),
                usage: None,
                duration: None,
                sphere: None,
                keywords: vec!["sorcerer".into()],
            },
        );
        help.entries.insert(
            441,
            HelpEntry {
                id: 441,
                title: "Warrior".into(),
                content: "Warriors are skilled in many areas of combat, but excel in none and\n\
                          have no special bonuses. Some warriors forget to specialize."
                    .into(),
                min_level: 0,
                category: Some("class".into()),
                usage: None,
                duration: None,
                sphere: None,
                keywords: vec!["warriors".into()],
            },
        );
        world.insert_resource(help);
        world
    }

    #[test]
    fn class_menu_describes_each_base_class_within_80_columns() {
        let world = class_world();
        let menu = render_class_menu(&world);
        assert!(menu.contains("1) Sorcerer  (key stat: INT)"), "{menu}");
        assert!(menu.contains("2) Warrior  (key stat: STR)"), "{menu}");
        assert!(
            menu.contains("Sorcerers are dabblers in the arcane realms."),
            "{menu}"
        );
        assert!(
            menu.contains("Warriors are skilled in many areas of combat"),
            "{menu}"
        );
        assert!(!menu.contains("Paladin"), "subclass listed: {menu}");
        assert!(
            !menu.contains("Some warriors"),
            "only the first sentence: {menu}"
        );
        for line in menu.lines() {
            assert!(line.chars().count() <= 79, "{} cols: {line}", line.len());
        }
        assert!(menu.ends_with("Class: "), "{menu}");
    }

    #[test]
    fn class_pick_takes_name_prefix_or_number() {
        let world = class_world();
        let options = class_options(&world);
        assert_eq!(
            pick(&options, "warrior"),
            Pick::Found((4, "Warrior".into()))
        );
        assert_eq!(pick(&options, "sorc"), Pick::Found((1, "Sorcerer".into())));
        assert_eq!(pick(&options, "1"), Pick::Found((1, "Sorcerer".into())));
        assert_eq!(pick(&options, "mage"), Pick::None);
        assert_eq!(pick(&options, "paladin"), Pick::None);
    }

    #[test]
    fn a_builder_written_class_description_beats_the_help_article() {
        let mut world = class_world();
        world
            .resource_mut::<ClassCatalog>()
            .by_id
            .get_mut(&1)
            .unwrap()
            .description = Some("A caster of arcane spells. Fragile early on.".into());
        assert_eq!(
            class_blurb(&world, 1, "Sorcerer").as_deref(),
            Some("A caster of arcane spells.")
        );
    }

    #[test]
    fn long_blurbs_are_cut_at_a_word_boundary() {
        let long = "word ".repeat(80);
        let cut = first_sentence(&long);
        assert!(cut.chars().count() <= BLURB_MAX_CHARS + 3, "{cut}");
        assert!(cut.ends_with("..."), "{cut}");
    }

    #[test]
    fn stat_explainer_names_the_class_key_stat() {
        let text = stat_explainer("Sorcerer", Some("INT"));
        assert!(text.contains("bonus"), "{text}");
        assert!(text.contains("INT (intelligence) matters most"), "{text}");
        for line in text.lines() {
            assert!(line.chars().count() <= 79, "{line}");
        }
        assert!(!stat_explainer("Rogue", None).contains("matters most"));
    }

    #[test]
    fn class_key_stat_ignores_unknown_labels() {
        let mut world = class_world();
        assert_eq!(class_key_stat(&world, 1).as_deref(), Some("INT"));
        world
            .resource_mut::<ClassCatalog>()
            .by_id
            .get_mut(&1)
            .unwrap()
            .primary_stat = Some("LUCK".into());
        assert_eq!(class_key_stat(&world, 1), None);
    }

    #[test]
    fn relist_words() {
        assert!(wants_relist("?"));
        assert!(wants_relist(" List "));
        assert!(!wants_relist("human"));
    }
}
