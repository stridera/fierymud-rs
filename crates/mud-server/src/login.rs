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

/// Pre-login banner. Sent as raw bytes (no XML-Lite renderer in
/// the login path) so ANSI escapes are inlined directly. Bold red
/// flame border + bold yellow title + dim subtitle reads warm
/// without leaning on terminal-capability negotiation. The
/// 41-char interior keeps the box flush in 80-col clients.
const BANNER: &str = concat!(
    "\r\n",
    "\x1b[1;31m  /\\  /\\  /\\  /\\  /\\  /\\  /\\  /\\  /\\  /\\\x1b[0m\r\n",
    "\x1b[1;33m              fierymud-rs\x1b[0m\r\n",
    "\x1b[2m       a Rust rewrite of FieryMUD\x1b[0m\r\n",
    "\x1b[1;31m  \\/  \\/  \\/  \\/  \\/  \\/  \\/  \\/  \\/  \\/\x1b[0m\r\n",
    "\r\n",
    "\x1b[2m  Type your email or character name to begin.\x1b[0m\r\n",
    "\r\n",
);
/// Combined identifier prompt — accepts either an email or a
/// character name. Email is detected by the presence of '@' (the
/// only thing legacy MUD usernames couldn't legally contain).
const IDENT_PROMPT: &str = "Email or character name: ";
const PASSWORD_PROMPT: &str = "Password: ";
const NEW_PASSWORD_PROMPT: &str = "Choose a password: ";
const CONFIRM_PASSWORD_PROMPT: &str = "Re-enter password to confirm: ";
/// Minimum length for a freshly-created password. Mirrors the
/// existing `bcrypt::hash` cost path — bcrypt itself doesn't
/// enforce a length, but anything shorter than this is a
/// hard pass for a new account regardless.
const MIN_NEW_PASSWORD_LEN: usize = 6;
const NEW_CHARACTER_NAME_PROMPT: &str = "Character name: ";
/// Inclusive length window for a new character name. Lower bound
/// keeps single-letter ambiguity out of `who`-style listings;
/// upper bound matches the existing `Characters.name` column
/// width assumption (column itself is wider but UX past 20 chars
/// gets unwieldy in fixed-width prompts).
const MIN_CHARACTER_NAME_LEN: usize = 3;
const MAX_CHARACTER_NAME_LEN: usize = 20;

/// Default starting room when a character has no current/recall location set.
/// (0, 0) is "The Void" — fitting.
const FALLBACK_START: (i32, i32) = (0, 0);

/// Maximum disconnect window across which non-staff effects persist,
/// in seconds. Reconnect within this window restores active buffs /
/// debuffs / poisons / blinds with their elapsed time deducted from
/// `remaining_secs`. Beyond it, non-staff effects are wiped — closing
/// the "log off for the night and come back fresh" loop the design
/// targets, and intentionally not exploitable for short death-staving
/// disconnects since the timer keeps ticking. Staff-applied effects
/// (`EffectSource::Admin`) bypass this cap entirely; they're often
/// rewards and should outlive a single sleep cycle.
const EFFECT_DISCONNECT_CAP_SECS: i64 = 3600;

/// Persisted shape of one `EffectInstance` — flattened so we don't
/// have to round-trip through ECS-internal types. The runtime
/// component shape (`EffectInstance` + `AppliedTo` + optional
/// `ModifyDelta`) collapses into this single record per entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedEffectInstance {
    kind: i32,
    name: String,
    strength: i32,
    remaining_secs: i32,
    source: mud_world::EffectSource,
    ability_id: Option<i32>,
    /// Present iff the effect entity also had a `ModifyDelta` (stat-
    /// modifying buff). Captured as a 2-tuple to keep the JSON shape
    /// shallow: (`target_label`, amount).
    modify_delta: Option<(String, i32)>,
}

/// Persisted shape of the active-effects blob. Wraps the per-entry
/// list in an envelope that records the wall-clock save time, so the
/// load path can compute "elapsed since save" without trusting any
/// per-entry timestamp.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedEffects {
    saved_at_unix: i64,
    effects: Vec<PersistedEffectInstance>,
}

/// Shared restore logic: spawn one effect entity per persisted entry,
/// dropping non-Admin entries past the disconnect cap and adjusting
/// `remaining_secs` for elapsed time. Used by both the telnet login
/// path and the admin virtual-session path.
pub(crate) fn restore_persisted_effects(
    world: &mut World,
    entity: Entity,
    persisted: PersistedEffects,
) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let elapsed = now_unix.saturating_sub(persisted.saved_at_unix).max(0);
    for eff in persisted.effects {
        let is_admin = matches!(eff.source, mud_world::EffectSource::Admin);
        if !is_admin && elapsed > EFFECT_DISCONNECT_CAP_SECS {
            continue;
        }
        let restored_secs = if eff.remaining_secs < 0 {
            -1
        } else {
            let after = i64::from(eff.remaining_secs).saturating_sub(elapsed);
            if after <= 0 {
                continue;
            }
            i32::try_from(after).unwrap_or(eff.remaining_secs)
        };
        let mut effect_entity = world.spawn((
            mud_world::EffectInstance {
                kind: eff.kind,
                name: eff.name.clone(),
                strength: eff.strength,
                remaining_secs: restored_secs,
                source: eff.source,
                ability_id: eff.ability_id,
            },
            mud_world::AppliedTo(entity),
        ));
        if let Some((target, amount)) = eff.modify_delta {
            effect_entity.insert(mud_world::ModifyDelta { target, amount });
        }
    }
}

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
    /// Identifier didn't match anything in the database. Ask the
    /// user whether they want to create a new account / character
    /// rather than silently bouncing them through a doomed
    /// password check.
    ConfirmCreate {
        identifier: String,
        is_email: bool,
    },
    /// Confirm-create answered "yes". Collect a password for the
    /// new account. The character-name path collapses both
    /// account + character creation behind one identifier — the
    /// password covers the user-row.
    AwaitingNewPassword {
        identifier: String,
        is_email: bool,
    },
    /// Re-prompt to verify the new password matches what the user
    /// just typed. Mismatches bounce back to `AwaitingNewPassword`.
    /// Ephemeral plaintext lives only on this stage value — gone
    /// when the stage advances.
    ConfirmNewPassword {
        identifier: String,
        is_email: bool,
        first_attempt: String,
    },
    /// Email-path creation flow: we've got the email + a confirmed
    /// password; ask the user what their character should be named.
    /// Validates length / charset and checks the database for an
    /// existing character with the same name. Plaintext password
    /// rides along until the eventual `Users` + `Characters`
    /// INSERT slice. Character-name-path skips this stage entirely
    /// since the identifier IS the character name.
    AwaitingCharacterName {
        email: String,
        password_plaintext: String,
    },
    /// Pick a race for the new character. Prompt lists the
    /// `PLAYABLE_RACES` set; input matches case-insensitively. The
    /// `email` field is `None` when the character-name path skipped
    /// `AwaitingCharacterName` (the identifier was already a name).
    AwaitingRace {
        email: Option<String>,
        character_name: String,
        password_plaintext: String,
    },
    /// Pick a class for the new character. Prompt lists every
    /// `ClassCatalog` row with `is_subclass = false`; subclasses
    /// (Paladin, Anti-Paladin, Conjurer, …) require a follow-on
    /// specialization step that lands later. Input is matched
    /// case-insensitively against `plain_name`.
    AwaitingClass {
        email: Option<String>,
        character_name: String,
        password_plaintext: String,
        race: &'static str,
    },
    /// Pick a gender for the new character. Prompt lists the
    /// schema's `Characters.gender` accepted values (`male`,
    /// `female`, `neutral`). Stored verbatim — gendered Lua
    /// triggers read this string directly via `actor.gender`.
    AwaitingGender {
        email: Option<String>,
        character_name: String,
        password_plaintext: String,
        race: &'static str,
        class_id: i32,
        class_plain_name: String,
    },
    /// Show the freshly-rolled stat block and ask the player
    /// whether to keep it or roll again. `accept` advances
    /// (today: terminator with the "DB INSERT comes next" line);
    /// `reroll` (or `r`) generates a fresh 3d6×6 spread without
    /// resetting earlier draft fields.
    ReviewStatRoll {
        email: Option<String>,
        character_name: String,
        password_plaintext: String,
        race: &'static str,
        class_id: i32,
        class_plain_name: String,
        gender: &'static str,
        stats: CoreStats,
    },
    CharSelect { user: User, characters: Vec<CharacterRow> },
}

/// Gender values accepted by the `Characters.gender` column. The
/// schema column is plain text but the runtime + triggers only
/// handle these three casings; new options need a code-side
/// review before adding here.
const PLAYABLE_GENDERS: &[&str] = &["male", "female", "neutral"];

/// Player-eligible races for the creation flow's race prompt.
/// A subset of the schema's `Race` enum — monster / NPC variants
/// (DRAGON, DEMON, GOBLIN, etc.) stay out of the picker. Order
/// drives the listing the player sees.
const PLAYABLE_RACES: &[&str] = &[
    "HUMAN",
    "ELF",
    "HALF_ELF",
    "DWARF",
    "HALFLING",
    "GNOME",
    "GOLIATH",
];

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

    /// Reverse lookup: which `ConnId` (if any) is currently driving
    /// the given player entity. Linear scan over `playing` — fine at
    /// realistic player counts. Used by the idle-kick path to route
    /// a disconnect through the canonical `on_disconnect` save flow.
    #[must_use]
    pub fn find_conn(&self, entity: Entity) -> Option<ConnId> {
        self.playing
            .iter()
            .find_map(|(cid, e)| if *e == entity { Some(*cid) } else { None })
    }

    pub fn on_connect(&mut self, conn_id: ConnId, outbound: Outbound) {
        let _ = outbound.try_send(BANNER.as_bytes().to_vec());
        let _ = outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
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
            // SaveOutcome dropped — broadcast autosave can't surface
            // a per-player partial-save message anyway, and the
            // tracing::warn inside save_player covers staff
            // diagnostics.
            let _ = save_player(world, entity, pool).await;
        }
    }

    pub async fn on_disconnect(&mut self, world: &mut World, conn_id: ConnId, pool: &PgPool) {
        self.login.remove(&conn_id);
        if let Some(entity) = self.playing.remove(&conn_id) {
            // Disconnect path — player is gone before we could
            // report a partial save. The tracing::warn inside
            // save_player covers diagnostics.
            let _ = save_player(world, entity, pool).await;
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
                    failed_login_attempts: 0,
                    locked_until: None,
                };
                // Registration gate: when `security.enable_new_player_creation`
                // is false, the unknown-identifier path returns a
                // closed-registration message instead of routing to the
                // create-account flow. Default true (legacy permissive).
                let registration_open = world
                    .resource::<mud_world::RuntimeConfig>()
                    .get_bool("security", "enable_new_player_creation", true);
                let mut routed_to_password = false;
                if is_email {
                    let lookup = users::find_by_email(pool, trimmed).await;
                    match lookup {
                        Ok(Some(user)) => {
                            ctx.stage = Stage::AwaitingPassword { user, preselected: None };
                            routed_to_password = true;
                        }
                        Ok(None) => {
                            if !registration_open {
                                let _ = ctx.outbound.try_send(
                                    "New character creation is currently closed.\r\n"
                                        .as_bytes()
                                        .to_vec(),
                                );
                                ctx.stage = Stage::AwaitingIdentifier;
                                let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                                return;
                            }
                            ctx.stage = Stage::ConfirmCreate {
                                identifier: trimmed.to_string(),
                                is_email: true,
                            };
                            send_confirm_create_prompt(&ctx.outbound, trimmed, true);
                        }
                        Err(e) => {
                            warn!(conn_id, error = %e, "user lookup failed");
                            let _ = ctx.outbound.try_send("Server error.\r\n".as_bytes().to_vec());
                            ctx.stage = Stage::AwaitingIdentifier;
                            let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                            return;
                        }
                    }
                } else {
                    // Character-name path. Look up the row by name; if found,
                    // resolve its user_id → User. Unknown names route to
                    // ConfirmCreate so the player gets a clear "no such
                    // character — make a new one?" prompt instead of a
                    // silent bcrypt-fail bounce.
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
                            routed_to_password = true;
                        }
                        Ok(None) => {
                            if !registration_open {
                                let _ = ctx.outbound.try_send(
                                    "New character creation is currently closed.\r\n"
                                        .as_bytes()
                                        .to_vec(),
                                );
                                ctx.stage = Stage::AwaitingIdentifier;
                                let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                                return;
                            }
                            ctx.stage = Stage::ConfirmCreate {
                                identifier: trimmed.to_string(),
                                is_email: false,
                            };
                            send_confirm_create_prompt(&ctx.outbound, trimmed, false);
                        }
                        Err(e) => {
                            warn!(conn_id, error = %e, "character lookup failed");
                            let _ = ctx.outbound.try_send("Server error.\r\n".as_bytes().to_vec());
                            ctx.stage = Stage::AwaitingIdentifier;
                            let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                            return;
                        }
                    }
                }
                if routed_to_password {
                    let _ = ctx.outbound.try_send(PASSWORD_PROMPT.as_bytes().to_vec());
                }
            }

            Stage::ConfirmCreate { identifier, is_email } => {
                let answer = trimmed.to_ascii_lowercase();
                let yes = matches!(answer.as_str(), "y" | "yes");
                let no = matches!(answer.as_str(), "n" | "no" | "");
                if yes {
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Great — let's set up `{identifier}`. Pick a password \
                             at least {MIN_NEW_PASSWORD_LEN} characters long.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingNewPassword { identifier, is_email };
                    let _ = ctx.outbound.try_send(NEW_PASSWORD_PROMPT.as_bytes().to_vec());
                } else if no {
                    let _ = ctx.outbound.try_send(
                        "Okay — please enter an existing email or character name.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                } else {
                    let _ = ctx.outbound.try_send(
                        "Please answer 'yes' or 'no'.\r\n".as_bytes().to_vec(),
                    );
                    ctx.stage = Stage::ConfirmCreate { identifier, is_email };
                }
            }

            Stage::AwaitingNewPassword { identifier, is_email } => {
                if trimmed.len() < MIN_NEW_PASSWORD_LEN {
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Password must be at least {MIN_NEW_PASSWORD_LEN} \
                             characters. Try again.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingNewPassword { identifier, is_email };
                    let _ = ctx.outbound.try_send(NEW_PASSWORD_PROMPT.as_bytes().to_vec());
                    return;
                }
                ctx.stage = Stage::ConfirmNewPassword {
                    identifier,
                    is_email,
                    first_attempt: trimmed.to_string(),
                };
                let _ = ctx.outbound.try_send(CONFIRM_PASSWORD_PROMPT.as_bytes().to_vec());
            }

            Stage::ConfirmNewPassword {
                identifier,
                is_email,
                first_attempt,
            } => {
                if trimmed != first_attempt {
                    let _ = ctx.outbound.try_send(
                        "Passwords don't match. Let's start over.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    ctx.stage = Stage::AwaitingNewPassword { identifier, is_email };
                    let _ = ctx.outbound.try_send(NEW_PASSWORD_PROMPT.as_bytes().to_vec());
                    return;
                }
                // Password confirmed. Email path needs to collect a
                // character name next; character-name path already
                // has the name (= the identifier they typed).
                if is_email {
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Password set for `{identifier}`. Now choose your \
                             character's name ({MIN_CHARACTER_NAME_LEN}–{MAX_CHARACTER_NAME_LEN} \
                             letters).\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingCharacterName {
                        email: identifier,
                        password_plaintext: first_attempt,
                    };
                    let _ = ctx
                        .outbound
                        .try_send(NEW_CHARACTER_NAME_PROMPT.as_bytes().to_vec());
                } else {
                    // Character-name path: identifier IS the
                    // character name. Advance to race selection.
                    ctx.stage = Stage::AwaitingRace {
                        email: None,
                        character_name: identifier,
                        password_plaintext: first_attempt,
                    };
                    send_race_prompt(&ctx.outbound);
                }
            }

            Stage::AwaitingCharacterName {
                email,
                password_plaintext,
            } => {
                let name = trimmed;
                if let Err(reason) = validate_new_character_name(name) {
                    let _ = ctx.outbound.try_send(format!("{reason}\r\n").into_bytes());
                    ctx.stage = Stage::AwaitingCharacterName {
                        email,
                        password_plaintext,
                    };
                    let _ = ctx
                        .outbound
                        .try_send(NEW_CHARACTER_NAME_PROMPT.as_bytes().to_vec());
                    return;
                }
                match characters::find_by_name(pool, name).await {
                    Ok(Some(_)) => {
                        let _ = ctx.outbound.try_send(
                            format!(
                                "Sorry, the name `{name}` is already taken. \
                                 Pick another.\r\n"
                            )
                            .into_bytes(),
                        );
                        ctx.stage = Stage::AwaitingCharacterName {
                            email,
                            password_plaintext,
                        };
                        let _ = ctx
                            .outbound
                            .try_send(NEW_CHARACTER_NAME_PROMPT.as_bytes().to_vec());
                    }
                    Ok(None) => {
                        // Name is available. Advance to race
                        // selection.
                        ctx.stage = Stage::AwaitingRace {
                            email: Some(email),
                            character_name: name.to_string(),
                            password_plaintext,
                        };
                        send_race_prompt(&ctx.outbound);
                    }
                    Err(e) => {
                        warn!(conn_id, error = %e, "character-name uniqueness check failed");
                        let _ = ctx.outbound.try_send(
                            "Server error checking the name. Please try again.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        ctx.stage = Stage::AwaitingCharacterName {
                            email,
                            password_plaintext,
                        };
                        let _ = ctx
                            .outbound
                            .try_send(NEW_CHARACTER_NAME_PROMPT.as_bytes().to_vec());
                    }
                }
            }

            Stage::AwaitingRace {
                email,
                character_name,
                password_plaintext,
            } => {
                let Some(race) = match_playable_race(trimmed) else {
                    let _ = ctx.outbound.try_send(
                        format!("`{trimmed}` isn't one of the available races.\r\n")
                            .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingRace {
                        email,
                        character_name,
                        password_plaintext,
                    };
                    send_race_prompt(&ctx.outbound);
                    return;
                };
                // Advance to class selection. Catalog comes from the
                // world-scope resource the loader populated at boot.
                ctx.stage = Stage::AwaitingClass {
                    email,
                    character_name,
                    password_plaintext,
                    race,
                };
                send_class_prompt(&ctx.outbound, world);
            }

            Stage::AwaitingClass {
                email,
                character_name,
                password_plaintext,
                race,
            } => {
                let Some((class_id, class_plain_name)) = match_base_class(world, trimmed) else {
                    let _ = ctx.outbound.try_send(
                        format!("`{trimmed}` isn't one of the available classes.\r\n")
                            .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingClass {
                        email,
                        character_name,
                        password_plaintext,
                        race,
                    };
                    send_class_prompt(&ctx.outbound, world);
                    return;
                };
                ctx.stage = Stage::AwaitingGender {
                    email,
                    character_name,
                    password_plaintext,
                    race,
                    class_id,
                    class_plain_name,
                };
                send_gender_prompt(&ctx.outbound);
            }

            Stage::AwaitingGender {
                email,
                character_name,
                password_plaintext,
                race,
                class_id,
                class_plain_name,
            } => {
                let Some(gender) = match_playable_gender(trimmed) else {
                    let _ = ctx.outbound.try_send(
                        format!(
                            "`{trimmed}` isn't a recognized gender — pick one of the listed values.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingGender {
                        email,
                        character_name,
                        password_plaintext,
                        race,
                        class_id,
                        class_plain_name,
                    };
                    send_gender_prompt(&ctx.outbound);
                    return;
                };
                let stats = roll_starting_stats();
                send_stat_review(&ctx.outbound, &stats);
                ctx.stage = Stage::ReviewStatRoll {
                    email,
                    character_name,
                    password_plaintext,
                    race,
                    class_id,
                    class_plain_name,
                    gender,
                    stats,
                };
            }

            Stage::ReviewStatRoll {
                email,
                character_name,
                password_plaintext,
                race,
                class_id,
                class_plain_name,
                gender,
                stats,
            } => {
                let answer = trimmed.to_ascii_lowercase();
                let accepted = matches!(answer.as_str(), "a" | "accept" | "y" | "yes" | "");
                let rerolled = matches!(answer.as_str(), "r" | "reroll" | "n" | "no");
                if rerolled {
                    let new_stats = roll_starting_stats();
                    send_stat_review(&ctx.outbound, &new_stats);
                    ctx.stage = Stage::ReviewStatRoll {
                        email,
                        character_name,
                        password_plaintext,
                        race,
                        class_id,
                        class_plain_name,
                        gender,
                        stats: new_stats,
                    };
                    return;
                }
                if !accepted {
                    let _ = ctx.outbound.try_send(
                        "Please answer 'accept' or 'reroll'.\r\n".as_bytes().to_vec(),
                    );
                    ctx.stage = Stage::ReviewStatRoll {
                        email,
                        character_name,
                        password_plaintext,
                        race,
                        class_id,
                        class_plain_name,
                        gender,
                        stats,
                    };
                    return;
                }
                // Accepted. Persist both rows: first `Users` (with
                // a synthesized email if the player entered a
                // character name rather than an email), then
                // `Characters` linked to the new user_id. World-
                // spawn-on-success lands in the final slice;
                // today the player's bounced back to the identifier
                // prompt to log in fresh and confirm the round-trip
                // worked.
                let effective_email = email
                    .clone()
                    .unwrap_or_else(|| format!("{character_name}@local.fierymud-rs"));
                let display_name = effective_email
                    .split('@')
                    .next()
                    .unwrap_or(&effective_email)
                    .to_string();
                let hashed = match bcrypt::hash(&password_plaintext, bcrypt::DEFAULT_COST) {
                    Ok(h) => h,
                    Err(e) => {
                        warn!(conn_id, error = %e, "bcrypt hash failed");
                        let _ = ctx.outbound.try_send(
                            "Server error securing your password. Please try again.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        drop(password_plaintext);
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                drop(password_plaintext);
                // Wrap both INSERTs in a transaction so a failure
                // on the character side rolls the user row back —
                // the player can retry from the identifier prompt
                // without leaving an orphan account behind.
                let mut tx = match pool.begin().await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!(conn_id, error = %e, "creation tx begin failed");
                        let _ = ctx.outbound.try_send(
                            "Server error opening a transaction. Please try again.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                let user_id = match users::create(
                    &mut *tx,
                    &effective_email,
                    &display_name,
                    &hashed,
                )
                .await
                {
                    Ok(id) => id,
                    Err(e) => {
                        warn!(conn_id, error = %e, "user create failed");
                        let _ = ctx.outbound.try_send(
                            format!(
                                "Couldn't create the account ({e}). Please try again \
                                 with a different identifier.\r\n"
                            )
                            .into_bytes(),
                        );
                        // Drop the tx — uncommitted, so the user
                        // INSERT (if any) gets rolled back.
                        drop(tx);
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                let new_character = mud_db::characters::NewCharacter {
                    user_id: &user_id,
                    name: &character_name,
                    race,
                    gender,
                    class_id,
                    strength: stats.strength,
                    intelligence: stats.intelligence,
                    wisdom: stats.wisdom,
                    dexterity: stats.dexterity,
                    constitution: stats.constitution,
                    charisma: stats.charisma,
                };
                let character_id = match mud_db::characters::create(&mut *tx, &new_character).await
                {
                    Ok(id) => id,
                    Err(e) => {
                        warn!(conn_id, error = %e, "character create failed");
                        let _ = ctx.outbound.try_send(
                            format!(
                                "Couldn't create the character ({e}). The account \
                                 INSERT was rolled back; please try again.\r\n"
                            )
                            .into_bytes(),
                        );
                        drop(tx);
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                if let Err(e) = tx.commit().await {
                    warn!(conn_id, error = %e, "creation tx commit failed");
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Couldn't finalize creation ({e}). Both rows have been \
                             rolled back; please try again.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                // Both rows are committed. Re-fetch the User + the
                // freshly-INSERTed CharacterRow so we can hand them
                // to the same `complete_login` path the password
                // arm uses — spawns the player entity, hydrates
                // empty inventory / aliases / etc., and migrates
                // the conn from `login` to `playing`. Failures here
                // are bizarre (we just wrote these rows) but stay
                // recoverable: bounce back to the identifier prompt
                // and the player can log in fresh.
                let new_user = match users::find_by_id(pool, &user_id).await {
                    Ok(Some(u)) => u,
                    Ok(None) => {
                        warn!(conn_id, %user_id, "fresh user vanished post-commit");
                        let _ = ctx.outbound.try_send(
                            "Account created but couldn't reload it. Please log in.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                    Err(e) => {
                        warn!(conn_id, error = %e, "user reload failed");
                        let _ = ctx.outbound.try_send(
                            "Account created but couldn't reload it. Please log in.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                let new_char = match characters::find_by_name(pool, &character_name).await {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        warn!(conn_id, %character_name, "fresh character vanished post-commit");
                        let _ = ctx.outbound.try_send(
                            "Character created but couldn't reload it. Please log in.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                    Err(e) => {
                        warn!(conn_id, error = %e, "character reload failed");
                        let _ = ctx.outbound.try_send(
                            "Character created but couldn't reload it. Please log in.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                let _ = ctx.outbound.try_send(
                    format!(
                        "Welcome to FieryMUD, {character_name}! Your {gender} {race} \
                         {class_plain_name} is ready (character id {character_id}, user \
                         id {user_id}). Stepping into the world…\r\n"
                    )
                    .into_bytes(),
                );
                self.complete_login(conn_id, world, pool, new_user, new_char).await;
            }

            Stage::AwaitingPassword { user, preselected } => {
                // Lockout pre-check: if `locked_until` is set and in
                // the future, refuse before bcrypt — both to save the
                // CPU cost and to keep the lock effective even when
                // an attacker stops typing the right password.
                let now = chrono::Utc::now().naive_utc();
                if let Some(locked_until) = user.locked_until
                    && locked_until > now
                {
                    let secs_remaining = (locked_until - now).num_seconds().max(1);
                    info!(
                        conn_id,
                        email = %user.email,
                        secs_remaining,
                        "auth refused: account locked"
                    );
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Account is temporarily locked after too many failed \
                             attempts. Try again in {secs_remaining}s.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                let ok = user
                    .password_hash
                    .as_ref()
                    .is_some_and(|h| bcrypt::verify(trimmed, h).unwrap_or(false));
                if !ok {
                    // Throttle: bump the failed-login counter and lock
                    // the account once it crosses
                    // `security.max_login_attempts`. Live config; a 0
                    // or missing row disables the throttle (legacy
                    // permissive behavior).
                    let cfg = world.resource::<mud_world::RuntimeConfig>();
                    let max_attempts = cfg.get_i32("security", "max_login_attempts", 0);
                    let lock_minutes = cfg.get_i32("security", "login_timeout_minutes", 15);
                    let attempts_after = user.failed_login_attempts.saturating_add(1);
                    let lock_now = max_attempts > 0 && attempts_after >= max_attempts;
                    let _ = mud_db::users::record_failed_login(
                        pool,
                        &user.id,
                        if lock_now { Some(lock_minutes) } else { None },
                    )
                    .await;
                    info!(
                        conn_id,
                        email = %user.email,
                        attempts_after,
                        max_attempts,
                        locked = lock_now,
                        "auth failure"
                    );
                    let msg = if lock_now {
                        format!(
                            "Invalid credentials. Account locked for {lock_minutes} \
                             minutes after {attempts_after} failed attempts.\r\n"
                        )
                    } else {
                        "Invalid credentials.\r\n".to_string()
                    };
                    let _ = ctx.outbound.try_send(msg.into_bytes());
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                // Auth succeeded — reset the failed-login counter so
                // a previously-throttled account doesn't carry a
                // partial strike count into the next session.
                if user.failed_login_attempts > 0 || user.locked_until.is_some() {
                    let _ = mud_db::users::clear_failed_logins(pool, &user.id).await;
                }
                // Wizlock: when admin has set the global gate, only
                // Builder+ accounts may proceed. Refused after auth
                // so we don't leak whether the gate is on
                // pre-credential. Reset on server restart so a
                // forgotten lock doesn't outlive the deploy.
                let wizlock_active = world
                    .get_resource::<mud_world::WizLock>()
                    .is_some_and(|w| w.active);
                if wizlock_active && !user.role.at_least(mud_db::enums::UserRole::Builder) {
                    info!(
                        conn_id,
                        user_id = %user.id,
                        "auth refused: wizlock active"
                    );
                    let _ = ctx.outbound.try_send(
                        "The mud is currently locked for staff only. Please try again later.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                // Ban check. Refuses post-auth so we don't leak
                // whether an email exists pre-password. The conn
                // stays in AwaitingIdentifier (mirrors auth-failure
                // path); player can't proceed past the ban message.
                if let Ok(Some(ban)) = mud_db::bans::active_for(pool, &user.id).await {
                    info!(
                        conn_id,
                        user_id = %user.id,
                        reason = %ban.reason,
                        "auth refused: banned"
                    );
                    let until = ban
                        .expires_at
                        .map(|t| format!(" (expires {t} UTC)"))
                        .unwrap_or_default();
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Your account is banned: {}{until}\r\n",
                            ban.reason
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
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
                        let _ = ctx.outbound.try_send("Server error.\r\n".as_bytes().to_vec());
                        ctx.stage = Stage::AwaitingIdentifier;
                        let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                        return;
                    }
                };
                if chars.is_empty() {
                    let _ = ctx
                        .outbound
                        .try_send("No characters on this account.\r\n".as_bytes().to_vec());
                    ctx.stage = Stage::AwaitingIdentifier;
                    let _ = ctx.outbound.try_send(IDENT_PROMPT.as_bytes().to_vec());
                    return;
                }
                let mut menu = String::from("\r\nCharacters:\r\n");
                for (idx, c) in chars.iter().enumerate() {
                    menu.push_str(&format!("  {}. {} (level {})\r\n", idx + 1, c.name, c.level));
                }
                menu.push_str("Pick a number: ");
                let _ = ctx.outbound.try_send(menu.into_bytes());
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
                        .try_send(format!("Pick 1-{}.\r\n", characters.len()).into_bytes());
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
    #[allow(clippy::too_many_lines)]
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
        // Achievement unlock list. Empty for new characters.
        let achievement_rows = mud_db::achievements::unlocked_for(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "achievements load failed");
                Vec::new()
            });
        // Lifetime kill counter, persisted in the JSON column on
        // Characters. Defaults to 0 for new characters / null JSON.
        let kill_total: i32 = mud_db::characters::load_kill_tracking(pool, &char_row.id)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.get("total").and_then(serde_json::Value::as_i64))
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0);
        // Drunkenness counter, persisted on Characters.
        let drunk = mud_db::characters::load_drunkenness(pool, &char_row.id)
            .await
            .unwrap_or(0);
        // Last 10 received tells, newest first. Hydrates TellLog
        // so `lasttells` shows continuity across reconnects.
        let recent_tells = mud_db::tell_messages::recent_for(pool, &char_row.id, 10)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "tell history load failed");
                Vec::new()
            });
        // Optional clan membership.
        let clan = mud_db::clans::membership_for(pool, &char_row.id)
            .await
            .unwrap_or(None);

        // Script-vars + trophy JSON blobs. Either may be NULL on
        // first login or for never-touched characters; the
        // unwrap_or_else paths log + drop so a one-shot load
        // failure can't reject login.
        let script_vars_json = mud_db::characters::load_script_vars(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "script_vars load failed");
                None
            });
        let trophy_json = mud_db::characters::load_trophy(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "trophy load failed");
                None
            });
        let spell_cooldowns_json = mud_db::characters::load_spell_cooldowns(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "spell_cooldowns load failed");
                None
            });
        let cooldowns_json = mud_db::characters::load_cooldowns(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "cooldowns load failed");
                None
            });
        let ignore_list_json = mud_db::characters::load_ignore_list(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "ignore_list load failed");
                None
            });
        let effect_instances_json = mud_db::characters::load_effect_instances(pool, &char_row.id)
            .await
            .unwrap_or_else(|e| {
                warn!(conn_id, error = %e, "effect_instances load failed");
                None
            });

        // Housing summary — Ok(None) for the typical player who
        // doesn't own a house. Unwrap-Some path fires the rest of
        // the housing fetches; an error logs and skips.
        let house_summary = match mud_db::housing::for_character(pool, &char_row.id).await {
            Ok(Some(h)) => {
                let rooms = mud_db::housing::rooms_for_house(pool, h.id)
                    .await
                    .unwrap_or_default();
                let exits = mud_db::housing::exits_for_house(pool, h.id)
                    .await
                    .unwrap_or_default();
                let items = mud_db::housing::items_for_house(pool, h.id)
                    .await
                    .unwrap_or_default();
                let guests = mud_db::housing::guests_for_house(pool, h.id)
                    .await
                    .unwrap_or_default();
                Some((h, rooms, exits, items, guests))
            }
            Ok(None) => None,
            Err(e) => {
                warn!(conn_id, error = %e, "housing load failed");
                None
            }
        };
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
        // Stamp last_login exactly once at successful spawn — split
        // from save_state, which used to overwrite it on every
        // autosave (so the column meant "last save," not "last
        // login"). PreviousLogin was already captured from char_row
        // before this call, so the displayed "Last login" line keeps
        // showing the prior session's start.
        if let Err(e) = mud_db::characters::update_last_login(pool, &char_row.id).await {
            warn!(conn_id, error = %e, "last_login update failed");
        }
        let known_abilities = KnownAbilities::from_rows(&ability_rows);
        let ability_count = known_abilities.entries.len();
        let aliases = mud_world::Aliases::from_rows(&alias_rows);
        let alias_count = aliases.entries.len();
        let summary = AccountSummary {
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            characters: all_chars
                .iter()
                .map(|c| (c.name.clone(), c.level))
                .collect(),
        };
        // Build CharacterAchievements + ZoneVisits from the loaded
        // rows, applying the runtime convention: a `zone_<N>_cleared`
        // row only counts as "unlocked" once the visited-rooms set
        // covers the whole zone roster. In-progress visited sets
        // hydrate ZoneVisits so a player who comes back in the
        // middle of a zone walk doesn't lose their progress.
        let (ca_built, zv_built) = build_achievement_components(world, &achievement_rows);
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
            if !achievement_rows.is_empty() {
                if !ca_built.unlocked.is_empty() {
                    e.insert(ca_built);
                }
                if !zv_built.by_zone.is_empty() {
                    e.insert(zv_built);
                }
            }
            // KillStats is always inserted so the bump path can
            // mutate it without a "needs_init" branch on every kill.
            e.insert(mud_world::KillStats { total: kill_total });
            if drunk > 0 {
                e.insert(mud_world::Drunkenness(drunk));
            }
            // Restore wizinvis on reconnect — staff who logged out
            // while invis stay invis until they `vis` it off. Skip
            // the insert when the column is 0 so the visible-by-
            // default path doesn't carry an empty component.
            if char_row.invis_level > 0 {
                e.insert(mud_world::WizInvis(char_row.invis_level));
            }
            // Frozen lock — re-attach if the column is set. A frozen
            // player who reconnects can't dispatch commands until an
            // admin runs `thaw`. Without this, freeze was a session-
            // only sanction that the player could trivially escape
            // by `quit` + reconnect.
            if char_row.freeze_level.is_some() {
                e.insert(mud_world::Frozen);
            }
            // Wimpy threshold — the on/off switch is the `Wimpy`
            // PlayerFlag (loaded above), not this value. The component
            // is the *override percentage* used when the flag is on;
            // absent → combat.rs falls back to the 25% default. Skip
            // the insert when the column is 0 so the default path
            // doesn't carry an empty component.
            if char_row.wimpy_threshold > 0 {
                e.insert(mud_world::WimpyThreshold(char_row.wimpy_threshold));
            }
            // Poofs — only attach when at least one side is set.
            // Both NULL → no component → renderer falls back to
            // the generic vanish/appear lines.
            if char_row.poof_in.is_some() || char_row.poof_out.is_some() {
                e.insert(mud_world::Poofs {
                    poof_in: char_row.poof_in.clone(),
                    poof_out: char_row.poof_out.clone(),
                });
            }
            // ScriptVars — JSON object → BTreeMap. Tolerate a
            // garbage/legacy shape silently (drop the data) rather
            // than rejecting login.
            if let Some(json) = script_vars_json
                && let Ok(map) = serde_json::from_value::<
                    std::collections::BTreeMap<String, String>,
                >(json)
                && !map.is_empty()
            {
                e.insert(mud_world::ScriptVars(map));
            }
            // Trophy — JSON list → Trophy. Same tolerant pattern;
            // the kill counter just resets to empty on bad data
            // instead of bouncing the player off the server.
            if let Some(json) = trophy_json
                && let Ok(entries) = serde_json::from_value::<
                    std::collections::VecDeque<mud_world::TrophyEntry>,
                >(json)
                && !entries.is_empty()
            {
                e.insert(mud_world::Trophy { entries });
            }
            // SpellSlots — JSON object {in_flight: [...]} → component.
            // Tolerant: missing/garbage data starts the player with a
            // fresh empty pool rather than bouncing login.
            if let Some(json) = spell_cooldowns_json
                && let Ok(slots) = serde_json::from_value::<mud_world::SpellSlots>(json)
                && !slots.in_flight.is_empty()
            {
                e.insert(slots);
            }
            // IgnoreList — JSON array of lowercased names. Tolerant:
            // garbage data starts with an empty list rather than
            // bouncing login.
            if let Some(json) = ignore_list_json
                && let Ok(list) = serde_json::from_value::<Vec<String>>(json)
                && !list.is_empty()
            {
                e.insert(mud_world::IgnoreList(list));
            }
            // Cooldowns — JSON map of ability_id → unix_secs_ready_at.
            // Convert to Instant by computing the offset from `now`,
            // dropping any keys whose ready_at has already passed.
            // Future-proof: if the system clock jumped backwards
            // since save, the saturating_add keeps the offset
            // non-negative.
            if let Some(json) = cooldowns_json
                && let Ok(map) = serde_json::from_value::<
                    std::collections::HashMap<String, i64>,
                >(json)
            {
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                let now_inst = std::time::Instant::now();
                let mut cd = mud_world::Cooldowns::default();
                for (k, ready_unix) in map {
                    let Ok(id) = k.parse::<i32>() else { continue };
                    let secs_left = ready_unix.saturating_sub(now_unix);
                    if secs_left <= 0 {
                        continue;
                    }
                    cd.ready_at.insert(
                        id,
                        now_inst + std::time::Duration::from_secs(u64::try_from(secs_left).unwrap_or(0)),
                    );
                }
                if !cd.ready_at.is_empty() {
                    e.insert(cd);
                }
            }
            if let Some(c) = clan {
                e.insert(mud_world::ClanMembership {
                    clan_id: c.clan_id,
                    rank: c.rank,
                    clan_name: c.clan_name,
                    clan_abbrev: c.clan_abbrev,
                });
            }
            // Hydrate TellLog from the persisted tail. Insertion
            // order: newest-first as returned by `recent_for`, but
            // `push_at` puts each at the front, so we iterate the
            // *oldest* first to land newest at the head.
            if !recent_tells.is_empty() {
                use std::time::SystemTime;
                let mut log = mud_world::TellLog::with_cap(10);
                for row in recent_tells.iter().rev() {
                    let when = SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(
                            u64::try_from(row.sent_at.and_utc().timestamp().max(0))
                                .unwrap_or(0),
                        );
                    log.push_at(row.sender_name.clone(), when);
                }
                e.insert(log);
            }
            if let Some((house, rooms, exits, items, guests)) = house_summary {
                e.insert(mud_world::HouseSummary {
                    house_id: house.id,
                    entrance_room: mud_world::WorldKey {
                        zone: house.entrance_room_zone_id,
                        id: house.entrance_room_id,
                    },
                    return_room: house
                        .return_room_zone_id
                        .zip(house.return_room_id)
                        .map(|(zone, id)| mud_world::WorldKey { zone, id }),
                    rooms: rooms
                        .into_iter()
                        .map(|r| mud_world::HouseRoomEntry {
                            id: r.id,
                            local_index: r.local_index,
                            name: r.name,
                            description: r.description,
                            is_peaceful: r.is_peaceful,
                            capacity: r.capacity,
                        })
                        .collect(),
                    exits: exits
                        .into_iter()
                        .map(|x| mud_world::HouseExitEntry {
                            from_room_id: x.from_room_id,
                            to_room_id: x.to_room_id,
                            direction: x.direction,
                        })
                        .collect(),
                    items: items
                        .into_iter()
                        .map(|i| mud_world::HouseItemEntry {
                            id: i.id,
                            room_id: i.room_id,
                            object_zone_id: i.object_zone_id,
                            object_id: i.object_id,
                        })
                        .collect(),
                    guests: guests
                        .into_iter()
                        .map(|g| mud_world::HouseGuestEntry {
                            character_id: g.character_id,
                            can_place: g.can_place,
                        })
                        .collect(),
                });
            }
        }
        // EffectInstances — wall-clock-stamped envelope. The helper
        // drops non-Admin entries past EFFECT_DISCONNECT_CAP_SECS,
        // restores the rest with elapsed time deducted, and spawns
        // one effect entity per surviving record.
        if let Some(json) = effect_instances_json
            && let Ok(persisted) = serde_json::from_value::<PersistedEffects>(json)
        {
            restore_persisted_effects(world, entity, persisted);
        }
        // Track the spawn room toward zone-clear, so a player who
        // logs in inside the last unvisited room of a zone gets the
        // unlock immediately instead of having to step out and back.
        // Also apply any environmental effects bound to the room so
        // logging in mid-aura doesn't skip the application.
        if let Some(room) = world.get::<Located>(entity).map(|l| l.0) {
            commands::mark_room_visited(world, entity, room);
            commands::apply_room_environment_at_login(world, entity, room);
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

/// Translate the raw `CharacterAchievement` rows into the runtime
/// pair of components the rest of the world expects:
///
/// * `CharacterAchievements` — unlocked set. A row counts as
///   unlocked iff it's a one-shot (no progress), or the
///   matching zone roster is fully visited.
/// * `ZoneVisits` — partial-progress visited rooms keyed by zone.
///
/// Pulls room counts from `WorldKeyIndex` and code lookup from
/// `AchievementCatalog`; both must already be installed as
/// resources by the loader.
fn build_achievement_components(
    world: &World,
    rows: &[mud_db::achievements::CharacterAchievementRow],
) -> (mud_world::CharacterAchievements, mud_world::ZoneVisits) {
    use std::collections::HashSet;
    let zone_room_counts: HashMap<i32, usize> = world
        .get_resource::<WorldKeyIndex>()
        .map(|ki| {
            let mut counts: HashMap<i32, usize> = HashMap::new();
            for (z, _) in ki.rooms.keys() {
                *counts.entry(*z).or_insert(0) += 1;
            }
            counts
        })
        .unwrap_or_default();
    let achievement_codes: HashMap<i32, String> = world
        .get_resource::<mud_world::AchievementCatalog>()
        .map(|c| {
            c.by_id
                .iter()
                .map(|(id, def)| (*id, def.code.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut ca = mud_world::CharacterAchievements::default();
    let mut zv = mud_world::ZoneVisits::default();
    for row in rows {
        let code = achievement_codes.get(&row.achievement_id);
        let zone_n = code.and_then(|c| {
            c.strip_prefix("zone_")
                .and_then(|s| s.strip_suffix("_cleared"))
                .and_then(|s| s.parse::<i32>().ok())
        });
        if let Some(n) = zone_n {
            let visited: HashSet<i32> = row
                .progress
                .as_ref()
                .and_then(|p| p.get("visited"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_i64().map(|x| i32::try_from(x).unwrap_or(0)))
                        .collect()
                })
                .unwrap_or_default();
            let total = zone_room_counts.get(&n).copied().unwrap_or(0);
            if total > 0 && visited.len() >= total {
                ca.unlocked.insert(row.achievement_id);
            }
            if !visited.is_empty() {
                zv.by_zone.insert(n, visited);
            }
        } else {
            ca.unlocked.insert(row.achievement_id);
        }
    }
    (ca, zv)
}

/// Single spawn path for a player entity. The `Located(room_entity)`
/// component is added in a follow-up insert (after spawn) only when
/// the starting room resolved — keeping the core bundle one place
/// avoids the recurring "did I update both branches?" bug we hit
/// three times before consolidating.
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_player(world: &mut World, user: &User, c: &CharacterRow, outbound: Outbound) -> Entity {
    let race_start = world
        .resource::<mud_world::RaceDefaults>()
        .start_room_by_race
        .get(&c.race)
        .copied();
    let (zone, room) = pick_starting_room(c, race_start);

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
        ward_pct: 0,
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
        let _ = outbound.try_send(format!(
            "\r\nWelcome, {name}.\r\nYou appear in: {room_name}\r\n\r\n",
            name = c.name,
        ).into_bytes());
    } else {
        let _ = outbound.try_send(
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
            Prompt(commands::sanitize_prompt_template(&c.prompt)),
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
        // Lifetime time-played in seconds, with a paired anchor
        // for the save-time accumulator. Each call to save_player
        // credits `now - LastPersistedAt` into TimePlayed (both
        // component and DB column) and resets the anchor — so
        // autosave + final-save together cover the full session
        // without double-counting any window.
        e.insert(mud_world::TimePlayed(c.time_played));
        e.insert(mud_world::LastPersistedAt(std::time::Instant::now()));
        // Capture the previous-session login timestamp BEFORE
        // save_state's UPDATE NOW() runs and overwrites it with
        // the current login. Absent for first-time logins.
        if let Some(ts) = c.last_login {
            e.insert(mud_world::PreviousLogin(ts.and_utc().timestamp()));
        }
    }
    entity
}

#[allow(clippy::too_many_lines)]
/// Outcome of one `save_player` run. Save is all-or-nothing — every
/// per-character DB write runs inside a single Postgres transaction,
/// and if any write fails the whole tx rolls back. So the outcome
/// reduces to three states:
///
/// * `aborted = true` — the entity wasn't a player at all (no Account
///   component; e.g. mob via `switch`). Nothing was written and
///   nothing needed to be.
/// * `committed = true, error = None` — every write succeeded and the
///   tx committed; durable.
/// * `committed = false, error = Some(msg)` — at least one write
///   failed, tx rolled back, character row is unchanged from the last
///   successful save. `cmd_save` surfaces `error` to the player so
///   they know to retry.
#[derive(Debug, Default)]
pub(crate) struct SaveOutcome {
    pub aborted: bool,
    pub committed: bool,
    pub error: Option<String>,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn save_player(
    world: &mut World,
    entity: Entity,
    pool: &PgPool,
) -> SaveOutcome {
    let Some(account) = world.get::<Account>(entity).cloned() else {
        return SaveOutcome {
            aborted: true,
            ..SaveOutcome::default()
        };
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
    // Wizinvis level — `WizInvis(level)` round-trips through
    // `Characters.invis_level`. Absent component → 0 (visible).
    let invis_level = world
        .get::<mud_world::WizInvis>(entity)
        .map_or(0, |w| w.0);
    // Frozen marker → freeze_level. The schema column is
    // nullable; we send None when the marker is absent, so an
    // unfreeze on this session genuinely clears the lock instead
    // of leaving a stale level on the row.
    let freeze_level: Option<i32> = world
        .get::<mud_world::Frozen>(entity)
        .map(|_| 1);
    // Wimpy threshold — `WimpyThreshold(pct)` round-trips through
    // the schema column. The on/off switch is the `Wimpy` PlayerFlag,
    // not this value: 0 means "no explicit override; use the 25%
    // default when the flag is on." Absent component → store 0; on
    // reload the login path skips the insert (since `> 0` is false)
    // so combat falls back to the default. Mirrors the contract in
    // combat.rs:909 and the load comment above.
    let wimpy_threshold = world
        .get::<mud_world::WimpyThreshold>(entity)
        .map_or(0, |w| w.0);
    // Poofs — clone the message strings out so the borrow checker
    // doesn't drag a `&Poofs` into the awaited `save_state` call.
    let (poof_in, poof_out) = world
        .get::<mud_world::Poofs>(entity)
        .map_or((None, None), |p| (p.poof_in.clone(), p.poof_out.clone()));

    // Snapshot every Item rooted at the player — both directly carried
    // and nested inside any container the player carries. BFS keeps
    // parents before children so save_inventory_diff can resolve
    // `parent_idx` for newly-acquired items inside newly-acquired
    // containers. `entity_for_idx` is the parallel Vec we use to write
    // back assigned PersistedItemId(s) after the diff returns.
    let (new_items, entity_for_idx): (
        Vec<mud_db::character_items::CharacterItemSnap>,
        Vec<Entity>,
    ) = {
        use std::collections::HashMap;
        // Single query — every per-item field we need so the build
        // loop below doesn't reborrow World. `Charges` and
        // `LiquidContainer` are both Optional since most items have
        // neither.
        type ItemSnap = (
            Entity,
            Entity,
            WorldKey,
            Option<EquippedSlot>,
            Option<mud_world::PersistedItemId>,
            Option<mud_world::Charges>,
            Option<mud_world::LiquidContainer>,
        );
        let all_items: Vec<ItemSnap> = {
            let mut q = world.query::<(
                Entity,
                &Located,
                &WorldKey,
                Option<&EquippedSlot>,
                Option<&mud_world::PersistedItemId>,
                Option<&mud_world::Charges>,
                Option<&mud_world::LiquidContainer>,
                &Item,
            )>();
            q.iter(world)
                .map(|(e, l, wk, eq, pid, ch, lc, _)| {
                    (e, l.0, *wk, eq.copied(), pid.copied(), ch.copied(), lc.cloned())
                })
                .collect()
        };
        // BFS from `entity` (the player) through "is parent of" edges.
        let mut order: Vec<ItemSnap> = Vec::new();
        let mut entity_to_idx: HashMap<Entity, usize> = HashMap::new();
        let mut frontier: Vec<Entity> = vec![entity];
        while let Some(parent) = frontier.pop() {
            for snap in &all_items {
                let (e, p, _, _, _, _, _) = snap;
                if *p == parent && !entity_to_idx.contains_key(e) {
                    entity_to_idx.insert(*e, order.len());
                    order.push(snap.clone());
                    frontier.push(*e);
                }
            }
        }
        // Pull persisted-id of the parent (if loaded) from the entity
        // map so the diff can set container_id directly without waiting
        // for the parent's INSERT.
        let parent_pid_lookup: HashMap<Entity, Option<i32>> = order
            .iter()
            .map(|(e, _, _, _, pid, _, _)| (*e, pid.map(|p| p.0)))
            .collect();

        let mut snaps: Vec<mud_db::character_items::CharacterItemSnap> = Vec::with_capacity(order.len());
        let mut ents: Vec<Entity> = Vec::with_capacity(order.len());
        for (e, parent, wk, eq, pid, ch, lc) in &order {
            let parent_persisted_id = if *parent == entity {
                None
            } else {
                parent_pid_lookup.get(parent).copied().flatten()
            };
            let parent_idx = if *parent == entity {
                None
            } else {
                entity_to_idx.get(parent).copied()
            };
            snaps.push(mud_db::character_items::CharacterItemSnap {
                persisted_id: pid.map(|p| p.0),
                object_zone_id: wk.zone,
                object_id: wk.id,
                equipped_location: eq.map(|s| s.0.db_label().to_string()),
                parent_persisted_id,
                parent_idx,
                charges: ch.map(|c| c.0),
                liquid_remaining: lc.as_ref().map(|l| l.remaining),
                liquid_type: lc.as_ref().map(|l| l.liquid.clone()),
            });
            ents.push(*e);
        }
        (snaps, ents)
    };
    let item_count = new_items.len();

    // Pre-collect all snapshot values so the inside-tx block doesn't
    // re-borrow the world. Each helper that takes a JSON blob /
    // counter value reads from these locals; the world only gets
    // re-borrowed at the very end (post-commit) to stamp PersistedItemId
    // and bump TimePlayed/LastPersistedAt.
    let drunk = world
        .get::<mud_world::Drunkenness>(entity)
        .map_or(0, |d| d.0);
    let bank = world.get::<BankWealth>(entity).map_or(0, |b| b.0);
    let script_vars_json = world
        .get::<mud_world::ScriptVars>(entity)
        .filter(|sv| !sv.0.is_empty())
        .and_then(|sv| serde_json::to_value(&sv.0).ok());
    let trophy_json = world
        .get::<mud_world::Trophy>(entity)
        .filter(|t| !t.entries.is_empty())
        .and_then(|t| serde_json::to_value(&t.entries).ok());
    let spell_cooldowns_json = world
        .get::<mud_world::SpellSlots>(entity)
        .filter(|s| !s.in_flight.is_empty())
        .and_then(|s| serde_json::to_value(s).ok());
    // Cooldowns: ready_at Instants → wall-clock unix seconds so the
    // value is meaningful across process restarts. Drop already-
    // expired keys so the JSON stays small.
    let cooldowns_json: Option<serde_json::Value> = world
        .get::<mud_world::Cooldowns>(entity)
        .and_then(|cd| {
            let now = std::time::Instant::now();
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            let map: std::collections::HashMap<String, i64> = cd
                .ready_at
                .iter()
                .filter_map(|(id, ready_at)| {
                    if *ready_at <= now {
                        return None;
                    }
                    let secs_left = ready_at.duration_since(now).as_secs();
                    Some((id.to_string(), now_unix.saturating_add(i64::try_from(secs_left).unwrap_or(0))))
                })
                .collect();
            if map.is_empty() {
                None
            } else {
                serde_json::to_value(&map).ok()
            }
        });
    let ignore_list_json: Option<serde_json::Value> = world
        .get::<mud_world::IgnoreList>(entity)
        .filter(|l| !l.0.is_empty())
        .and_then(|l| serde_json::to_value(&l.0).ok());
    // Active EffectInstances on this player. Query for every effect
    // entity whose AppliedTo points at the player; flatten to the
    // persistence shape with optional ModifyDelta. Permanent effects
    // (`remaining_secs < 0`) and short-lived buffs alike are
    // captured — the load path is what enforces the 1h cap.
    let effect_instances_json: Option<serde_json::Value> = {
        let mut q = world.query::<(
            &mud_world::EffectInstance,
            &mud_world::AppliedTo,
            Option<&mud_world::ModifyDelta>,
        )>();
        let effects: Vec<PersistedEffectInstance> = q
            .iter(world)
            .filter(|(_, applied, _)| applied.0 == entity)
            .map(|(inst, _, modd)| PersistedEffectInstance {
                kind: inst.kind,
                name: inst.name.clone(),
                strength: inst.strength,
                remaining_secs: inst.remaining_secs,
                source: inst.source.clone(),
                ability_id: inst.ability_id,
                modify_delta: modd.map(|m| (m.target.clone(), m.amount)),
            })
            .collect();
        if effects.is_empty() {
            None
        } else {
            let saved_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            serde_json::to_value(&PersistedEffects {
                saved_at_unix,
                effects,
            })
            .ok()
        }
    };
    let ability_rows: Vec<mud_db::character_abilities::CharacterAbilityRow> = world
        .get::<KnownAbilities>(entity)
        .map(KnownAbilities::to_rows)
        .unwrap_or_default();
    let alias_rows: Vec<mud_db::character_aliases::CharacterAliasRow> = world
        .get::<mud_world::Aliases>(entity)
        .map(mud_world::Aliases::to_rows)
        .unwrap_or_default();
    let alias_count = alias_rows.len();
    let core_stats_payload: Option<mud_db::characters::CoreStatsPayload> = world
        .get::<CoreStats>(entity)
        .copied()
        .map(Into::into);
    // Time-played accumulator. We compute the deltas now, but only
    // bump the in-memory anchor AFTER the tx commits — otherwise a
    // rolled-back save would advance the local counter without the
    // DB row reflecting it.
    let now_inst = std::time::Instant::now();
    let session_delta_secs: i32 = world
        .get::<mud_world::LastPersistedAt>(entity)
        .map(|a| now_inst.duration_since(a.0).as_secs())
        .and_then(|s| i32::try_from(s).ok())
        .unwrap_or(0);
    let new_time_played: Option<i32> = if session_delta_secs > 0 {
        Some(
            world
                .get::<mud_world::TimePlayed>(entity)
                .map_or(0, |t| t.0)
                .saturating_add(session_delta_secs),
        )
    } else {
        None
    };

    // === Single transaction wraps every per-character DB write ===
    //
    // All-or-nothing: if any save_X fails, the `?` short-circuits,
    // the inner block returns Err, the tx drops without commit (auto-
    // rollback), and the character row is unchanged from the last
    // successful save. Postgres SAVEPOINT is implicit on `?`-bubbling
    // so we don't manage it explicitly. PersistedItemId stamping
    // happens AFTER commit so a rolled-back save can't leave entities
    // pointing at row IDs that don't exist.
    let cid = account.character_id.clone();
    let tx_result: Result<std::collections::HashMap<usize, i32>, mud_db::sqlx::Error> = async {
        let mut tx = pool.begin().await?;
        characters::save_state(
            &mut *tx,
            &cid,
            &mud_db::characters::CharacterStatePayload {
                hit_points: hp,
                stamina,
                current_room_zone_id: zone_id,
                current_room_id: room_id,
                recall_room_zone_id: recall_zone,
                recall_room_id: recall_room,
                player_flags: &flags,
                prompt: &prompt,
                title: title.as_deref(),
                description: description.as_deref(),
                wealth,
                experience,
                skill_points,
                hunger,
                thirst,
                invis_level,
                freeze_level,
                wimpy_threshold,
                poof_in: poof_in.as_deref(),
                poof_out: poof_out.as_deref(),
            },
        )
        .await?;
        let assigned =
            mud_db::character_items::save_inventory_diff(&mut tx, &cid, &new_items).await?;
        mud_db::characters::save_drunkenness(&mut *tx, &cid, drunk).await?;
        mud_db::characters::save_script_vars(&mut *tx, &cid, script_vars_json.as_ref()).await?;
        mud_db::characters::save_trophy(&mut *tx, &cid, trophy_json.as_ref()).await?;
        mud_db::characters::save_spell_cooldowns(
            &mut *tx,
            &cid,
            spell_cooldowns_json.as_ref(),
        )
        .await?;
        mud_db::characters::save_cooldowns(&mut *tx, &cid, cooldowns_json.as_ref()).await?;
        mud_db::characters::save_ignore_list(&mut *tx, &cid, ignore_list_json.as_ref()).await?;
        mud_db::characters::save_effect_instances(
            &mut *tx,
            &cid,
            effect_instances_json.as_ref(),
        )
        .await?;
        mud_db::characters::save_bank_wealth(&mut *tx, &cid, bank).await?;
        if let Some(t) = new_time_played {
            mud_db::characters::save_time_played(&mut *tx, &cid, t).await?;
        }
        mud_db::character_abilities::save_for(&mut tx, &cid, &ability_rows).await?;
        mud_db::character_aliases::save_for(&mut tx, &cid, &alias_rows).await?;
        if let Some(stats) = &core_stats_payload {
            characters::save_core_stats(&mut *tx, &cid, stats).await?;
        }
        tx.commit().await?;
        Ok(assigned)
    }
    .await;

    let mut outcome = SaveOutcome::default();
    match tx_result {
        Ok(assigned) => {
            outcome.committed = true;
            // Stamp newly-INSERTed CharacterItems rows' ids onto the
            // entities. Done AFTER tx.commit() so a rolled-back save
            // can't leave entities pointing at non-existent rows.
            for (idx, new_id) in assigned {
                if let Some(target) = entity_for_idx.get(idx).copied()
                    && let Ok(mut em) = world.get_entity_mut(target)
                {
                    em.insert(mud_world::PersistedItemId(new_id));
                }
            }
            // Bump the time-played anchor on success only, mirroring
            // the same "DB-first, then in-memory" rule.
            if let Some(t) = new_time_played
                && let Ok(mut em) = world.get_entity_mut(entity)
            {
                em.insert(mud_world::TimePlayed(t));
                em.insert(mud_world::LastPersistedAt(now_inst));
            }
        }
        Err(e) => {
            warn!(error = %e, character_id = %account.character_id, "save tx failed; rolled back");
            outcome.error = Some(e.to_string());
        }
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
        committed = outcome.committed,
        "player saved"
    );
    outcome
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
            // Stamp the row's id so save_inventory_diff knows to UPDATE
            // this row instead of issuing a delete-and-reinsert that
            // would clobber DB columns the runtime doesn't own
            // (condition, instance_flags, custom_name, etc.).
            if let Ok(mut e) = world.get_entity_mut(item_entity) {
                e.insert(mud_world::PersistedItemId(row.id));
            }
            if let Some(slot_str) = row.equipped_location.as_deref()
                && let Some(slot) = Slot::from_label(slot_str)
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                e.insert(EquippedSlot(slot));
            }
            // Charges: prefer the persisted per-instance value when
            // the row has one (>= 0); fall back to the proto's binding
            // charges so freshly-inserted-by-admin rows that left
            // charges at the schema default `-1` still get a sensible
            // initial pool. Wands that were half-spent before logout
            // now come back half-spent.
            let proto_charges = world
                .resource::<mud_world::ObjectAbilityCatalog>()
                .by_key
                .get(&(proto.zone_id, proto.id))
                .and_then(|v| v.first().and_then(|b| b.charges));
            let resolved_charges = if row.charges >= 0 {
                Some(row.charges)
            } else {
                proto_charges
            };
            if let Some(charges) = resolved_charges
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                e.insert(mud_world::Charges(charges));
            }
            // LiquidContainer: if the row has a saved liquid_type, use
            // the saved liquid+remaining; otherwise the proto default
            // (already attached above) stands. This makes flask /
            // waterskin state survive disconnect — no more "drink three
            // sips, log out, log back in to a full skin" exploit.
            if let Some(saved_liq) = row.liquid_type.clone()
                && let Ok(mut e) = world.get_entity_mut(item_entity)
                && let Some(mut lc) = e.get_mut::<mud_world::LiquidContainer>()
            {
                lc.liquid = saved_liq;
                lc.remaining = row.liquid_remaining.clamp(0, lc.capacity);
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

/// Validate a freshly-typed character name against the runtime's
/// length / charset rules. Returns `Ok(())` on success, or an
/// `Err(message)` ready to ship straight to the player. Doesn't
/// hit the DB — uniqueness is checked separately so the cheap
/// rejection path doesn't waste a query.
fn validate_new_character_name(name: &str) -> Result<(), String> {
    let len = name.chars().count();
    if !(MIN_CHARACTER_NAME_LEN..=MAX_CHARACTER_NAME_LEN).contains(&len) {
        return Err(format!(
            "Character name must be {MIN_CHARACTER_NAME_LEN}–{MAX_CHARACTER_NAME_LEN} \
             characters."
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(String::from(
            "Character name may only contain letters (A-Z, a-z).",
        ));
    }
    Ok(())
}

/// Render the race-selection prompt. Lists `PLAYABLE_RACES` in a
/// single comma-joined line; players type any of the names back.
fn send_race_prompt(outbound: &Outbound) {
    let mut msg = String::from("Available races: ");
    for (i, race) in PLAYABLE_RACES.iter().enumerate() {
        if i > 0 {
            msg.push_str(", ");
        }
        msg.push_str(race);
    }
    msg.push_str("\r\nRace: ");
    let _ = outbound.try_send(msg.into_bytes());
}

/// Match a freshly-typed race against the playable list. Returns
/// the canonical SHOUTCASE form on success — used as both the
/// pretty echo and the `Characters.race` enum value at INSERT
/// time. Case-insensitive equality only; partial-match would be
/// ambiguous between e.g. ELF and `HALF_ELF`.
fn match_playable_race(input: &str) -> Option<&'static str> {
    let needle = input.trim().to_ascii_uppercase();
    PLAYABLE_RACES.iter().copied().find(|r| *r == needle)
}

/// Render the class-selection prompt. Lists every `ClassCatalog`
/// entry with `is_subclass = false` — the four base classes today
/// (Sorcerer / Cleric / Warrior / Rogue). Subclass specializations
/// (Paladin, Diabolist, Pyromancer, …) lock in via a follow-on
/// stage once the gating data lands.
fn send_class_prompt(outbound: &Outbound, world: &World) {
    let mut bases: Vec<String> = world
        .resource::<mud_world::ClassCatalog>()
        .by_id
        .values()
        .filter(|c| !c.is_subclass)
        .map(|c| c.plain_name.clone())
        .collect();
    bases.sort();
    let mut msg = String::from("Available classes: ");
    msg.push_str(&bases.join(", "));
    msg.push_str("\r\nClass: ");
    let _ = outbound.try_send(msg.into_bytes());
}

/// Look up a base class by `plain_name`, case-insensitively.
/// Returns `(class_id, canonical_plain_name)` on hit so the caller
/// can echo the canonical-cased form and stash the id for the
/// future `Characters` INSERT.
fn match_base_class(world: &World, input: &str) -> Option<(i32, String)> {
    let needle = input.trim().to_ascii_lowercase();
    world
        .resource::<mud_world::ClassCatalog>()
        .by_id
        .values()
        .find(|c| !c.is_subclass && c.plain_name.to_ascii_lowercase() == needle)
        .map(|c| (c.id, c.plain_name.clone()))
}

/// Render the gender-selection prompt. Lists the three accepted
/// values from `PLAYABLE_GENDERS` — neutral is the schema
/// default and stays in the picker for nonbinary characters.
fn send_gender_prompt(outbound: &Outbound) {
    let mut msg = String::from("Available genders: ");
    msg.push_str(&PLAYABLE_GENDERS.join(", "));
    msg.push_str("\r\nGender: ");
    let _ = outbound.try_send(msg.into_bytes());
}

/// Match a freshly-typed gender against the accepted list.
/// Case-insensitive equality; returns the canonical lowercase
/// form so the persisted `Characters.gender` value is consistent
/// regardless of how the player typed it.
fn match_playable_gender(input: &str) -> Option<&'static str> {
    let needle = input.trim().to_ascii_lowercase();
    PLAYABLE_GENDERS.iter().copied().find(|g| *g == needle)
}

/// Classic 3d6-per-stat roll for a brand-new character. No race
/// or class adjustments here — those layer on at spawn time
/// from per-race / per-class modifier tables once the gating
/// data lands. Returns six stats in the canonical order STR /
/// INT / WIS / DEX / CON / CHA.
fn roll_starting_stats() -> CoreStats {
    let roll = || -> i32 {
        (0..3)
            .map(|_| i32::try_from(rand::random_range(1u32..=6)).unwrap_or(1))
            .sum()
    };
    CoreStats {
        strength: roll(),
        intelligence: roll(),
        wisdom: roll(),
        dexterity: roll(),
        constitution: roll(),
        charisma: roll(),
    }
}

/// Render the freshly-rolled stat block + accept/reroll prompt.
/// Bonuses come from the same `CoreStats::bonus` helper used by
/// score so the player sees what their numbers will mean before
/// committing.
fn send_stat_review(outbound: &Outbound, stats: &CoreStats) {
    let line = format!(
        "Rolled stats:\r\n  STR {:>2} ({:+})  INT {:>2} ({:+})  WIS {:>2} ({:+})\r\n  \
         DEX {:>2} ({:+})  CON {:>2} ({:+})  CHA {:>2} ({:+})\r\nAccept or reroll? \
         (accept/reroll): ",
        stats.strength,
        CoreStats::bonus(stats.strength),
        stats.intelligence,
        CoreStats::bonus(stats.intelligence),
        stats.wisdom,
        CoreStats::bonus(stats.wisdom),
        stats.dexterity,
        CoreStats::bonus(stats.dexterity),
        stats.constitution,
        CoreStats::bonus(stats.constitution),
        stats.charisma,
        CoreStats::bonus(stats.charisma),
    );
    let _ = outbound.try_send(line.into_bytes());
}

/// Shared prompt for the `ConfirmCreate` doorway. `is_email`
/// switches the noun so the player sees "account" / "character"
/// matching the identifier they typed.
fn send_confirm_create_prompt(outbound: &Outbound, identifier: &str, is_email: bool) {
    let kind_label = if is_email { "account" } else { "character" };
    let _ = outbound.try_send(
        format!(
            "I don't see a {kind_label} for `{identifier}`. Create a new one? (yes/no): "
        )
        .into_bytes(),
    );
}

/// Spawn-room priority chain, in descending order:
///
/// 1. **Last-save location** (`current_room_*`) — what `save_state`
///    writes on every save / autosave / disconnect. A character who
///    rented / camped / disconnected mid-zone comes back where they
///    left off.
/// 2. **Recall point** (`recall_room_*`) — set when the player
///    touched a touchstone. Used when the persisted location is
///    unset (e.g. a never-saved fresh character whose creation flow
///    set recall but not `current_room`).
/// 3. **Race starting room** — per-race default from `Races.start_room_*`.
///    The right place for a fresh character to land before they've
///    earned a recall.
/// 4. **Void** — last-resort error fallback (zone 0, room 0). Reached
///    when even the race lookup is missing (e.g. unmapped legacy
///    race string, or NULL columns in the `Races` row).
fn pick_starting_room(c: &CharacterRow, race_start: Option<(i32, i32)>) -> (i32, i32) {
    if let (Some(z), Some(r)) = (c.current_room_zone_id, c.current_room_id) {
        return (z, r);
    }
    if let (Some(z), Some(r)) = (c.recall_room_zone_id, c.recall_room_id) {
        return (z, r);
    }
    if let Some(rs) = race_start {
        return rs;
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
            time_played: 0,
            last_login: None,
            invis_level: 0,
            freeze_level: None,
            wimpy_threshold: 0,
            poof_in: None,
            poof_out: None,
        }
    }

    #[test]
    fn current_room_wins_when_both_set() {
        let r = row(Some((30, 5)), Some((10, 1)));
        assert_eq!(pick_starting_room(&r, Some((50, 1))), (30, 5));
    }

    #[test]
    fn falls_back_to_recall_when_current_unset() {
        let r = row(None, Some((10, 1)));
        assert_eq!(pick_starting_room(&r, Some((50, 1))), (10, 1));
    }

    #[test]
    fn falls_back_to_race_when_current_and_recall_unset() {
        let r = row(None, None);
        assert_eq!(pick_starting_room(&r, Some((50, 1))), (50, 1));
    }

    #[test]
    fn falls_back_to_void_when_everything_unset() {
        let r = row(None, None);
        assert_eq!(pick_starting_room(&r, None), FALLBACK_START);
    }

    #[test]
    fn partial_current_falls_through_to_recall() {
        // current has only zone, no room id — both must be Some to use it.
        let mut r = row(None, Some((10, 1)));
        r.current_room_zone_id = Some(30);
        // current_room_id stays None — pick_starting_room should skip it.
        assert_eq!(pick_starting_room(&r, Some((50, 1))), (10, 1));
    }

    // --- creation-flow validators ---
    //
    // These guard the user-typed inputs to the login creation flow
    // (slices 3-6). Snapshot tests in spirit — the per-validator
    // contract should stay stable as the picker lists evolve.

    #[test]
    fn character_name_rejects_too_short() {
        assert!(validate_new_character_name("ab").is_err());
    }

    #[test]
    fn character_name_rejects_too_long() {
        let too_long = "a".repeat(MAX_CHARACTER_NAME_LEN + 1);
        assert!(validate_new_character_name(&too_long).is_err());
    }

    #[test]
    fn character_name_rejects_non_letters() {
        assert!(validate_new_character_name("Strider2").is_err());
        assert!(validate_new_character_name("Hax0r").is_err());
        assert!(validate_new_character_name("hyphen-name").is_err());
        assert!(validate_new_character_name("with space").is_err());
    }

    #[test]
    fn character_name_accepts_mixed_case_letters() {
        assert!(validate_new_character_name("Strider").is_ok());
        assert!(validate_new_character_name("aragorn").is_ok());
        assert!(validate_new_character_name("MAGES").is_ok());
    }

    #[test]
    fn race_match_is_case_insensitive() {
        assert_eq!(match_playable_race("human"), Some("HUMAN"));
        assert_eq!(match_playable_race("Half_Elf"), Some("HALF_ELF"));
        assert_eq!(match_playable_race("ELF"), Some("ELF"));
    }

    #[test]
    fn race_match_rejects_partial_or_unknown() {
        // No prefix matching — ELF and HALF_ELF would collide.
        assert_eq!(match_playable_race("hu"), None);
        // Schema enum value but not in the playable subset.
        assert_eq!(match_playable_race("DEMON"), None);
        assert_eq!(match_playable_race("DRAGON_FIRE"), None);
    }

    #[test]
    fn gender_match_is_case_insensitive() {
        assert_eq!(match_playable_gender("male"), Some("male"));
        assert_eq!(match_playable_gender("FEMALE"), Some("female"));
        assert_eq!(match_playable_gender("Neutral"), Some("neutral"));
    }

    #[test]
    fn gender_match_rejects_unknown() {
        assert_eq!(match_playable_gender("nonbinary"), None);
        assert_eq!(match_playable_gender(""), None);
    }
}
