//! Test-only: dumps every REGISTRY command's in-code help strings as JSON so
//! `fierylib/scripts/gen_command_help_sql.py` can seed them into the
//! `Command` table. Run with:
//!
//! ```text
//! HELP_DUMP_PATH=/tmp/command-help.json \
//!   cargo test -p mud-server dump_command_help -- --ignored
//! ```

#[cfg(test)]
mod tests {
    use crate::commands::all_commands;

    #[test]
    #[ignore = "generator: writes $HELP_DUMP_PATH"]
    fn dump_command_help() {
        let path = std::env::var("HELP_DUMP_PATH").expect("set HELP_DUMP_PATH");
        let mut rows: Vec<serde_json::Value> = all_commands()
            .map(|c| {
                serde_json::json!({
                    "name": c.names[0],
                    "aliases": &c.names[1..],
                    "category": c.category.label(),
                    "min_role": format!("{:?}", c.min_role),
                    "usage": c.help.usage,
                    "summary": c.help.summary,
                    "long": c.help.long,
                })
            })
            .collect();
        rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        std::fs::write(path, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
    }
}
