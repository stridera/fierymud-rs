//! Process-environment flags shared by startup code.
//!
//! `MUD_ENV=production` is the production marker (set in the deploy env file
//! alongside `ADMIN_TOKEN`). It turns "log a warning" safety nets into hard
//! refusals: an unset admin token aborts startup, and dev mode cannot be
//! enabled.

/// True when `value` spells a truthy flag (`true` / `1`, case-insensitive).
pub(crate) fn is_truthy(value: &str) -> bool {
    let v = value.trim();
    v.eq_ignore_ascii_case("true") || v == "1"
}

/// Whether `MUD_ENV` names a production deployment.
pub(crate) fn is_production_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        v.eq_ignore_ascii_case("production") || v.eq_ignore_ascii_case("prod")
    })
}

pub(crate) fn is_production() -> bool {
    is_production_value(std::env::var("MUD_ENV").ok().as_deref())
}

/// Dev mode (everyone is Implementor) needs an explicit `MUD_DEV_MODE`
/// opt-in in the process environment and is never allowed in production.
/// The `server.dev_mode` `GameConfig` row alone is not enough: a DB write must
/// not be able to open admin commands to every account.
pub(crate) fn dev_mode_allowed_for(production: bool, mud_dev_mode: Option<&str>) -> bool {
    !production && mud_dev_mode.is_some_and(is_truthy)
}

pub(crate) fn dev_mode_allowed() -> bool {
    dev_mode_allowed_for(
        is_production(),
        std::env::var("MUD_DEV_MODE").ok().as_deref(),
    )
}

/// An env value that is empty or whitespace-only counts as unset.
pub(crate) fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_detection() {
        assert!(is_production_value(Some("production")));
        assert!(is_production_value(Some(" PROD ")));
        assert!(!is_production_value(Some("dev")));
        assert!(!is_production_value(None));
    }

    #[test]
    fn dev_mode_requires_env_and_not_production() {
        assert!(!dev_mode_allowed_for(false, None));
        assert!(!dev_mode_allowed_for(false, Some("")));
        assert!(!dev_mode_allowed_for(false, Some("no")));
        assert!(dev_mode_allowed_for(false, Some("true")));
        assert!(dev_mode_allowed_for(false, Some("1")));
        assert!(!dev_mode_allowed_for(true, Some("true")));
    }

    #[test]
    fn blank_values_are_unset() {
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(non_blank(Some("  \t".into())), None);
        assert_eq!(non_blank(Some("abc".into())), Some("abc".into()));
        assert_eq!(non_blank(None), None);
    }
}
