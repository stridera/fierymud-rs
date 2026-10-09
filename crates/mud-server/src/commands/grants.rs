//! `grant` / `revoke` / `ungrant` — per-character command grants (legacy
//! `privileges.cpp`, `do_grant`).
//!
//! A *grant* lets one character use a command above their rank; a *revoke*
//! takes away a command they would otherwise have. The lists live on the
//! [`CommandGrants`] component (persisted as `Characters.command_grants`)
//! and are honoured by `command_permitted` and abbreviation resolution.
//!
//! Legacy rules kept:
//! - the target must be online and strictly lower level than the grantor
//!   (you cannot touch your own, an equal's or a superior's commands);
//! - a grantor can only grant or revoke a command they can use themselves;
//! - each entry records the grantor and a level, and an entry placed at a
//!   higher level than yours cannot be changed by you.
//!
//! Tightened (this is authorization code):
//! - the entry level cannot exceed the grantor's own level;
//! - the delegation commands themselves cannot be granted;
//! - a mortal can only be granted commands up to immortal rank with no
//!   permission requirement ([`MORTAL_GRANT_CAP`]).
//!
//! Not ported: command groups and privilege flags (`grant <n> group|flag`).
//! This server has no command-group table or privilege-flag set.

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, CommandGrants, GrantEntry, Online, PendingSave, Player, Profile};

use crate::commands::{
    Abbrev, Category, Command, Help, all_commands, record_admin_action, resolve_abbrev, send_to,
    try_insert, visible_with,
};

/// Highest command rank a mortal (no staff role) may be granted. Legacy
/// had no explicit cap beyond "the grantor can use it"; granting a mortal
/// builder or coder commands is an escalation path this port refuses.
pub(crate) const MORTAL_GRANT_CAP: UserRole = UserRole::Immortal;

/// Commands that hand out or take back access. Granting these would let a
/// grantee mint further grants, so they are never grantable.
const DELEGATION_COMMANDS: &[&str] = &["grant", "revoke", "ungrant"];

const NO_PERSON: &str = "There is no one by that name here.\r\n";

const USAGE: &str = "Usage: grant <name> command <command> [ level ]\r\n\
                     \x20      revoke <name> command <command> [ level ]\r\n\
                     \x20      ungrant <name> command <command>\r\n\
                     \x20      grant <name> [ clear | list ]\r\n";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Grant,
    Revoke,
    Ungrant,
}

impl Action {
    fn audit_verb(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke => "revoke",
            Self::Ungrant => "ungrant",
        }
    }
}

const GRANT_LONG: &str = "Coder+. Per-character command access. A grant lets a character use one \
command above their rank; a revoke takes a command away. Only online players strictly below \
your level, and only commands you can use yourself. 'grant <name>' lists a character's grants \
and revokes; 'grant <name> clear' drops the ones at or below your level. A mortal can be \
granted commands up to immortal rank only. Command groups and privilege flags do not exist \
on this server.";

macro_rules! grant_command {
    ($name:literal, $usage:literal, $summary:literal, $run:ident) => {
        inventory::submit! {
            Command {
                names: &[$name],
                min_role: UserRole::Coder,
                required_perm: None,
                category: Category::Admin,
                help: Help {
                    usage: $usage,
                    summary: $summary,
                    long: GRANT_LONG,
                },
                run: $run,
            }
        }
    };
}

grant_command!(
    "grant",
    "grant <name> [command <command> [level] | list | clear]",
    "Let a character use a command above their rank.",
    cmd_grant
);
grant_command!(
    "revoke",
    "revoke <name> command <command> [level]",
    "Take a command away from a character.",
    cmd_revoke
);
grant_command!(
    "ungrant",
    "ungrant <name> command <command>",
    "Remove a grant or revoke from a character.",
    cmd_ungrant
);

fn cmd_grant(world: &mut World, player: Entity, args: &str) {
    run(world, player, args, Action::Grant);
}

fn cmd_revoke(world: &mut World, player: Entity, args: &str) {
    run(world, player, args, Action::Revoke);
}

fn cmd_ungrant(world: &mut World, player: Entity, args: &str) {
    run(world, player, args, Action::Ungrant);
}

/// Effective level for the grant ordering: the character level, raised to
/// the floor of their staff role so an account-promoted staffer whose
/// character is still low level outranks the people they manage.
pub(crate) fn authority_level(world: &World, e: Entity) -> i32 {
    let level = world.get::<Profile>(e).map_or(0, |p| p.level);
    let floor = match world.get::<Account>(e).map(|a| a.role) {
        Some(UserRole::Implementor) => 105,
        Some(UserRole::Coder) => 104,
        Some(UserRole::HeadBuilder) => 103,
        Some(UserRole::Builder) => 101,
        Some(UserRole::Immortal) => 100,
        _ => 0,
    };
    level.max(floor)
}

fn run(world: &mut World, player: Entity, args: &str, action: Action) {
    let mut words = args.split_whitespace();
    let Some(target_name) = words.next() else {
        send_to(world, player, USAGE);
        return;
    };
    let Some(target) = super::admin_world::find_actor_anywhere(world, player, target_name) else {
        send_to(world, player, NO_PERSON);
        return;
    };
    if world.get::<Player>(target).is_none()
        || world.get::<Online>(target).is_none()
        || world.get::<Account>(target).is_none()
    {
        send_to(
            world,
            player,
            "Only players can have their commands changed.\r\n",
        );
        return;
    }
    let sub = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    let (mine, theirs) = (
        authority_level(world, player),
        authority_level(world, target),
    );
    let target_display = crate::commands::name_of(world, target);

    if action == Action::Grant && mine >= theirs && (sub.is_empty() || abbrev(sub, "list")) {
        list_grants(world, player, target, &target_display);
    } else if target != player && mine <= theirs {
        send_to(
            world,
            player,
            format!("You cannot grant or revoke {target_display}'s commands.\r\n"),
        );
    } else if abbrev(sub, "command") {
        record_admin_action(world, player, action.audit_verb(), args);
        command_change(world, player, target, &target_display, action, &rest);
    } else if abbrev(sub, "group") || abbrev(sub, "flag") {
        send_to(
            world,
            player,
            "Command groups and privilege flags are not used on this server. \
             Grant commands one at a time.\r\n",
        );
    } else if action == Action::Revoke {
        send_to(world, player, USAGE);
    } else if abbrev(sub, "clear") {
        record_admin_action(world, player, action.audit_verb(), args);
        clear_grants(world, player, target, mine);
    } else {
        send_to(world, player, USAGE);
    }
}

/// Legacy `is_abbrev`: a non-empty prefix of `full`, case-insensitive.
fn abbrev(typed: &str, full: &str) -> bool {
    !typed.is_empty()
        && full
            .get(..typed.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(typed))
}

fn render_list(entries: &[GrantEntry]) -> String {
    use std::fmt::Write;
    entries.iter().fold(String::new(), |mut out, g| {
        let _ = write!(
            out,
            "  CMD {:>20} {} ({})\r\n",
            g.command, g.grantor, g.level
        );
        out
    })
}

fn list_grants(world: &World, player: Entity, target: Entity, target_name: &str) {
    let empty = CommandGrants::default();
    let grants = world.get::<CommandGrants>(target).unwrap_or(&empty);
    let mut out = format!("{target_name}'s grants:\r\n");
    if grants.grants.is_empty() {
        out.push_str("  None!\r\n");
    } else {
        out.push_str(&render_list(&grants.grants));
    }
    out.push_str(&format!("{target_name}'s revocations:\r\n"));
    if grants.revokes.is_empty() {
        out.push_str("  None!\r\n");
    } else {
        out.push_str(&render_list(&grants.revokes));
    }
    send_to(world, player, out);
}

/// Resolve a typed command word: an exact name or alias, else the legacy
/// abbreviation among the commands `player` can use.
fn find_command(world: &World, player: Entity, typed: &str) -> Option<&'static Command> {
    let typed = typed.to_ascii_lowercase();
    if let Some(c) = all_commands().find(|c| c.names.contains(&typed.as_str())) {
        return Some(c);
    }
    let acct = world.get::<Account>(player)?;
    match resolve_abbrev(
        &typed,
        acct.role,
        &acct.perms,
        world.get::<CommandGrants>(player),
        None,
    )? {
        Abbrev::Command(c) => Some(c),
        Abbrev::Social(_) => None,
    }
}

/// Why `player` may not apply `action` for `cmd` to `target`, if they may
/// not: the command must be one they can use, and a grant is further
/// limited by the delegation rule and the mortal cap.
fn grant_refusal(
    world: &World,
    player: Entity,
    target: Entity,
    target_name: &str,
    action: Action,
    cmd: &'static Command,
) -> Option<String> {
    let usable = world
        .get::<Account>(player)
        .is_some_and(|a| visible_with(cmd, a.role, &a.perms, world.get::<CommandGrants>(player)));
    if !usable {
        return Some("You cannot grant or revoke a command you yourself cannot use.\r\n".into());
    }
    if action != Action::Grant {
        return None;
    }
    if DELEGATION_COMMANDS.contains(&cmd.names[0]) {
        return Some("Access to grant, revoke and ungrant cannot itself be granted.\r\n".into());
    }
    let target_is_mortal = world
        .get::<Account>(target)
        .is_some_and(|a| a.role == UserRole::Player);
    if target_is_mortal && (!MORTAL_GRANT_CAP.at_least(cmd.min_role) || cmd.required_perm.is_some())
    {
        return Some(format!(
            "{target_name} is a mortal; mortals can only be granted commands up to immortal \
             rank.\r\n"
        ));
    }
    None
}

fn command_change(
    world: &mut World,
    player: Entity,
    target: Entity,
    target_name: &str,
    action: Action,
    rest: &[&str],
) {
    let mine = authority_level(world, player);
    // Entry level: the number after the command, else the grantor's own,
    // never above the grantor's (a higher level would lock out peers).
    let level = rest
        .get(1)
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(mine)
        .clamp(0, mine);

    if target == player {
        send_to(
            world,
            player,
            "You cannot grant or revoke your own commands.\r\n",
        );
        return;
    }
    let Some(&typed) = rest.first() else {
        send_to(world, player, USAGE);
        return;
    };
    let Some(cmd) = find_command(world, player, typed) else {
        send_to(world, player, "No such command.\r\n");
        return;
    };
    let name = cmd.names[0];

    if let Some(refusal) = grant_refusal(world, player, target, target_name, action, cmd) {
        send_to(world, player, refusal);
        return;
    }

    let existing = world
        .get::<CommandGrants>(target)
        .cloned()
        .unwrap_or_default();
    let (same_list, other_list) = match action {
        Action::Revoke => (&existing.revokes, &existing.grants),
        Action::Grant | Action::Ungrant => (&existing.grants, &existing.revokes),
    };
    let in_same = same_list.iter().find(|g| g.command == name);
    let in_other = other_list.iter().find(|g| g.command == name);
    let past = if action == Action::Revoke {
        "revoked"
    } else {
        "granted"
    };

    if action != Action::Ungrant && in_same.is_some() {
        send_to(
            world,
            player,
            format!("{target_name} already has {typed} {past}.\r\n"),
        );
        return;
    }
    let blocker = in_other.or(if action == Action::Ungrant {
        in_same
    } else {
        None
    });
    if let Some(b) = blocker
        && b.level > mine
    {
        send_to(
            world,
            player,
            format!(
                "You cannot change {target_name}'s access to {typed}, because {} (level {}) \
                 granted or revoked it.\r\n",
                b.grantor, b.level
            ),
        );
        return;
    }

    let grantor = crate::commands::name_of(world, player);
    let mut grants = existing;
    grants.clear_command(name);
    let message = match action {
        Action::Ungrant => {
            format!("Revoked all grants on {target_name} for {typed}.\r\n")
        }
        Action::Grant | Action::Revoke => {
            let entry = GrantEntry {
                command: name.to_string(),
                grantor,
                level,
            };
            let (list, verb, prep) = if action == Action::Revoke {
                (&mut grants.revokes, "Revoked", "from")
            } else {
                (&mut grants.grants, "Granted", "to")
            };
            list.push(entry);
            format!("{verb} {typed} {prep} {target_name} at level {level}.\r\n")
        }
    };
    store(world, target, grants);
    send_to(world, player, message);
}

fn clear_grants(world: &mut World, player: Entity, target: Entity, mine: i32) {
    let mut grants = world
        .get::<CommandGrants>(target)
        .cloned()
        .unwrap_or_default();
    let before = grants.grants.len() + grants.revokes.len();
    grants.grants.retain(|g| g.level > mine);
    grants.revokes.retain(|g| g.level > mine);
    let count = before - (grants.grants.len() + grants.revokes.len());
    store(world, target, grants);
    send_to(
        world,
        player,
        format!(
            "{count} grant{} cleared.\r\n",
            if count == 1 { "" } else { "s" }
        ),
    );
}

/// Write the target's lists back, dropping the component when empty, and
/// queue a save so the change survives a crash.
fn store(world: &mut World, target: Entity, grants: CommandGrants) {
    if grants.is_empty() {
        if let Ok(mut em) = world.get_entity_mut(target) {
            em.remove::<CommandGrants>();
        }
    } else {
        try_insert(world, target, grants);
    }
    try_insert(world, target, PendingSave);
}
