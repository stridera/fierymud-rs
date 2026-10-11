//! The "who and what is out there" list commands: `areas`, `levels`,
//! `wizlist`, `followers` and `pets`.
//!
//! * `areas` lists the zones a player can visit, from the loaded `Zones`
//!   rows. The table has no level-range or builder columns, so only the
//!   name is shown; god zones and the internal plumbing zones are left
//!   out. A level range will slot in here once the schema carries one.
//! * `levels` prints the XP table for the player's class, straight from
//!   `LevelDefinition` scaled by `Class.exp_gain_factor`.
//! * `wizlist` lists the staff, read from the character/user roles at
//!   the time it is typed (async, DB-backed).
//! * `followers` / `pets` are read-only views of whoever follows you.
//!   Dismissing a pet is a separate, still-undecided feature.

#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;

use bevy_ecs::prelude::{Entity, With, World};
use mud_db::enums::{MAX_MORTAL_LEVEL, UserRole, effective_rank};
use mud_world::*;

use crate::commands::off_tick;
use crate::commands::*;

inventory::submit! {
    Command {
        names: &["areas"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "areas [<text>]",
            summary: "List the areas of the realm.",
            long: "Lists every area open to players, with its zone number. \
                   Give some text to list only the areas whose name \
                   contains it, e.g. 'areas forest'.",
        },
        run: cmd_areas,
    }
}

inventory::submit! {
    Command {
        names: &["levels"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "levels",
            summary: "Show the experience needed for every level.",
            long: "Prints the experience your class needs to reach each \
                   level, and marks the level you are on. Use 'level' to \
                   see your own progress toward the next one.",
        },
        run: cmd_levels,
    }
}

inventory::submit! {
    AsyncCommand {
        dispatch: |world, player, pool, head, _args| match head {
            "wizlist" => Some(Box::pin(cmd_wizlist(world, player, pool))),
            _ => None,
        },
    }
}

inventory::submit! {
    Command {
        names: &["wizlist"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Info,
        help: Help {
            usage: "wizlist",
            summary: "List the staff of the game.",
            long: "Shows the immortals who run the game, grouped by rank. \
                   A '*' marks staff who are online right now.",
        },
        run: crate::commands::cmd_mail_stub,
    }
}

inventory::submit! {
    Command {
        names: &["followers"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "followers",
            summary: "List everyone following you.",
            long: "Shows the players and creatures currently following \
                   you, where each one is, and how healthy it looks. \
                   'pets' narrows the list to creatures.",
        },
        run: cmd_followers,
    }
}

inventory::submit! {
    Command {
        names: &["pets"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Group,
        help: Help {
            usage: "pets",
            summary: "List the creatures following you.",
            long: "Shows your pets, hirelings and charmed creatures, \
                   where each one is, and how healthy it looks. Use \
                   'call' to bring them to you.",
        },
        run: cmd_pets,
    }
}

// ---------------------------------------------------------------- areas

fn cmd_areas(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim().to_ascii_lowercase();
    let rows = area_rows(world, player);
    let total = rows.len();
    let shown: Vec<&(i32, String)> = rows
        .iter()
        .filter(|(_, name)| {
            needle.is_empty()
                || render_color_tags(name, ColorMode::Strip)
                    .to_ascii_lowercase()
                    .contains(needle.as_str())
        })
        .collect();
    if shown.is_empty() {
        send_to(
            world,
            player,
            format!("No area matches '{}'.\r\n", args.trim()),
        );
        return;
    }
    let mut out = if needle.is_empty() {
        format!("\r\n<b:cyan>Areas of the realm</> ({total}):\r\n")
    } else {
        format!(
            "\r\n<b:cyan>Areas matching '{}'</> ({} of {total}):\r\n",
            args.trim(),
            shown.len()
        )
    };
    for (id, name) in shown {
        out.push_str(&format!("  <dim>[{id:>3}]</> {name}\r\n"));
    }
    out.push_str("\r\n<dim>Type 'areas <text>' to search by name.</>\r\n");
    send_to(world, player, out);
}

/// `(zone id, name)` of every zone `viewer` may be told about: loaded, not an
/// internal plumbing zone, with at least one room, and not a god zone unless
/// the viewer is staff (Immortal+). Sorted by zone id.
fn area_rows(world: &mut World, viewer: Entity) -> Vec<(i32, String)> {
    let sees_god_zones = crate::room_access::is_immortal(world, viewer);
    let zones: Vec<(i32, Entity)> = world
        .resource::<WorldKeyIndex>()
        .zones
        .iter()
        .map(|(id, e)| (*id, *e))
        .collect();
    let mut rooms_per_zone: HashMap<Entity, usize> = HashMap::new();
    for located in world.query_filtered::<&Located, With<Room>>().iter(world) {
        *rooms_per_zone.entry(located.0).or_insert(0) += 1;
    }
    let mut rows: Vec<(i32, String)> = zones
        .into_iter()
        .filter(|(id, e)| {
            !zone_is_internal(*id)
                && (sees_god_zones || world.get::<GodZone>(*e).is_none())
                && rooms_per_zone.get(e).copied().unwrap_or(0) > 0
        })
        .map(|(id, e)| (id, name_of(world, e)))
        .collect();
    rows.sort_by_key(|(id, _)| *id);
    rows
}

// --------------------------------------------------------------- levels

fn cmd_levels(world: &mut World, player: Entity, _args: &str) {
    const COLUMNS: usize = 3;
    let Some((level, class_id, xp)) = world
        .get::<Profile>(player)
        .map(|p| (p.level, p.class_id, p.experience))
    else {
        send_to(world, player, "You have no profile.\r\n");
        return;
    };
    let factor = class_exp_factor(world, class_id);
    let class_name = class_id
        .and_then(|id| world.resource::<ClassCatalog>().by_id.get(&id).cloned())
        .map_or_else(|| "no class".to_string(), |c| c.name);
    let table = world.resource::<LevelTable>();
    let cells: Vec<(i32, i32)> = (2..=MAX_MORTAL_LEVEL)
        .filter_map(|lvl| table.exp_for_class(lvl, factor).map(|exp| (lvl, exp)))
        .collect();
    if cells.is_empty() {
        send_to(world, player, "There is no experience table loaded.\r\n");
        return;
    }
    let next = table.exp_for_class(level + 1, factor);
    // Column-major so a level's neighbours read down the screen, the way
    // the legacy table ran.
    let per_col = cells.len().div_ceil(COLUMNS);
    let mut out =
        format!("\r\n<b:cyan>Experience needed to reach each level</> ({class_name}):\r\n\r\n");
    for row in 0..per_col {
        out.push_str("  ");
        for col in 0..COLUMNS {
            let Some((lvl, exp)) = cells.get(col * per_col + row) else {
                continue;
            };
            let text = format!("[{lvl:>2}] {:>12}", format_thousands(*exp));
            if *lvl == level {
                out.push_str(&format!("<b:yellow>{text}</>"));
            } else {
                out.push_str(&text);
            }
            out.push_str("   ");
        }
        let trimmed = out.trim_end_matches(' ').len();
        out.truncate(trimmed);
        out.push_str("\r\n");
    }
    if mud_db::enums::is_staff_level(level) {
        out.push_str("\r\n<dim>Experience has no meaning for you.</>\r\n");
    } else if let Some(next) = next.filter(|_| level < MAX_MORTAL_LEVEL) {
        out.push_str(&format!(
            "\r\nYou are level {level}, with {} experience; {} more for level {}.\r\n",
            format_thousands(xp),
            format_thousands((next - xp).max(0)),
            level + 1,
        ));
    } else {
        out.push_str(&format!(
            "\r\nYou are level {level}, the highest there is.\r\n"
        ));
    }
    send_to(world, player, out);
}

// -------------------------------------------------------------- wizlist

/// One staff member as the wizlist sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaffRow {
    pub name: String,
    pub level: i32,
    pub role: UserRole,
}

fn role_title(role: UserRole) -> &'static str {
    match role {
        UserRole::Implementor => "Implementors",
        UserRole::Coder => "Coders",
        UserRole::HeadBuilder => "Head Builders",
        UserRole::Builder => "Builders",
        UserRole::Immortal => "Immortals",
        UserRole::Player => "Players",
    }
}

fn parse_role(text: &str) -> UserRole {
    match text {
        "IMMORTAL" => UserRole::Immortal,
        "BUILDER" => UserRole::Builder,
        "HEAD_BUILDER" => UserRole::HeadBuilder,
        "CODER" => UserRole::Coder,
        "IMPLEMENTOR" => UserRole::Implementor,
        _ => UserRole::Player,
    }
}

/// Staff characters: level 100 and up, or an account with a staff role.
/// The effective role is the higher of the account role and the one the
/// character's level implies, as everywhere else.
pub(crate) async fn staff_rows(
    pool: &mud_db::sqlx::PgPool,
) -> Result<Vec<StaffRow>, mud_db::sqlx::Error> {
    use mud_db::sqlx::Row;
    let rows = mud_db::sqlx::query(
        r#"
        SELECT c.name, c.level, u.role::text AS role
        FROM "Characters" c
        LEFT JOIN "Users" u ON u.id = c.user_id
        WHERE c.deleted_at IS NULL
          AND (c.level >= 100 OR u.role <> 'PLAYER'::"UserRole")
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let level: i32 = r.get("level");
            let account_role =
                parse_role(r.get::<Option<String>, _>("role").as_deref().unwrap_or(""));
            StaffRow {
                name: r.get("name"),
                level,
                role: effective_rank(level, account_role),
            }
        })
        .filter(|s| s.role != UserRole::Player)
        .collect())
}

/// Render the wizlist: one block per role, highest rank first, names in
/// a grid sorted by level then name. `online` holds lowercase names of
/// staff the viewer can see online; they get a `*`.
pub(crate) fn render_wizlist(rows: &[StaffRow], online: &[String], width: usize) -> String {
    if rows.is_empty() {
        return "\r\nThe halls of the immortals are empty.\r\n".to_string();
    }
    let mut out = String::from("\r\n<b:cyan>FieryMUD Staff</>\r\n");
    let mut roles: Vec<UserRole> = rows.iter().map(|r| r.role).collect();
    roles.sort_by_key(|r| std::cmp::Reverse(r.rank()));
    roles.dedup();
    let mut any_online = false;
    for role in roles {
        let mut members: Vec<&StaffRow> = rows.iter().filter(|r| r.role == role).collect();
        members.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.name.cmp(&b.name)));
        let cells: Vec<String> = members
            .iter()
            .map(|m| {
                if online.iter().any(|o| o.eq_ignore_ascii_case(&m.name)) {
                    any_online = true;
                    format!("{}<b:green>*</>", m.name)
                } else {
                    m.name.clone()
                }
            })
            .collect();
        out.push_str(&format!("\r\n  <b:yellow>{}</>\r\n", role_title(role)));
        let col = crate::layout::grid_col_width([cells.as_slice()], 2, 16);
        for line in crate::layout::format_grid(&cells, col, 4, width) {
            out.push_str(&line);
            out.push_str("\r\n");
        }
    }
    if any_online {
        out.push_str("\r\n<dim>* = online now</>\r\n");
    }
    out
}

pub(crate) async fn cmd_wizlist(world: &mut World, player: Entity, pool: &mud_db::sqlx::PgPool) {
    let online: Vec<String> = {
        let mut q = world.query_filtered::<(Entity, &Named), (With<Player>, With<Online>)>();
        let seen: Vec<(Entity, String)> = q.iter(world).map(|(e, n)| (e, n.name.clone())).collect();
        seen.into_iter()
            .filter(|(e, _)| can_see_player(world, player, *e))
            .map(|(_, n)| n)
            .collect()
    };
    let width = crate::layout::wrap_width(world, player);
    let pool = pool.clone();
    // Off the tick, and ordered: whatever the player types next waits for
    // the list instead of overtaking it.
    let list = async move {
        match staff_rows(&pool).await {
            Ok(rows) => render_wizlist(&rows, &online, width),
            Err(e) => {
                tracing::warn!(error = %e, "wizlist query failed");
                "The immortals' list can't be read right now.\r\n".to_string()
            }
        }
    };
    off_tick::run(world, player, list, |world, player, text| {
        send_to(world, player, text);
    })
    .await;
}

// ------------------------------------------------------ followers, pets

fn cmd_followers(world: &mut World, player: Entity, _args: &str) {
    list_followers(world, player, false);
}

fn cmd_pets(world: &mut World, player: Entity, _args: &str) {
    list_followers(world, player, true);
}

/// Everything (or, with `mobs_only`, every creature) following `player`.
fn list_followers(world: &mut World, player: Entity, mobs_only: bool) {
    let here = world.get::<Located>(player).map(|l| l.0);
    let mut found: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Follower, Option<&Mob>)>();
        q.iter(world)
            .filter(|(_, f, mob)| f.0 == player && (!mobs_only || mob.is_some()))
            .map(|(e, _, _)| e)
            .collect()
    };
    if found.is_empty() {
        let msg = if mobs_only {
            "You have no pets or hirelings following you.\r\n"
        } else {
            "Nobody is following you.\r\n"
        };
        send_to(world, player, msg);
        return;
    }
    found.sort_by_key(|e| e.index());
    let mut out = if mobs_only {
        String::from("\r\n<b:cyan>Your pets</>:\r\n")
    } else {
        String::from("\r\n<b:cyan>Following you</>:\r\n")
    };
    for e in found {
        let name = cap_sentence_start(&name_of(world, e));
        let pad = 26usize.saturating_sub(crate::layout::text_width(&name));
        let place = match world.get::<Located>(e).map(|l| l.0) {
            Some(room) if Some(room) == here => "here".to_string(),
            Some(room) => name_of(world, room),
            None => "nowhere".to_string(),
        };
        let health = world
            .get::<Health>(e)
            .map_or_else(String::new, |h| format!("  <dim>{}/{} hp</>", h.hp, h.max));
        let tag = if world.get::<Mountable>(e).is_some() {
            " <dim>(mount)</>"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {name}{}{place}{health}{tag}\r\n",
            " ".repeat(pad.max(1))
        ));
    }
    if mobs_only {
        out.push_str("\r\n<dim>Type 'call' to bring them to you.</>\r\n");
    }
    send_to(world, player, out);
}
