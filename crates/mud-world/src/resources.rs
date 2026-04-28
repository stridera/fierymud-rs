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

/// Catalog of object prototypes loaded from the Objects table at startup.
/// Spawning a real instance copies the relevant fields onto a new entity.
#[derive(Resource, Debug, Default)]
pub struct ObjectPrototypes {
    pub by_key: HashMap<(i32, i32), ObjectProto>,
}

#[derive(Debug, Clone)]
pub struct ObjectProto {
    pub zone_id: i32,
    pub id: i32,
    pub r#type: mud_db::enums::ObjectType,
    pub name: String,
    pub keywords: Vec<String>,
    pub weight: f64,
    pub level: i32,
}

/// Catalog of social commands ("smile", "bow", "hug" …) loaded from the
/// Social table at startup. Looked up by name when the command dispatcher
/// fails to find a builtin.
#[derive(Resource, Debug, Default)]
pub struct SocialRegistry {
    pub by_name: HashMap<String, SocialDef>,
}

impl SocialRegistry {
    pub fn get(&self, name: &str) -> Option<&SocialDef> {
        self.by_name.get(&name.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone)]
pub struct SocialDef {
    pub name: String,
    pub hide: bool,
    pub char_no_arg: Option<String>,
    pub others_no_arg: Option<String>,
    pub char_found: Option<String>,
    pub others_found: Option<String>,
    pub vict_found: Option<String>,
    pub not_found: Option<String>,
    pub char_auto: Option<String>,
    pub others_auto: Option<String>,
}
