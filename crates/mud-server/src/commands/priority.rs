//! Command abbreviation priority.
//!
//! Legacy `FieryMUD` resolved a typed verb by walking `cmd_info[]` in table
//! order and taking the first entry the typed text was a prefix of
//! (`interpreter.cpp`, `command_interpreter`): "the order they appear in
//! the command list" is the priority ("who" before "whisper", "l" is
//! look, "k" is kill). This module mirrors that table order.
//!
//! Why this lives in code and not in the `Command` table: the Rust command
//! registry is code-defined (every handler is an `inventory::submit!`ed fn
//! pointer; nothing reads the `Command` rows at runtime) and the order is
//! a parser invariant tied to that registry, not builder content. Commands
//! the legacy table never had (new Rust-only verbs) rank after every
//! legacy name, ordered by name length then alphabetically, so resolution
//! is deterministic regardless of link order.
//!
//! Socials are not listed: they stay an exact-name fallback
//! (`try_dispatch_social`). The legacy `qui` guard entry (a hidden command
//! that refuses to quit) is handled by the dispatcher.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Legacy `cmd_info[]` order, socials and hidden (`level -1`) entries
/// removed. Index = priority (lower wins).
pub(crate) const LEGACY_ORDER: &[&str] = &[
    "north",
    "east",
    "south",
    "west",
    "up",
    "down",
    "at",
    "abort",
    "abandon",
    "advance",
    "aggr",
    "alert",
    "alias",
    "anews",
    "assist",
    "ask",
    "autoboot",
    "backstab",
    "ban",
    "bandage",
    "balance",
    "bash",
    "bodyslam",
    "berserk",
    "bless",
    "boardadmin",
    "buck",
    "buy",
    "bug",
    "cast",
    "call",
    "camp",
    "cartwheel",
    "chant",
    "check",
    "clan",
    "claw",
    "clear",
    "close",
    "cls",
    "consider",
    "color",
    "compare",
    "commands",
    "consent",
    "conceal",
    "coredump",
    "corner",
    "create",
    "credits",
    "clist",
    "csearch",
    "ctell",
    "date",
    "dc",
    "deposit",
    "desc",
    "diagnose",
    "dismount",
    "display",
    "disband",
    "dig",
    "disarm",
    "disengage",
    "doorbash",
    "douse",
    "drag",
    "drink",
    "drop",
    "dump",
    "eat",
    "edit",
    "echo",
    "electrify",
    "emote",
    "emote's",
    ":",
    "enter",
    "equipment",
    "exits",
    "examine",
    "exchange",
    "experience",
    "extinguish",
    "elist",
    "enum",
    "esearch",
    "force",
    "flee",
    "first aid",
    "fill",
    "fly",
    "follow",
    "freeze",
    "get",
    "gecho",
    "give",
    "glance",
    "goto",
    "go",
    "gossip",
    ".",
    "gouge",
    "group",
    "grab",
    "greport",
    "gretreat",
    "gsay",
    "gtell",
    "guard",
    "grant",
    "gedit",
    "help",
    "hedit",
    "handbook",
    "hcontrol",
    "hhroom",
    "hide",
    "hit",
    "hitall",
    "hold",
    "hotboot",
    "howl",
    "inventory",
    "identify",
    "idea",
    "iedit",
    "imotd",
    "innate",
    "inspect",
    "infodump",
    "ignore",
    "inctime",
    "hour",
    "info",
    "insult",
    "invis",
    "ispell",
    "junk",
    "kick",
    "kill",
    "kneel",
    "ksearch",
    "look",
    "layhands",
    "last",
    "lasttells",
    "lastgos",
    "leave",
    "level",
    "light",
    "list",
    "listspells",
    "lock",
    "linkload",
    "load",
    "lure",
    "memorize",
    "maul",
    "medit",
    "mcopy",
    "motd",
    "mail",
    "meditate",
    "mount",
    "music",
    "mute",
    "murder",
    "mlist",
    "mnum",
    "msearch",
    "mstat",
    "news",
    "notitle",
    "note",
    "naccept",
    "ndecline",
    "nlist",
    "order",
    "open",
    "olc",
    "oedit",
    "ocopy",
    "olist",
    "olocate",
    "onum",
    "osearch",
    "ostat",
    "put",
    "palm",
    "page",
    "pardon",
    "peace",
    "peck",
    "perform",
    "petition",
    "pfilemaint",
    "pick",
    "play",
    "players",
    "point",
    "policy",
    "poofin",
    "poofout",
    "pour",
    "pray",
    "prompt",
    "pscan",
    "ptell",
    "purge",
    "quaff",
    "qecho",
    "quit",
    "qsay",
    "rest",
    "read",
    "rend",
    "report",
    "reply",
    "reload",
    "recite",
    "receive",
    "recline",
    "remove",
    "rent",
    "reroll",
    "rescue",
    "restore",
    "rrestore",
    "pain",
    "rpain",
    "retreat",
    "redit",
    "rcopy",
    "rename",
    "revoke",
    "roar",
    "roundhouse",
    "rlist",
    "rnum",
    "rsearch",
    "rstat",
    "sstat",
    "say",
    "'",
    "save",
    "score",
    "scan",
    "scribe",
    "sdedit",
    "sell",
    "send",
    "set",
    "search",
    "sedit",
    "shout",
    "shadow",
    "shapechange",
    "show",
    "shutdow",
    "shutdown",
    "sip",
    "sit",
    "skills",
    "skillset",
    "slist",
    "snum",
    "ssearch",
    "sleep",
    "snoop",
    "songs",
    "socials",
    "split",
    "spells",
    "springleap",
    "stand",
    "stat",
    "stay",
    "steal",
    "stow",
    "stomp",
    "study",
    "summon",
    "switch",
    "syslog",
    "subclass",
    "tell",
    "terminate",
    "take",
    "tantrum",
    "tame",
    "taste",
    "tedit",
    "teleport",
    "thaw",
    "throatcut",
    "title",
    "time",
    "toggle",
    "touch",
    "track",
    "transfer",
    "trigedit",
    "trigcopy",
    "tripup",
    "trophy",
    "typo",
    "unlock",
    "unban",
    "ungrant",
    "use",
    "unaffect",
    "users",
    "uptime",
    "value",
    "varset",
    "varunset",
    "version",
    "visible",
    "viewdam",
    "vnum",
    "vlist",
    "vsearch",
    "vstat",
    "zstat",
    "estat",
    "oestat",
    "restat",
    "vitem",
    "vwear",
    "wake",
    "walk",
    "wear",
    "weather",
    "who",
    "whoami",
    "where",
    "whisper",
    "wield",
    "withdraw",
    "wiznet",
    ";",
    "wizhelp",
    "wizlist",
    "wizlock",
    "write",
    "xnames",
    "zedit",
    "zlist",
    "znum",
    "zreset",
    "zsearch",
    "game",
    "world",
    "attach",
    "detach",
    "tlist",
    "tnum",
    "tsearch",
    "tstat",
    "qadd",
    "qdel",
    "qlist",
    "qstat",
    "objupdate",
];

static RANK: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for (i, n) in LEGACY_ORDER.iter().enumerate() {
        // First occurrence wins, like the legacy linear scan.
        m.entry(*n).or_insert(i);
    }
    m
});

/// Priority of a legacy command name, if the legacy table had it.
pub(crate) fn legacy_rank(name: &str) -> Option<usize> {
    RANK.get(name).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{all_commands, longest_prefix_match, resolve_by_prefix};
    use mud_db::enums::{Permission, UserRole};

    /// What the dispatcher resolves `typed` to for an ordinary player:
    /// exact name first, then the legacy-priority abbreviation.
    fn resolve(typed: &str) -> Option<&'static str> {
        resolve_as(typed, UserRole::Player, &[])
    }

    fn resolve_as(typed: &str, role: UserRole, perms: &[Permission]) -> Option<&'static str> {
        longest_prefix_match(&[typed])
            .map(|(c, _)| c)
            .or_else(|| resolve_by_prefix(typed, role, perms))
            .map(|c| c.names[0])
    }

    const ALL_PERMS: &[Permission] = &[
        Permission::Build,
        Permission::Code,
        Permission::Admin,
        Permission::God,
        Permission::Shutdown,
        Permission::Wizlock,
        Permission::Syslog,
        Permission::Log,
        Permission::Force,
        Permission::Snoop,
        Permission::Freeze,
        Permission::Thaw,
        Permission::Ban,
        Permission::Unban,
        Permission::Dc,
        Permission::Advance,
        Permission::Restore,
        Permission::Notitle,
        Permission::Squelch,
        Permission::Teleport,
        Permission::Transfer,
        Permission::Summon,
        Permission::Invisible,
        Permission::Nohassle,
        Permission::ZoneReset,
        Permission::Wiznet,
    ];

    fn canonical_of(legacy_name: &str) -> Option<&'static str> {
        all_commands()
            .find(|c| c.names.contains(&legacy_name))
            .map(|c| c.names[0])
    }

    #[test]
    fn issue_1_who_beats_whisper() {
        assert_eq!(resolve("who"), Some("who"));
        assert_eq!(resolve("wh"), Some("who"));
        assert_eq!(resolve("whi"), Some("whisper"));
        assert_eq!(resolve("whis"), Some("whisper"));
    }

    #[test]
    fn single_letter_abbreviations_follow_legacy_table_order() {
        // Verified against `cmd_info[]`: directions come first, then the
        // main list in the order the legacy table has it.
        for (typed, want) in [
            ("n", "north"),
            ("e", "east"),
            ("s", "south"),
            ("w", "west"),
            ("u", "up"),
            ("d", "down"),
            ("l", "look"),
            ("i", "inventory"),
            ("ki", "kick"),
            ("kil", "kill"),
            ("g", "get"),
            ("t", "tell"),
            ("sc", "score"),
            ("eq", "equipment"),
            ("ex", "exits"),
            ("r", "rest"),
            ("res", "rest"),
            ("rec", "recite"),
            ("wi", "wield"),
            ("we", "west"),
            ("dr", "drag"),
            // `at` is staff-only, so a mortal `a` skips it (legacy `can_use_command`).
            ("a", "abort"),
        ] {
            let got = resolve(typed);
            assert_eq!(
                got.and_then(canonical_of).or(got),
                canonical_of(want).or(Some(want)),
                "`{typed}` should resolve like legacy `{want}`"
            );
        }
    }

    /// Typed text that is not a full command name resolves to the first
    /// legacy entry it abbreviates (among the commands this server has).
    /// Rust-only movement aliases are the documented exceptions.
    #[test]
    fn every_legacy_prefix_resolves_to_first_implemented_legacy_match() {
        // Staff see every command, so the whole table is in play.
        let implemented: Vec<&str> = LEGACY_ORDER
            .iter()
            .copied()
            .filter(|l| !l.contains(' ') && canonical_of(l).is_some())
            .collect();
        // New directions and `out`/`in` have no legacy entry to defer to;
        // `k` stays "kill" (legacy table order would make it `kick`) because
        // it is the one-letter attack every player types.
        let exempt = ["ne", "nw", "se", "sw", "o", "in", "k"];
        let mut bad = Vec::new();
        for l in &implemented {
            for end in 1..=l.len() {
                let p = &l[..end];
                if exempt.contains(&p) {
                    continue;
                }
                let first = implemented.iter().find(|n| n.starts_with(p)).unwrap();
                let want = canonical_of(first).unwrap();
                match resolve_as(p, UserRole::Implementor, ALL_PERMS) {
                    Some(got) if got == want => {}
                    // Destructive verbs are refused when abbreviated.
                    got => {
                        if !crate::commands::abbrev_blocked(want) {
                            bad.push(format!("{p}: want {want} got {got:?}"));
                        }
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "{} mismatches:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    #[test]
    fn k_is_the_one_letter_attack() {
        assert_eq!(resolve("k"), canonical_of("kill"));
        assert_eq!(resolve("ki"), Some("kick"));
    }

    #[test]
    fn exact_name_always_wins_over_longer_names() {
        // `at` is listed before `abort`; `ban` and `bandage`; `who` and
        // `whoami`; `go` and `goto` are all exact names.
        assert_eq!(resolve("who"), Some("who"));
        assert_eq!(resolve("whoami").map(|_| ()), Some(()));
        assert_eq!(resolve("goto"), canonical_of("goto"));
        assert_eq!(resolve("north"), Some("north"));
    }

    #[test]
    fn destructive_commands_do_not_abbreviate() {
        use crate::commands::abbrev_blocked;
        assert!(abbrev_blocked("quit"));
        assert!(abbrev_blocked("delete"));
        assert!(!abbrev_blocked("flee"));
        assert!(!abbrev_blocked("drop"));
    }

    #[test]
    fn unknown_prefix_resolves_to_nothing() {
        assert_eq!(resolve("zzzzq"), None);
        assert_eq!(resolve(""), None);
    }

    #[test]
    fn legacy_rank_is_table_position() {
        assert!(legacy_rank("who") < legacy_rank("whisper"));
        assert!(legacy_rank("north") < legacy_rank("look"));
        assert_eq!(legacy_rank("nonexistent"), None);
    }
}
