use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{characters, characters::CharacterRow, sqlx::PgPool, users, users::User};
use mud_net::{ConnId, Outbound};
use mud_db::character_items::CharacterItemRow;
use mud_world::{
    Account, AccountSummary, AttachedTriggers, BankWealth, BoardLink, CombatStats, CoreStats,
    Description, EquippedSlot, Health, Item, Keywords, KnownAbilities, LiquidContainer, Located,
    LoggedInAt, Named, ObjectPrototypes, Online, Player, PlayerFlags, Posture, PostureKind,
    Profile, Prompt, RecallPoint, Slot, Stamina, Title, TriggerCatalog, Wealth, WearableIn,
    WorldKey, WorldKeyIndex, wear_flags_primary_slot,
};
use tracing::{info, warn};

use crate::commands::{self, Connection};

const BANNER: &str = "\r\n=========================================\r\n   fierymud-rs (Rust ECS rewrite)\r\n=========================================\r\n";
/// Combined identifier prompt — accepts either an email or a
/// character name. Email is detected by the presence of '@' (the
/// only thing legacy MUD usernames couldn't legally contain).
const IDENT_PROMPT: &str = "Email or character name: ";
const PASSWORD_PROMPT: &str = "Password: ";

/// Default starting room when a character has no current/recall location set.
/// (0, 0) is "The Void" — fitting.
const FALLBACK_START: (i32, i32) = (0, 0);

pub enum Stage {
    /// Initial prompt accepts either an email (contains `@`) or a
    /// character name. Email path leads to `CharSelect` like before;
    /// character-name path skips the menu and lands directly in the
    /// world after the password check.
    AwaitingIdentifier,
    AwaitingPassword {
        user: User,
        /// Character chosen at the identifier prompt (character-name
        /// path). When `Some`, the menu is skipped on auth success.
        preselected: Option<Box<CharacterRow>>,
    },
    CharSelect { user: User, characters: Vec<CharacterRow> },
}

pub struct LoginCtx {
    pub outbound: Outbound,
    pub stage: Stage,
}

pub struct ConnRouter {
    login: HashMap<ConnId, LoginCtx>,
    playing: HashMap<ConnId, Entity>,
}

impl ConnRouter {
    pub fn new() -> Self {
        Self {
            login: HashMap::new(),
            playing: HashMap::new(),
        }
    }

    pub fn live_connections(&self) -> usize {
        self.login.len() + self.playing.len()
    }

    pub fn on_connect(&mut self, conn_id: ConnId, outbound: Outbound) {
        let _ = outbound.send(BANNER.as_bytes().to_vec());
        let _ = outbound.send(IDENT_PROMPT.as_bytes().to_vec());
        self.login.insert(
            conn_id,
            LoginCtx {
                outbound,
                stage: Stage::AwaitingIdentifier,
            },
        );
    }

    /// Run the `save_player` path for every still-connected character
    /// that has finished login. Called once on graceful shutdown so a
    /// Ctrl-C doesn't lose hp/stamina/inventory/location for whoever
    /// happened to be online — without this, `on_disconnect` only
    /// fires on actual telnet disconnects and Ctrl-C drops the
    /// process before that path runs.
    pub async fn save_all_online(&self, world: &mut World, pool: &PgPool) {
        // Snapshot the (conn_id, entity) pairs so we don't borrow self
        // across `.await` calls — save_player takes &mut World.
        let entries: Vec<Entity> = self.playing.values().copied().collect();
        for entity in entries {
            save_player(world, entity, pool).await;
        }
    }

    pub async fn on_disconnect(&mut self, world: &mut World, conn_id: ConnId, pool: &PgPool) {
        self.login.remove(&conn_id);
        if let Some(entity) = self.playing.remove(&conn_id) {
            save_player(world, entity, pool).await;
            // Despawn the player AND every item they were carrying / wearing.
            // Located(player) catches both inventory and equipped (EquippedSlot
            // is additive, items are still Located on the carrier).
            let items: Vec<Entity> = {
                let mut q = world.query::<(Entity, &Located, &Item)>();
                q.iter(world)
                    .filter(|(_, l, _)| l.0 == entity)
                    .map(|(e, _, _)| e)
                    .collect()
            };
            for item in items {
                world.despawn(item);
            }
            world.despawn(entity);
        }
    }

    pub async fn on_line(
        &mut self,
        conn_id: ConnId,
        text: String,
        pool: &PgPool,
        world: &mut World,
    ) {
        if self.login.contains_key(&conn_id) {
            self.advance_login(conn_id, text, pool, world).await;
        } else if let Some(&entity) = self.playing.get(&conn_id) {
            // Async pre-dispatch: a tight allow-list of commands that
            // need DB access (mail today). Returns true when handled
            // here; falls through to the sync dispatcher otherwise.
            if !commands::try_dispatch_async(world, entity, pool, &text).await {
                commands::dispatch(world, entity, &text);
            }
            // dispatch marks the player for prompt at its start; flush
            // sends one prompt each to the player and to everyone else
            // who received output during the turn.
            commands::flush_prompts(world);
        }
    }

    // The state machine is naturally a sequence of stage-arm bodies; splitting
    // would just hide the linear flow.
    #[allow(clippy::too_many_lines)]
    async fn advance_login(
        &mut self,
        conn_id: ConnId,
        text: String,
        pool: &PgPool,
        world: &mut World,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        let trimmed = text.trim();

        match std::mem::replace(&mut ctx.stage, Stage::AwaitingIdentifier) {
            Stage::AwaitingIdentifier => {
                // Branch on '@' — emails contain it, character names don't.
                // Either path lands in AwaitingPassword; the character path
                // also stashes a preselected character so we can skip the
                // CharSelect menu.
                let is_email = trimmed.contains('@');
                let sentinel_user = || User {
                    id: String::new(),
                    email: trimmed.to_string(),
                    display_name: String::new(),
                    password_hash: None,
                    role: mud_db::enums::UserRole::Player,
                };
                if is_email {
                    let lookup = users::find_by_email(pool, trimmed).await;
                    match lookup {
                        Ok(Some(user)) => {
                            ctx.stage = Stage::AwaitingPassword { user, preselected: None };
                        }
                        Ok(None) => {
                            ctx.stage = Stage::AwaitingPassword {
                                user: sentinel_user(),
                                preselected: None,
                            };
                        }
                        Err(e) => {
                            warn!(conn_id, error = %e, "user lookup failed");
                            let _ = ctx.outbound.send("Server error.\r\n".as_bytes().to_vec());
                            ctx.stage = Stage::AwaitingIdentifier;
                            let _ = ctx.outbound.send(IDENT_PROMPT.as_bytes().to_vec());
                            return;
                        }
                    }
                } else {
                    // Character-name path. Look up the row by name; if found,
                    // resolve its user_id → User. Don't leak whether the name
                    // exists — always advance to password and let bcrypt
                    // verify fail.
                    let char_lookup = characters::find_by_name(pool, trimmed).await;
                    match char_lookup {
                        Ok(Some(c)) => {
                            let user_lookup: Option<User> = match c.user_id.as_deref() {
                                Some(uid) => match users::find_by_id(pool, uid).await {
                                    Ok(u) => u,
                                    Err(e) => {
                                        warn!(conn_id, error = %e, "user lookup failed");
                                        None
                                    }
                                },
                                None => None,
                            };
                            ctx.stage = Stage::AwaitingPassword {
                                user: user_lookup.unwrap_or_else(sentinel_user),
                                preselected: Some(Box::new(c)),
                            };
                        }
                        Ok(None) => {
                            ctx.stage = Stage::AwaitingPassword {
                                user: sentinel_user(),
                                preselected: None,
                            };
                        }
                        Err(e) => {
                            warn!(conn_id, error = %e, "character lookup failed");
                            let _ = ctx.outbound.send("Server error.\r\n".as_bytes().to_vec());
                            ctx.stage = Stage::AwaitingIdentifier;
                            let _ = ctx.outbound.send(IDENT_PROMPT.as_bytes().to_vec());
                            return;
                        }
                    }
                }
                let _ = ctx.outbound.send(PASSWORD_PROMPT.as_bytes().to_vec());
            }

            Stage::AwaitingPassword { user, preselected } => {
                let ok = user
                    .password_hash
                    .as_ref()
                    .is_some_and(|h| bcrypt::verify(trimmed, h).unwrap_or(false));
                if !ok {
                    info!(conn_id, email = %user.email, "auth failure");
                    let _ = ctx.outbound.send("Invalid credentials.\r\n".as_bytes().to_vec());
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                info!(conn_id, user_id = %user.id, email = %user.email, "auth success");

                // Character-name path: preselected character → spawn it
                // directly without showing the CharSelect menu.
                if let Some(char_row) = preselected {
                    self.complete_login(conn_id, world, pool, user, *char_row).await;
                    return;
                }

                // Email path: list all characters and show the menu.
                let chars = match characters::list_for_user(pool, &user.id).await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(conn_id, error = %e, "character list failed");
                        let _ = ctx.outbound.send("Server error.\r\n".as_bytes().to_vec());
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                if chars.is_empty() {
                    let _ = ctx
                        .outbound
                        .send("No characters on this account.\r\n".as_bytes().to_vec());
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                let mut menu = String::from("\r\nCharacters:\r\n");
                for (idx, c) in chars.iter().enumerate() {
                    menu.push_str(&format!("  {}. {} (level {})\r\n", idx + 1, c.name, c.level));
                }
                menu.push_str("Pick a number: ");
                let _ = ctx.outbound.send(menu.into_bytes());
                ctx.stage = Stage::CharSelect {
                    user,
                    characters: chars,
                };
            }

            Stage::CharSelect { user, characters } => {
                let pick = trimmed.parse::<usize>().ok();
                let Some(char_row) = pick
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| characters.get(i))
                    .cloned()
                else {
                    let _ = ctx
                        .outbound
                        .send(format!("Pick 1-{}.\r\n", characters.len()).into_bytes());
                    ctx.stage = Stage::CharSelect { user, characters };
                    return;
                };
                self.complete_login(conn_id, world, pool, user, char_row).await;
            }
        }
    }

    /// Final login step: load the chosen character's saved state
    /// (items / abilities / aliases / account siblings), spawn the
    /// Player entity, and migrate the connection from `login` to
    /// `playing`. Shared by both the email + char-select path and
    /// the direct character-name path.
    async fn complete_login(
        &mut self,
        conn_id: ConnId,
        world: &mut World,
        pool: &PgPool,
        user: User,
        char_row: CharacterRow,
    ) {
        let item_rows = mud_db::character_items::list_for(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "character_items load failed");
                Vec::new()
            });
        let ability_rows = mud_db::character_abilities::list_for(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "character_abilities load failed");
                Vec::new()
            });
        let alias_rows = mud_db::character_aliases::list_for(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "character_aliases load failed");
                Vec::new()
            });
        // AccountSummary lists all sibling characters on the account.
        // Empty list when the character has no associated user (some
        // legacy imports) — the summary just shows the chosen one.
        let all_chars: Vec<CharacterRow> = if user.id.is_empty() {
            vec![char_row.clone()]
        } else {
            characters::list_for_user(pool, &user.id)
                .await
                .unwrap_or_else(|e| {
                    warn!(conn_id, error = %e, "character list failed");
                    vec![char_row.clone()]
                })
        };

        let LoginCtx { outbound, .. } = self.login.remove(&conn_id).unwrap();
        let entity = spawn_player(world, &user, &char_row, outbound);
        let item_count = spawn_inventory(world, entity, &item_rows);
        let known_abilities = KnownAbilities {
            entries: ability_rows
                .iter()
                .map(|r| (r.ability_id, r.proficiency, r.known))
                .collect(),
        };
        let ability_count = known_abilities.entries.len();
        let aliases = mud_world::Aliases {
            entries: alias_rows
                .iter()
                .map(|r| (r.alias.clone(), r.command.clone()))
                .collect(),
        };
        let alias_count = aliases.entries.len();
        let summary = AccountSummary {
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            characters: all_chars
                .iter()
                .map(|c| (c.name.clone(), c.level))
                .collect(),
        };
        if let Ok(mut e) = world.get_entity_mut(entity) {
            e.insert(known_abilities);
            e.insert(aliases);
            e.insert(summary);
            if let Some(t) = char_row.title.as_deref()
                && !t.trim().is_empty()
            {
                e.insert(Title(t.trim().to_string()));
            }
            if let Some(d) = char_row.description.as_deref()
                && !d.trim().is_empty()
            {
                e.insert(Description(d.trim().to_string()));
            }
        }
        self.playing.insert(conn_id, entity);
        commands::send_prompt(world, entity);
        info!(
            conn_id,
            char_name = %char_row.name,
            char_level = char_row.level,
            item_count,
            ability_count,
            alias_count,
            "player spawned"
        );
    }
}

/// Single spawn path for a player entity. The `Located(room_entity)`
/// component is added in a follow-up insert (after spawn) only when
/// the starting room resolved — keeping the core bundle one place
/// avoids the recurring "did I update both branches?" bug we hit
/// three times before consolidating.
pub(crate) fn spawn_player(world: &mut World, user: &User, c: &CharacterRow, outbound: Outbound) -> Entity {
    let (zone, room) = pick_starting_room(c);

    let index = world.resource::<WorldKeyIndex>();
    let room_entity = index
        .rooms
        .get(&(zone, room))
        .copied()
        .or_else(|| index.rooms.get(&FALLBACK_START).copied());
    // Recall point: only set when the row has both coordinates AND the room
    // is loaded. Missing recall is a normal state (`recall` will report it).
    let recall_entity = match (c.recall_room_zone_id, c.recall_room_id) {
        (Some(rz), Some(rr)) => index.rooms.get(&(rz, rr)).copied(),
        _ => None,
    };

    let health = Health {
        hp: c.hit_points,
        max: c.hit_points_max,
    };
    let stamina = Stamina {
        current: c.stamina,
        max: c.stamina_max,
    };
    let combat = CombatStats {
        hit_roll: c.hit_roll,
        dmg_roll: c.damage_roll,
        ac: c.armor_class,
        alignment: c.alignment,
    };
    let core_stats = CoreStats {
        strength: c.strength,
        dexterity: c.dexterity,
        constitution: c.constitution,
        intelligence: c.intelligence,
        wisdom: c.wisdom,
        charisma: c.charisma,
    };

    // Welcome line — only when we have a room to land in.
    if let Some(room_entity) = room_entity {
        let room_name = commands::name_or(world, room_entity, "<unknown>");
        let _ = outbound.send(format!(
            "\r\nWelcome, {name}.\r\nYou appear in: {room_name}\r\n\r\n",
            name = c.name,
        ).into_bytes());
    } else {
        let _ = outbound.send(
            format!(
                "No starting room available (tried ({zone},{room}) and fallback {FALLBACK_START:?}).\r\n",
            )
            .into_bytes(),
        );
    }

    let entity = world
        .spawn((
            Player,
            Online,
            Named { name: c.name.clone() },
            Account {
                user_id: user.id.clone(),
                character_id: c.id.clone(),
                role: user.role,
                perms: c.permissions.clone(),
            },
            Connection(outbound),
            health,
            stamina,
            combat,
            core_stats,
            Posture(PostureKind::Standing),
            PlayerFlags(c.player_flags.clone()),
            Prompt(c.prompt.clone()),
            LoggedInAt(std::time::Instant::now()),
            (
                Profile {
                    level: c.level,
                    class_id: c.class_id,
                    race: c.race.clone(),
                    experience: c.experience,
                    gender: c.gender.clone(),
                },
                Wealth(c.wealth),
                BankWealth(c.bank_wealth),
                mud_world::SkillPoints(c.skill_points),
            ),
        ))
        .id();
    if let Ok(mut e) = world.get_entity_mut(entity) {
        if let Some(re) = room_entity {
            e.insert(Located(re));
        }
        if let Some(re) = recall_entity {
            e.insert(RecallPoint(re));
        }
        // Hunger/Thirst loaded from the row. Tick consumer is a
        // follow-up; for now the gauges just round-trip.
        e.insert(mud_world::Hunger(c.hunger));
        e.insert(mud_world::Thirst(c.thirst));
    }
    entity
}

#[allow(clippy::too_many_lines)]
async fn save_player(world: &mut World, entity: Entity, pool: &PgPool) {
    let Some(account) = world.get::<Account>(entity).cloned() else {
        return;
    };
    let hp = world.get::<Health>(entity).map_or(0, |h| h.hp);
    let stamina = world.get::<Stamina>(entity).map_or(0, |s| s.current);
    let (zone_id, room_id) = world
        .get::<Located>(entity)
        .and_then(|l| world.get::<WorldKey>(l.0).copied())
        .map_or((None, None), |wk| (Some(wk.zone), Some(wk.id)));
    let flags = world
        .get::<PlayerFlags>(entity)
        .map(|f| f.0.clone())
        .unwrap_or_default();
    let prompt = world
        .get::<Prompt>(entity)
        .map(|p| p.0.clone())
        .unwrap_or_default();
    let title = world.get::<Title>(entity).map(|t| t.0.clone());
    let description = world.get::<Description>(entity).map(|d| d.0.clone());
    let wealth = world.get::<Wealth>(entity).map_or(0, |w| w.0);
    let experience = world.get::<Profile>(entity).map_or(0, |p| p.experience);
    let skill_points = world
        .get::<mud_world::SkillPoints>(entity)
        .map_or(0, |s| s.0);
    let hunger = world.get::<mud_world::Hunger>(entity).map_or(0, |h| h.0);
    let thirst = world.get::<mud_world::Thirst>(entity).map_or(0, |t| t.0);
    let (recall_zone, recall_room) = world
        .get::<RecallPoint>(entity)
        .and_then(|r| world.get::<WorldKey>(r.0).copied())
        .map_or((None, None), |wk| (Some(wk.zone), Some(wk.id)));

    // Snapshot every Item rooted at the player — both directly carried
    // and nested inside any container the player carries. BFS keeps
    // parents before children so the eventual save_for can resolve
    // `parent_idx` correctly.
    let new_items: Vec<mud_db::character_items::NewCharacterItem> = {
        use std::collections::HashMap;
        // Snapshot every item entity together with its Located parent and
        // metadata once, so the rest of the loop doesn't reborrow World.
        let all_items: Vec<(Entity, Entity, WorldKey, Option<EquippedSlot>)> = {
            let mut q = world.query::<(Entity, &Located, &WorldKey, Option<&EquippedSlot>, &Item)>();
            q.iter(world)
                .map(|(e, l, wk, eq, _)| (e, l.0, *wk, eq.copied()))
                .collect()
        };
        // BFS from `entity` (the player) through "is parent of" edges.
        let mut order: Vec<(Entity, Entity, WorldKey, Option<EquippedSlot>)> = Vec::new();
        let mut entity_to_idx: HashMap<Entity, usize> = HashMap::new();
        let mut frontier: Vec<Entity> = vec![entity];
        while let Some(parent) = frontier.pop() {
            for (e, p, wk, eq) in &all_items {
                if *p == parent && !entity_to_idx.contains_key(e) {
                    entity_to_idx.insert(*e, order.len());
                    order.push((*e, *p, *wk, *eq));
                    frontier.push(*e);
                }
            }
        }
        order
            .into_iter()
            .map(|(_, parent, wk, eq)| mud_db::character_items::NewCharacterItem {
                object_zone_id: wk.zone,
                object_id: wk.id,
                equipped_location: eq.map(|s| s.0.db_label().to_string()),
                parent_idx: if parent == entity {
                    None
                } else {
                    entity_to_idx.get(&parent).copied()
                },
            })
            .collect()
    };
    let item_count = new_items.len();

    if let Err(e) = characters::save_state(
        pool,
        &account.character_id,
        hp,
        stamina,
        zone_id,
        room_id,
        recall_zone,
        recall_room,
        &flags,
        &prompt,
        title.as_deref(),
        description.as_deref(),
        wealth,
        experience,
        skill_points,
        hunger,
        thirst,
    )
    .await
    {
        warn!(error = %e, character_id = %account.character_id, "save failed");
        return;
    }
    if let Err(e) = mud_db::character_items::save_for(
        pool,
        &account.character_id,
        &new_items,
    )
    .await
    {
        warn!(error = %e, character_id = %account.character_id, "items save failed");
    }

    // Persist KnownAbilities → CharacterAbilities so `study`-acquired
    // spells round-trip across reconnect.
    let ability_rows: Vec<mud_db::character_abilities::CharacterAbilityRow> = world
        .get::<KnownAbilities>(entity)
        .map(|ka| {
            ka.entries
                .iter()
                .map(|(id, prof, known)| mud_db::character_abilities::CharacterAbilityRow {
                    ability_id: *id,
                    known: *known,
                    proficiency: *prof,
                })
                .collect()
        })
        .unwrap_or_default();
    if let Err(e) = mud_db::character_abilities::save_for(
        pool,
        &account.character_id,
        &ability_rows,
    )
    .await
    {
        warn!(error = %e, character_id = %account.character_id, "abilities save failed");
        return;
    }

    // Persist Aliases → CharacterAliases so user-defined shortcuts
    // round-trip across reconnect.
    let alias_rows: Vec<mud_db::character_aliases::CharacterAliasRow> = world
        .get::<mud_world::Aliases>(entity)
        .map(|al| {
            al.entries
                .iter()
                .map(|(alias, command)| mud_db::character_aliases::CharacterAliasRow {
                    alias: alias.clone(),
                    command: command.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let alias_count = alias_rows.len();
    if let Err(e) =
        mud_db::character_aliases::save_for(pool, &account.character_id, &alias_rows).await
    {
        warn!(error = %e, character_id = %account.character_id, "aliases save failed");
        return;
    }

    // Persist mutable CoreStats (changes via `train`, future stat
    // adjusters). Skipped silently if the entity has no CoreStats —
    // shouldn't happen for a fully-spawned player but defensive.
    if let Some(stats) = world.get::<CoreStats>(entity).copied()
        && let Err(e) = characters::save_core_stats(
            pool,
            &account.character_id,
            stats.strength,
            stats.dexterity,
            stats.constitution,
            stats.intelligence,
            stats.wisdom,
            stats.charisma,
        )
        .await
    {
        warn!(error = %e, character_id = %account.character_id, "core_stats save failed");
        return;
    }

    info!(
        character_id = %account.character_id,
        hp,
        zone_id,
        room_id,
        recall_zone,
        recall_room,
        flag_count = flags.len(),
        item_count,
        alias_count,
        "player saved"
    );
}

/// Materialize each saved `CharacterItem` into a live Item entity. Top-
/// level rows (`container_id IS NULL`) get `Located(player)`; nested
/// rows get `Located(parent_item_entity)` so the existing structural
/// `Located` chain models bag-in-bag inventory. Multi-pass walk over
/// the row set handles arbitrary nesting depth: items whose parent
/// entity hasn't been spawned yet roll over to a later pass; the loop
/// stops when no row makes progress.
///
/// Skips rows whose prototype isn't loaded (logs a warn) and orphan
/// rows whose parent never spawned (also logged). Returns total spawn
/// count for the login info line.
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_inventory(world: &mut World, player: Entity, rows: &[CharacterItemRow]) -> usize {
    use std::collections::HashMap;
    // row.id → spawned Entity. Top-level items spawn first; nested rows
    // wait for their parent to land.
    let mut spawned: HashMap<i32, Entity> = HashMap::new();
    let mut pending: Vec<&CharacterItemRow> = rows.iter().collect();

    loop {
        let mut made_progress = false;
        let mut still_pending: Vec<&CharacterItemRow> = Vec::with_capacity(pending.len());
        for row in pending {
            // Determine the parent entity to attach to:
            //   - container_id is None → player (top-level inventory)
            //   - container_id is Some(parent_row_id) → spawned[parent_row_id] if known
            //     (otherwise this row gets re-queued for the next pass)
            let parent_entity = match row.container_id {
                None => Some(player),
                Some(parent_row_id) => spawned.get(&parent_row_id).copied(),
            };
            let Some(parent_entity) = parent_entity else {
                still_pending.push(row);
                continue;
            };
            let proto = world
                .resource::<ObjectPrototypes>()
                .by_key
                .get(&(row.object_zone_id, row.object_id))
                .cloned();
            let Some(proto) = proto else {
                warn!(
                    row_id = row.id,
                    object_zone_id = row.object_zone_id,
                    object_id = row.object_id,
                    "character_items row references missing ObjectProto; skipping"
                );
                made_progress = true;
                continue;
            };
            // Mirror the proto-derived attach set the loader's reset
            // pass uses: WearableIn (so saved equipment stays
            // wearable), BoardLink (boards in inventory still link),
            // LiquidContainer (drink containers stay drinkable —
            // bug from this morning where a saved water skin came
            // back without LiquidContainer and `drink` rejected it),
            // AttachedTriggers (so on_get / on_drop fire on saved
            // items). Without this list, inventory rehydration only
            // produces a bare Item with no capability components.
            let primary_slot = wear_flags_primary_slot(&proto.wear_flags);
            let trigger_keys = world
                .resource::<TriggerCatalog>()
                .object_attachments
                .get(&(proto.zone_id, proto.id))
                .cloned();
            let mut bundle = world.spawn((
                Item,
                Named { name: proto.name.clone() },
                Keywords(proto.keywords.clone()),
                WorldKey {
                    zone: proto.zone_id,
                    id: proto.id,
                },
                Located(parent_entity),
            ));
            if let Some(desc) = proto.examine_description.clone() {
                bundle.insert(Description(desc));
            }
            if let Some(s) = primary_slot {
                bundle.insert(WearableIn(s));
            }
            if let Some(board_id) = proto.board_id {
                bundle.insert(BoardLink(board_id));
            }
            if let Some(liq) = proto.liquid.clone() {
                bundle.insert(LiquidContainer {
                    liquid: liq.liquid,
                    capacity: liq.capacity,
                    remaining: liq.remaining,
                    poisoned: liq.poisoned,
                });
            }
            if let Some(fuel) = proto.light_fuel {
                bundle.insert(mud_world::LightFuel {
                    capacity: fuel.capacity,
                    remaining: fuel.remaining,
                });
            }
            if let Some(keys) = trigger_keys {
                bundle.insert(AttachedTriggers(keys));
            }
            let item_entity = bundle.id();
            if let Some(slot_str) = row.equipped_location.as_deref()
                && let Some(slot) = Slot::from_label(slot_str)
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                e.insert(EquippedSlot(slot));
            }
            // Restore Charges from the ObjectAbilities binding's
            // charges value. CharacterItems doesn't store per-instance
            // charges yet, so wand/staff items reset to full on
            // reconnect — generous but consistent with how loadobj
            // spawns. Logged in SUGGESTIONS for proper persistence.
            if let Some(charges) = world
                .resource::<mud_world::ObjectAbilityCatalog>()
                .by_key
                .get(&(proto.zone_id, proto.id))
                .and_then(|v| v.first().and_then(|b| b.charges))
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                e.insert(mud_world::Charges(charges));
            }
            spawned.insert(row.id, item_entity);
            made_progress = true;
        }
        pending = still_pending;
        if !made_progress {
            break;
        }
    }
    if !pending.is_empty() {
        for row in &pending {
            warn!(
                row_id = row.id,
                container_id = row.container_id,
                "character_items row's container parent never spawned; orphan dropped"
            );
        }
    }
    spawned.len()
}

fn pick_starting_room(c: &CharacterRow) -> (i32, i32) {
    if let (Some(z), Some(r)) = (c.current_room_zone_id, c.current_room_id) {
        return (z, r);
    }
    if let (Some(z), Some(r)) = (c.recall_room_zone_id, c.recall_room_id) {
        return (z, r);
    }
    FALLBACK_START
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        cur: Option<(i32, i32)>,
        recall: Option<(i32, i32)>,
    ) -> CharacterRow {
        CharacterRow {
            id: "c".into(),
            name: "Tester".into(),
            user_id: Some("u".into()),
            level: 1,
            hit_points: 10,
            hit_points_max: 10,
            stamina: 10,
            stamina_max: 10,
            hit_roll: 0,
            damage_roll: 0,
            armor_class: 10,
            alignment: 0,
            permissions: vec![],
            player_flags: vec![],
            prompt: String::new(),
            current_room_zone_id: cur.map(|c| c.0),
            current_room_id: cur.map(|c| c.1),
            recall_room_zone_id: recall.map(|r| r.0),
            recall_room_id: recall.map(|r| r.1),
            class_id: None,
            race: "HUMAN".into(),
            experience: 0,
            title: None,
            description: None,
            strength: 13,
            dexterity: 13,
            constitution: 13,
            intelligence: 13,
            wisdom: 13,
            charisma: 13,
            wealth: 0,
            bank_wealth: 0,
            gender: "neutral".into(),
            skill_points: 0,
            hunger: 0,
            thirst: 0,
        }
    }

    #[test]
    fn current_room_wins_when_both_set() {
        let r = row(Some((30, 5)), Some((10, 1)));
        assert_eq!(pick_starting_room(&r), (30, 5));
    }

    #[test]
    fn falls_back_to_recall_when_current_unset() {
        let r = row(None, Some((10, 1)));
        assert_eq!(pick_starting_room(&r), (10, 1));
    }

    #[test]
    fn falls_back_to_void_when_neither_set() {
        let r = row(None, None);
        assert_eq!(pick_starting_room(&r), FALLBACK_START);
    }

    #[test]
    fn partial_current_falls_through_to_recall() {
        // current has only zone, no room id — both must be Some to use it.
        let mut r = row(None, Some((10, 1)));
        r.current_room_zone_id = Some(30);
        // current_room_id stays None — pick_starting_room should skip it.
        assert_eq!(pick_starting_room(&r), (10, 1));
    }
}
