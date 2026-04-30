use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shop {
    /// Composite zone of the shop entry.
    pub zone_id: i32,
    /// Local id of the shop entry within its zone.
    pub id: i32,
    /// `(zone, id)` of the keeper mob that fronts this shop.
    pub keeper_zone_id: i32,
    pub keeper_id: i32,
    /// Multiplier applied to the object's base cost when the shop sells
    /// to the player. `1.0` = sell at base cost.
    pub buy_profit: f64,
    /// Multiplier applied to the object's base cost when the shop buys
    /// back from the player. `< 1.0` = pay less than base.
    pub sell_profit: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShopItem {
    pub shop_zone_id: i32,
    pub shop_id: i32,
    /// `(zone, id)` of the object proto being offered.
    pub object_zone_id: i32,
    pub object_id: i32,
    /// `-1` = unlimited stock, `>=0` = current stock count.
    pub amount: i32,
    /// Override price in copper; `0` falls back to the object's base
    /// cost.
    pub price: i32,
}

pub async fn list_shops(pool: &PgPool) -> sqlx::Result<Vec<Shop>> {
    sqlx::query_as!(
        Shop,
        r#"
        SELECT
            zone_id,
            id,
            keeper_zone_id,
            keeper_id,
            buy_profit::float8 AS "buy_profit!: f64",
            sell_profit::float8 AS "sell_profit!: f64"
        FROM "Shops"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn list_shop_items(pool: &PgPool) -> sqlx::Result<Vec<ShopItem>> {
    sqlx::query_as!(
        ShopItem,
        r#"
        SELECT
            shop_zone_id,
            shop_id,
            object_zone_id,
            object_id,
            amount,
            price
        FROM "ShopItems"
        ORDER BY shop_zone_id, shop_id, object_zone_id, object_id
        "#
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShopAccept {
    pub shop_zone_id: i32,
    pub shop_id: i32,
    /// `ObjectType` enum label (`WEAPON`, `ARMOR`, etc.). Compared as
    /// a string because the runtime's `ObjectType::label()` returns
    /// the matching token.
    pub object_type: String,
    /// Optional keyword whitelist. Empty means "any item of that
    /// type is accepted"; non-empty means at least one keyword from
    /// the item's `Keywords` must match (case-insensitive).
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShopMob {
    pub shop_zone_id: i32,
    pub shop_id: i32,
    pub mob_zone_id: i32,
    pub mob_id: i32,
    pub amount: i32,
    pub price: i32,
}

pub async fn list_shop_mobs(pool: &PgPool) -> sqlx::Result<Vec<ShopMob>> {
    sqlx::query_as!(
        ShopMob,
        r#"
        SELECT
            shop_zone_id,
            shop_id,
            mob_zone_id,
            mob_id,
            amount,
            price
        FROM "ShopMobs"
        ORDER BY shop_zone_id, shop_id, mob_zone_id, mob_id
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn list_shop_accepts(pool: &PgPool) -> sqlx::Result<Vec<ShopAccept>> {
    sqlx::query_as!(
        ShopAccept,
        r#"
        SELECT
            shop_zone_id,
            shop_id,
            type AS "object_type!: String",
            keywords AS "keywords!: Vec<String>"
        FROM "ShopAccepts"
        ORDER BY shop_zone_id, shop_id
        "#
    )
    .fetch_all(pool)
    .await
}
