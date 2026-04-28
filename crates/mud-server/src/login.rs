use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::{characters, characters::CharacterRow, sqlx::PgPool, users, users::User};
use mud_net::{ConnId, Outbound};
use mud_world::{Account, Located, Named, Online, Player, WorldKeyIndex};
use tracing::{info, warn};

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

    pub fn on_disconnect(&mut self, world: &mut World, conn_id: ConnId) {
        self.login.remove(&conn_id);
        if let Some(entity) = self.playing.remove(&conn_id) {
            // Despawn the player entity. Save-on-disconnect comes later.
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
        } else if self.playing.contains_key(&conn_id) {
            // Gameplay command processing arrives in step 6+. For now, just
            // acknowledge so the player knows the line was received.
            if let Some(_entity) = self.playing.get(&conn_id) {
                // No-op — log only.
                info!(conn_id, text, "in-game line (commands not yet wired)");
            }
        }
    }

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
                let Some(char_row) = pick.and_then(|n| n.checked_sub(1)).and_then(|i| characters.get(i)).cloned() else {
                    let _ = ctx
                        .outbound
                        .send(format!("Pick 1-{}.\r\n", characters.len()));
                    ctx.stage = Stage::CharSelect { user, characters };
                    return;
                };

                // Move out of login, spawn entity.
                let outbound = ctx.outbound.clone();
                self.login.remove(&conn_id);
                let entity = spawn_player(world, &user, &char_row, &outbound);
                self.playing.insert(conn_id, entity);
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

fn spawn_player(world: &mut World, user: &User, c: &CharacterRow, outbound: &Outbound) -> Entity {
    let (zone, room) = pick_starting_room(c);

    let index = world.resource::<WorldKeyIndex>();
    let room_entity = index.rooms.get(&(zone, room)).copied().or_else(|| {
        index
            .rooms
            .get(&FALLBACK_START)
            .copied()
    });

    let Some(room_entity) = room_entity else {
        let _ = outbound.send(format!(
            "No starting room available (tried ({zone},{room}) and fallback {:?}).\r\n",
            FALLBACK_START
        ));
        // Spawn a "stranded" player without a Located so they don't crash later.
        return world
            .spawn((
                Player,
                Online,
                Named { name: c.name.clone() },
                Account(user.id.clone()),
            ))
            .id();
    };

    let room_name = world
        .get::<Named>(room_entity)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| "<unknown>".into());

    let _ = outbound.send(format!(
        "\r\nWelcome, {name}.\r\nYou appear in: {room}\r\n\r\n",
        name = c.name,
        room = room_name,
    ));

    world
        .spawn((
            Player,
            Online,
            Named { name: c.name.clone() },
            Account(user.id.clone()),
            Located(room_entity),
        ))
        .id()
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
