//! Staff location and prototype search commands ported from legacy
//! (`act.wizard.cpp` `do_at`, `act.wizinfo.cpp` `do_vstat`, `vsearch.cpp`).
//!
//! | command                         | legacy level      | Rust role |
//! |---------------------------------|-------------------|-----------|
//! | `at`                            | `LVL_ATTENDANT-1` | Builder   |
//! | `vnum` `mnum` `onum` `rnum` `tnum` | `LVL_ATTENDANT` | Builder   |
//! | `vstat`                         | `LVL_ATTENDANT`   | Builder   |
//! | `vsearch` `esearch` `tsearch`   | `LVL_ATTENDANT`   | Builder   |
//!
//! Legacy levels 101 and 102 both map to the `Builder` role
//! (`UserRole::from_level`). Every id printed here is the composite
//! `zone:id`; the same form is accepted back by `goto`, `at` and `vstat`.
//!
//! Skipped from legacy `vsearch`: shop / zone / reset-command / skill
//! searches and the long tail of mob and object value fields (flag sets,
//! per-type values, applies). The commands that remain cover the
//! prototype, trigger and exit tables a builder reaches for most.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{
    Description, Exits, Ghost, Located, MobPrototypes, Named, ObjectPrototypes, Room, RoomSector,
    TriggerAttach, TriggerCatalog, WorldKey,
};

use crate::commands::admin_inspect::{cmd_mstat, cmd_ostat};
use crate::commands::admin_world::resolve_staff_destination;
use crate::commands::{Category, Command, Help, send_rendered, send_to, skip_n_tokens};

/// Most rows one search prints.
const RESULT_LIMIT: usize = 50;

inventory::submit! {
    Command {
        names: &["at"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "at <location> <command>",
            summary: "Run a command as if you were standing somewhere else.",
            long: "Builder+. The location takes the same forms as 'goto': \
                   a room id in your zone, '<zone> <id>' or '<zone>:<id>', \
                   'home', or the name of a player or mob (their room). \
                   You are moved there for the one command and put back \
                   afterwards, with no poof messages. If the command moves \
                   you (goto, a teleport) you stay where it left you; if \
                   you die you stay where you died. Moving ends any fight \
                   you are in, as it did in the legacy 'at'.",
        },
        run: cmd_at,
    }
}

inventory::submit! {
    Command {
        names: &["vnum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "vnum <mobiles|objects|rooms|triggers|exits> <keywords> [in <zone>]",
            summary: "Find prototypes by keyword and print their zone:id.",
            long: VNUM_HELP,
        },
        run: cmd_vnum,
    }
}

inventory::submit! {
    Command {
        names: &["mnum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mnum <keywords> [in <zone>]",
            summary: "Find mob prototypes by keyword (zone:id).",
            long: VNUM_HELP,
        },
        run: cmd_mnum,
    }
}

inventory::submit! {
    Command {
        names: &["onum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "onum <keywords> [in <zone>]",
            summary: "Find object prototypes by keyword (zone:id).",
            long: VNUM_HELP,
        },
        run: cmd_onum,
    }
}

inventory::submit! {
    Command {
        names: &["rnum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rnum <keywords> [in <zone>]",
            summary: "Find rooms by name keyword (zone:id).",
            long: VNUM_HELP,
        },
        run: cmd_rnum,
    }
}

inventory::submit! {
    Command {
        names: &["tnum"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "tnum <keywords> [in <zone>]",
            summary: "Find triggers by name keyword (zone:id).",
            long: VNUM_HELP,
        },
        run: cmd_tnum,
    }
}

inventory::submit! {
    Command {
        names: &["vstat"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "vstat <mob|obj> <zone> <id> | <zone>:<id> | <id>",
            summary: "Show a mob or object prototype's stats without spawning it.",
            long: "Builder+. Prints the same detail as 'mstat' (mob) or \
                   'ostat' (object) for a prototype by id. Nothing is \
                   loaded into the world. The id is '<zone> <id>', \
                   '<zone>:<id>' as 'vnum' prints it, or a bare '<id>' in \
                   your current zone. Unlike mstat / ostat, a name is not \
                   accepted: find ids with 'vnum' first.",
        },
        run: cmd_vstat,
    }
}

inventory::submit! {
    Command {
        names: &["vsearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "vsearch <mobiles|objects|rooms|triggers|exits> <field> <query> [in <zone>]",
            summary: "Search prototypes, triggers or exits by a chosen field.",
            long: VSEARCH_HELP,
        },
        run: cmd_vsearch,
    }
}

inventory::submit! {
    Command {
        names: &["esearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "esearch <field> <query> [in <zone>]",
            summary: "Search room exits and doors by keyword, description, key or target.",
            long: VSEARCH_HELP,
        },
        run: cmd_esearch,
    }
}

inventory::submit! {
    Command {
        names: &["tsearch"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "tsearch <field> <query> [in <zone>]",
            summary: "Search triggers by name, type, argument, script text or attachment.",
            long: VSEARCH_HELP,
        },
        run: cmd_tsearch,
    }
}

const VNUM_HELP: &str = "Builder+. Lists prototypes whose name or keywords \
    start with every word you give, as 'zone:id name', sorted by id and \
    capped at 50 rows. 'vnum' takes a type first (mobiles, objects, rooms, \
    triggers, exits), 'mnum' / 'onum' / 'rnum' / 'tnum' are the same \
    search for one type. Add 'in <zone>' to stay in one zone. Pair with \
    'vstat', 'rstat' or 'tstat', or jump with 'goto <zone>:<id>'.";

const VSEARCH_HELP: &str = "Builder+. Search one table by a field. Run it \
    with only a type (or 'esearch' / 'tsearch' with nothing) to list the \
    fields. Text fields match a substring; 'name' fields match whole-word \
    prefixes of the name and keywords. Number fields take 5, >5, <5, >=5, \
    <=5, !5 or 5..9 (an inclusive range). Add 'in <zone>' at the end to \
    stay in one zone. Shops, zones, reset commands and skills are not \
    searchable here.";

// ---------------------------------------------------------------------------
// at
// ---------------------------------------------------------------------------

/// `at <location> <command>`. The location is the first word, or the first
/// two when both are numbers (`at 30 45 look`), like `goto`.
pub(crate) fn cmd_at(world: &mut World, player: Entity, args: &str) {
    let args = args.trim();
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() {
        send_to(
            world,
            player,
            "You must supply a room number or a name.\r\n",
        );
        return;
    }
    let loc_len =
        if tokens.len() > 2 && tokens[0].parse::<i32>().is_ok() && tokens[1].parse::<i32>().is_ok()
        {
            2
        } else {
            1
        };
    let command = skip_n_tokens(args, loc_len).trim();
    if command.is_empty() {
        send_to(world, player, "What do you want to do there?\r\n");
        return;
    }
    let Some(location) = resolve_staff_destination(world, player, &tokens[..loc_len]) else {
        return;
    };
    let Some(original) = world.get::<Located>(player).map(|l| l.0) else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };

    if original != location {
        mud_world::movement::stop_fighting_both_ways(world, player);
        world.entity_mut(player).insert(Located(location));
    }
    // A panic in the command must not leave the caller in the wrong room:
    // put them back, then let the panic carry on.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        crate::commands::dispatch(world, player, command);
    }));
    if original != location {
        return_from_at(world, player, location, original);
    }
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
}

/// Legacy `do_at` tail: only if the caller is still in the borrowed room
/// do they go back. A command that moved them (goto, teleport, flee) wins,
/// and so does death: a ghost stays where it fell.
fn return_from_at(world: &mut World, player: Entity, location: Entity, original: Entity) {
    if world.get_entity(player).is_err() || world.get_entity(original).is_err() {
        return;
    }
    let still_there = world
        .get::<Located>(player)
        .is_some_and(|l| l.0 == location);
    if still_there && world.get::<Ghost>(player).is_none() {
        mud_world::movement::stop_fighting_both_ways(world, player);
        world.entity_mut(player).insert(Located(original));
    }
}

// ---------------------------------------------------------------------------
// Search model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mob,
    Object,
    Room,
    Trigger,
    Exit,
}

impl Kind {
    const fn noun(self) -> &'static str {
        match self {
            Self::Mob => "mob prototype",
            Self::Object => "object prototype",
            Self::Room => "room",
            Self::Trigger => "trigger",
            Self::Exit => "exit",
        }
    }

    /// Field table: `(names, class)`; the first name is canonical.
    const fn fields(self) -> &'static [(&'static [&'static str], Class)] {
        match self {
            Self::Mob => &[
                (&["name", "alias", "keywords"], Class::Words),
                (&["short"], Class::Text),
                (&["long"], Class::Text),
                (&["desc", "description"], Class::Text),
                (&["race"], Class::Text),
                (&["gender", "sex"], Class::Text),
                (&["role"], Class::Text),
                (&["level"], Class::Num),
                (&["alignment"], Class::Num),
            ],
            Self::Object => &[
                (&["name", "alias", "keywords"], Class::Words),
                (&["short", "shortdesc"], Class::Text),
                (&["long", "longdesc"], Class::Text),
                (&["desc", "description"], Class::Text),
                (&["type"], Class::Text),
                (&["wear", "worn"], Class::Text),
                (&["level"], Class::Num),
                (&["weight"], Class::Num),
                (&["cost"], Class::Num),
            ],
            Self::Room => &[
                (&["name", "title"], Class::Words),
                (&["desc", "description"], Class::Text),
                (&["sector"], Class::Text),
            ],
            Self::Trigger => &[
                (&["name"], Class::Words),
                (&["type", "events"], Class::Text),
                (&["argument", "arg"], Class::Text),
                (&["commands", "script"], Class::Text),
                (&["intention", "attach"], Class::Text),
                (&["numericarg", "numargs"], Class::Num),
            ],
            Self::Exit => &[
                (&["keyword", "name"], Class::Words),
                (&["description", "desc"], Class::Text),
                (&["key"], Class::Num),
                (&["room", "to"], Class::Num),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Whole-word prefixes of a name and keyword list.
    Words,
    /// Case-insensitive substring.
    Text,
    /// Number with a comparison.
    Num,
}

/// Prototype kinds by typed word, abbreviations allowed, in legacy
/// `vsearch_modes` order (`m` is mobiles, `o` objects, `r` rooms, `t`
/// triggers).
fn parse_kind(word: &str) -> Option<Kind> {
    const MODES: &[(&str, Kind)] = &[
        ("mobiles", Kind::Mob),
        ("objects", Kind::Object),
        ("rooms", Kind::Room),
        ("triggers", Kind::Trigger),
        ("exits", Kind::Exit),
        ("doors", Kind::Exit),
    ];
    let w = word.to_ascii_lowercase();
    if w.is_empty() {
        return None;
    }
    MODES
        .iter()
        .find(|(name, _)| name.starts_with(&w))
        .map(|(_, k)| *k)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Cmp {
    Eq,
    Gt,
    Lt,
    Ge,
    Le,
    Ne,
    Between(i64, i64),
}

#[derive(Debug, PartialEq)]
struct NumQuery {
    cmp: Cmp,
    n: i64,
}

impl NumQuery {
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once("..") {
            let (a, b) = (a.parse::<i64>().ok()?, b.parse::<i64>().ok()?);
            return Some(Self {
                cmp: Cmp::Between(a.min(b), a.max(b)),
                n: a,
            });
        }
        let (cmp, rest) = if let Some(r) = s.strip_prefix(">=") {
            (Cmp::Ge, r)
        } else if let Some(r) = s.strip_prefix("<=") {
            (Cmp::Le, r)
        } else if let Some(r) = s.strip_prefix("!=").or_else(|| s.strip_prefix('!')) {
            (Cmp::Ne, r)
        } else if let Some(r) = s.strip_prefix('>') {
            (Cmp::Gt, r)
        } else if let Some(r) = s.strip_prefix('<') {
            (Cmp::Lt, r)
        } else {
            (Cmp::Eq, s)
        };
        Some(Self {
            cmp,
            n: rest.trim().parse().ok()?,
        })
    }

    fn test(&self, v: i64) -> bool {
        match self.cmp {
            Cmp::Eq => v == self.n,
            Cmp::Gt => v > self.n,
            Cmp::Lt => v < self.n,
            Cmp::Ge => v >= self.n,
            Cmp::Le => v <= self.n,
            Cmp::Ne => v != self.n,
            Cmp::Between(lo, hi) => (lo..=hi).contains(&v),
        }
    }
}

/// The compiled query: which field, and what to match it against.
enum Query {
    Words(String),
    Text(String),
    Num(NumQuery),
}

/// A field's value on one candidate.
enum Val {
    /// `(name, keywords)`.
    Words(String, Vec<String>),
    Text(String),
    Num(i64),
    /// The field does not apply (an exit with no key).
    Absent,
}

impl Query {
    fn matches(&self, val: &Val) -> bool {
        match (self, val) {
            (Self::Words(q), Val::Words(name, kws)) => {
                let kw = (!kws.is_empty()).then_some(kws.as_slice());
                mud_world::targeting::entity_matches(q, name, kw)
            }
            (Self::Text(q), Val::Text(t)) => t.to_ascii_lowercase().contains(q),
            (Self::Num(q), Val::Num(n)) => q.test(*n),
            _ => false,
        }
    }
}

/// One search result: `((zone, id), row text)`.
type Hit = ((i32, i32), String);

/// Trailing `in <zone>`, split off the query text.
fn split_zone_filter(tokens: &[&str]) -> Result<(Vec<String>, Option<i32>), String> {
    let n = tokens.len();
    if n >= 2 && tokens[n - 2].eq_ignore_ascii_case("in") {
        let zone = tokens[n - 1]
            .parse::<i32>()
            .map_err(|_| "The zone after 'in' must be a number.\r\n".to_string())?;
        return Ok((
            tokens[..n - 2].iter().map(|s| (*s).to_string()).collect(),
            Some(zone),
        ));
    }
    Ok((tokens.iter().map(|s| (*s).to_string()).collect(), None))
}

fn field_list(kind: Kind) -> String {
    let names: Vec<&str> = kind.fields().iter().map(|(n, _)| n[0]).collect();
    format!(
        "Allowed {} search fields: {}\r\n",
        kind.noun(),
        names.join(", ")
    )
}

// ---------------------------------------------------------------------------
// Collecting candidates
// ---------------------------------------------------------------------------

fn visible(world: &World, viewer: Entity, zone: i32, filter: Option<i32>) -> bool {
    filter.is_none_or(|z| z == zone) && crate::room_access::zone_visible_to(world, viewer, zone)
}

#[allow(clippy::too_many_lines)]
fn collect_hits(
    world: &mut World,
    viewer: Entity,
    kind: Kind,
    field: &str,
    query: &Query,
    zone_filter: Option<i32>,
) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    match kind {
        Kind::Mob => {
            for ((z, id), p) in &world.resource::<MobPrototypes>().by_key {
                if !visible(world, viewer, *z, zone_filter) {
                    continue;
                }
                let val = match field {
                    "name" => Val::Words(p.name.clone(), p.keywords.clone()),
                    "short" => Val::Text(p.name.clone()),
                    "long" => Val::Text(p.room_description.clone()),
                    "desc" => Val::Text(p.examine_description.clone()),
                    "race" => Val::Text(p.race.clone()),
                    "gender" => Val::Text(p.gender.clone()),
                    "role" => Val::Text(p.role.label().to_string()),
                    "level" => Val::Num(i64::from(p.level)),
                    "alignment" => Val::Num(i64::from(p.alignment)),
                    _ => Val::Absent,
                };
                if query.matches(&val) {
                    hits.push(((*z, *id), format!("{} (L{} {})", p.name, p.level, p.race)));
                }
            }
        }
        Kind::Object => {
            for ((z, id), p) in &world.resource::<ObjectPrototypes>().by_key {
                if !visible(world, viewer, *z, zone_filter) {
                    continue;
                }
                let val = match field {
                    "name" => Val::Words(p.name.clone(), p.keywords.clone()),
                    "short" => Val::Text(p.name.clone()),
                    "long" => Val::Text(p.room_description.clone()),
                    "desc" => Val::Text(p.examine_description.clone().unwrap_or_default()),
                    "type" => Val::Text(p.r#type.label().to_string()),
                    "wear" => Val::Text(
                        p.wear_flags
                            .iter()
                            .map(|w| format!("{w:?}"))
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    "level" => Val::Num(i64::from(p.level)),
                    // Whole weight units, like the stat output.
                    #[allow(clippy::cast_possible_truncation)]
                    "weight" => Val::Num(p.weight.round() as i64),
                    "cost" => Val::Num(i64::from(p.cost)),
                    _ => Val::Absent,
                };
                if query.matches(&val) {
                    hits.push((
                        (*z, *id),
                        format!("{} (L{} {})", p.name, p.level, p.r#type.label()),
                    ));
                }
            }
        }
        Kind::Room => {
            let mut q = world.query_filtered::<(
                &WorldKey,
                &Named,
                Option<&Description>,
                Option<&RoomSector>,
            ), With<Room>>();
            let rows: Vec<(WorldKey, String, String, String)> = q
                .iter(world)
                .map(|(k, n, d, s)| {
                    (
                        *k,
                        n.name.clone(),
                        d.map(|d| d.0.clone()).unwrap_or_default(),
                        s.map(|s| format!("{:?}", s.0)).unwrap_or_default(),
                    )
                })
                .collect();
            for (k, name, desc, sector) in rows {
                if !visible(world, viewer, k.zone, zone_filter) {
                    continue;
                }
                let val = match field {
                    "name" => Val::Words(name.clone(), Vec::new()),
                    "desc" => Val::Text(desc),
                    "sector" => Val::Text(sector),
                    _ => Val::Absent,
                };
                if query.matches(&val) {
                    hits.push(((k.zone, k.id), name));
                }
            }
        }
        Kind::Trigger => {
            for ((z, id), t) in &world.resource::<TriggerCatalog>().by_key {
                if !visible(world, viewer, *z, zone_filter) {
                    continue;
                }
                let val = match field {
                    "name" => Val::Words(t.name.clone(), Vec::new()),
                    "type" => Val::Text(
                        t.flags
                            .iter()
                            .map(|f| format!("{f:?}"))
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    "argument" => Val::Text(t.arg_list.join(" ")),
                    "commands" => Val::Text(t.commands.clone()),
                    "intention" => Val::Text(attach_label(t.attach_type).to_string()),
                    "numericarg" => Val::Num(i64::from(t.num_args)),
                    _ => Val::Absent,
                };
                if query.matches(&val) {
                    hits.push((
                        (*z, *id),
                        format!("{} [{}]", t.name, attach_label(t.attach_type)),
                    ));
                }
            }
        }
        Kind::Exit => collect_exits(world, viewer, field, query, zone_filter, &mut hits),
    }
    hits.sort();
    hits
}

fn attach_label(a: TriggerAttach) -> &'static str {
    match a {
        TriggerAttach::Mob => "mobile",
        TriggerAttach::Object => "object",
        TriggerAttach::World => "room",
    }
}

fn collect_exits(
    world: &mut World,
    viewer: Entity,
    field: &str,
    query: &Query,
    zone_filter: Option<i32>,
    hits: &mut Vec<Hit>,
) {
    let mut q = world.query_filtered::<(&WorldKey, &Named, &Exits), With<Room>>();
    let rooms: Vec<(WorldKey, String, Exits)> = q
        .iter(world)
        .map(|(k, n, e)| (*k, n.name.clone(), e.clone()))
        .collect();
    for (k, room_name, exits) in rooms {
        if !visible(world, viewer, k.zone, zone_filter) {
            continue;
        }
        for (dir, ex) in &exits.0 {
            let to_key = ex
                .to
                .and_then(|e| world.get::<WorldKey>(e).map(|w| (w.zone, w.id)));
            let val = match field {
                "keyword" => Val::Words(String::new(), ex.keywords.clone()),
                "description" => Val::Text(ex.description.clone().unwrap_or_default()),
                "key" => ex
                    .key
                    .map_or(Val::Absent, |(_, kid)| Val::Num(i64::from(kid))),
                "room" => to_key.map_or(Val::Absent, |(_, tid)| Val::Num(i64::from(tid))),
                _ => Val::Absent,
            };
            if !query.matches(&val) {
                continue;
            }
            let dir_name = format!("{dir:?}").to_ascii_lowercase();
            let to = to_key.map_or_else(|| "nowhere".to_string(), |(z, i)| format!("{z}:{i}"));
            let kw = if ex.keywords.is_empty() {
                String::new()
            } else {
                format!(" '{}'", ex.keywords.join(" "))
            };
            hits.push((
                (k.zone, k.id),
                format!("{room_name}: {dir_name}{kw} -> {to}"),
            ));
        }
    }
}

fn send_hits(world: &World, player: Entity, kind: Kind, what: &str, hits: &[Hit]) {
    if hits.is_empty() {
        send_to(
            world,
            player,
            format!("No {}s match {what}.\r\n", kind.noun()),
        );
        return;
    }
    let total = hits.len();
    let shown = total.min(RESULT_LIMIT);
    let mut out = format!(
        "\r\n<b:cyan>{shown} of {total} {} match(es) for {what}:</>\r\n",
        kind.noun()
    );
    for (i, ((z, id), text)) in hits.iter().take(RESULT_LIMIT).enumerate() {
        out.push_str(&format!(
            "{:>4}. <dim>[{z:>3}:{id:<4}]</> {text}\r\n",
            i + 1
        ));
    }
    if total > RESULT_LIMIT {
        out.push_str(&format!(
            "  <dim>... {} more: narrow the search or add 'in <zone>'.</>\r\n",
            total - RESULT_LIMIT
        ));
    }
    send_rendered(world, player, &out);
}

// ---------------------------------------------------------------------------
// vnum / mnum / onum / rnum / tnum
// ---------------------------------------------------------------------------

fn run_vnum(world: &mut World, player: Entity, kind: Kind, args: &str, cmd: &str) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let (words, zone) = match split_zone_filter(&tokens) {
        Ok(p) => p,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    if words.is_empty() {
        send_to(
            world,
            player,
            format!("Usage: {cmd} <keywords> [in <zone>]\r\n"),
        );
        return;
    }
    let needle = words.join(" ").to_ascii_lowercase();
    let hits = collect_hits(
        world,
        player,
        kind,
        "name",
        &Query::Words(needle.clone()),
        zone,
    );
    send_hits(world, player, kind, &format!("'{needle}'"), &hits);
}

pub(crate) fn cmd_vnum(world: &mut World, player: Entity, args: &str) {
    let mut it = args.split_whitespace();
    let Some(type_word) = it.next() else {
        send_to(
            world,
            player,
            "Usage: vnum <mobiles|objects|rooms|triggers|exits> <keywords> [in <zone>]\r\n",
        );
        return;
    };
    let Some(kind) = parse_kind(type_word) else {
        send_to(
            world,
            player,
            format!("Unrecognized vnum type: {type_word}\r\n"),
        );
        return;
    };
    let rest = skip_n_tokens(args, 1);
    run_vnum(world, player, kind, rest, "vnum <type>");
}

pub(crate) fn cmd_mnum(world: &mut World, player: Entity, args: &str) {
    run_vnum(world, player, Kind::Mob, args, "mnum");
}

pub(crate) fn cmd_onum(world: &mut World, player: Entity, args: &str) {
    run_vnum(world, player, Kind::Object, args, "onum");
}

pub(crate) fn cmd_rnum(world: &mut World, player: Entity, args: &str) {
    run_vnum(world, player, Kind::Room, args, "rnum");
}

pub(crate) fn cmd_tnum(world: &mut World, player: Entity, args: &str) {
    run_vnum(world, player, Kind::Trigger, args, "tnum");
}

// ---------------------------------------------------------------------------
// vsearch / esearch / tsearch
// ---------------------------------------------------------------------------

/// Parse `<field> <query...> [in <zone>]` for `kind` and print the hits.
fn run_search(world: &mut World, player: Entity, kind: Kind, args: &str) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let Some(field_word) = tokens.first() else {
        send_to(world, player, field_list(kind));
        return;
    };
    let fw = field_word.to_ascii_lowercase();
    let Some((names, class)) = kind
        .fields()
        .iter()
        .find(|(names, _)| names.contains(&fw.as_str()))
        .or_else(|| {
            // Abbreviations, legacy style: first field the word prefixes.
            kind.fields()
                .iter()
                .find(|(names, _)| names.iter().any(|n| n.starts_with(&fw)))
        })
    else {
        send_to(
            world,
            player,
            format!(
                "Unrecognized search field: {field_word}\r\n{}",
                field_list(kind)
            ),
        );
        return;
    };
    let canonical = names[0];
    let (words, zone) = match split_zone_filter(&tokens[1..]) {
        Ok(p) => p,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    if words.is_empty() {
        send_to(world, player, format!("Search {canonical} for what?\r\n"));
        return;
    }
    let text = words.join(" ");
    let query = match class {
        Class::Words => Query::Words(text.to_ascii_lowercase()),
        Class::Text => Query::Text(text.to_ascii_lowercase()),
        Class::Num => {
            let Some(n) = NumQuery::parse(&text) else {
                send_to(
                    world,
                    player,
                    "That field takes a number: 5, >5, <5, >=5, <=5, !5 or 5..9.\r\n",
                );
                return;
            };
            Query::Num(n)
        }
    };
    let hits = collect_hits(world, player, kind, canonical, &query, zone);
    send_hits(world, player, kind, &format!("{canonical} '{text}'"), &hits);
}

pub(crate) fn cmd_vsearch(world: &mut World, player: Entity, args: &str) {
    let Some(type_word) = args.split_whitespace().next() else {
        send_to(
            world,
            player,
            "Usage: vsearch <mobiles|objects|rooms|triggers|exits> <field> <query> [in <zone>]\r\n",
        );
        return;
    };
    let Some(kind) = parse_kind(type_word) else {
        send_to(
            world,
            player,
            format!("Unrecognized vsearch mode: {type_word}\r\n"),
        );
        return;
    };
    run_search(world, player, kind, skip_n_tokens(args, 1));
}

pub(crate) fn cmd_esearch(world: &mut World, player: Entity, args: &str) {
    run_search(world, player, Kind::Exit, args);
}

pub(crate) fn cmd_tsearch(world: &mut World, player: Entity, args: &str) {
    run_search(world, player, Kind::Trigger, args);
}

// ---------------------------------------------------------------------------
// vstat
// ---------------------------------------------------------------------------

pub(crate) fn cmd_vstat(world: &mut World, player: Entity, args: &str) {
    const USAGE: &str = "Usage: vstat { obj | mob } <zone> <id> | <zone>:<id> | <id>\r\n";
    let mut tokens = args.split_whitespace();
    let (Some(type_word), Some(first)) = (tokens.next(), tokens.next()) else {
        send_to(world, player, USAGE);
        return;
    };
    let second = tokens.next();
    if tokens.next().is_some() {
        send_to(world, player, USAGE);
        return;
    }
    // Ids only: `zone:id`, `zone id` or `id`.
    let id_args = match (first.split_once(':'), second) {
        (Some((z, i)), None) => format!("{z} {i}"),
        (None, Some(id)) => format!("{first} {id}"),
        (None, None) => first.to_string(),
        _ => {
            send_to(world, player, USAGE);
            return;
        }
    };
    if !id_args
        .split_whitespace()
        .all(|t| t.chars().all(|c| c.is_ascii_digit()))
    {
        send_to(world, player, USAGE);
        return;
    }
    let tw = type_word.to_ascii_lowercase();
    if "mobile".starts_with(&tw) {
        cmd_mstat(world, player, &id_args);
    } else if "object".starts_with(&tw) {
        cmd_ostat(world, player, &id_args);
    } else {
        send_to(
            world,
            player,
            "That'll have to be either 'obj' or 'mob'.\r\n",
        );
    }
}

#[cfg(test)]
#[path = "staff_find_tests.rs"]
mod tests;
