use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{characters, characters::CharacterRow, sqlx::PgPool, users, users::User};
use mud_net::{ConnId, Outbound};
use mud_world::{
    Account, CombatStats, Health, Located, Named, Online, Player, PlayerFlags, Posture,
    PostureKind, Prompt, WorldKey, WorldKeyIndex,
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
            commands::dispatch(world, entity, &text);
            commands::send_prompt(world, entity);
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

                // Drop the &mut ctx borrow by removing — the LoginCtx and its
                // outbound move into the Player entity's Connection component.
                let LoginCtx { outbound, .. } = self.login.remove(&conn_id).unwrap();
                let entity = spawn_player(world, &user, &char_row, outbound);
                self.playing.insert(conn_id, entity);
                commands::send_prompt(world, entity);
                info!(
                    conn_id,
                    char_name = %char_row.name,
                    char_level = char_row.level,
                    "player spawned"
                );
            }
        }
    }
}

fn spawn_player(world: &mut World, user: &User, c: &CharacterRow, outbound: Outbound) -> Entity {
    let (zone, room) = pick_starting_room(c);

    let index = world.resource::<WorldKeyIndex>();
    let room_entity = index
        .rooms
        .get(&(zone, room))
        .copied()
        .or_else(|| index.rooms.get(&FALLBACK_START).copied());

    let health = Health {
        hp: c.hit_points,
        max: c.hit_points_max,
    };
    let combat = CombatStats {
        hit_roll: c.hit_roll,
        dmg_roll: c.damage_roll,
        ac: c.armor_class,
        alignment: c.alignment,
    };

    let Some(room_entity) = room_entity else {
        let _ = outbound.send(format!(
            "No starting room available (tried ({zone},{room}) and fallback {FALLBACK_START:?}).\r\n",
        ));
        // Spawn a "stranded" player without a Located so they don't crash later.
        return world
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
                combat,
                Posture(PostureKind::Standing),
                PlayerFlags(c.player_flags.clone()),
                Prompt(c.prompt.clone()),
            ))
            .id();
    };

    let room_name = world
        .get::<Named>(room_entity)
        .map_or_else(|| "<unknown>".to_string(), |n| n.name.clone());

    let _ = outbound.send(format!(
        "\r\nWelcome, {name}.\r\nYou appear in: {room_name}\r\n\r\n",
        name = c.name,
    ));

    world
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
            Located(room_entity),
            Connection(outbound),
            health,
            combat,
            Posture(PostureKind::Standing),
            PlayerFlags(c.player_flags.clone()),
        ))
        .id()
}

async fn save_player(world: &World, entity: Entity, pool: &PgPool) {
    let Some(account) = world.get::<Account>(entity).cloned() else {
        return;
    };
    let hp = world.get::<Health>(entity).map_or(0, |h| h.hp);
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

    if let Err(e) = characters::save_state(
        pool,
        &account.character_id,
        hp,
        zone_id,
        room_id,
        &flags,
        &prompt,
    )
    .await
    {
        warn!(error = %e, character_id = %account.character_id, "save failed");
    } else {
        info!(
            character_id = %account.character_id,
            hp,
            zone_id,
            room_id,
            flag_count = flags.len(),
            "player saved"
        );
    }
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
