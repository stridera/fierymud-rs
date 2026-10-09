//! Command abbreviation priority.
//!
//! Legacy `FieryMUD` resolved a typed verb by walking `cmd_info[]` in table
//! order and taking the first entry the typed text was a prefix of
//! (`interpreter.cpp`, `command_interpreter`): "the order they appear in
//! the command list" is the priority ("who" before "whisper", "l" is
//! look). This module mirrors that table order.
//!
//! Why this lives in code and not in the `Command` table: the Rust command
//! registry is code-defined (every handler is an `inventory::submit!`ed fn
//! pointer; nothing reads the `Command` rows at runtime) and the order is
//! a parser invariant tied to that registry, not builder content. Commands
//! the legacy table never had (new Rust-only verbs) rank after every
//! legacy name, ordered by name length then alphabetically, so resolution
//! is deterministic regardless of link order.
//!
//! Socials sit inline in the table, as they did in `cmd_info[]`, so they
//! abbreviate by the same rule (`gig` is giggle, `ha` is halo). A social
//! the legacy table never had ranks after every legacy name. A command the
//! legacy table never had (`sneak`, `accept`) ranks before every social,
//! so `sn` is sneak and `ac` accept although legacy, lacking both, gave
//! snicker and ack. An exact
//! command name always beats a social; the legacy `qui` guard entry (a
//! hidden command that refuses to quit) is handled by the dispatcher.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Legacy `cmd_info[]` order with socials (`do_action` rows) inline and
/// hidden (`level -1`) entries removed. Index = priority (lower wins).
/// Social names are ranked here but only resolve when the DB `Social`
/// table has them.
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
    "ack",
    "advance",
    "aggr",
    "alert",
    "alias",
    "accuse",
    "afk",
    "agree",
    "amaze",
    "anews",
    "apologize",
    "applaud",
    "assist",
    "ask",
    "autoboot",
    "ayt",
    "backstab",
    "ban",
    "bandage",
    "balance",
    "bang",
    "bark",
    "bash",
    "bodyslam",
    "beckon",
    "beer",
    "beg",
    "berserk",
    "bite",
    "bird",
    "blink",
    "bleed",
    "bless",
    "blush",
    "boardadmin",
    "boggle",
    "bonk",
    "bored",
    "bounce",
    "bow",
    "brb",
    "buck",
    "burp",
    "buy",
    "bug",
    "bye",
    "cast",
    "cackle",
    "call",
    "camp",
    "cartwheel",
    "chant",
    "chuckle",
    "check",
    "cheer",
    "choke",
    "clan",
    "clap",
    "claw",
    "clear",
    "close",
    "cls",
    "consider",
    "color",
    "compare",
    "comfort",
    "comb",
    "commands",
    "consent",
    "conceal",
    "coredump",
    "corner",
    "cough",
    "create",
    "credits",
    "cringe",
    "cry",
    "clist",
    "csearch",
    "ctell",
    "cuddle",
    "curse",
    "curtsey",
    "dance",
    "date",
    "daydream",
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
    "dream",
    "drink",
    "drop",
    "drool",
    "duck",
    "duh",
    "dump",
    "eat",
    "edit",
    "echo",
    "electrify",
    "emote",
    "emote's",
    ":",
    "embrace",
    "enter",
    "envy",
    "equipment",
    "exits",
    "examine",
    "exchange",
    "experience",
    "extinguish",
    "eyebrow",
    "elist",
    "enum",
    "esearch",
    "force",
    "flee",
    "fart",
    "first aid",
    "fill",
    "flanic",
    "flex",
    "flip",
    "flirt",
    "fly",
    "follow",
    "fool",
    "fondle",
    "freeze",
    "french",
    "frown",
    "fume",
    "get",
    "gag",
    "gape",
    "gasp",
    "gecho",
    "give",
    "giggle",
    "glance",
    "glare",
    "glomp",
    "glower",
    "goto",
    "go",
    "gossip",
    ".",
    "gouge",
    "groan",
    "group",
    "grab",
    "greport",
    "greet",
    "gretreat",
    "grin",
    "grope",
    "grovel",
    "growl",
    "grumble",
    "gsay",
    "gtell",
    "guard",
    "grant",
    "gedit",
    "help",
    "hedit",
    "handbook",
    "halo",
    "hcontrol",
    "hhroom",
    "hi5",
    "hiccup",
    "hide",
    "hiss",
    "hit",
    "hitall",
    "hold",
    "hop",
    "hotboot",
    "howl",
    "hug",
    "hunger",
    "inventory",
    "identify",
    "idea",
    "iedit",
    "imitate",
    "imotd",
    "impale",
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
    "kiss",
    "kneel",
    "ksearch",
    "look",
    "lag",
    "laugh",
    "layhands",
    "last",
    "lasttells",
    "lastgos",
    "lean",
    "leave",
    "level",
    "light",
    "list",
    "listspells",
    "lick",
    "lock",
    "linkload",
    "load",
    "love",
    "lure",
    "memorize",
    "maul",
    "moan",
    "medit",
    "mcopy",
    "motd",
    "mail",
    "massage",
    "meditate",
    "moon",
    "mosh",
    "mount",
    "mourn",
    "mumble",
    "music",
    "mute",
    "mutter",
    "murder",
    "mlist",
    "mnum",
    "msearch",
    "mstat",
    "nap",
    "news",
    "nibble",
    "nod",
    "nog",
    "noogie",
    "notitle",
    "note",
    "nudge",
    "nuzzle",
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
    "panic",
    "pant",
    "pat",
    "page",
    "pardon",
    "peace",
    "peck",
    "peer",
    "perform",
    "pet",
    "petition",
    "pfilemaint",
    "pick",
    "play",
    "players",
    "point",
    "poke",
    "policy",
    "ponder",
    "poofin",
    "poofout",
    "pounce",
    "pour",
    "pout",
    "pray",
    "prompt",
    "protect",
    "pscan",
    "ptell",
    "puke",
    "punch",
    "purr",
    "purge",
    "quaff",
    "qecho",
    "quit",
    "qsay",
    "rest",
    "raise",
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
    "rofl",
    "roll",
    "ready",
    "ruffle",
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
    "salute",
    "scribe",
    "scare",
    "scold",
    "scratch",
    "scream",
    "screw",
    "sdedit",
    "sell",
    "send",
    "set",
    "search",
    "sedit",
    "seduce",
    "shout",
    "shake",
    "shadow",
    "shapechange",
    "shiver",
    "show",
    "shrug",
    "shudder",
    "shutdow",
    "shutdown",
    "sigh",
    "sing",
    "sip",
    "sit",
    "skills",
    "skillset",
    "slist",
    "snum",
    "ssearch",
    "sleep",
    "slap",
    "slobber",
    "smell",
    "smile",
    "smirk",
    "smoke",
    "snicker",
    "snap",
    "snarl",
    "sneeze",
    "sniff",
    "snoogie",
    "snore",
    "snort",
    "snowball",
    "snoop",
    "snuggle",
    "songs",
    "socials",
    "spam",
    "split",
    "spells",
    "spank",
    "spit",
    "springleap",
    "squeeze",
    "stand",
    "stare",
    "stat",
    "stay",
    "steal",
    "steam",
    "stow",
    "stomp",
    "stroke",
    "strut",
    "study",
    "sulk",
    "summon",
    "swat",
    "sweat",
    "switch",
    "syslog",
    "subclass",
    "tell",
    "terminate",
    "tackle",
    "take",
    "tantrum",
    "tango",
    "tame",
    "tap",
    "tarzan",
    "taunt",
    "taste",
    "tease",
    "tedit",
    "teleport",
    "thank",
    "think",
    "thaw",
    "thirst",
    "throatcut",
    "throw",
    "tip",
    "title",
    "tickle",
    "time",
    "toggle",
    "tongue",
    "touch",
    "track",
    "transfer",
    "trigedit",
    "trigcopy",
    "tripup",
    "trophy",
    "tug",
    "twibble",
    "twiddle",
    "twitch",
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
    "veto",
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
    "wave",
    "wear",
    "wait",
    "weather",
    "wet",
    "who",
    "whap",
    "whatever",
    "whoami",
    "where",
    "whisper",
    "whine",
    "whistle",
    "wield",
    "wiggle",
    "wince",
    "wink",
    "withdraw",
    "wiznet",
    ";",
    "wizhelp",
    "wizlist",
    "wizlock",
    "worship",
    "write",
    "xnames",
    "yawn",
    "yodel",
    "zone",
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
    "\n",
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
                "'{typed}' should resolve like legacy '{want}'"
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
        // `k` matches nothing on purpose (`MIN_ABBREV`): it is too easy to
        // hit when `l` (look) was meant.
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
    fn bare_k_is_unknown_but_longer_k_abbreviations_work() {
        // Neither staff nor mortals get a command (or the "type it out"
        // refusal for kick) from a lone `k`.
        assert_eq!(resolve("k"), None);
        assert_eq!(resolve_as("k", UserRole::Implementor, ALL_PERMS), None);
        assert_eq!(resolve("ki"), Some("kick"));
        assert_eq!(resolve("kic"), Some("kick"));
        assert_eq!(resolve("kick"), Some("kick"));
        assert_eq!(resolve("kil"), canonical_of("kill"));
        assert_eq!(resolve("kill"), canonical_of("kill"));
        assert_eq!(resolve("kn"), Some("kneel"));
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
