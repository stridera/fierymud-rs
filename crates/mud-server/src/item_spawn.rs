//! Shared "fresh item from prototype" spawn. The implementation lives in
//! `mud_world::item_spawn` so the world loader and the Lua host share it
//! with the server; see there for the contract.

pub(crate) use mud_world::spawn_item_from_proto;
