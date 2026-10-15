//! Shared parsing for the `(zone, id)` composite key every world entity has.
//!
//! Every Lua API that names a prototype or room by its composite key takes an
//! [`IdArg`] as its argument list, so one rule applies everywhere. Accepted:
//!
//! * `f(30, 1)`: two integers (the original form),
//! * `f({30, 1})`: a positional table, `{zone, id}`,
//! * `f({zone = 30, id = 1})`: a named table. `zone_id` / `local_id`, the
//!   spelling the entity fields use, are accepted as aliases.
//!
//! Anything else is an error that names the accepted forms. APIs whose key is
//! optional use [`OptIdArg`] instead: no arguments (or a single `nil`) is
//! `None`; a half-given key is still an error.

use mlua::{FromLua, FromLuaMulti, Lua, MultiValue, Table, Value};

/// A `(zone, id)` composite key as received from Lua.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdArg {
    pub zone: i32,
    pub id: i32,
}

/// An optional [`IdArg`]: no arguments / `nil` means `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptIdArg(pub Option<IdArg>);

const EXPECTED: &str =
    "expected (zone, id) integers, a {zone, id} table, or a {zone = Z, id = I} table";

fn bad(from: &'static str, message: String) -> mlua::Error {
    mlua::Error::FromLuaConversionError {
        from,
        to: "(zone, id)".to_string(),
        message: Some(message),
    }
}

fn int(lua: &Lua, v: Value, what: &str) -> mlua::Result<i32> {
    let type_name = v.type_name();
    match v {
        Value::Integer(_) | Value::Number(_) | Value::String(_) => i32::from_lua(v, lua)
            .map_err(|_| bad(type_name, format!("{EXPECTED}: {what} is not an integer"))),
        _ => Err(bad(
            type_name,
            format!("{EXPECTED}: {what} must be an integer, got {type_name}"),
        )),
    }
}

fn from_table(lua: &Lua, t: &Table) -> mlua::Result<IdArg> {
    let named_zone: Value = t.raw_get("zone")?;
    let named_id: Value = t.raw_get("id")?;
    let (zone, id) = if !named_zone.is_nil() || !named_id.is_nil() {
        (named_zone, named_id)
    } else {
        let z: Value = t.raw_get("zone_id")?;
        let i: Value = t.raw_get("local_id")?;
        if !z.is_nil() || !i.is_nil() {
            (z, i)
        } else {
            if t.raw_len() != 2 {
                return Err(bad(
                    "table",
                    format!(
                        "{EXPECTED}: the table has {} positional entries (need exactly 2) and no zone/id keys",
                        t.raw_len()
                    ),
                ));
            }
            (t.raw_get(1)?, t.raw_get(2)?)
        }
    };
    if zone.is_nil() || id.is_nil() {
        return Err(bad(
            "table",
            format!(
                "{EXPECTED}: the table is missing its {}",
                if zone.is_nil() { "zone" } else { "id" }
            ),
        ));
    }
    Ok(IdArg {
        zone: int(lua, zone, "zone")?,
        id: int(lua, id, "id")?,
    })
}

impl IdArg {
    /// Parse a whole argument list. Extra trailing values are ignored, as
    /// Lua does for any call.
    pub fn parse(mut values: MultiValue, lua: &Lua) -> mlua::Result<IdArg> {
        match values.pop_front() {
            Some(Value::Table(t)) => from_table(lua, &t),
            Some(Value::Nil) | None => Err(bad("nil", format!("{EXPECTED}, got nothing"))),
            Some(zone) => {
                let Some(id) = values.pop_front().filter(|v| !v.is_nil()) else {
                    return Err(bad(
                        zone.type_name(),
                        format!("{EXPECTED}: a lone {} is not a key", zone.type_name()),
                    ));
                };
                Ok(IdArg {
                    zone: int(lua, zone, "zone")?,
                    id: int(lua, id, "id")?,
                })
            }
        }
    }
}

impl FromLuaMulti for IdArg {
    fn from_lua_multi(values: MultiValue, lua: &Lua) -> mlua::Result<Self> {
        IdArg::parse(values, lua)
    }
}

impl FromLuaMulti for OptIdArg {
    fn from_lua_multi(values: MultiValue, lua: &Lua) -> mlua::Result<Self> {
        if values.iter().all(Value::is_nil) {
            return Ok(OptIdArg(None));
        }
        IdArg::parse(values, lua).map(|k| OptIdArg(Some(k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lua: &Lua, code: &str) -> mlua::Result<(i32, i32)> {
        let f = lua.create_function(|_, k: IdArg| Ok((k.zone, k.id)))?;
        lua.globals().set("f", f)?;
        lua.load(code).eval()
    }

    #[test]
    fn accepts_every_form() {
        let lua = Lua::new();
        for code in [
            "return f(30, 1)",
            "return f({30, 1})",
            "return f({zone = 30, id = 1})",
            "return f({zone_id = 30, local_id = 1})",
            "return f(30.0, 1)",
        ] {
            assert_eq!(run(&lua, code).unwrap(), (30, 1), "{code}");
        }
    }

    #[test]
    fn rejects_malformed_with_a_clear_message() {
        let lua = Lua::new();
        for code in [
            "return f()",
            "return f(nil)",
            "return f(30)",
            "return f({})",
            "return f({30})",
            "return f({30, 1, 2})",
            "return f({zone = 30})",
            "return f({id = 1})",
            "return f({\"a\", 1})",
            "return f({zone = true, id = 1})",
            "return f(30, {})",
            "return f(\"x\", 1)",
        ] {
            let err = run(&lua, code).expect_err(code).to_string();
            assert!(err.contains("(zone, id)"), "{code}: {err}");
        }
    }

    #[test]
    fn optional_form() {
        let lua = Lua::new();
        let f = lua
            .create_function(|_, k: OptIdArg| Ok(k.0.map(|k| (k.zone, k.id)).is_some()))
            .unwrap();
        lua.globals().set("f", f).unwrap();
        assert!(!lua.load("return f()").eval::<bool>().unwrap());
        assert!(!lua.load("return f(nil)").eval::<bool>().unwrap());
        assert!(lua.load("return f(1, 2)").eval::<bool>().unwrap());
        assert!(lua.load("return f({1, 2})").eval::<bool>().unwrap());
        assert!(lua.load("return f(1)").eval::<bool>().is_err());
    }
}
