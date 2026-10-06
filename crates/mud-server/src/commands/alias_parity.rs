//! Test-only: pins the C++ command names / aliases that the parity
//! audit found missing, so a future rename of the canonical command
//! can't silently drop them. Each row is `typed -> canonical (names[0])`
//! and was checked against the C++ `Commands().command(..).alias(..)`
//! registrations.

#[cfg(test)]
mod tests {
    use crate::commands::{AsyncCommand, longest_prefix_match};
    use bevy_ecs::prelude::*;

    /// `(typed alias, canonical command it must resolve to)`.
    const ALIASES: &[(&str, &str)] = &[
        ("questlog", "quests"),
        ("storage", "chest"),
        ("vault", "chest"),
        ("dep", "deposit"),
        ("with", "withdraw"),
        ("val", "value"),
        ("ass", "assist"),
        ("fol", "follow"),
        ("gr", "group"),
        ("sk", "skills"),
        ("sh", "shout"),
        ("wh", "whisper"),
        ("h", "help"),
        (".", "gossip"),
        ("o", "out"),
        ("comm", "commands"),
        ("diag", "diagnose"),
        ("cl", "clan"),
        ("we", "wear"),
        ("p", "put"),
        ("gi", "give"),
        ("dr", "drop"),
        ("comp", "compare"),
        ("check", "mailbox"),
        ("getmail", "readmail"),
        ("receive", "readmail"),
        ("go", "goto"),
        ("med", "meditate"),
        ("conc", "concentrate"),
    ];

    /// Brand-new commands: each must resolve to itself.
    const NEW_COMMANDS: &[&str] = &[
        "exchange",
        "repair",
        "scribe",
        "concentrate",
        "subclass",
        "palm",
        "stow",
        "cartwheel",
        "write",
        "call",
    ];

    fn canonical(word: &str) -> Option<&'static str> {
        longest_prefix_match(&[word]).map(|(cmd, _)| cmd.names[0])
    }

    #[test]
    fn aliases_resolve_to_intended_command() {
        for (alias, want) in ALIASES {
            assert_eq!(
                canonical(alias),
                Some(*want),
                "alias `{alias}` should resolve to `{want}`"
            );
        }
    }

    #[test]
    fn new_commands_are_registered() {
        for name in NEW_COMMANDS {
            assert_eq!(canonical(name), Some(*name), "`{name}` not registered");
        }
    }

    /// Aliases backed by an `AsyncCommand` need the head string in the
    /// module's async dispatch match too, or the sync stub runs instead.
    #[test]
    fn async_backed_aliases_are_claimed_by_async_dispatch() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let pool = mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let mut world = World::new();
        let player = world.spawn_empty().id();
        for head in [
            "questlog", "storage", "vault", "check", "getmail", "receive",
        ] {
            let claimed = inventory::iter::<AsyncCommand>()
                .any(|cmd| (cmd.dispatch)(&mut world, player, &pool, head, "").is_some());
            assert!(claimed, "no async dispatch claims `{head}`");
        }
    }
}
