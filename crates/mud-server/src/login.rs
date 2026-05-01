use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{characters, characters::CharacterRow, sqlx::PgPool, users, users::User};
use mud_net::{ConnId, Outbound};
use mud_db::character_items::CharacterItemRow;
use mud_world::{
    Account, AccountSummary, CombatStats, CoreStats, Description, EquippedSlot, Health, Item,
    Keywords, KnownAbilities, Located, LoggedInAt, Named, Online, ObjectPrototypes, Player,
    BankWealth, PlayerFlags, Posture, PostureKind, Profile, Prompt, RecallPoint, Slot, Stamina,
    Title, Wealth, WorldKey, WorldKeyIndex,
};
use tracing::{info, warn};

use crate::commands::{self, Connection};

const BANNER: &str = "\r\n=========================================\r\n   fierymud-rs (Rust ECS rewrite)\r\n=========================================\r\n";
const EMAIL_PROMPT: &str = "Email: ";
const PASSWORD_PROMPT: &str = "Password: ";

/// Default starting room when a character has no current/recall location set.
/// (0, 0) is "The Void" — fitting.
const FALLBACK_START: (i32, i32) = (0, 0);

pub enum Stage {
    AwaitingEmail,
    AwaitingPassword { user: User },
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
        let _ = outbound.send(BANNER.into());
        let _ = outbound.send(EMAIL_PROMPT.into());
        self.login.insert(
            conn_id,
            LoginCtx {
                outbound,
                stage: Stage::AwaitingEmail,
            },
        );
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

        match std::mem::replace(&mut ctx.stage, Stage::AwaitingEmail) {
            Stage::AwaitingEmail => {
                // Look up by email. Don't reveal whether the email exists —
                // ask for password regardless, fail later. (Constant-time'ish.)
                let lookup = users::find_by_email(pool, trimmed).await;
                match lookup {
                    Ok(Some(user)) => {
                        ctx.stage = Stage::AwaitingPassword { user };
                    }
                    Ok(None) => {
                        // Stash a sentinel "user" so the prompt advances; the
                        // password check will fail anyway.
                        ctx.stage = Stage::AwaitingPassword {
                            user: User {
                                id: String::new(),
                                email: trimmed.into(),
                                display_name: String::new(),
                                password_hash: None,
                                role: mud_db::enums::UserRole::Player,
                            },
                        };
                    }
                    Err(e) => {
                        warn!(conn_id, error = %e, "user lookup failed");
                        let _ = ctx.outbound.send("Server error.\r\n".into());
                        ctx.stage = Stage::AwaitingEmail;
                        let _ = ctx.outbound.send(EMAIL_PROMPT.into());
                        return;
                    }
                }
                let _ = ctx.outbound.send(PASSWORD_PROMPT.into());
            }

            Stage::AwaitingPassword { user } => {
                let ok = user
                    .password_hash
                    .as_ref()
                    .is_some_and(|h| bcrypt::verify(trimmed, h).unwrap_or(false));
                if !ok {
                    info!(conn_id, email = %user.email, "auth failure");
                    let _ = ctx.outbound.send("Invalid credentials.\r\n".into());
                    ctx.stage = Stage::AwaitingEmail;
                    let _ = ctx.outbound.send(EMAIL_PROMPT.into());
                    return;
                }
                info!(conn_id, user_id = %user.id, email = %user.email, "auth success");

                let chars = match characters::list_for_user(pool, &user.id).await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(conn_id, error = %e, "character list failed");
                        let _ = ctx.outbound.send("Server error.\r\n".into());
                        ctx.stage = Stage::AwaitingEmail;
                        let _ = ctx.outbound.send(EMAIL_PROMPT.into());
                        return;
                    }
                };
                if chars.is_empty() {
                    let _ = ctx
                        .outbound
                        .send("No characters on this account.\r\n".into());
                    ctx.stage = Stage::AwaitingEmail;
                    let _ = ctx.outbound.send(EMAIL_PROMPT.into());
                    return;
                }
                let mut menu = String::from("\r\nCharacters:\r\n");
                for (idx, c) in chars.iter().enumerate() {
                    menu.push_str(&format!("  {}. {} (level {})\r\n", idx + 1, c.name, c.level));
                }
                menu.push_str("Pick a number: ");
                let _ = ctx.outbound.send(menu);
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
                        .send(format!("Pick 1-{}.\r\n", characters.len()));
                    ctx.stage = Stage::CharSelect { user, characters };
                    return;
                };

                // Pre-load the character's saved inventory before we spawn —
                // we're still in async context here, spawn_player is sync.
                let item_rows = match mud_db::character_items::list_for(pool, &char_row.id).await
                {
                    Ok(rows) => rows,
                    Err(e) => {
                        warn!(conn_id, error = %e, "character_items load failed");
                        Vec::new()
                    }
                };
                // What spells/skills they know.
                let ability_rows =
                    match mud_db::character_abilities::list_for(pool, &char_row.id).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            warn!(conn_id, error = %e, "character_abilities load failed");
                            Vec::new()
                        }
                    };
                // Saved command aliases.
                let alias_rows =
                    match mud_db::character_aliases::list_for(pool, &char_row.id).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            warn!(conn_id, error = %e, "character_aliases load failed");
                            Vec::new()
                        }
                    };
                // Drop the &mut ctx borrow by removing — the LoginCtx and its
                // outbound move into the Player entity's Connection component.
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
                    characters: characters
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
    }
}

/// Single spawn path for a player entity. The `Located(room_entity)`
/// component is added in a follow-up insert (after spawn) only when
/// the starting room resolved — keeping the core bundle one place
/// avoids the recurring "did I update both branches?" bug we hit
/// three times before consolidating.
fn spawn_player(world: &mut World, user: &User, c: &CharacterRow, outbound: Outbound) -> Entity {
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
        ));
    } else {
        let _ = outbound.send(format!(
            "No starting room available (tried ({zone},{room}) and fallback {FALLBACK_START:?}).\r\n",
        ));
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
    let (recall_zone, recall_room) = world
        .get::<RecallPoint>(entity)
        .and_then(|r| world.get::<WorldKey>(r.0).copied())
        .map_or((None, None), |wk| (Some(wk.zone), Some(wk.id)));

    // Snapshot every Item Located on the player. Items inside containers
    // the player is carrying have Located(container_item) — those stay in
    // the DB on the previous save until container-chain support lands;
    // walking just the directly-carried set here matches the load path.
    let new_items: Vec<mud_db::character_items::NewCharacterItem> = {
        let mut q = world.query::<(&Located, &WorldKey, Option<&EquippedSlot>, &Item)>();
        q.iter(world)
            .filter(|(l, _, _, _)| l.0 == entity)
            .map(|(_, wk, eq, _)| mud_db::character_items::NewCharacterItem {
                object_zone_id: wk.zone,
                object_id: wk.id,
                equipped_location: eq.map(|s| s.0.db_label().to_string()),
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

/// Materialize each saved `CharacterItem` into a live Item entity attached
/// to `player`. Skips rows whose prototype isn't loaded (logs a warn) and
/// rows with a `container_id` that we don't yet resolve. Returns how many
/// items were spawned (for the login info line).
fn spawn_inventory(world: &mut World, player: Entity, rows: &[CharacterItemRow]) -> usize {
    let mut spawned = 0usize;
    for row in rows {
        if row.container_id.is_some() {
            // Container chain handling is a follow-up — for now items
            // inside containers stay parked in the DB and don't appear
            // in the player's inventory.
            continue;
        }
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
            continue;
        };
        let mut bundle = world.spawn((
            Item,
            Named { name: proto.name.clone() },
            Keywords(proto.keywords.clone()),
            WorldKey { zone: proto.zone_id, id: proto.id },
            Located(player),
        ));
        if let Some(desc) = proto.examine_description.clone() {
            bundle.insert(Description(desc));
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
        spawned += 1;
    }
    spawned
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
