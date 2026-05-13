#![allow(clippy::doc_markdown)]
//! Quest dialogue trees (Wave 4.11).
//!
//! Loads `QuestDialogue`, `DialogueTrees`, `DialogueNodes`, and
//! `DialogueResponses` for the runtime catalog. The TALK_TO_NPC
//! path uses `lookup_quest_dialogue` to find the dialogue bound to
//! the current objective, then steps through nodes/responses by
//! keyword match.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogueTreeRow {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogueNodeRow {
    pub id: i32,
    pub dialogue_tree_id: i32,
    pub npc_message: String,
    pub order: i32,
    pub is_root: bool,
    pub is_terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogueResponseRow {
    pub id: i32,
    pub node_id: i32,
    pub next_node_id: Option<i32>,
    pub match_type: String,
    pub match_keywords: Vec<String>,
    pub display_hint: Option<String>,
    pub order: i32,
}

/// One QuestDialogue row joined with its parent tree id. Bound to
/// a specific TALK_TO_NPC objective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestDialogueRow {
    pub id: i32,
    pub quest_zone_id: i32,
    pub quest_id: i32,
    pub phase_id: i32,
    pub objective_id: i32,
    pub npc_message: String,
    pub match_type: String,
    pub match_keywords: Vec<String>,
    pub dialogue_tree_id: Option<i32>,
}

/// All dialogue trees + nodes + responses in one shot. Caller
/// (loader) groups by `(tree.id)` and builds the in-memory catalog.
pub async fn list_trees(pool: &PgPool) -> sqlx::Result<Vec<DialogueTreeRow>> {
    sqlx::query_as!(
        DialogueTreeRow,
        r#"
        SELECT id AS "id!: i32", name AS "name!: String"
        FROM "DialogueTree"
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Every node across every tree. Caller groups by `dialogue_tree_id`.
pub async fn list_nodes(pool: &PgPool) -> sqlx::Result<Vec<DialogueNodeRow>> {
    sqlx::query_as!(
        DialogueNodeRow,
        r#"
        SELECT
            id AS "id!: i32",
            dialogue_tree_id AS "dialogue_tree_id!: i32",
            npc_message AS "npc_message!: String",
            "order" AS "order!: i32",
            is_root AS "is_root!: bool",
            is_terminal AS "is_terminal!: bool"
        FROM "DialogueNode"
        ORDER BY dialogue_tree_id, "order", id
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Every response across every node.
pub async fn list_responses(pool: &PgPool) -> sqlx::Result<Vec<DialogueResponseRow>> {
    sqlx::query_as!(
        DialogueResponseRow,
        r#"
        SELECT
            id AS "id!: i32",
            node_id AS "node_id!: i32",
            next_node_id,
            match_type::text AS "match_type!: String",
            match_keywords AS "match_keywords!: Vec<String>",
            display_hint,
            "order" AS "order!: i32"
        FROM "DialogueResponse"
        ORDER BY node_id, "order", id
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Every QuestDialogue row. Keyed at runtime by
/// `(quest_zone, quest_id, phase_id, objective_id)`.
pub async fn list_quest_dialogues(pool: &PgPool) -> sqlx::Result<Vec<QuestDialogueRow>> {
    sqlx::query_as!(
        QuestDialogueRow,
        r#"
        SELECT
            id AS "id!: i32",
            quest_zone_id AS "quest_zone_id!: i32",
            quest_id AS "quest_id!: i32",
            phase_id AS "phase_id!: i32",
            objective_id AS "objective_id!: i32",
            npc_message AS "npc_message!: String",
            match_type::text AS "match_type!: String",
            match_keywords AS "match_keywords!: Vec<String>",
            dialogue_tree_id
        FROM "QuestDialogue"
        "#,
    )
    .fetch_all(pool)
    .await
}

/// One-shot lookup for the runtime: find the QuestDialogue bound
/// to `(quest_zone, quest_id, phase_id, objective_id)`. Used when
/// a player says/asks something at a TALK_TO_NPC objective target.
pub async fn lookup_quest_dialogue(
    pool: &PgPool,
    quest_zone_id: i32,
    quest_id: i32,
    phase_id: i32,
    objective_id: i32,
) -> sqlx::Result<Option<QuestDialogueRow>> {
    sqlx::query_as!(
        QuestDialogueRow,
        r#"
        SELECT
            id AS "id!: i32",
            quest_zone_id AS "quest_zone_id!: i32",
            quest_id AS "quest_id!: i32",
            phase_id AS "phase_id!: i32",
            objective_id AS "objective_id!: i32",
            npc_message AS "npc_message!: String",
            match_type::text AS "match_type!: String",
            match_keywords AS "match_keywords!: Vec<String>",
            dialogue_tree_id
        FROM "QuestDialogue"
        WHERE quest_zone_id = $1
          AND quest_id = $2
          AND phase_id = $3
          AND objective_id = $4
        "#,
        quest_zone_id,
        quest_id,
        phase_id,
        objective_id,
    )
    .fetch_optional(pool)
    .await
}
