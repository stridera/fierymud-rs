//! Quest dialogue runtime (Wave 4.11).
//!
//! `DialogueCatalog` is the in-memory mirror of `DialogueTree` /
//! `DialogueNode` / `DialogueResponse` rows. The loader fills it at
//! boot. `ActiveQuestDialogues` tracks each player's current
//! position inside a dialogue tree (which node they're on) so the
//! next `say`/`ask` can match against that node's responses.
//!
//! Wire-in points:
//!   - Loader: `load_catalog` reads all three tables, plus
//!     `QuestDialogue` for the per-objective binding.
//!   - `cmd_ask` on a mob: [`try_advance_active_tree`] first walks a
//!     conversation the player is already in; otherwise the TALK_TO_NPC
//!     bump (`bump_talk_quest_progress`) gates each objective on its
//!     `QuestDialogue` keywords and opens the conversation via
//!     [`open_dialogue`].

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
    /// Optional builder-authored hint shown to the player under the
    /// NPC's line (e.g. "Ask about the paladin path").
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
    pub by_objective: HashMap<(i32, i32, i32, i32), mud_db::dialogue::QuestDialogueRow>,
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

/// Where one player is inside a dialogue tree, and which mob they are
/// talking to (asking anyone else ends the conversation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveDialogue {
    pub tree_id: i32,
    pub node_id: i32,
    /// Prototype `(zone, id)` of the mob holding the conversation.
    pub mob: (i32, i32),
}

/// Per-player tracking of "where is this player in a dialogue
/// tree?" Keyed by player Entity bits. Stored as a Resource so the
/// `ask` handler can walk the tree across messages.
#[derive(Resource, Default, Debug, Clone)]
pub(crate) struct ActiveQuestDialogues {
    pub by_player: HashMap<u64, ActiveDialogue>,
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
pub(crate) fn matches(utterance: &str, match_type: &str, keywords: &[String]) -> bool {
    let u = utterance.to_ascii_lowercase();
    match match_type {
        "EXACT" => keywords.iter().any(|k| u == k.to_ascii_lowercase()),
        "CONTAINS" => keywords.iter().any(|k| u.contains(&k.to_ascii_lowercase())),
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
                tokens.contains(&lk)
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

/// Does `topic` satisfy the opening keywords of an objective's
/// dialogue? A binding with no keywords accepts any topic.
pub(crate) fn binding_matches(row: &mud_db::dialogue::QuestDialogueRow, topic: &str) -> bool {
    row.match_keywords.is_empty() || matches(topic, &row.match_type, &row.match_keywords)
}

/// What the NPC says when a conversation is opened, and where in the
/// tree the player now stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogueOpen {
    pub message: String,
    /// Builder hints for the replies the node accepts.
    pub hints: Vec<String>,
    /// `(tree, node)` to track from now on; `None` for a single-line
    /// reply or a terminal node.
    pub enter: Option<(i32, i32)>,
}

fn hints_of(node: &DialogueNode) -> Vec<String> {
    node.responses
        .iter()
        .filter_map(|r| r.display_hint.clone())
        .filter(|h| !h.trim().is_empty())
        .collect()
}

/// Open the conversation bound to an objective. With a linked tree the
/// root node speaks and the player is tracked from it; otherwise the
/// binding's own `npc_message` is the whole reply.
pub(crate) fn open_dialogue(
    catalog: &DialogueCatalog,
    row: &mud_db::dialogue::QuestDialogueRow,
) -> DialogueOpen {
    if let Some(tree_id) = row.dialogue_tree_id
        && let Some(root) = catalog.root_of(tree_id)
    {
        return DialogueOpen {
            message: root.npc_message.clone(),
            hints: if root.is_terminal {
                Vec::new()
            } else {
                hints_of(root)
            },
            enter: (!root.is_terminal).then_some((tree_id, root.id)),
        };
    }
    DialogueOpen {
        message: row.npc_message.clone(),
        hints: Vec::new(),
        enter: None,
    }
}

/// Keep the TALK_TO_NPC rows `topic` is allowed to advance: rows with
/// no dialogue bound pass through; a bound row needs its opening
/// keywords matched. Also returns the conversation the first matched
/// binding opens.
pub(crate) fn gate_talk_rows(
    rows: Vec<mud_db::quest_objectives::ObjectiveProgressRow>,
    catalog: &DialogueCatalog,
    topic: &str,
) -> (
    Vec<mud_db::quest_objectives::ObjectiveProgressRow>,
    Option<DialogueOpen>,
) {
    let mut opened = None;
    let kept = rows
        .into_iter()
        .filter(|row| {
            let Some(binding) = catalog.lookup_objective(
                row.quest_zone_id,
                row.quest_id,
                row.phase_id,
                row.objective_id,
            ) else {
                return true;
            };
            if !binding_matches(binding, topic) {
                return false;
            }
            if opened.is_none() {
                opened = Some(open_dialogue(catalog, binding));
            }
            true
        })
        .collect();
    (kept, opened)
}

/// Walk the conversation a player is already in (Wave 4.13).
/// Returns the next node's reply when `utterance` matches one of the
/// current node's responses, moving (or ending, at a terminal node or
/// a response with no next node) the tracker. Returns `None` when the
/// player is not in a conversation with `mob`, or nothing matched, so
/// the caller falls back to the objective path. Purely in-memory.
///
/// Asking a different mob than the conversation's ends the
/// conversation.
pub(crate) fn try_advance_active_tree(
    world: &mut World,
    player: Entity,
    mob: (i32, i32),
    utterance: &str,
) -> Option<DialogueOpen> {
    let player_bits = player.to_bits();
    let active = world
        .get_resource::<ActiveQuestDialogues>()?
        .by_player
        .get(&player_bits)
        .copied()?;
    if active.mob != mob {
        world
            .resource_mut::<ActiveQuestDialogues>()
            .by_player
            .remove(&player_bits);
        return None;
    }
    let catalog = world.get_resource::<DialogueCatalog>()?;
    let node = catalog.node(active.tree_id, active.node_id)?;
    let resp = node
        .responses
        .iter()
        .find(|r| matches(utterance, &r.match_type, &r.match_keywords))?;
    let next = resp
        .next_node_id
        .and_then(|id| catalog.node(active.tree_id, id))
        .cloned();
    let mut tracker = world.resource_mut::<ActiveQuestDialogues>();
    let Some(next) = next else {
        // The response ended the conversation without a reply.
        tracker.by_player.remove(&player_bits);
        return None;
    };
    let open = DialogueOpen {
        message: next.npc_message.clone(),
        hints: if next.is_terminal {
            Vec::new()
        } else {
            hints_of(&next)
        },
        enter: (!next.is_terminal).then_some((active.tree_id, next.id)),
    };
    match open.enter {
        Some((tree_id, node_id)) => {
            tracker.by_player.insert(
                player_bits,
                ActiveDialogue {
                    tree_id,
                    node_id,
                    mob,
                },
            );
        }
        None => {
            tracker.by_player.remove(&player_bits);
        }
    }
    Some(open)
}

/// Show the NPC's line (and any reply hints) to the player.
pub(crate) fn say_reply(world: &World, player: Entity, mob_name: &str, open: &DialogueOpen) {
    let mut text = format!("{mob_name} says, \"{}\"\r\n", open.message);
    for hint in &open.hints {
        text.push_str(&format!("  - {hint}\r\n"));
    }
    crate::commands::send_to(world, player, text);
}

/// Deliver an opened conversation: show it and, for a tree, start
/// tracking the player at its current node.
pub(crate) fn deliver_reply(
    world: &mut World,
    player: Entity,
    mob_name: &str,
    mob: (i32, i32),
    open: &DialogueOpen,
) {
    say_reply(world, player, mob_name, open);
    let bits = player.to_bits();
    if let Some((tree_id, node_id)) = open.enter {
        if let Some(mut tracker) = world.get_resource_mut::<ActiveQuestDialogues>() {
            tracker.by_player.insert(
                bits,
                ActiveDialogue {
                    tree_id,
                    node_id,
                    mob,
                },
            );
        }
    } else if let Some(mut tracker) = world.get_resource_mut::<ActiveQuestDialogues>() {
        tracker.by_player.remove(&bits);
    }
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
        resp_by_node
            .entry(r.node_id)
            .or_default()
            .push(DialogueResponse {
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
            catalog
                .root_node_by_tree
                .insert(node.dialogue_tree_id, node.id);
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
        catalog
            .by_objective
            .insert((d.quest_zone_id, d.quest_id, d.phase_id, d.objective_id), d);
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
        assert!(matches(
            "tell me about paladin oaths",
            "CONTAINS",
            &kw(&["paladin"])
        ));
        assert!(!matches("druid lore", "CONTAINS", &kw(&["paladin"])));
        // Case-insensitive too.
        assert!(matches("THE Paladin Code", "CONTAINS", &kw(&["paladin"])));
    }

    #[test]
    fn matches_starts_with() {
        assert!(matches(
            "paladin tell me more",
            "STARTS_WITH",
            &kw(&["paladin"])
        ));
        assert!(!matches("about paladin", "STARTS_WITH", &kw(&["paladin"])));
    }

    #[test]
    fn matches_any_of_tokens() {
        // "yes" is a whole word in the utterance.
        assert!(matches("yes please", "ANY_OF", &kw(&["yes", "no"])));
        assert!(matches(
            "absolutely no thanks",
            "ANY_OF",
            &kw(&["yes", "no"])
        ));
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
        let at = |tree_id, node_id| ActiveDialogue {
            tree_id,
            node_id,
            mob: (30, 1),
        };
        a.by_player.insert(123, at(1, 5));
        a.by_player.insert(456, at(2, 7));
        assert_eq!(a.by_player.get(&123).copied(), Some(at(1, 5)));
        assert_eq!(a.by_player.get(&456).copied(), Some(at(2, 7)));
        a.by_player.remove(&123);
        assert!(!a.by_player.contains_key(&123));
    }

    // ---- tree walking (2-node tree) ----

    fn node(id: i32, msg: &str, terminal: bool, responses: Vec<DialogueResponse>) -> DialogueNode {
        DialogueNode {
            id,
            npc_message: msg.into(),
            is_root: id == 10,
            is_terminal: terminal,
            responses,
        }
    }

    /// Tree 1: root 10 "Do you seek the path?" --yes--> terminal 11.
    fn two_node_catalog() -> DialogueCatalog {
        let mut cat = DialogueCatalog::default();
        cat.root_node_by_tree.insert(1, 10);
        cat.nodes_by_tree.insert(
            1,
            vec![
                node(
                    10,
                    "Do you seek the path?",
                    false,
                    vec![DialogueResponse {
                        next_node_id: Some(11),
                        match_type: "ANY_OF".into(),
                        match_keywords: kw(&["yes", "aye"]),
                        display_hint: Some("Say yes".into()),
                    }],
                ),
                node(11, "Then go north.", true, vec![]),
            ],
        );
        cat.by_objective
            .insert((30, 5, 1, 1), binding(Some(1), &["path"]));
        cat
    }

    fn binding(tree: Option<i32>, words: &[&str]) -> mud_db::dialogue::QuestDialogueRow {
        mud_db::dialogue::QuestDialogueRow {
            id: 1,
            quest_zone_id: 30,
            quest_id: 5,
            phase_id: 1,
            objective_id: 1,
            npc_message: "Hello there.".into(),
            match_type: "CONTAINS".into(),
            match_keywords: kw(words),
            dialogue_tree_id: tree,
        }
    }

    fn talk_row(objective_id: i32) -> mud_db::quest_objectives::ObjectiveProgressRow {
        mud_db::quest_objectives::ObjectiveProgressRow {
            character_quest_id: "cq".into(),
            quest_zone_id: 30,
            quest_id: 5,
            phase_id: 1,
            objective_id,
            required_count: 1,
            scope: "SOLO".into(),
            show_progress: true,
            player_description: "talk".into(),
            current_count: 0,
        }
    }

    fn world_with(cat: DialogueCatalog) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(cat);
        world.insert_resource(ActiveQuestDialogues::default());
        let player = world.spawn_empty().id();
        (world, player)
    }

    #[test]
    fn keywords_gate_the_opening_and_empty_keywords_accept_anything() {
        let b = binding(None, &["paladin"]);
        assert!(binding_matches(&b, "tell me of the Paladin way"));
        assert!(!binding_matches(&b, "hello"));
        assert!(binding_matches(&binding(None, &[]), "hello"));
    }

    #[test]
    fn gate_drops_unmatched_bound_rows_and_keeps_unbound_ones() {
        let cat = two_node_catalog();
        // Objective 1 is bound (keyword "path"); objective 2 has no dialogue.
        let rows = vec![talk_row(1), talk_row(2)];
        let (kept, opened) = gate_talk_rows(rows.clone(), &cat, "hello");
        assert_eq!(
            kept.iter().map(|r| r.objective_id).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(opened.is_none(), "nothing matched, nobody speaks");

        let (kept, opened) = gate_talk_rows(rows, &cat, "show me the path");
        assert_eq!(kept.len(), 2);
        let open = opened.expect("binding matched");
        assert_eq!(open.message, "Do you seek the path?");
        assert_eq!(open.hints, vec!["Say yes".to_string()]);
        assert_eq!(open.enter, Some((1, 10)));
    }

    #[test]
    fn a_binding_without_a_tree_replies_with_its_own_message() {
        let cat = DialogueCatalog::default();
        let open = open_dialogue(&cat, &binding(None, &["hi"]));
        assert_eq!(open.message, "Hello there.");
        assert_eq!(open.enter, None);
    }

    #[test]
    fn two_node_tree_walks_to_the_terminal_node() {
        let (mut world, player) = world_with(two_node_catalog());
        let mob = (30, 1);
        // Opening the conversation starts tracking at the root.
        let cat = world.resource::<DialogueCatalog>().clone();
        let open = open_dialogue(&cat, cat.lookup_objective(30, 5, 1, 1).unwrap());
        deliver_reply(&mut world, player, "Sage", mob, &open);
        assert_eq!(
            world.resource::<ActiveQuestDialogues>().by_player[&player.to_bits()],
            ActiveDialogue {
                tree_id: 1,
                node_id: 10,
                mob
            }
        );

        // Off-script words do nothing and keep the conversation open.
        assert!(try_advance_active_tree(&mut world, player, mob, "banana").is_none());
        assert!(
            world
                .resource::<ActiveQuestDialogues>()
                .by_player
                .contains_key(&player.to_bits())
        );

        // A matching response moves to the terminal node and ends it.
        let reply = try_advance_active_tree(&mut world, player, mob, "Aye I do").unwrap();
        assert_eq!(reply.message, "Then go north.");
        assert_eq!(reply.enter, None);
        assert!(
            !world
                .resource::<ActiveQuestDialogues>()
                .by_player
                .contains_key(&player.to_bits())
        );
        // Nothing left to walk.
        assert!(try_advance_active_tree(&mut world, player, mob, "yes").is_none());
    }

    #[test]
    fn asking_another_mob_ends_the_conversation() {
        let (mut world, player) = world_with(two_node_catalog());
        world
            .resource_mut::<ActiveQuestDialogues>()
            .by_player
            .insert(
                player.to_bits(),
                ActiveDialogue {
                    tree_id: 1,
                    node_id: 10,
                    mob: (30, 1),
                },
            );
        assert!(try_advance_active_tree(&mut world, player, (30, 2), "yes").is_none());
        assert!(
            world
                .resource::<ActiveQuestDialogues>()
                .by_player
                .is_empty()
        );
    }
}
