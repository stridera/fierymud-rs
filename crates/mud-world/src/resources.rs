use std::collections::HashMap;

use bevy_ecs::prelude::*;

/// Maps composite (zone, id) keys from the schema to live entities in the world.
/// Used by spawn/lookup code to translate DB references to runtime handles.
#[derive(Resource, Debug, Default)]
pub struct WorldKeyIndex {
    pub zones: HashMap<i32, Entity>,
    pub rooms: HashMap<(i32, i32), Entity>,
}

/// Catalog of effect *types* loaded from the Effect table at startup.
/// Active applications live as ECS entities (EffectInstance + AppliedTo);
/// the catalog supplies metadata that doesn't change per-application.
#[derive(Resource, Debug, Default)]
pub struct EffectCatalog {
    pub by_id: HashMap<i32, EffectDef>,
}

impl EffectCatalog {
    pub fn find_by_name(&self, name: &str) -> Option<&EffectDef> {
        self.by_id
            .values()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone)]
pub struct EffectDef {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub effect_type: String,
    pub tags: Vec<String>,
    pub presence_override: Option<String>,
}
