//! Quest dialogue runtime (Wave 4.11).
//!
//! `DialogueCatalog` is the in-memory mirror of `DialogueTree` /
//! `DialogueNode` / `DialogueResponse` rows. The loader fills it at
//! boot. `ActiveQuestDialogues` tracks each player's current
//! position inside a dialogue tree (which node they're on) so the
//! next `say`/`ask` can match against that node's responses.
//!
//! Wire-in points:
//!   - Loader: `init_dialogue_catalog` reads all three tables, plus
//!     `QuestDialogue` for the per-objective binding.
//!   - `cmd_say` / `cmd_ask` after the usual TALK_TO_NPC progress
//!     bump: call `try_advance_dialogue` to check whether the
//!     player's utterance matches a keyword.

#![allow(clippy::doc_markdown)]

use std::collections::HashMap;

use bevy_ecs::prelude::*;

/// One dialogue node, indexed by `(tree_id, node_id)` in the
/// catalog. `is_root` is consumed at load time (populates
/// `root_node_by_tree`); kept on the node for round-trip
/// integrity when admin tooling dumps the catalog.
#[derive(Debug, Clone)]
pub(crate) struct DialogueNode {
    pub id: i32,
    pub npc_message: String,
    #[allow(dead_code)] // surfaced via root_node_by_tree at load
    pub is_root: bool,
    pub is_terminal: bool,
    /// Responses ordered by `order` then id.
    pub responses: Vec<DialogueResponse>,
}

#[derive(Debug, Clone)]
pub(crate) struct DialogueResponse {
    pub next_node_id: Option<i32>,
    pub match_type: String, // "EXACT" | "CONTAINS" | "STARTS_WITH" | "ANY_OF" | "REGEX"
    pub match_keywords: Vec<String>,
    /// Optional builder-authored hint surfaced via admin tooling
    /// (e.g. "say 'yes'"). Currently stored only — no command yet
    /// exposes it to players.
    #[allow(dead_code)]
    pub display_hint: Option<String>,
}

/// Full dialogue catalog (Wave 4.11). Keyed by `(tree_id, node_id)`
/// for fast in-runtime lookup. Also indexes
/// `(quest_zone, quest_id, phase, objective)` →
/// `QuestDialogueRow` so the TALK_TO_NPC objective handler can
/// resolve "what should this NPC say to me?"
#[derive(Resource, Default, Debug, Clone)]
pub(crate) struct DialogueCatalog {
    /// `tree_id` → list of nodes for that tree.
    pub nodes_by_tree: HashMap<i32, Vec<DialogueNode>>,
    /// `tree_id` → id of the root node.
    pub root_node_by_tree: HashMap<i32, i32>,
    /// Per-objective dialogue binding.
    pub by_objective:
        HashMap<(i32, i32, i32, i32), mud_db::dialogue::QuestDialogueRow>,
}

impl DialogueCatalog {
    pub fn lookup_objective(
        &self,
        quest_zone: i32,
        quest_id: i32,
        phase: i32,
        objective: i32,
    ) -> Option<&mud_db::dialogue::QuestDialogueRow> {
        self.by_objective
            .get(&(quest_zone, quest_id, phase, objective))
    }

    pub fn node(&self, tree_id: i32, node_id: i32) -> Option<&DialogueNode> {
        self.nodes_by_tree
            .get(&tree_id)
            .and_then(|nodes| nodes.iter().find(|n| n.id == node_id))
    }

    pub fn root_of(&self, tree_id: i32) -> Option<&DialogueNode> {
        let root_id = *self.root_node_by_tree.get(&tree_id)?;
        self.node(tree_id, root_id)
    }
}

/// Per-player tracking of "where is this player in a dialogue
/// tree?" Keyed by player Entity. Stored as a Resource so the
/// `say`/`ask` handler can walk the tree across messages.
#[derive(Resource, Default, Debug, Clone)]
pub(crate) struct ActiveQuestDialogues {
    /// `Entity` (as u64 bits) → `(tree_id, current_node_id)`.
    pub by_player: HashMap<u64, (i32, i32)>,
}

/// String-match dispatch (Wave 4.11). Supported types:
/// EXACT / CONTAINS / STARTS_WITH / ANY_OF / REGEX.
///
/// Case-insensitive across the board — players say "PALADIN" or
/// "paladin" and either should match. REGEX patterns are compiled
/// per-utterance with a leading `(?i)` so the same case-insensitive
/// semantic applies; invalid patterns log a warning and fall through
/// to CONTAINS for that keyword (a typo in the regex shouldn't be
/// silently no-match-everything for the player).
pub(crate) fn matches(
    utterance: &str,
    match_type: &str,
    keywords: &[String],
) -> bool {
    let u = utterance.to_ascii_lowercase();
    match match_type {
        "EXACT" => keywords
            .iter()
            .any(|k| u == k.to_ascii_lowercase()),
        "CONTAINS" => keywords
            .iter()
            .any(|k| u.contains(&k.to_ascii_lowercase())),
        "STARTS_WITH" => keywords
            .iter()
            .any(|k| u.starts_with(&k.to_ascii_lowercase())),
        "ANY_OF" => {
            // ANY_OF is documented as "any token in the utterance
            // matches one keyword" — treat as whitespace-split
            // CONTAINS.
            let tokens: Vec<String> = u.split_whitespace().map(str::to_string).collect();
            keywords.iter().any(|k| {
                let lk = k.to_ascii_lowercase();
                tokens.iter().any(|t| *t == lk)
            })
        }
        "REGEX" => keywords.iter().any(|k| {
            // Compile per-call; for MUD-scale dialogue corpora the
            // hit is trivial vs. the host-app DB round trips that
            // bracket this call.
            let pattern = format!("(?i){k}");
            match regex::Regex::new(&pattern) {
                Ok(re) => re.is_match(utterance),
                Err(e) => {
                    tracing::warn!(
                        keyword = %k,
                        error = %e,
                        "dialogue REGEX keyword failed to compile; falling back to CONTAINS",
                    );
                    u.contains(&k.to_ascii_lowercase())
                }
            }
        }),
        _ => false,
    }
}

/// Fast-path advance when the player is mid-tree (Wave 4.13).
/// Returns `Some(npc_message)` when an active tree's current node
/// matches the utterance — bumps the tracker forward and yields
/// the next node's `npc_message` for the caller to emit. Returns
/// `None` when no tracker is active, no response matches, or the
/// tracker walks off the end (terminal node reached).
///
/// Separate from `try_advance_dialogue` because the mid-tree case
/// needs zero DB access — purely in-memory catalog walk — and the
/// caller (synchronous `cmd_ask`) wants to short-circuit before
/// it spawns the async dialogue-attempt task.
pub(crate) fn try_advance_active_tree(
    world: &mut World,
    player: Entity,
    utterance: &str,
) -> Option<String> {
    let player_bits = player.to_bits();
    let (tree_id, current_node_id) = world
        .get_resource::<ActiveQuestDialogues>()?
        .by_player
        .get(&player_bits)
        .copied()?;
    let catalog = world.get_resource::<DialogueCatalog>()?.clone();
    let node = catalog.node(tree_id, current_node_id)?;
    for resp in &node.responses {
        if !matches(utterance, &resp.match_type, &resp.match_keywords) {
            continue;
        }
        // Walk to the next node when present; otherwise terminate.
        if let Some(next_id) = resp.next_node_id
            && let Some(next) = catalog.node(tree_id, next_id)
        {
            if next.is_terminal {
                if let Some(mut a) = world.get_resource_mut::<ActiveQuestDialogues>() {
                    a.by_player.remove(&player_bits);
                }
            } else if let Some(mut a) =
                world.get_resource_mut::<ActiveQuestDialogues>()
            {
                a.by_player.insert(player_bits, (tree_id, next.id));
            }
            return Some(next.npc_message.clone());
        }
        // Response matched but has no next node — terminate the
        // active tree. Caller still gets `None` (no reply line)
        // because the response itself had nothing to say.
        if let Some(mut a) = world.get_resource_mut::<ActiveQuestDialogues>() {
            a.by_player.remove(&player_bits);
        }
        return None;
    }
    None
}

/// Try to advance the dialogue when a player says/asks something
/// to a TALK_TO_NPC mob (Wave 4.11). Returns `Some(npc_message)`
/// when a keyword matched (caller emits it to the room as the NPC
/// reply); `None` when no match.
///
/// Two layers:
/// 1. If the player is mid-tree (`ActiveQuestDialogues`), match
///    against the current node's responses; advance to next node
///    on match.
/// 2. If not mid-tree, check the QuestDialogue row's own
///    `match_keywords`; on match, if the row has a linked tree,
///    enter the tree at its root.
#[allow(dead_code)] // composed of `try_advance_active_tree` + the dispatch path
pub(crate) fn try_advance_dialogue(
    world: &mut World,
    player: Entity,
    quest_zone: i32,
    quest_id: i32,
    phase: i32,
    objective: i32,
    utterance: &str,
) -> Option<String> {
    let player_bits = player.to_bits();
    // Snapshot to release the resource borrow before mutating.
    let active = world
        .get_resource::<ActiveQuestDialogues>()
        .and_then(|a| a.by_player.get(&player_bits).copied());
    let catalog = world.get_resource::<DialogueCatalog>()?.clone();
    if let Some((tree_id, current_node_id)) = active
        && let Some(node) = catalog.node(tree_id, current_node_id)
    {
        for resp in &node.responses {
            if matches(utterance, &resp.match_type, &resp.match_keywords) {
                // Advance.
                if let Some(next_id) = resp.next_node_id
                    && let Some(next) = catalog.node(tree_id, next_id)
                {
                    if next.is_terminal {
                        if let Some(mut a) =
                            world.get_resource_mut::<ActiveQuestDialogues>()
                        {
                            a.by_player.remove(&player_bits);
                        }
                    } else if let Some(mut a) =
                        world.get_resource_mut::<ActiveQuestDialogues>()
                    {
                        a.by_player.insert(player_bits, (tree_id, next.id));
                    }
                    return Some(next.npc_message.clone());
                }
                // No next node — terminate.
                if let Some(mut a) = world.get_resource_mut::<ActiveQuestDialogues>() {
                    a.by_player.remove(&player_bits);
                }
                return None;
            }
        }
        return None;
    }
    // Not mid-tree: match against the QuestDialogue row's keywords.
    let row = catalog.lookup_objective(quest_zone, quest_id, phase, objective)?;
    if !matches(utterance, &row.match_type, &row.match_keywords) {
        return None;
    }
    // Optionally enter a linked tree at its root.
    if let Some(tree_id) = row.dialogue_tree_id
        && let Some(root) = catalog.root_of(tree_id)
    {
        if !root.is_terminal
            && let Some(mut a) = world.get_resource_mut::<ActiveQuestDialogues>()
        {
            a.by_player.insert(player_bits, (tree_id, root.id));
        }
        return Some(root.npc_message.clone());
    }
    // No tree — the QuestDialogue row's own `npc_message` is the
    // immediate response.
    Some(row.npc_message.clone())
}

/// Loader entry: hydrate the DialogueCatalog from the DB. Call
/// once at boot after `init_resources`.
pub(crate) async fn load_catalog(
    world: &mut World,
    pool: &mud_db::sqlx::PgPool,
) -> Result<(), mud_db::sqlx::Error> {
    let trees = mud_db::dialogue::list_trees(pool).await?;
    let nodes = mud_db::dialogue::list_nodes(pool).await?;
    let responses = mud_db::dialogue::list_responses(pool).await?;
    let dialogues = mud_db::dialogue::list_quest_dialogues(pool).await?;

    let mut catalog = DialogueCatalog::default();
    // Index responses by node_id.
    let mut resp_by_node: HashMap<i32, Vec<DialogueResponse>> = HashMap::new();
    for r in responses {
        resp_by_node.entry(r.node_id).or_default().push(DialogueResponse {
            next_node_id: r.next_node_id,
            match_type: r.match_type,
            match_keywords: r.match_keywords,
            display_hint: r.display_hint,
        });
    }
    // Group nodes by tree.
    for node in nodes {
        let resps = resp_by_node.remove(&node.id).unwrap_or_default();
        if node.is_root {
            catalog.root_node_by_tree.insert(node.dialogue_tree_id, node.id);
        }
        catalog
            .nodes_by_tree
            .entry(node.dialogue_tree_id)
            .or_default()
            .push(DialogueNode {
                id: node.id,
                npc_message: node.npc_message,
                is_root: node.is_root,
                is_terminal: node.is_terminal,
                responses: resps,
            });
    }
    // Trees themselves (drop unused; we just need their ids to be
    // valid in `nodes_by_tree`).
    let _ = trees;

    for d in dialogues {
        catalog.by_objective.insert(
            (d.quest_zone_id, d.quest_id, d.phase_id, d.objective_id),
            d,
        );
    }
    world.insert_resource(catalog);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn matches_exact_is_case_insensitive() {
        assert!(matches("PALADIN", "EXACT", &kw(&["paladin"])));
        assert!(matches("paladin", "EXACT", &kw(&["PALADIN"])));
        // "paladin oath" is not the exact word "paladin"
        assert!(!matches("paladin oath", "EXACT", &kw(&["paladin"])));
    }

    #[test]
    fn matches_contains_substring() {
        assert!(matches("tell me about paladin oaths", "CONTAINS", &kw(&["paladin"])));
        assert!(!matches("druid lore", "CONTAINS", &kw(&["paladin"])));
        // Case-insensitive too.
        assert!(matches("THE Paladin Code", "CONTAINS", &kw(&["paladin"])));
    }

    #[test]
    fn matches_starts_with() {
        assert!(matches("paladin tell me more", "STARTS_WITH", &kw(&["paladin"])));
        assert!(!matches("about paladin", "STARTS_WITH", &kw(&["paladin"])));
    }

    #[test]
    fn matches_any_of_tokens() {
        // "yes" is a whole word in the utterance.
        assert!(matches("yes please", "ANY_OF", &kw(&["yes", "no"])));
        assert!(matches("absolutely no thanks", "ANY_OF", &kw(&["yes", "no"])));
        // Substring inside another word does NOT count for ANY_OF.
        assert!(!matches("noisy", "ANY_OF", &kw(&["no"])));
    }

    #[test]
    fn matches_regex_compiles_pattern() {
        // A real regex with alternation + glob.
        assert!(matches(
            "hello strange world",
            "REGEX",
            &kw(&[r"hello.*world"]),
        ));
        assert!(!matches(
            "hello strange friend",
            "REGEX",
            &kw(&[r"hello.*world"]),
        ));
        // Anchored, with character classes.
        assert!(matches(
            "yes, please",
            "REGEX",
            &kw(&[r"^(yes|aye|sure)\b"]),
        ));
        // Case-insensitive prefix.
        assert!(matches("YES!", "REGEX", &kw(&[r"yes"])));
    }

    #[test]
    fn matches_regex_invalid_pattern_falls_back_to_contains() {
        // Unbalanced bracket — does not compile. The keyword is also
        // a substring of the utterance, so the CONTAINS fallback
        // matches and the trigger still fires.
        assert!(matches("noisy weasel [bad", "REGEX", &kw(&["[bad"])));
        // Invalid pattern + keyword not a substring → no match.
        assert!(!matches("quiet weasel", "REGEX", &kw(&["[bad"])));
    }

    #[test]
    fn matches_unknown_type_is_no_match() {
        assert!(!matches("yes", "FOOBAR", &kw(&["yes"])));
    }

    #[test]
    fn matches_empty_keywords_never_matches() {
        assert!(!matches("anything", "EXACT", &[]));
        assert!(!matches("anything", "CONTAINS", &[]));
        assert!(!matches("anything", "STARTS_WITH", &[]));
        assert!(!matches("anything", "ANY_OF", &[]));
    }

    #[test]
    fn dialogue_catalog_root_lookup() {
        let mut cat = DialogueCatalog::default();
        cat.root_node_by_tree.insert(1, 10);
        cat.nodes_by_tree.insert(
            1,
            vec![DialogueNode {
                id: 10,
                npc_message: "hi".into(),
                is_root: true,
                is_terminal: false,
                responses: vec![],
            }],
        );
        let root = cat.root_of(1).expect("root present");
        assert_eq!(root.id, 10);
        assert_eq!(root.npc_message, "hi");
        // Unknown tree → None.
        assert!(cat.root_of(99).is_none());
    }

    #[test]
    fn dialogue_catalog_node_lookup() {
        let mut cat = DialogueCatalog::default();
        cat.nodes_by_tree.insert(
            7,
            vec![
                DialogueNode {
                    id: 1,
                    npc_message: "first".into(),
                    is_root: true,
                    is_terminal: false,
                    responses: vec![],
                },
                DialogueNode {
                    id: 2,
                    npc_message: "second".into(),
                    is_root: false,
                    is_terminal: true,
                    responses: vec![],
                },
            ],
        );
        assert_eq!(cat.node(7, 1).unwrap().npc_message, "first");
        assert_eq!(cat.node(7, 2).unwrap().npc_message, "second");
        assert!(cat.node(7, 999).is_none());
        assert!(cat.node(99, 1).is_none());
    }

    #[test]
    fn active_dialogues_tracks_per_player() {
        let mut a = ActiveQuestDialogues::default();
        a.by_player.insert(123, (1, 5));
        a.by_player.insert(456, (2, 7));
        assert_eq!(a.by_player.get(&123).copied(), Some((1, 5)));
        assert_eq!(a.by_player.get(&456).copied(), Some((2, 7)));
        a.by_player.remove(&123);
        assert!(a.by_player.get(&123).is_none());
    }
}
