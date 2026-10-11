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

/// Parse a port-override env value. `None` means unset/blank/unparseable
/// (the caller falls through to `GameConfig` / address env / default).
pub(crate) fn parse_port(value: Option<&str>) -> Option<u16> {
    value.and_then(|v| v.trim().parse::<u16>().ok())
}

/// Read a port override from the environment (`MUD_TELNET_PORT` etc.).
/// A set-but-invalid value is logged and ignored.
pub(crate) fn port_from_env(name: &str) -> Option<u16> {
    let raw = non_blank(std::env::var(name).ok())?;
    let parsed = parse_port(Some(&raw));
    if parsed.is_none() {
        tracing::warn!(var = name, value = %raw, "ignoring invalid port override");
    }
    parsed
}

/// Telnet listen address. Precedence: `MUD_TELNET_PORT` > `server.port`
/// `GameConfig` > `MUD_LISTEN_ADDR` > `0.0.0.0:4003`. A `MUD_TELNET_PORT` of 0
/// is not a valid listen port and is ignored.
pub(crate) fn resolve_telnet_addr(
    env_port: Option<u16>,
    cfg_port: i32,
    env_addr: Option<String>,
) -> String {
    if let Some(p) = env_port.filter(|p| *p > 0) {
        format!("0.0.0.0:{p}")
    } else if cfg_port > 0 {
        format!("0.0.0.0:{cfg_port}")
    } else {
        env_addr.unwrap_or_else(|| "0.0.0.0:4003".into())
    }
}

/// TLS listen address, or `None` when `MUD_TLS_PORT=0` turns TLS off.
/// Precedence: `MUD_TLS_PORT` > `server.tls_port` `GameConfig` >
/// `MUD_TLS_LISTEN_ADDR` > `0.0.0.0:4443`.
pub(crate) fn resolve_tls_addr(
    env_port: Option<u16>,
    cfg_port: i32,
    env_addr: Option<String>,
) -> Option<String> {
    match env_port {
        Some(0) => None,
        Some(p) => Some(format!("0.0.0.0:{p}")),
        None if cfg_port > 0 => Some(format!("0.0.0.0:{cfg_port}")),
        None => Some(env_addr.unwrap_or_else(|| "0.0.0.0:4443".into())),
    }
}

/// Admin HTTP listen address. `ADMIN_LISTEN_ADDR` supplies host (and port)
/// as before; `ADMIN_PORT` replaces just the port. Default `127.0.0.1:8080`.
pub(crate) fn resolve_admin_addr(
    env_port: Option<u16>,
    env_addr: Option<&str>,
) -> std::net::SocketAddr {
    let mut addr: std::net::SocketAddr = env_addr
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("default admin addr parses"));
    if let Some(p) = env_port.filter(|p| *p > 0) {
        addr.set_port(p);
    }
    addr
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

    #[test]
    fn port_parsing() {
        assert_eq!(parse_port(Some("5003")), Some(5003));
        assert_eq!(parse_port(Some(" 0 ")), Some(0));
        assert_eq!(parse_port(Some("abc")), None);
        assert_eq!(parse_port(Some("70000")), None);
        assert_eq!(parse_port(None), None);
    }

    #[test]
    fn telnet_env_port_beats_game_config() {
        assert_eq!(resolve_telnet_addr(Some(5003), 4003, None), "0.0.0.0:5003");
        assert_eq!(resolve_telnet_addr(None, 4010, None), "0.0.0.0:4010");
        assert_eq!(
            resolve_telnet_addr(None, 0, Some("127.0.0.1:9".into())),
            "127.0.0.1:9"
        );
        assert_eq!(resolve_telnet_addr(Some(0), 0, None), "0.0.0.0:4003");
    }

    #[test]
    fn tls_port_zero_disables() {
        assert_eq!(resolve_tls_addr(Some(0), 4443, None), None);
        assert_eq!(
            resolve_tls_addr(Some(5443), 4443, None),
            Some("0.0.0.0:5443".into())
        );
        assert_eq!(
            resolve_tls_addr(None, 4443, None),
            Some("0.0.0.0:4443".into())
        );
        assert_eq!(resolve_tls_addr(None, 0, None), Some("0.0.0.0:4443".into()));
    }

    #[test]
    fn admin_port_replaces_port_only() {
        assert_eq!(resolve_admin_addr(None, None).to_string(), "127.0.0.1:8080");
        assert_eq!(
            resolve_admin_addr(Some(8180), None).to_string(),
            "127.0.0.1:8180"
        );
        assert_eq!(
            resolve_admin_addr(Some(8180), Some("0.0.0.0:9000")).to_string(),
            "0.0.0.0:8180"
        );
        assert_eq!(
            resolve_admin_addr(None, Some("0.0.0.0:9000")).to_string(),
            "0.0.0.0:9000"
        );
    }
}
