//! Staff catalog listings (issue #65): `olist`, `mlist`, `rlist`.
//!
//! Legacy `vlist` modes (`vsearch.cpp`: `do_osearch` / `do_msearch` /
//! `do_rsearch` with `SCMD_VLIST`, bound to `olist` / `mlist` / `rlist` at
//! `LVL_ATTENDANT`) list every object / mobile prototype / room in a vnum
//! range, defaulting to the caller's zone. Ids here are composite
//! `(zone, id)`, so the range is a zone plus an optional id span:
//!
//! ```text
//! mlist                      your current zone
//! mlist *                    every visible zone
//! mlist 30                   zone 30
//! mlist 30 100 199           ids 100-199 in zone 30
//! mlist 30:100 30:199        same, `zone:id` form
//! mlist 30 from 100          id 100 onward
//! mlist 30 page 2            page 2 of a long listing
//! ```

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Located, MobPrototypes, Named, ObjectPrototypes, Room, WorldKey};

use crate::commands::{Category, Command, Help, pad_visible, send_rendered, send_to};

/// Rows per page of a listing (a zone can hold hundreds of prototypes).
const PAGE_SIZE: usize = 50;

inventory::submit! {
    Command {
        names: &["olist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "olist [*|<zone> [[from] <id> [to] [<id>]]] [page <n>]",
            summary: "List object prototypes in a zone or id range.",
            long: LIST_HELP_OBJECTS,
        },
        run: cmd_olist,
    }
}

inventory::submit! {
    Command {
        names: &["mlist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "mlist [*|<zone> [[from] <id> [to] [<id>]]] [page <n>]",
            summary: "List mob prototypes in a zone or id range.",
            long: LIST_HELP_MOBS,
        },
        run: cmd_mlist,
    }
}

inventory::submit! {
    Command {
        names: &["rlist"],
        min_role: UserRole::Builder,
        required_perm: None,
        category: Category::Admin,
        help: Help {
            usage: "rlist [*|<zone> [[from] <id> [to] [<id>]]] [page <n>]",
            summary: "List rooms in a zone or id range.",
            long: LIST_HELP_ROOMS,
        },
        run: cmd_rlist,
    }
}

const LIST_HELP_OBJECTS: &str = "Builder+. Lists object prototypes from the \
    loaded catalog as '[zone:id] name', sorted by (zone, id). With no \
    argument it lists your current zone; '*' lists every zone; '<zone>' \
    lists that zone; '<zone> <first> <last>' (or '<zone>:<first> \
    <zone>:<last>') narrows to an id span, and a missing <last> means \
    'to the end of the zone'. Long lists are paged 50 rows at a time: add \
    'page <n>'. Pair with 'ostat <zone> <id>' for a prototype's detail, \
    or 'osearch' to search by name.";

const LIST_HELP_MOBS: &str = "Builder+. Lists mob prototypes from the \
    loaded catalog as '[zone:id] name', sorted by (zone, id). With no \
    argument it lists your current zone; '*' lists every zone; '<zone>' \
    lists that zone; '<zone> <first> <last>' (or '<zone>:<first> \
    <zone>:<last>') narrows to an id span, and a missing <last> means \
    'to the end of the zone'. Long lists are paged 50 rows at a time: add \
    'page <n>'. Pair with 'mstat <zone> <id>' for a prototype's detail, \
    or 'msearch' to search by name.";

const LIST_HELP_ROOMS: &str = "Builder+. Lists loaded rooms as '[zone:id] \
    name', sorted by (zone, id). With no argument it lists your current \
    zone; '*' lists every zone; '<zone>' lists that zone; '<zone> <first> \
    <last>' (or '<zone>:<first> <zone>:<last>') narrows to an id span, and \
    a missing <last> means 'to the end of the zone'. Long lists are paged \
    50 rows at a time: add 'page <n>'. Pair with 'rstat' for a room's \
    detail, or 'rsearch' to search by name.";

/// Which ids a listing covers.
#[derive(Debug, PartialEq, Eq)]
struct ListRange {
    /// `None` = every visible zone.
    zone: Option<i32>,
    lo: i32,
    hi: i32,
    /// 1-based page number.
    page: usize,
}

/// One end of a range: `zone:id` or a bare number.
fn parse_bound(token: &str) -> Option<(Option<i32>, i32)> {
    if let Some((z, i)) = token.split_once(':') {
        return Some((Some(z.parse().ok()?), i.parse().ok()?));
    }
    Some((None, token.parse().ok()?))
}

/// Parse the argument text of a listing command. `Err` carries the full
/// message for the caller.
fn parse_range(world: &World, player: Entity, args: &str) -> Result<ListRange, String> {
    const USAGE: &str = "Usage: <list> [*|<zone> [[from] <id> [to] [<id>]]] [page <n>]  \
                         (also <zone>:<id> <zone>:<id>)\r\n";
    let mut tokens: Vec<&str> = args
        .split_whitespace()
        .filter(|t| !t.eq_ignore_ascii_case("from") && !t.eq_ignore_ascii_case("to"))
        .collect();

    let mut page = 1;
    if tokens.len() >= 2 && tokens[tokens.len() - 2].eq_ignore_ascii_case("page") {
        page = tokens[tokens.len() - 1]
            .parse::<usize>()
            .ok()
            .filter(|p| *p >= 1)
            .ok_or_else(|| "The page must be a number, 1 or more.\r\n".to_string())?;
        tokens.truncate(tokens.len() - 2);
    }

    let full = |zone: Option<i32>| ListRange {
        zone,
        lo: i32::MIN,
        hi: i32::MAX,
        page,
    };
    let bad = || USAGE.to_string();

    match tokens.as_slice() {
        [] => {
            let zone = world
                .get::<Located>(player)
                .and_then(|l| world.get::<WorldKey>(l.0))
                .map(|k| k.zone)
                .ok_or_else(|| "You are nowhere; give a zone.\r\n".to_string())?;
            Ok(full(Some(zone)))
        }
        ["*"] => Ok(full(None)),
        [first, rest @ ..] => {
            // Both `30 100 199` and `30:100 30:199` reduce to zone 30
            // plus up to two id ends.
            let (first_zone, first_num) = parse_bound(first).ok_or_else(bad)?;
            let (zone, mut ends) = match first_zone {
                Some(z) => (z, vec![first_num]),
                None => (first_num, Vec::new()),
            };
            for token in rest {
                let (token_zone, id) = parse_bound(token).ok_or_else(bad)?;
                if token_zone.is_some_and(|z| z != zone) {
                    return Err("A range must stay inside one zone.\r\n".to_string());
                }
                ends.push(id);
            }
            match ends.as_slice() {
                [] => Ok(full(Some(zone))),
                [lo] => Ok(ListRange {
                    zone: Some(zone),
                    lo: *lo,
                    hi: i32::MAX,
                    page,
                }),
                [a, b] => Ok(ListRange {
                    zone: Some(zone),
                    lo: *a.min(b),
                    hi: *a.max(b),
                    page,
                }),
                _ => Err(bad()),
            }
        }
    }
}

impl ListRange {
    fn contains(&self, world: &World, viewer: Entity, zone: i32, id: i32) -> bool {
        crate::room_access::zone_visible_to(world, viewer, zone)
            && self.zone.is_none_or(|z| z == zone)
            && (self.lo..=self.hi).contains(&id)
    }

    /// `"zone 30"`, `"zone 30, ids 100-199"`, `"every zone"`.
    fn describe(&self) -> String {
        match self.zone {
            None => "every zone".to_string(),
            Some(z) if self.lo == i32::MIN && self.hi == i32::MAX => format!("zone {z}"),
            Some(z) if self.hi == i32::MAX => format!("zone {z}, ids {}+", self.lo),
            Some(z) => format!("zone {z}, ids {}-{}", self.lo, self.hi),
        }
    }
}

/// Sort, page and send a listing. `rows` are `((zone, id), text)`.
fn send_listing(
    world: &World,
    player: Entity,
    range: &ListRange,
    kind: &str,
    mut rows: Vec<((i32, i32), String)>,
) {
    if rows.is_empty() {
        send_to(
            world,
            player,
            format!("No {kind} found in {}.\r\n", range.describe()),
        );
        return;
    }
    rows.sort_by_key(|(key, _)| *key);
    let total = rows.len();
    let pages = total.div_ceil(PAGE_SIZE);
    if range.page > pages {
        send_to(
            world,
            player,
            format!(
                "There are only {pages} page(s) of {kind} in {}.\r\n",
                range.describe()
            ),
        );
        return;
    }
    let start = (range.page - 1) * PAGE_SIZE;
    let shown = &rows[start..total.min(start + PAGE_SIZE)];
    let mut out = format!(
        "\r\n<b:cyan>{total} {kind} in {}</> (page {} of {pages}):\r\n",
        range.describe(),
        range.page
    );
    for (i, ((zone, id), text)) in shown.iter().enumerate() {
        out.push_str(&format!(
            "{:>4}. <dim>[{zone:>3}:{id:<4}]</> {text}\r\n",
            start + i + 1
        ));
    }
    if range.page < pages {
        out.push_str(&format!(
            "  <dim>... more: add 'page {}'.</>\r\n",
            range.page + 1
        ));
    }
    send_rendered(world, player, &out);
}

pub(crate) fn cmd_olist(world: &mut World, player: Entity, args: &str) {
    let range = match parse_range(world, player, args) {
        Ok(r) => r,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    let rows: Vec<((i32, i32), String)> = world
        .resource::<ObjectPrototypes>()
        .by_key
        .iter()
        .filter(|((z, id), _)| range.contains(world, player, *z, *id))
        .map(|(key, p)| {
            (
                *key,
                format!(
                    "{} L{:<3} {}",
                    pad_visible(&p.name, 39),
                    p.level,
                    p.r#type.label()
                ),
            )
        })
        .collect();
    send_listing(world, player, &range, "object prototype(s)", rows);
}

pub(crate) fn cmd_mlist(world: &mut World, player: Entity, args: &str) {
    let range = match parse_range(world, player, args) {
        Ok(r) => r,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    let rows: Vec<((i32, i32), String)> = world
        .resource::<MobPrototypes>()
        .by_key
        .iter()
        .filter(|((z, id), _)| range.contains(world, player, *z, *id))
        .map(|(key, p)| {
            (
                *key,
                format!("{} L{:<3} {}", pad_visible(&p.name, 39), p.level, p.race),
            )
        })
        .collect();
    send_listing(world, player, &range, "mob prototype(s)", rows);
}

pub(crate) fn cmd_rlist(world: &mut World, player: Entity, args: &str) {
    let range = match parse_range(world, player, args) {
        Ok(r) => r,
        Err(msg) => {
            send_to(world, player, msg);
            return;
        }
    };
    let rows: Vec<((i32, i32), String)> = {
        let mut q = world.query_filtered::<(&WorldKey, &Named), With<Room>>();
        q.iter(world)
            .filter(|(k, _)| range.contains(world, player, k.zone, k.id))
            .map(|(k, n)| ((k.zone, k.id), n.name.clone()))
            .collect()
    };
    send_listing(world, player, &range, "room(s)", rows);
}
