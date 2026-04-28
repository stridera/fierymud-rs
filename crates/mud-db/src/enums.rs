use serde::{Deserialize, Serialize};

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ResetMode", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResetMode {
    Never,
    Empty,
    Normal,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Hemisphere", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Hemisphere {
    Northwest,
    Northeast,
    Southwest,
    Southeast,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Climate", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Climate {
    None,
    Semiarid,
    Arid,
    Oceanic,
    Temperate,
    Subtropical,
    Tropical,
    Subarctic,
    Arctic,
    Alpine,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Sector", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Sector {
    Structure,
    City,
    Field,
    Forest,
    Hills,
    Mountain,
    Shallows,
    Water,
    Underwater,
    Air,
    Road,
    Grasslands,
    Cave,
    Ruins,
    Swamp,
    Beach,
    Underdark,
    Astralplane,
    Airplane,
    Fireplane,
    Earthplane,
    Etherealplane,
    Avernus,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Direction", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Direction {
    North,
    East,
    South,
    West,
    Up,
    Down,
    Northeast,
    Northwest,
    Southeast,
    Southwest,
    In,
    Out,
    Portal,
    None,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ExitState", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExitState {
    Open,
    Closed,
    Locked,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ObjectType", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectType {
    Nothing,
    Light,
    Scroll,
    Wand,
    Staff,
    Weapon,
    Fireweapon,
    Missile,
    Treasure,
    Armor,
    Potion,
    Worn,
    Other,
    Trash,
    Trap,
    Container,
    Note,
    Drinkcontainer,
    Key,
    Food,
    Money,
    Pen,
    Boat,
    Fountain,
    Portal,
    Rope,
    Spellbook,
    Wall,
    Touchstone,
    Board,
    Instrument,
    Vehicle,
    Corpse,
    Kit,
    Wings,
    Perfume,
    Disguise,
    Poison,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "MobRole", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MobRole {
    Trash,
    Normal,
    Elite,
    Miniboss,
    Boss,
    RaidBoss,
}
