use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use mud_db::character_items::CharacterItemRow;
use mud_db::{characters, characters::CharacterRow, sqlx::PgPool, users, users::User};
use mud_net::{ConnId, Outbound};
use mud_world::{
    Account, AccountSummary, AttachedTriggers, BankWealth, BoardLink, CombatStats, CoreStats,
    Description, EquippedSlot, Follower, Ghost, Health, Item, Keywords, KnownAbilities,
    LiquidContainer, Located, LoggedInAt, Mob, MobPrototypes, Named, ObjectPrototypes, Online,
    Player, PlayerFlags, Posture, PostureKind, Profile, Prompt, RecallPoint, Slot, Stamina, Title,
    TriggerCatalog, Wealth, WearableIn, WorldKey, WorldKeyIndex, wear_flags_primary_slot,
};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{error, info, warn};

use crate::autosave::SaveCoordinator;
use crate::commands::{self, Connection};

/// Pre-login banner — XML-Lite content. Live banner content lives
/// in the schema's `LoginMessage` table (stage `WELCOME_BANNER`,
/// variant `default`) and is loaded into [`mud_world::LoginMessages`]
/// at boot. This constant is the compile-time fallback used when
/// the row is missing (fresh DB / empty table). `login_message_bytes`
/// renders the markup to ANSI before it reaches the wire, so
/// builders can edit the row without touching escape sequences.
///
/// 5-stop xterm-256 fire gradient (`<c196>` → `<c202>` → `<c208>`
/// → `<c214>` → `<c220>`) — the same palette the legacy C++ logo
/// uses, peaking at the Y in the middle and fading back through
/// 214/208/202 for M U D so the flame is brightest at its core.
/// Glyphs are the `╗ ╝ ║ ═` Unicode box-drawing block, which
/// Mudlet, `BlightMud`, and the major web clients all render with
/// their default fonts; 16-color terminals quietly down-sample
/// the gradient to their nearest match.
const BANNER_FALLBACK: &str = "\
\r\n\
   <c196> ███████╗</> <c196>██╗</><c202>███████╗</><c202>██████╗ </><c208>██╗   ██╗</><c214>███╗   ███╗</><c214>██╗   ██╗</><c220>██████╗ </>\r\n\
   <c196> ██╔════╝</> <c196>██║</><c202>██╔════╝</><c202>██╔══██╗</><c208>╚██╗ ██╔╝</><c214>████╗ ████║</><c214>██║   ██║</><c220>██╔══██╗</>\r\n\
   <c196> █████╗  </> <c196>██║</><c202>█████╗  </><c202>██████╔╝</><c208> ╚████╔╝ </><c214>██╔████╔██║</><c214>██║   ██║</><c220>██║  ██║</>\r\n\
   <c196> ██╔══╝  </> <c196>██║</><c202>██╔══╝  </><c202>██╔══██╗</><c208>  ╚██╔╝  </><c214>██║╚██╔╝██║</><c214>██║   ██║</><c220>██║  ██║</>\r\n\
   <c196> ██║     </> <c196>██║</><c202>███████╗</><c202>██║  ██║</><c208>   ██║   </><c214>██║ ╚═╝ ██║</><c214>╚██████╔╝</><c220>██████╔╝</>\r\n\
   <c196> ╚═╝     </> <c196>╚═╝</><c202>╚══════╝</><c202>╚═╝  ╚═╝</><c208>   ╚═╝   </><c214>╚═╝     ╚═╝</><c214> ╚═════╝ </><c220>╚═════╝ </>\r\n\
\r\n\
   <c238>━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━</>\r\n\
   <c220>             A classic fantasy MUD, forged in fire.</>\r\n\
   <c244>                         www.fierymud.org</>\r\n\
   <c238>━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━</>\r\n\
\r\n\
   <c244>Login with your account email or character name.</>\r\n\
\r\n\
";
/// Pure-ASCII variant of [`BANNER_FALLBACK`] for clients that aren't
/// known to render UTF-8. Used when the `LoginMessage` table has no
/// `WELCOME_BANNER` row at all; with rows present an `ascii` variant
/// row is preferred, else the default row is transliterated on the way
/// out by the connection writer.
const BANNER_FALLBACK_ASCII: &str = concat!(
    "\n",
    "<c196>   _____  ___  _____  ____  __   __ __  __  _   _  ____  </>\n",
    "<c202>  |  ___||_ _|| ____||  _ \\ \\ \\ / /|  \\/  || | | ||  _ \\ </>\n",
    "<c208>  | |_    | | |  _|  | |_) | \\ V / | |\\/| || | | || | | |</>\n",
    "<c214>  |  _|   | | | |___ |  _ <   | |  | |  | || |_| || |_| |</>\n",
    "<c220>  |_|    |___||_____||_| \\_\\  |_|  |_|  |_| \\___/ |____/ </>\n",
    "\n",
    "<c220>             A classic fantasy MUD, forged in fire.</>\n",
    "<c244>                         www.fierymud.org</>\n",
    "\n",
    "<c244>Login with your account email or character name.</>\n",
    "\n",
);
/// Combined identifier prompt — accepts either an email or a
/// character name. Email is detected by the presence of '@' (the
/// only thing legacy MUD usernames couldn't legally contain).
/// Compile-time fallback for the `EMAIL_PROMPT` `LoginMessage` row.
const IDENT_PROMPT_FALLBACK: &str = "Email or character name: ";
const PASSWORD_PROMPT_FALLBACK: &str = "Password: ";
const NEW_PASSWORD_PROMPT_FALLBACK: &str = "Choose a password: ";
const CONFIRM_PASSWORD_PROMPT_FALLBACK: &str = "Re-enter password to confirm: ";
/// Minimum length for a freshly-created password. Mirrors the
/// existing `bcrypt::hash` cost path — bcrypt itself doesn't
/// enforce a length, but anything shorter than this is a
/// hard pass for a new account regardless.
const MIN_NEW_PASSWORD_LEN: usize = 6;
const NEW_CHARACTER_NAME_PROMPT_FALLBACK: &str = "Character name: ";

/// Notice shown once, before the first password prompt, on plain
/// (unencrypted) telnet connections. Compile-time fallback for the
/// `PLAIN_TELNET_NOTICE` `LoginMessage` row; `{tls_port}` is replaced
/// with the configured TLS port.
const PLAIN_TELNET_NOTICE_FALLBACK: &str = "\r\n\
<c220>Security notice:</> this connection is unencrypted, so anything you type \
(including your password) can be read in transit. If your client supports TLS, \
connect to port <c220>{tls_port}</> instead. Or type <c220>code</> at the \
password prompt to log in by approving a short code on the website - no \
password is sent.\r\n\r\n";

/// Default for `server.tls_port` when the row is unset (matches the
/// listener's own default in `main.rs`).
const DEFAULT_TLS_PORT: i32 = 4443;
/// Default for `security.web_approval_timeout_secs`.
const DEFAULT_WEB_APPROVAL_TIMEOUT_SECS: i64 = 120;
/// Default for `security.website_url`.
const DEFAULT_WEBSITE_URL: &str = "https://muditor.utaboshi.com";
/// How often a pending device code is polled in the database.
const WEB_APPROVAL_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Device-code alphabet: uppercase letters and digits minus the
/// look-alikes `0 O 1 I` (32 symbols, so `random_range` is unbiased).
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;

/// Generate a fresh random device code (8 chars, no hyphen).
fn generate_login_code() -> String {
    (0..CODE_LEN)
        .map(|_| char::from(CODE_ALPHABET[rand::random_range(0..CODE_ALPHABET.len())]))
        .collect()
}

/// `ABCDEFGH` -> `ABCD-EFGH` for display and for the website URL.
fn format_login_code(code: &str) -> String {
    let (a, b) = code.split_at(code.len() / 2);
    format!("{a}-{b}")
}

/// Max device codes one IP may request per [`CODE_RATE_WINDOW`].
const CODE_RATE_MAX: usize = 5;
const CODE_RATE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// In-memory per-IP limiter for device-code generation: at most
/// [`CODE_RATE_MAX`] codes per [`CODE_RATE_WINDOW`]. Connections with
/// no known peer address share one bucket.
#[derive(Debug, Default)]
pub struct CodeRateLimiter {
    hits: HashMap<IpAddr, VecDeque<Instant>>,
}

impl CodeRateLimiter {
    /// Record a code request from `ip` at `now`. Returns `false`
    /// (and records nothing) when the IP is over its quota.
    fn try_acquire(&mut self, ip: IpAddr, now: Instant) -> bool {
        // Opportunistic sweep so one-off visitors don't accumulate.
        if self.hits.len() > 1024 {
            self.hits.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) < CODE_RATE_WINDOW)
            });
        }
        let q = self.hits.entry(ip).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= CODE_RATE_WINDOW)
        {
            q.pop_front();
        }
        if q.len() >= CODE_RATE_MAX {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// Aborts the wrapped poll task when dropped, so any path that drops
/// the `AwaitingWebApproval` stage (disconnect, cancel, resolution)
/// also stops its database poller.
pub struct PollGuard(tokio::task::AbortHandle);

impl Drop for PollGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// State of a pending website-approval (device code) login.
pub struct WebLogin {
    /// Code without the hyphen, as stored in `GameLoginCode.code`.
    code: String,
    /// `GameLoginCode.id`.
    code_id: String,
    expires_at: Instant,
    /// Website account that must approve the code. Empty for an
    /// unlinked legacy character: the website links the character to
    /// the approving account during approval, and
    /// `resolve_web_approval` re-reads the character to find out which.
    user_id: String,
    user: User,
    /// Character picked at the identifier prompt (character-name
    /// path); `None` for the email path, which lands in `CharSelect`.
    preselected: Option<Box<CharacterRow>>,
    /// Shown for an email with no account: no database row backs it and
    /// it can never be approved. See [`ConnRouter::begin_decoy_approval`].
    decoy: bool,
    _poller: PollGuard,
}

/// Look up a `LoginMessage` row by stage, render its XML-Lite
/// markup to ANSI, and return the bytes ready to write to an
/// `Outbound` channel. Stage names match the schema's
/// `LoginStage` enum labels verbatim (`"WELCOME_BANNER"`,
/// `"EMAIL_PROMPT"`, ...). Falls back to the supplied compile-time
/// default when the row is missing or the resource is absent —
/// keeps a fresh database (no `LoginMessage` rows yet) producing
/// a working login flow.
///
/// Login-stage entities don't have a `Connection` component, so
/// the normal [`crate::commands::send_to`] path doesn't apply;
/// we render here and write the bytes directly to the outbound
/// channel held in `LoginCtx`.
fn login_message_bytes(world: &World, stage: &str, fallback: &str) -> Vec<u8> {
    let raw = world
        .get_resource::<mud_world::LoginMessages>()
        .map_or(fallback, |m| m.get_or(stage, "default", fallback));
    crate::commands::render_color_tags(raw, crate::commands::ColorMode::Ansi).into_bytes()
}
/// Queue a login prompt (a frame that waits for input) followed by the
/// `IAC EOR` prompt-end marker, as the in-game prompt does. The marker
/// travels as its own frame so the prompt text still goes through the
/// writer's colour/newline encoder; the writer drops the marker for
/// clients that never negotiated EOR.
fn send_prompt(outbound: &Outbound, bytes: Vec<u8>) {
    let _ = outbound.try_send(bytes);
    let _ = outbound.try_send(mud_net::iac_eor());
}

/// [`send_prompt`] for a data-driven `LoginMessage` prompt row.
fn send_login_prompt(outbound: &Outbound, world: &World, stage: &str, fallback: &str) {
    send_prompt(outbound, login_message_bytes(world, stage, fallback));
}

/// The connect banner for a client that can (`ascii == false`) or can't
/// render UTF-8. Data-driven: a `WELCOME_BANNER` row with variant
/// `ascii` serves plain clients, the `default` row everyone else (and
/// plain clients too when no `ascii` row exists; the writer then
/// transliterates it). Compiled-in banners only back an empty table.
fn welcome_banner_bytes(world: &World, ascii: bool) -> Vec<u8> {
    const STAGE: &str = "WELCOME_BANNER";
    let msgs = world.get_resource::<mud_world::LoginMessages>();
    let ascii_row = msgs
        .filter(|_| ascii)
        .and_then(|m| m.by_key.get(&(STAGE.to_string(), "ascii".to_string())));
    let raw = ascii_row.map_or_else(
        || {
            let fallback = if ascii {
                BANNER_FALLBACK_ASCII
            } else {
                BANNER_FALLBACK
            };
            msgs.map_or(fallback, |m| m.get_or(STAGE, "default", fallback))
        },
        String::as_str,
    );
    crate::commands::render_color_tags(raw, crate::commands::ColorMode::Ansi).into_bytes()
}

/// Rendered plain-telnet security notice (`PLAIN_TELNET_NOTICE` row or
/// the compiled fallback) with `{tls_port}` substituted from
/// `server.tls_port`.
fn plain_telnet_notice_bytes(world: &World) -> Vec<u8> {
    let port =
        world
            .get_resource::<mud_world::RuntimeConfig>()
            .map_or(DEFAULT_TLS_PORT, |c| {
                match c.get_i32("server", "tls_port", 0) {
                    p if p > 0 => p,
                    _ => DEFAULT_TLS_PORT,
                }
            });
    let raw = world
        .get_resource::<mud_world::LoginMessages>()
        .map_or(PLAIN_TELNET_NOTICE_FALLBACK, |m| {
            m.get_or(
                "PLAIN_TELNET_NOTICE",
                "default",
                PLAIN_TELNET_NOTICE_FALLBACK,
            )
        })
        .replace("{tls_port}", &port.to_string());
    let mut text = crate::commands::render_color_tags(&raw, crate::commands::ColorMode::Ansi);
    // The prompt that follows is written straight after this block, so
    // a row without a trailing line break glued "Password:" onto the
    // notice (#49). Prompts are the only login text left unterminated.
    // `render_color_tags` closes an unclosed tag with a trailing SGR
    // reset, so test for the line break just before that reset and put
    // the CRLF there (otherwise "...\r\n<red>" got a second one).
    let body_len = text.trim_end_matches("\x1b[0m").len();
    if !text[..body_len].ends_with('\n') {
        text.insert_str(body_len, "\r\n");
    }
    text.into_bytes()
}

/// Shape check for the first login prompt, run before any lookup or
/// echo. Email-shaped input (contains `@`, the website device-code
/// path) must be free of control bytes and whitespace; anything else
/// must look like a legacy character name (`_parse_name` in
/// `fierymud_legacy/src/db.cpp`: ASCII letters only, at least two of
/// them). The upper bound is the creation cap rather than legacy's 16
/// so characters already created with 17-20 letters can still log in.
/// Case is not enforced (lookup is case-insensitive).
fn is_valid_login_identifier(s: &str) -> bool {
    const MAX_EMAIL_LEN: usize = 254;
    if s.contains('@') {
        return s.len() <= MAX_EMAIL_LEN && !s.chars().any(|c| c.is_control() || c.is_whitespace());
    }
    (2..=MAX_CHARACTER_NAME_LEN).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Verify a plaintext password against a stored hash, transparently
/// handling both bcrypt (new accounts + migrated legacy accounts) and
/// the original `FieryMUD` `CircleMUD` `crypt(3)` hash format (legacy
/// imported characters that haven't logged in yet).
///
/// Bcrypt hashes always start with `$2` (variants `$2a$` / `$2b$` /
/// `$2y$`); the legacy hash is a bare 10-character truncation of a
/// DES `crypt(3)` output with the salt as the first 2 chars. The
/// branch is by prefix so a malformed bcrypt-shaped hash doesn't
/// silently fall through to the legacy path.
///
/// The legacy path mirrors `interpreter.cpp:2502` in the C++ server:
/// `crypt(arg, stored)` compared against `stored` over the first
/// `MAX_PWD_LENGTH = 10` characters (the C++ side used a
/// case-insensitive `strncasecmp`; here the compare is exact because
/// `crypt` output is case-sensitive). The stored hash's first two
/// characters double as the salt — the C++ creation code seeds
/// `crypt()` with the character name, but the resulting hash embeds
/// those two name chars at its head, so the verify side doesn't need
/// the name.
///
/// On success the caller is expected to re-hash with bcrypt and
/// migrate the stored value so subsequent logins go through the
/// modern path.
///
/// This is the synchronous, CPU-bound primitive (bcrypt cost 12 is
/// ~250 ms). Never call it from the game loop — use
/// [`verify_password_blocking`], which runs it on the blocking pool.
fn verify_password_any(plaintext: &str, hash: &str) -> bool {
    if hash.starts_with("$2") {
        return bcrypt::verify(plaintext, hash).unwrap_or(false);
    }
    // A stored value shorter than MAX_PWD_LENGTH would compare only
    // the salt prefix (the first two characters of `crypt` output are
    // the salt itself), accepting any password.
    if hash.len() < LEGACY_MIN_HASH_LEN {
        return false;
    }
    let salt = &hash[..2];
    #[allow(deprecated)]
    let Ok(full) = pwhash::unix_crypt::hash_with(salt, plaintext) else {
        return false;
    };
    let cmp_len = hash.len().min(full.len());
    // crypt(3) output is case-sensitive (base64-style alphabet), so
    // the comparison is exact and constant-time.
    hash.as_bytes()[..cmp_len]
        .ct_eq(&full.as_bytes()[..cmp_len])
        .into()
}

/// `MAX_PWD_LENGTH` from the legacy C++ server: the stored legacy
/// hash is a 10-character truncation of the DES `crypt(3)` output.
const LEGACY_MIN_HASH_LEN: usize = 10;

/// Hash a new password with bcrypt at the default cost. CPU-bound;
/// use [`hash_password_blocking`] from async code.
fn hash_password(plaintext: &str) -> Result<String, String> {
    bcrypt::hash(plaintext, bcrypt::DEFAULT_COST).map_err(|e| e.to_string())
}

/// [`verify_password_any`] on the blocking pool so the (single
/// threaded) game runtime keeps servicing other work. A panicked or
/// cancelled job counts as a failed verification.
async fn verify_password_blocking(plaintext: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password_any(&plaintext, &hash))
        .await
        .unwrap_or(false)
}

/// [`hash_password`] on the blocking pool.
async fn hash_password_blocking(plaintext: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || hash_password(&plaintext))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// Wrong passwords allowed on one connection before it is dropped.
const MAX_FAILED_PASSWORDS_PER_CONN: u32 = 5;

/// Lockout window for the in-memory throttle, from the same
/// `security.login_timeout_minutes` value the account path uses.
fn lock_window(lock_minutes: i32) -> Duration {
    Duration::from_secs(u64::try_from(lock_minutes.max(0)).unwrap_or(0) * 60)
}

/// Default for `security.legacy_max_login_attempts`.
const DEFAULT_LEGACY_MAX_LOGIN_ATTEMPTS: i32 = 10;

/// Strike threshold for the in-memory per-name lockout of unlinked
/// legacy characters. Deliberately separate from (and looser than)
/// `security.max_login_attempts`: those characters are lockable by
/// anyone who knows the name, so a tight threshold would let a
/// griefer lock out thousands of them. `<= 0` disables.
fn legacy_max_attempts(cfg: &mud_world::RuntimeConfig) -> i32 {
    cfg.get_i32(
        "security",
        "legacy_max_login_attempts",
        DEFAULT_LEGACY_MAX_LOGIN_ATTEMPTS,
    )
}

/// In-memory failed-login counter for imported legacy characters that
/// have no linked `Users` row (so `record_failed_login` has nothing to
/// update). Keyed on the lower-cased character name; same
/// threshold / window semantics as the account path
/// (`security.legacy_max_login_attempts`, `security.login_timeout_minutes`).
/// Entry value is `(consecutive failures, time of last failure)`.
#[derive(Debug, Default)]
pub struct LegacyLoginThrottle {
    entries: HashMap<String, (u32, Instant)>,
}

impl LegacyLoginThrottle {
    fn key(name: &str) -> String {
        name.trim().to_ascii_lowercase()
    }

    /// Time remaining on an active lock, if any. `max <= 0` disables.
    fn locked_for(&self, key: &str, now: Instant, max: i32, window: Duration) -> Option<Duration> {
        if max <= 0 {
            return None;
        }
        let (count, last) = self.entries.get(key)?;
        if i64::from(*count) < i64::from(max) {
            return None;
        }
        window
            .checked_sub(now.saturating_duration_since(*last))
            .filter(|d| !d.is_zero())
    }

    /// Record a failed attempt; returns `(attempts, locked_now)`.
    /// A failure arriving after the window has elapsed starts a new
    /// count, so locks and partial strikes both expire.
    fn record_failure(
        &mut self,
        key: &str,
        now: Instant,
        max: i32,
        window: Duration,
    ) -> (i32, bool) {
        if self.entries.len() > 256 {
            self.entries
                .retain(|_, (_, last)| now.saturating_duration_since(*last) < window);
        }
        let entry = self.entries.entry(key.to_string()).or_insert((0, now));
        if now.saturating_duration_since(entry.1) >= window {
            entry.0 = 0;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
        let attempts = i32::try_from(entry.0).unwrap_or(i32::MAX);
        (attempts, max > 0 && attempts >= max)
    }

    fn clear(&mut self, key: &str) {
        self.entries.remove(key);
    }
}

/// Everything collected by the creation flow, minus the plaintext
/// password (which only lives long enough to be hashed).
pub struct NewCharDraft {
    email: Option<String>,
    character_name: String,
    race: String,
    class_id: i32,
    class_plain_name: String,
    gender: &'static str,
    stats: CoreStats,
}

/// Completion message for an off-thread credential job. Produced on
/// the blocking pool, consumed by the main loop via
/// [`ConnRouter::on_auth_done`].
pub struct AuthDone {
    conn_id: ConnId,
    kind: AuthDoneKind,
}

enum AuthDoneKind {
    Password {
        user: User,
        preselected: Option<Box<CharacterRow>>,
        ok: bool,
        /// bcrypt re-hash of the typed password, computed only when
        /// verification succeeded for an unlinked legacy character.
        migration_hash: Option<Result<String, String>>,
    },
    Create {
        draft: NewCharDraft,
        hashed: Result<String, String>,
    },
    /// Result of the character-name lookups started at the identifier
    /// prompt; see [`ConnRouter::finish_char_lookup`].
    CharLookup {
        identifier: String,
        result: CharLookup,
    },
    /// Result of the account lookup started by `code` at the email
    /// prompt; see [`ConnRouter::finish_email_lookup`].
    EmailLookup {
        email: String,
        result: mud_db::sqlx::Result<Option<User>>,
    },
    /// The poller saw the device-code row leave PENDING (or reach its
    /// deadline); the main loop re-reads it and resolves the login.
    WebApprovalWake { code_id: String },
    /// The wait for the character's previous session to finish saving
    /// ended (`settled == false`: it is still failing). See
    /// [`ConnRouter::complete_login_inner`].
    SaveSettled {
        user: User,
        char_row: Box<CharacterRow>,
        settled: bool,
    },
}

/// How long a relogging character waits for its previous session's
/// pending save (quit-save retry / in-flight autosave) to land before the
/// login is refused. See [`ConnRouter::complete_login_inner`].
const PREVIOUS_SAVE_WAIT: Duration = Duration::from_secs(10);

/// How long a linkdead character that is no longer fighting stays in the
/// world before it is saved and removed. Legacy `check_idling` extracts a
/// link-less player after 12 idle ticks of 75 s (`limits.cpp`).
const LINKDEAD_TIMEOUT_TICKS: u64 = 12 * 75 * crate::TICK_HZ;

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

/// Rest / repose: offline Repose fill rate and pool cap per tier,
/// both in basis points (1/100 of a percent) of the XP needed to
/// advance from the character's current level to the next, so the
/// pool scales with progression. Indexed by `restTier` (0..=3); tier 0
/// is `NONE` / `QUIT` and contributes nothing. **TUNABLE**, see
/// `docs/design/rest-and-repose.md` §"Tier table":
///
/// | tier | fill / hour | cap  | time to cap |
/// |------|-------------|------|-------------|
/// | 0    | 0           | 0    | n/a         |
/// | 1    | 2.5%        | 10%  | 4 h         |
/// | 2    | 5.0%        | 25%  | 5 h         |
/// | 3    | 10.0%       | 50%  | 5 h         |
const REPOSE_FILL_BP_PER_HOUR: [i64; 4] = [0, 250, 500, 1000];
/// See [`REPOSE_FILL_BP_PER_HOUR`].
const REPOSE_CAP_BP: [i64; 4] = [0, 1000, 2500, 5000];
const BASIS_POINTS: i128 = 10_000;

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
    /// Present iff the effect entity carried a `SpellResistanceDelta`
    /// (`PROT_FROM_FIRE` / `STONE_SKIN`, ...): `(element, percent)`. A
    /// player's `Resistances` are rebuilt from race / class / gear at
    /// login, so a restored spell has to re-apply the bump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resistance: Option<(mud_db::enums::ElementType, i32)>,
    /// `"evil"` / `"good"` for a `PROT_FROM_EVIL` / `PROT_FROM_GOOD`
    /// instance (its `AlignmentProtectionTag`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    align_protect: Option<String>,
    /// The instance carried `InvisibleSource` (`INVISIBLE` / `MASS_INVIS`,
    /// an `invisible` status flag).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    invisible_source: bool,
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

/// Persisted shape of one pet entry — proto key + the runtime
/// state we want to round-trip. Saved location intentionally absent
/// (Q9): pets always respawn next to the player, sidestepping
/// zone-not-loaded edge cases and matching the "the pet was with
/// you" mental model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedPet {
    proto_zone_id: i32,
    proto_id: i32,
    /// Custom-named pet keeps its name. Hire path renames to
    /// `"<player>'s <mob_name>"`; charm leaves the proto name. We
    /// just round-trip whatever's in `Named`.
    name: String,
    hp: i32,
    max_hp: i32,
}

/// Persisted active-pets envelope. Same 1h disconnect-cap pattern as
/// `PersistedEffects`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedPets {
    saved_at_unix: i64,
    pets: Vec<PersistedPet>,
}

/// `ScriptVars` as persisted: the player's vars plus the queued camp
/// wake kit (see [`mud_world::PENDING_WAKE_KIT_KEY`]) while one is
/// pending. Once the wake consumer drops the component, the next save
/// omits the key, so the bonus applies exactly once.
pub(crate) fn script_vars_for_save(world: &World, entity: Entity) -> Option<serde_json::Value> {
    let mut map = world
        .get::<mud_world::ScriptVars>(entity)
        .map(|sv| sv.0.clone())
        .unwrap_or_default();
    map.remove(mud_world::PENDING_WAKE_KIT_KEY);
    if let Some(pending) = world.get::<mud_world::PendingWakeAttachments>(entity) {
        map.insert(
            mud_world::PENDING_WAKE_KIT_KEY.to_string(),
            pending.to_var(),
        );
    }
    if map.is_empty() {
        return None;
    }
    serde_json::to_value(&map).ok()
}

/// Lift the persisted camp wake kit out of the loaded `ScriptVars`. The
/// key is always removed (it is not a player-visible var); the pending
/// attachment is only restored while the character is still queued on
/// a camp rest, which is the only source that consumes it.
pub(crate) fn take_pending_wake(
    map: &mut std::collections::BTreeMap<String, String>,
    rest_source: mud_db::enums::RestSource,
) -> Option<mud_world::PendingWakeAttachments> {
    let raw = map.remove(mud_world::PENDING_WAKE_KIT_KEY)?;
    if rest_source != mud_db::enums::RestSource::Camp {
        return None;
    }
    mud_world::PendingWakeAttachments::from_var(&raw)
}

/// Hydrate `ScriptVars` (and the queued camp wake kit) onto a freshly
/// loaded player entity from the persisted JSON. Shared by the telnet
/// login and the admin virtual-session loader so both strip the reserved
/// wake-kit key out of the visible vars and restore it as a component,
/// which `script_vars_for_save` then writes back on save.
pub(crate) fn insert_loaded_script_vars(
    e: &mut bevy_ecs::world::EntityWorldMut<'_>,
    json: serde_json::Value,
    rest_source: mud_db::enums::RestSource,
) {
    let Ok(mut map) = serde_json::from_value::<std::collections::BTreeMap<String, String>>(json)
    else {
        return;
    };
    if let Some(pending) = take_pending_wake(&mut map, rest_source) {
        e.insert(pending);
    }
    if !map.is_empty() {
        e.insert(mud_world::ScriptVars(map));
    }
}

/// Shared restore logic: spawn one mob entity per persisted pet
/// entry, dropping all entries past the disconnect cap (no staff-
/// exception equivalent for pets — they're player-owned investments,
/// not staff rewards). Located next to the player; HP restored
/// verbatim (a wounded pet stays wounded). Everything else comes
/// from the proto via `mud_world::spawn_mob_from_proto`.
pub(crate) fn restore_persisted_pets(world: &mut World, player: Entity, persisted: PersistedPets) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let elapsed = now_unix.saturating_sub(persisted.saved_at_unix).max(0);
    if elapsed > EFFECT_DISCONNECT_CAP_SECS {
        // All pets exceeded the cap — drop them all. Mirrors the
        // effects rule: log off for the night, come back without.
        return;
    }
    let player_room = match world.get::<Located>(player) {
        Some(l) => l.0,
        None => return,
    };
    for pet in persisted.pets {
        let proto = world
            .resource::<MobPrototypes>()
            .by_key
            .get(&(pet.proto_zone_id, pet.proto_id))
            .cloned();
        let Some(proto) = proto else {
            warn!(
                proto = ?(pet.proto_zone_id, pet.proto_id),
                "persisted pet's proto missing; skipping"
            );
            continue;
        };
        // Same constructor as hire / mount summons so the pet carries
        // every proto-derived component (default effects, shop and
        // trigger markers, natural attack, ...). No aggro or LOAD
        // trigger runs here: pets are servants, not fresh spawns.
        let pet_entity = mud_world::spawn_mob_from_proto(world, &proto, player_room, None);
        if let Ok(mut em) = world.get_entity_mut(pet_entity) {
            // Persisted state goes on top; the proto's rolled HP is
            // replaced so a wounded pet stays wounded.
            em.insert((
                // Pets saved before possessives dropped the article still
                // read "Owner's a stout mare".
                Named {
                    name: crate::commands::fix_possessive_article(&pet.name),
                },
                Health {
                    hp: pet.hp,
                    max: pet.max_hp,
                },
                Posture(PostureKind::Standing),
                Follower(player),
                mud_world::PersistentPet,
            ));
        }
    }
}

/// Install the permanent `RaceEffects` of the player's race (see
/// [`mud_world::mob_effects::apply_race_effects`]). Idempotent. Shared
/// by the telnet login and the admin virtual-session path.
pub(crate) fn apply_player_race_effects(world: &mut World, entity: Entity) {
    let Some(race) = world
        .get::<mud_world::Profile>(entity)
        .map(|p| p.race.clone())
    else {
        return;
    };
    mud_world::mob_effects::apply_race_effects(world, entity, &race);
}

/// The status flag a saved instance stands for. A spell's instance is
/// named after its `flag` (`fly`, `bless`, ...); a status row with no
/// per-ability flag keeps the effect's own name and takes the effect's
/// default flag, which is what the cast path reads too.
fn restored_flag(world: &World, kind: i32, name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if mud_world::mob_effects::install_flag_marker_known(&lower) {
        return lower;
    }
    world
        .get_resource::<mud_world::EffectCatalog>()
        .and_then(|c| c.by_id.get(&kind))
        .filter(|d| d.effect_type == "status" && d.name.eq_ignore_ascii_case(name))
        .and_then(|d| d.default_params.get("flag"))
        .and_then(serde_json::Value::as_str)
        .map_or(lower, str::to_ascii_lowercase)
}

/// Put back the live state a restored spell instance stood for: the flag
/// marker through [`mud_world::mob_effects::install_flag_marker`] (the
/// path casting, wearing and mob / race effects use), plus the per-arm
/// state the cast path adds on top: the `Empowered` charge and the
/// `MaxAbsorbCircle` of a globe (max-wins, like casting over a worn globe).
/// Stat deltas are NOT handled here: [`restore_persisted_effects`] already
/// decides per target whether the saved row bakes them in.
fn reinstall_effect_state(
    world: &mut World,
    entity: Entity,
    effect: Entity,
    flag: &str,
    name: &str,
    strength: i32,
) {
    if mud_world::mob_effects::install_flag_marker(world, entity, flag) {
        mud_world::mob_effects::tag_flag_instance(world, effect, flag);
    }
    if flag == "empowered" {
        world.entity_mut(entity).insert(mud_world::Empowered);
    }
    if name.eq_ignore_ascii_case("globe") && strength > 0 {
        let existing = world
            .get::<mud_world::MaxAbsorbCircle>(entity)
            .map_or(0, |m| m.0);
        world
            .entity_mut(entity)
            .insert(mud_world::MaxAbsorbCircle(existing.max(strength)));
    }
}

/// Re-apply a saved resistance spell's bump to the player's `Resistances`
/// (rebuilt from race / class / gear at login, so it is not in there yet)
/// and tag the instance so its expiry reverses exactly this delta.
fn restore_spell_resistance(
    world: &mut World,
    entity: Entity,
    effect: Entity,
    element: mud_db::enums::ElementType,
    percent: i32,
) {
    if percent == 0 {
        return;
    }
    if let Some(mut r) = world.get_mut::<mud_world::Resistances>(entity) {
        let slot = r.0.entry(element).or_insert(0);
        *slot = slot.saturating_add(percent);
    } else {
        let mut map = std::collections::HashMap::new();
        map.insert(element, percent);
        world.entity_mut(entity).insert(mud_world::Resistances(map));
    }
    world
        .entity_mut(effect)
        .insert(mud_world::SpellResistanceDelta { element, percent });
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
    // Current hp / stamina were saved as they stood; re-applying a max
    // buff below must not top them up.
    let hp = world.get::<Health>(entity).map(|h| h.hp);
    let stamina = world.get::<Stamina>(entity).map(|s| s.current);
    for eff in persisted.effects {
        if mud_world::mob_effects::is_innate_effect(&eff.source) {
            continue;
        }
        let is_admin = matches!(eff.source, mud_world::EffectSource::Admin);
        // A stat buff's delta on a core stat is baked into the saved row.
        // Every other target (accuracy, max hp, ...) is rebuilt from base
        // columns at login, so a restored buff has to re-apply it for its
        // later expiry to subtract a delta that is actually there.
        let baked = |d: &(String, i32)| crate::equip_apply::is_persisted_stat_key(&d.0);
        if !is_admin && elapsed > EFFECT_DISCONNECT_CAP_SECS {
            // Dropped unrestored: give back a baked-in delta or it is
            // permanent.
            if let Some(d) = eff.modify_delta.as_ref().filter(|d| baked(d)) {
                crate::commands::reverse_modify_delta(world, entity, &d.0, d.1);
            }
            continue;
        }
        let restored_secs = if eff.remaining_secs < 0 {
            -1
        } else {
            let after = i64::from(eff.remaining_secs).saturating_sub(elapsed);
            if after <= 0 {
                if let Some(d) = eff.modify_delta.as_ref().filter(|d| baked(d)) {
                    crate::commands::reverse_modify_delta(world, entity, &d.0, d.1);
                }
                continue;
            }
            i32::try_from(after).unwrap_or(eff.remaining_secs)
        };
        // Record the delta that actually landed: a clamped stat (max hp
        // floored at 1, armor capped at 100) takes less than the saved
        // amount, and expiry must give back exactly that.
        let mut modify_delta = eff.modify_delta.clone();
        if let Some(d) = modify_delta.as_mut().filter(|d| !baked(d))
            && let Some(landed) =
                crate::commands::apply_modify_delta_actual(world, entity, &d.0, d.1)
        {
            d.1 = landed;
        }
        let flag = restored_flag(world, eff.kind, &eff.name);
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
        if let Some((target, amount)) = modify_delta {
            effect_entity.insert(mud_world::ModifyDelta { target, amount });
        }
        let effect = effect_entity.id();
        reinstall_effect_state(world, entity, effect, &flag, &eff.name, eff.strength);
        if let Some((element, percent)) = eff.resistance {
            restore_spell_resistance(world, entity, effect, element, percent);
        }
        let aligned = match eff.align_protect.as_deref() {
            Some("evil") => Some((mud_world::AlignmentProtectionTag::Evil, "protect_evil")),
            Some("good") => Some((mud_world::AlignmentProtectionTag::Good, "protect_good")),
            _ => None,
        };
        if let Some((tag, marker_flag)) = aligned {
            world.entity_mut(effect).insert(tag);
            mud_world::mob_effects::install_flag_marker(world, entity, marker_flag);
        }
        if eff.invisible_source {
            mud_world::mob_effects::install_flag_marker(world, entity, "invisible");
            world.entity_mut(effect).insert(mud_world::InvisibleSource);
        }
    }
    // Paralysis / stun markers follow their backing instances.
    crate::effects::sync_stunned(world, entity);
    if let Some(hp) = hp
        && let Some(mut h) = world.get_mut::<Health>(entity)
    {
        h.hp = hp.min(h.max);
    }
    if let Some(cur) = stamina
        && let Some(mut s) = world.get_mut::<Stamina>(entity)
    {
        s.current = cur.min(s.max);
    }
}

pub enum Stage {
    /// Initial prompt accepts either an email (contains `@`) or a
    /// character name. Email path leads to `CharSelect` like before;
    /// character-name path skips the menu and lands directly in the
    /// world after the password check.
    AwaitingIdentifier,
    /// GAME-password prompt for the character chosen at the
    /// identifier prompt. `game_hash` is that character's
    /// `Characters.password_hash` (bcrypt or legacy `crypt(3)`); the
    /// website `Users.password_hash` is never loaded. Typing `code`
    /// switches to [`Stage::AwaitingWebApproval`].
    AwaitingPassword {
        user: User,
        /// Character chosen at the identifier prompt (character-name
        /// path). When `Some`, the menu is skipped on auth success.
        preselected: Option<Box<CharacterRow>>,
        game_hash: String,
    },
    /// Device-code login: the player must approve the displayed code
    /// on the website. A background poller wakes the main loop when
    /// the row leaves PENDING; Enter re-checks immediately and
    /// `cancel` abandons the code.
    AwaitingWebApproval(Box<WebLogin>),
    /// Identifier didn't match anything in the database. Ask the
    /// user whether they want to create a new account / character
    /// rather than silently bouncing them through a doomed
    /// password check.
    ConfirmCreate { identifier: String, is_email: bool },
    /// Confirm-create answered "yes". Collect a password for the
    /// new account. The character-name path collapses both
    /// account + character creation behind one identifier — the
    /// password covers the user-row.
    AwaitingNewPassword { identifier: String, is_email: bool },
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
    /// playable `Races` rows (`Races.playable`); input matches case-insensitively. The
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
        race: String,
    },
    /// Pick a gender for the new character. Prompt lists the
    /// schema's `Characters.gender` accepted values (`male`,
    /// `female`, `neutral`). Stored verbatim — gendered Lua
    /// triggers read this string directly via `actor.gender`.
    AwaitingGender {
        email: Option<String>,
        character_name: String,
        password_plaintext: String,
        race: String,
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
        race: String,
        class_id: i32,
        class_plain_name: String,
        gender: &'static str,
        stats: CoreStats,
    },
    CharSelect {
        user: User,
        characters: Vec<CharacterRow>,
    },
    /// A password verification / hash job is running on the blocking
    /// pool. Input is ignored until `on_auth_done` resolves it.
    Authenticating,
}

/// Gender values accepted by the `Characters.gender` column. The
/// schema column is plain text but the runtime + triggers only
/// handle these three casings; new options need a code-side
/// review before adding here.
const PLAYABLE_GENDERS: &[&str] = &["male", "female", "neutral"];

pub struct LoginCtx {
    pub outbound: Outbound,
    pub stage: Stage,
    /// Wrong passwords entered on this connection so far.
    pub failed_attempts: u32,
    /// Remote address from `Inbound::Connected` (device-code rows
    /// record it; the rate limiter keys on its IP).
    pub peer: Option<SocketAddr>,
    /// Arrived over the TLS listener.
    pub tls: bool,
    /// Plain-telnet security notice already shown on this connection.
    pub notice_shown: bool,
}

/// TLS connection ids carry bit 40 (see `mud_net::serve_tls`).
fn conn_is_tls(conn_id: ConnId) -> bool {
    conn_id & (1u64 << 40) != 0
}

pub struct ConnRouter {
    login: HashMap<ConnId, LoginCtx>,
    playing: HashMap<ConnId, Entity>,
    /// Per-connection negotiated state — window size, terminal
    /// type, MTTS bitmap, and capability flags. Populated as
    /// telnet events arrive from `mud_net`. Persists across the
    /// login → playing transition so commands can consult the
    /// player's actual viewport / capabilities once spawned.
    caps: HashMap<ConnId, ConnCapabilities>,
    /// Failed-login counter for legacy characters with no `Users` row.
    legacy_throttle: LegacyLoginThrottle,
    /// Per-IP quota for device-code generation.
    code_limiter: CodeRateLimiter,
    /// Completion channel for off-thread password jobs. The sender is
    /// cloned into each job; the receiver is handed to the main loop
    /// via [`ConnRouter::take_auth_rx`].
    auth_tx: UnboundedSender<AuthDone>,
    auth_rx: Option<UnboundedReceiver<AuthDone>>,
    /// Socket-close hook; `mud_net::close_connection` in production.
    /// A field (not a direct call) so tests can observe which
    /// connections the router asked to close.
    close_conn: fn(ConnId) -> bool,
    /// Max wait for a relogging character's pending save; a field so
    /// tests can shorten it.
    save_wait: Duration,
    /// Test seam: name of a per-character table whose load should fail
    /// with an injected error. Always `None` in production.
    load_fault: Option<&'static str>,
}

/// Per-connection capability snapshot. Updated by the telnet
/// negotiation handlers (`on_window_size`, `on_terminal`,
/// `on_capability`); read by display code that wants to adapt
/// to the client (table widths, color depth gating, EOR-after-
/// prompt). All fields default to "we don't know yet" — a fresh
/// connection that hasn't replied to NAWS shows `cols == 0`,
/// which `layout::wrap_width` treats as "fall back to 80".
#[derive(Debug, Default, Clone)]
#[allow(clippy::struct_excessive_bools)] // independent capability flags
pub struct ConnCapabilities {
    pub cols: u16,
    pub rows: u16,
    /// First TTYPE response — the client's product name, e.g.
    /// `"Mudlet"`, `"BlightMud"`, `"MUSHCLIENT"`. Empty until
    /// the first poll lands.
    pub client_name: String,
    /// Second TTYPE response — TERM-style identifier, e.g.
    /// `"XTERM-256COLOR"`. Helpful for distinguishing terminal
    /// emulators connecting via raw `telnet`.
    pub term_name: String,
    /// MTTS capability bitmap from the third TTYPE poll. See
    /// [`mud_net::parse_mtts`] for the bit assignments. Bit 8
    /// indicates truecolor support; bit 3 indicates xterm-256.
    pub mtts: u32,
    pub gmcp: bool,
    pub eor: bool,
    pub mxp: bool,
    pub utf8: bool,
    /// `Core.Hello` / `Core.Supports.Set` payloads are logged once per
    /// connection (a client can repeat them without limit).
    hello_logged: bool,
    supports_logged: bool,
    /// Shared output capabilities (colour depth, charset) for this
    /// connection. The same handle the writer encodes with.
    pub output: mud_net::OutputHandle,
}

/// What the identifier prompt's character-name lookups found.
pub(crate) enum CharLookup {
    Found {
        character: Box<CharacterRow>,
        user: Option<User>,
        game_hash: String,
    },
    Unknown,
    Failed,
}

/// Look up a character by name together with its owning account and
/// GAME password hash. Runs off the world loop (see the identifier
/// prompt).
async fn lookup_character(pool: &PgPool, name: &str) -> CharLookup {
    let character = match characters::find_by_name(pool, name).await {
        Ok(Some(c)) => c,
        Ok(None) => return CharLookup::Unknown,
        Err(e) => {
            warn!(error = %e, "character lookup failed");
            return CharLookup::Failed;
        }
    };
    let user = match character.user_id.as_deref() {
        Some(uid) => match users::find_by_id(pool, uid).await {
            Ok(u) => u,
            Err(e) => {
                warn!(error = %e, "user lookup failed");
                None
            }
        },
        None => None,
    };
    // The GAME password is always the character's own
    // `Characters.password_hash` (bcrypt or legacy crypt(3));
    // `Users.password_hash` (the website password) is never loaded.
    let game_hash = match characters::load_password_hash(pool, &character.id).await {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "character password lookup failed");
            return CharLookup::Failed;
        }
    };
    CharLookup::Found {
        character: Box::new(character),
        user,
        game_hash,
    }
}

/// Shown before an email's login code, for registered and unregistered
/// addresses alike.
const WEBSITE_PASSWORD_NOTICE: &str = "The game does not accept your website password. \
Approve a login code on the website instead.\r\n";

/// Device-code timeout (seconds) and website base URL from live config.
fn web_approval_settings(world: &World) -> (u64, String) {
    let cfg = world.resource::<mud_world::RuntimeConfig>();
    let secs = cfg
        .get_i64(
            "security",
            "web_approval_timeout_secs",
            DEFAULT_WEB_APPROVAL_TIMEOUT_SECS,
        )
        .clamp(10, 3600);
    let url = cfg
        .get_string("security", "website_url", DEFAULT_WEBSITE_URL)
        .trim_end_matches('/')
        .to_string();
    (u64::try_from(secs).unwrap_or(120), url)
}

/// "2 minutes" / "1 minute" / "90 seconds".
fn describe_timeout(timeout_secs: u64) -> String {
    if timeout_secs.is_multiple_of(60) {
        let m = timeout_secs / 60;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else {
        format!("{timeout_secs} seconds")
    }
}

/// Longest GMCP payload (bytes) written to the log.
const GMCP_LOG_MAX: usize = 256;

/// `s` cut to at most `max` bytes on a char boundary. Client-controlled
/// text must never reach the log unbounded.
fn truncate_for_log(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Lock notice for an account whose `locked_until` is in the future.
fn locked_hint(user: &User, now: chrono::NaiveDateTime) -> Option<String> {
    let until = user.locked_until.filter(|t| *t > now)?;
    Some(format!(
        "This account is locked until {} UTC after too many failed passwords. \
         You can still log in by typing 'code' and approving it on the \
         website, or clear the lock there.\r\n",
        until.format("%Y-%m-%d %H:%M")
    ))
}

/// Send the identifier prompt and park the connection there.
fn reprompt_identifier(ctx: &mut LoginCtx, world: &World) {
    ctx.stage = Stage::AwaitingIdentifier;
    send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
}

/// A socket drop leaves the character in the world only when it is
/// mid-fight and alive; quitting players, ghosts, and everyone out of
/// combat are saved and despawned as before.
fn goes_linkdead(world: &mut World, entity: Entity) -> bool {
    commands::in_combat(world, entity)
        && world.get::<Health>(entity).is_some_and(|h| h.hp > 0)
        && world.get::<Ghost>(entity).is_none()
        && world.get::<commands::Quitting>(entity).is_none()
}

/// Detach a character from its (already gone) connection but keep it in the
/// world: combat, autosave, and everything else carry on without output.
fn go_linkdead(world: &mut World, entity: Entity) {
    // A queued alias chain must not keep running for a player who is gone.
    commands::input_queue::clear(world, entity);
    let since_tick = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
    if let Ok(mut e) = world.get_entity_mut(entity) {
        e.remove::<Connection>();
        e.insert(commands::Linkdead { since_tick });
    }
    if let Some(room) = world.get::<Located>(entity).map(|l| l.0) {
        let name = commands::name_of(world, entity);
        commands::broadcast_room_visual(
            world,
            room,
            entity,
            &[entity],
            &commands::cap_sentence_start(&format!("{name} has lost their link.\r\n")),
        );
    }
    info!(entity = ?entity, "connection lost mid-fight; character stays in the world");
}

/// Every `PersistentPet` (paid pet, charmed mob, saved mount) following
/// `owner`.
fn persistent_pets(world: &mut World, owner: Entity) -> Vec<Entity> {
    let mut q =
        world.query_filtered::<(Entity, &Follower), (With<Mob>, With<mud_world::PersistentPet>)>();
    q.iter(world)
        .filter(|(_, f)| f.0 == owner)
        .map(|(e, _)| e)
        .collect()
}

/// Move whatever `owner`'s persistent pets carry or wear into the owner's
/// pack. The save records a pet as prototype, name and hp only, so gear
/// handed to a pet (`give sword wolf`, an `order` pickup) would vanish with
/// the pet when its owner leaves; in the pack it is saved with the owner.
/// Must run BEFORE the owner's final save snapshot.
fn hand_pet_gear_to_owner(world: &mut World, owner: Entity) {
    for pet in persistent_pets(world, owner) {
        let items: Vec<Entity> = world
            .get::<mud_world::Contents>(pet)
            .map(|c| {
                c.iter()
                    .filter(|e| world.get::<Item>(*e).is_some())
                    .collect()
            })
            .unwrap_or_default();
        for item in items {
            // Worn gear stops affecting the pet, then rides in the pack
            // (nested items stay inside their container).
            crate::equip_apply::release_gear(world, item);
            commands::try_remove::<EquippedSlot>(world, item);
            if let Ok(mut e) = world.get_entity_mut(item) {
                e.insert(Located(owner));
            }
        }
    }
}

/// Remove every `PersistentPet` (paid pet, charmed mob, saved mount)
/// following `owner` from the world. Called when the owner leaves, after the
/// save that records them. Their gear was already handed to the owner
/// ([`hand_pet_gear_to_owner`]); anything left is destroyed with the pet.
fn despawn_persistent_pets(world: &mut World, owner: Entity) {
    for pet in persistent_pets(world, owner) {
        commands::extract_mob(world, pet, None, true);
    }
}

/// Marks a player whose session has ended but who could not be retired yet
/// because their save turn was busy (see [`retire_player`]). The entity stays
/// until [`ConnRouter::drain_retiring`] gets the turn; it has no connection.
#[derive(Component)]
pub(crate) struct Retiring;

/// Take a character out of the world: tell the room, snapshot the save,
/// then despawn it and everything it carries. The one exit shared by quit,
/// camp, a dropped link out of combat, idle kicks, and linkdead timeouts.
///
/// Never waits on the database. The character's write turn is taken only if
/// it is free right now ([`SaveCoordinator::try_begin_ordered`]); the
/// snapshot is then taken here, on the world thread, with the turn held (so
/// it sees every earlier write's item-id stamps and queued inbox updates,
/// exactly as a foreground save would), and only the write itself runs in a
/// spawned task ([`SaveCoordinator::spawn_final_save`]). The task counts as
/// an unfinished write before this returns, so a relog waits on the barrier.
///
/// Returns `false`, having changed nothing, when the turn is busy (a
/// background autosave, a chest or house write, a retry attempt is ahead):
/// the caller leaves the entity in place and tries again next tick.
#[must_use]
fn retire_player(world: &mut World, entity: Entity, pool: &PgPool) -> bool {
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    // An entity with no `Account` can't be saved; it is still removed.
    let character_id = world.get::<Account>(entity).map(|a| a.character_id.clone());
    let ordered = match &character_id {
        Some(cid) => match coordinator.try_begin_ordered(cid) {
            Some(ordered) => Some(ordered),
            None => return false,
        },
        None => None,
    };
    // Broadcast a Room.RemovePlayer diff so other clients in
    // the room update their "who's here" panel. Done before
    // save/despawn so the entity's Located is still valid.
    // Pair it with a text leave-broadcast so plain-telnet
    // clients without GMCP support also see the departure —
    // without this, players in the room had no signal an
    // ally just logged out / disconnected.
    if let Some(room) = world.get::<Located>(entity).map(|l| l.0) {
        commands::broadcast_room_player_diff(world, room, entity, "RemovePlayer");
        let player_name = commands::name_of(world, entity);
        // A deliberate `quit` gets the legacy departure line; a
        // dropped link / kick keeps the "fades from view" one.
        let departure = if world.get::<commands::Camped>(entity).is_some() {
            format!("{player_name} rolls up their bedroll and tunes out the world.\r\n")
        } else if world.get::<commands::Quitting>(entity).is_some() {
            format!("{player_name} has left the game.\r\n")
        } else {
            format!("{player_name} fades from view, retiring to dreams.\r\n")
        };
        commands::broadcast_room_visual(
            world,
            room,
            entity,
            &[entity],
            &commands::cap_sentence_start(&departure),
        );
    }
    // Gear on the owner's pets rides in the owner's pack: a pet is saved
    // without its items, so this must precede the save snapshot.
    hand_pet_gear_to_owner(world, entity);
    // Disconnect path: the player is gone before any partial-save report
    // could reach them. A failed write is retried by the coordinator from
    // the snapshot it owns, so the state isn't lost with the entity.
    if let Some(ordered) = ordered {
        // Same prelude as `save_player_inner`, for the same reasons: fold in
        // the write that just released the turn, then the DB-first updates
        // (quest gold, a failed house placement's item) not yet applied.
        coordinator.apply_completions(world);
        if world.contains_resource::<commands::PlayerUpdateInbox>() {
            commands::drain_player_updates(world);
        }
        let generation = ordered.next_generation();
        if let Some(mut snap) = snapshot_player(world, entity, generation) {
            snap.last_logout = Some(chrono::Utc::now().naive_utc());
            let pool = pool.clone();
            coordinator.spawn_final_save(ordered, snap, move |snap| {
                let pool = pool.clone();
                async move {
                    write_snapshot(&pool, &snap)
                        .await
                        .map_err(|e| e.to_string())
                }
            });
        }
    }
    // Legacy `extract_char` -> `ungroup`: a leaving leader hands the group to
    // the next member, a leaving member just drops out. Quit, camp, idle
    // kicks and linkdead timeouts all land here.
    commands::ungroup(world, entity, true, false);
    // A leaving rider leaves the mount behind, free of its rider link.
    crate::combat::clear_mount_links(world, entity);
    // The save above recorded the persistent pets and mounts; login respawns
    // them from that record, so the live ones must go or each relog leaves
    // a duplicate standing in the room.
    despawn_persistent_pets(world, entity);
    // Despawn the player AND every item they were carrying / wearing
    // (Located(player) catches both inventory and equipped —
    // EquippedSlot is additive), including items nested inside
    // carried containers. Walk the `Contents` index so this costs
    // O(carried), not a scan of every item in the world.
    let mut items: Vec<Entity> = Vec::new();
    let mut frontier: Vec<Entity> = vec![entity];
    while let Some(parent) = frontier.pop() {
        let Some(contents) = world.get::<mud_world::Contents>(parent) else {
            continue;
        };
        for child in contents.iter() {
            if world.get::<Item>(child).is_some() {
                items.push(child);
                frontier.push(child);
            }
        }
    }
    for item in items {
        world.despawn(item);
    }
    world.despawn(entity);
    true
}

impl ConnRouter {
    pub fn new() -> Self {
        let (auth_tx, auth_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            login: HashMap::new(),
            playing: HashMap::new(),
            caps: HashMap::new(),
            legacy_throttle: LegacyLoginThrottle::default(),
            code_limiter: CodeRateLimiter::default(),
            auth_tx,
            auth_rx: Some(auth_rx),
            close_conn: mud_net::close_connection,
            save_wait: PREVIOUS_SAVE_WAIT,
            load_fault: None,
        }
    }

    /// Hand the auth-completion receiver to the main loop (once).
    /// The loop must `select!` on it and feed results to
    /// [`ConnRouter::on_auth_done`].
    pub fn take_auth_rx(&mut self) -> Option<UnboundedReceiver<AuthDone>> {
        self.auth_rx.take()
    }

    pub fn live_connections(&self) -> usize {
        self.login.len() + self.playing.len()
    }

    /// Read the current capability snapshot for a connection.
    /// Returns `None` when the connection isn't tracked (already
    /// disconnected, or the negotiation events arrived before
    /// `Connected` — shouldn't happen in practice). Currently
    /// unused — admin-status tooling will read this.
    #[must_use]
    #[allow(dead_code)]
    pub fn caps(&self, conn_id: ConnId) -> Option<&ConnCapabilities> {
        self.caps.get(&conn_id)
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

    /// Accept a connection with the safe default output capabilities
    /// (16-colour ASCII). Production goes through
    /// [`Self::on_connect_with`] with the negotiated handle.
    #[cfg(test)]
    pub fn on_connect(
        &mut self,
        conn_id: ConnId,
        outbound: Outbound,
        peer: Option<SocketAddr>,
        world: &World,
    ) {
        self.on_connect_with(conn_id, outbound, peer, mud_net::OutputHandle::new(), world);
    }

    /// Accept a connection. `mud_net` releases `Connected` only once
    /// capability negotiation has settled (or timed out), so `output`
    /// already says whether this client can take UTF-8 and the banner
    /// is picked for it.
    pub fn on_connect_with(
        &mut self,
        conn_id: ConnId,
        outbound: Outbound,
        peer: Option<SocketAddr>,
        output: mud_net::OutputHandle,
        world: &World,
    ) {
        let ascii = output.charset() == mud_net::Charset::Ascii;
        self.caps.entry(conn_id).or_default().output = output;
        let _ = outbound.try_send(welcome_banner_bytes(world, ascii));
        send_login_prompt(&outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
        self.login.insert(
            conn_id,
            LoginCtx {
                outbound,
                stage: Stage::AwaitingIdentifier,
                failed_attempts: 0,
                peer,
                tls: conn_is_tls(conn_id),
                notice_shown: false,
            },
        );
    }

    /// Every character the autosave and shutdown saves must cover: the
    /// connected ones plus linkdead characters still fighting in the world.
    fn online_entities(&self, world: &mut World) -> Vec<Entity> {
        let mut entities: Vec<Entity> = self.playing.values().copied().collect();
        // A character waiting to be retired is still in the world and its
        // latest state is not saved yet.
        let mut q =
            world.query_filtered::<Entity, Or<(With<commands::Linkdead>, With<Retiring>)>>();
        entities.extend(q.iter(world));
        entities
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
        let entries = self.online_entities(world);
        for entity in entries {
            // SaveOutcome dropped — broadcast autosave can't surface
            // a per-player partial-save message anyway, and the
            // tracing::warn inside save_player covers staff
            // diagnostics. A failed write is retried in the background
            // (and awaited by the flush below, up to its timeout).
            hand_pet_gear_to_owner(world, entity);
            let outcome = save_player_final(world, entity, pool).await;
            retry_failed_save(world, outcome, pool);
        }
        // Background (autosave / `actor:save()`) writes may still be in
        // flight; the process must not exit until they have landed.
        let coordinator = world
            .get_resource::<SaveCoordinator>()
            .cloned()
            .unwrap_or_default();
        if !coordinator.flush(world, Duration::from_secs(30)).await {
            error!(
                character_ids = ?coordinator.unsettled_characters(),
                "shutdown: saves still pending after 30s; these characters' latest state \
                 was NOT persisted"
            );
        }
    }

    /// Periodic autosave, called from the tick. Never awaits: it snapshots
    /// at most `AUTOSAVE_PER_SCAN` due characters (see `autosave.rs` for
    /// the staggering rule) and spawns their DB writes.
    pub fn autosave_tick(&self, world: &mut World, pool: &PgPool, interval: Duration) {
        let Some(coordinator) = world.get_resource::<SaveCoordinator>().cloned() else {
            return;
        };
        coordinator.apply_completions(world);
        let online: HashMap<String, Entity> = self
            .online_entities(world)
            .into_iter()
            .filter(|&e| world.get::<Retiring>(e).is_none())
            .filter_map(|e| world.get::<Account>(e).map(|a| (a.character_id.clone(), e)))
            .collect();
        let ids: Vec<String> = online.keys().cloned().collect();
        for cid in coordinator.autosave_due(&ids, interval, crate::autosave::AUTOSAVE_PER_SCAN) {
            if let Some(&entity) = online.get(&cid) {
                spawn_background_save(world, entity, pool);
            }
        }
    }

    /// Never awaits the database: the save is snapshotted here and written by
    /// a spawned task (see [`retire_player`]).
    pub fn on_disconnect(&mut self, world: &mut World, conn_id: ConnId, pool: &PgPool) {
        // A device code whose connection vanished must not stay approvable.
        if let Some(LoginCtx {
            stage: Stage::AwaitingWebApproval(web),
            ..
        }) = self.login.remove(&conn_id)
        {
            let pool = pool.clone();
            tokio::spawn(async move {
                if let Err(e) = mud_db::game_login_code::expire_pending(&pool, &web.code_id).await {
                    warn!(conn_id, error = %e, "login code expire on disconnect failed");
                }
            });
        }
        self.caps.remove(&conn_id);
        if let Some(entity) = self.playing.remove(&conn_id) {
            // A dropped link mid-fight leaves the character in the world to
            // finish it (see `Linkdead`); every other exit saves and despawns.
            if goes_linkdead(world, entity) {
                go_linkdead(world, entity);
                return;
            }
            // Send Core.Goodbye before any teardown so the client
            // can show a clean disconnect message instead of a
            // raw "connection lost". Plain telnet clients ignore
            // the IAC bytes, so this costs nothing on the
            // unsupported path.
            commands::send_core_goodbye(world, entity, "See you next time!");
            if !retire_player(world, entity, pool) {
                // The save turn is busy; `drain_retiring` finishes the job
                // on a later tick instead of this one waiting for it.
                world.entity_mut(entity).insert(Retiring);
            }
        }
    }

    /// Retire every character left by a busy save turn
    /// ([`Retiring`]). Each tick, never awaits: one still busy is simply
    /// tried again next tick.
    pub fn drain_retiring(world: &mut World, pool: &PgPool) {
        let pending: Vec<Entity> = {
            let mut q = world.query_filtered::<Entity, With<Retiring>>();
            q.iter(world).collect()
        };
        for entity in pending {
            let _ = retire_player(world, entity, pool);
        }
    }

    /// Log out every player flagged [`commands::Quitting`] outside a typed
    /// command: a completed `camp` is the only such source today (`quit` and
    /// `rent` are drained by [`Self::on_line`] right after the command).
    pub fn drain_quitting(&mut self, world: &mut World, pool: &PgPool) {
        let pending: Vec<Entity> = {
            let mut q =
                world.query_filtered::<Entity, (With<commands::Quitting>, Without<Retiring>)>();
            q.iter(world).collect()
        };
        for entity in pending {
            if let Some(conn_id) = self.find_conn(entity) {
                self.on_disconnect(world, conn_id, pool);
                (self.close_conn)(conn_id);
            } else if let Ok(mut e) = world.get_entity_mut(entity) {
                // No connection to close (it already dropped): the marker
                // is stale, so clear it rather than retry forever.
                e.remove::<commands::Quitting>();
            }
        }
    }

    /// Save and despawn every linkdead character whose fight is over for
    /// good: dead (a ghost waits for `release`, which needs a player at the
    /// keyboard), or out of combat for [`LINKDEAD_TIMEOUT_TICKS`]. While a
    /// fight lasts the timer is held at zero. Called every tick from the
    /// main loop; costs one query over linkdead characters only.
    pub fn drain_linkdead(world: &mut World, pool: &PgPool) {
        let now = world.get_resource::<crate::TickCount>().map_or(0, |t| t.0);
        let linkdead: Vec<(Entity, u64)> = {
            let mut q = world.query::<(Entity, &commands::Linkdead)>();
            q.iter(world).map(|(e, l)| (e, l.since_tick)).collect()
        };
        for (entity, since) in linkdead {
            if world.get::<Ghost>(entity).is_none() {
                if commands::in_combat(world, entity) {
                    if let Some(mut l) = world.get_mut::<commands::Linkdead>(entity) {
                        l.since_tick = now;
                    }
                    continue;
                }
                if now.saturating_sub(since) < LINKDEAD_TIMEOUT_TICKS {
                    continue;
                }
            }
            // A busy save turn leaves the character linkdead; the next
            // tick tries again.
            if retire_player(world, entity, pool) {
                info!(entity = ?entity, "linkdead character removed from the world");
            }
        }
    }

    /// NAWS payload — the client reported its terminal viewport.
    /// Re-fires on every resize, so the snapshot tracks the live
    /// state. The width is mirrored onto the player as `ClientWidth`
    /// and drives server-side word wrap via `layout::wrap_width`.
    pub fn on_window_size(&mut self, conn_id: ConnId, cols: u16, rows: u16, world: &mut World) {
        let entry = self.caps.entry(conn_id).or_default();
        entry.cols = cols;
        entry.rows = rows;
        if let Some(&entity) = self.playing.get(&conn_id) {
            self.sync_client_width(conn_id, entity, world);
        }
    }

    /// Bind `conn_id` to the player `entity`: a fresh login spawn or a
    /// takeover / linkdead reconnect. The entity's change-gated GMCP cache
    /// ([`commands::clear_gmcp_sent`]) describes what the *previous* connection
    /// saw, so it is reset here; whatever Core.Hello / Core.Supports.Set the
    /// client sent on the login screen (before it was `playing`, so they
    /// could not clear it) is covered too, and the next prompt re-sends
    /// every package.
    fn bind_player(&mut self, conn_id: ConnId, entity: Entity, world: &mut World) {
        self.playing.insert(conn_id, entity);
        // Tell the network layer now, not on the next once-a-second
        // `sync_authenticated` sweep: a login script's first commands
        // arrive in that gap and must not meet the pre-login rate limit.
        mud_net::mark_authenticated(conn_id);
        commands::clear_gmcp_sent(world, entity);
        self.sync_client_width(conn_id, entity, world);
        self.attach_output(conn_id, entity, world);
    }

    /// Mirror the connection's NAWS width onto the player entity as
    /// [`mud_world::ClientWidth`] so command code (which only sees the
    /// ECS world) can word-wrap to the viewport. No-op until the
    /// client has reported a non-zero width.
    fn sync_client_width(&self, conn_id: ConnId, entity: Entity, world: &mut World) {
        let cols = self.caps.get(&conn_id).map_or(0, |c| c.cols);
        if cols == 0 {
            return;
        }
        if let Ok(mut e) = world.get_entity_mut(entity) {
            e.insert(mud_world::ClientWidth(cols));
        }
    }

    /// Hand the player entity its connection's output capabilities and
    /// apply the player's saved `color` / `charset` settings to them.
    fn attach_output(&self, conn_id: ConnId, entity: Entity, world: &mut World) {
        if let Some(c) = self.caps.get(&conn_id)
            && let Ok(mut e) = world.get_entity_mut(entity)
        {
            e.insert(crate::terminal::ClientOutput(c.output.clone()));
        }
        crate::terminal::sync_output(world, entity);
    }

    /// TTYPE / MTTS response. The MTTS cycle yields three
    /// responses to consecutive `IAC SB TTYPE SEND`s — index 1 is
    /// the client product name, index 2 is the terminal type, and
    /// index 3 is the MTTS bitmap. We persist all three so display
    /// code can both gate features (truecolor via the bitmap) and
    /// log the client identity.
    pub fn on_terminal(&mut self, conn_id: ConnId, index: u8, value: &str, _world: &mut World) {
        let entry = self.caps.entry(conn_id).or_default();
        match index {
            1 => entry.client_name = value.to_string(),
            2 => entry.term_name = value.to_string(),
            3 => {
                if let Some(bits) = mud_net::parse_mtts(value) {
                    entry.mtts = bits;
                }
            }
            _ => {
                // Mudlet's cycle stops at 3; further polls would
                // re-emit the bitmap. Ignore — we already have it.
            }
        }
    }

    /// Capability flag from the telnet negotiation layer. Names
    /// are stable strings emitted by `mud_net` (`"gmcp"`, `"eor"`,
    /// `"mxp"`, `"utf8"`). New names land here as we wire more
    /// options; unknown names log at debug level.
    ///
    /// On `gmcp` going true (i.e. client confirmed `IAC DO 201`),
    /// we push the connect-time GMCP intro burst — `Client.GUI`
    /// (Mudlet auto-install URL), `Client.Map` (mapper data URL),
    /// and `External.Discord.Info` (Discord application ID + invite
    /// link). Sending these in response to the capability flip
    /// matches the IRE / Mudlet idiom and gives Mudlet enough to
    /// either prompt the player to install our package or refresh
    /// an existing install.
    pub fn on_capability(&mut self, conn_id: ConnId, name: &str, on: bool, world: &mut World) {
        let entry = self.caps.entry(conn_id).or_default();
        let was_gmcp = entry.gmcp;
        match name {
            "gmcp" => entry.gmcp = on,
            "eor" => entry.eor = on,
            "mxp" => entry.mxp = on,
            "utf8" => entry.utf8 = on,
            _ => {
                tracing::debug!(conn_id, name, on, "unhandled capability flag");
                return;
            }
        }
        if name == "gmcp" && on && !was_gmcp {
            self.send_gmcp_intro_burst(conn_id, world);
        }
    }

    /// Connect-time GMCP introduction. Pushed exactly once, the
    /// first time the client confirms GMCP (`IAC DO 201`). Each
    /// frame is built by a separate helper so paths that only need
    /// a subset (e.g. an `External.Discord.Hello` handshake from
    /// the client) can re-emit just that frame without retriggering
    /// Mudlet's "install package?" prompt with a duplicate
    /// `Client.GUI`.
    ///
    /// All URLs / version strings / Discord identifiers come from
    /// the `GameConfig` table (category `gmcp`). Compile-time
    /// fallbacks below match the values the legacy MUD shipped
    /// with — adjusted for the Rust package name — so a fresh DB
    /// still produces a working intro burst. Operators override
    /// per-deployment via Muditor or psql without rebuilding.
    fn send_gmcp_intro_burst(&self, conn_id: ConnId, world: &World) {
        self.send_client_gui(conn_id, world);
        self.send_client_map(conn_id, world);
        self.send_external_discord_info(conn_id, world);
    }

    /// Push the `Client.GUI` install-prompt frame. Each frame is
    /// gated on its primary DB value being set — an empty URL means
    /// "operator didn't configure this for the current deployment"
    /// and we skip emission rather than offer an empty install URL.
    /// Mudlet bumps its install state on `version`, so this frame
    /// MUST NOT be re-emitted on every reconnect / Discord
    /// handshake — Mudlet treats a re-emit as a new install offer
    /// and prompts the user again.
    fn send_client_gui(&self, conn_id: ConnId, world: &World) {
        let Some(outbound) = self.outbound_for(conn_id) else {
            return;
        };
        let Some(cfg) = world.get_resource::<mud_world::RuntimeConfig>() else {
            return;
        };
        let gui_url = cfg.get_string("gmcp", "client_gui_url", "");
        let gui_version = cfg.get_string("gmcp", "client_gui_version", "");
        if !gui_url.is_empty() {
            let payload = format!(
                r#"{{"version":"{}","url":"{}"}}"#,
                json_escape(gui_version),
                json_escape(gui_url),
            );
            let _ = outbound.try_send(mud_net::gmcp_packet("Client.GUI", &payload));
        }
    }

    /// Push the `Client.Map` map-data URL frame. Empty URL =
    /// no hosted map, skip emission entirely.
    fn send_client_map(&self, conn_id: ConnId, world: &World) {
        let Some(outbound) = self.outbound_for(conn_id) else {
            return;
        };
        let Some(cfg) = world.get_resource::<mud_world::RuntimeConfig>() else {
            return;
        };
        let map_url = cfg.get_string("gmcp", "client_map_url", "");
        if !map_url.is_empty() {
            let payload = format!(r#"{{"url":"{}"}}"#, json_escape(map_url));
            let _ = outbound.try_send(mud_net::gmcp_packet("Client.Map", &payload));
        }
    }

    /// Push the `External.Discord.Info` rich-presence pairing
    /// frame. Safe to re-emit: Mudlet's Discord SDK uses the
    /// `application_id` to identify the right app config and treats
    /// repeats as idempotent. This is the frame the client asks for
    /// when it sends `External.Discord.Hello` (and `Get`) to start
    /// rich presence.
    fn send_external_discord_info(&self, conn_id: ConnId, world: &World) {
        let Some(outbound) = self.outbound_for(conn_id) else {
            return;
        };
        let Some(cfg) = world.get_resource::<mud_world::RuntimeConfig>() else {
            return;
        };
        let app_id = cfg.get_string("gmcp", "discord_application_id", "");
        let invite_url = cfg.get_string("gmcp", "discord_invite_url", "");
        if !app_id.is_empty() {
            let payload = format!(
                r#"{{"application_id":"{}","invite_url":"{}"}}"#,
                json_escape(app_id),
                json_escape(invite_url),
            );
            let _ = outbound.try_send(mud_net::gmcp_packet("External.Discord.Info", &payload));
        }
    }

    /// Resolve the outbound channel for a connection regardless of
    /// login stage. Returns the `LoginCtx` outbound while still
    /// pre-spawn; switches to the entity's `Connection` component
    /// once the player has spawned. Used by GMCP push paths that
    /// need to reach the wire from non-command code.
    fn outbound_for(&self, conn_id: ConnId) -> Option<Outbound> {
        if let Some(ctx) = self.login.get(&conn_id) {
            return Some(ctx.outbound.clone());
        }
        // Spawned-player path needs World access; not available
        // here. Callers running with World can do the lookup
        // themselves via `playing[conn_id]` → Connection. Today
        // the GMCP intro burst only fires pre-spawn (the client
        // confirms GMCP in the first round-trip, well before the
        // player picks a character) so this branch is rarely hit.
        let _ = conn_id;
        None
    }
}

/// Escape a string for safe embedding in a JSON literal — the
/// shape every GMCP payload builder in this crate needs. Replaces
/// backslash and double-quote; leaves bytes outside that pair
/// alone since the rest of our config strings are URL/identifier-
/// shaped (no control bytes, no Unicode-escape territory).
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// Continuation of `impl ConnRouter` — split because `json_escape`
// is a free function and Rust doesn't allow free fns inside an
// impl. The two blocks compose at compile time.
impl ConnRouter {
    /// GMCP package received from the client. Dispatches based on
    /// the package name; unknown packages are logged at debug
    /// level — useful when wiring support for a new client's
    /// vendor packages.
    ///
    /// Reaching this method means the parser successfully decoded
    /// a GMCP frame, so we mark the capability as on (defensive —
    /// the IAC DO 201 path already sets it, but a stray client
    /// that pushes GMCP without explicit negotiation still shows
    /// up here).
    #[allow(clippy::unused_async)] // async to match the other `on_*` handlers
    pub async fn on_gmcp(
        &mut self,
        conn_id: ConnId,
        package: &str,
        payload: &str,
        world: &mut World,
    ) {
        let entry = self.caps.entry(conn_id).or_default();
        entry.gmcp = true;
        let outbound = self.outbound_for(conn_id);

        match package {
            // Core.Ping — IRE keepalive. Echo the same payload
            // back so the client can compute round-trip latency.
            // Empty body is also valid; we echo whatever arrived.
            "Core.Ping" => {
                if let Some(out) = outbound {
                    let _ = out.try_send(mud_net::gmcp_packet("Core.Ping", payload));
                }
            }
            // Core.Hello — client identity announcement. Mudlet
            // sends `{"client":"Mudlet","version":"4.x"}`. Today
            // we just log; future work could record client name
            // for capability-gated rendering.
            "Core.Hello" => {
                let entry = self.caps.entry(conn_id).or_default();
                if !std::mem::replace(&mut entry.hello_logged, true) {
                    tracing::info!(
                        conn_id,
                        payload = truncate_for_log(payload, GMCP_LOG_MAX),
                        "GMCP Core.Hello"
                    );
                }
                // A (re)negotiating client has lost whatever we sent
                // before; forget it so the next prompt re-sends every
                // change-gated package.
                if let Some(&entity) = self.playing.get(&conn_id) {
                    commands::clear_gmcp_sent(world, entity);
                }
            }
            // Core.Supports.Set — client tells us which packages
            // it understands. Today we don't filter our outgoing
            // pushes by this, but the log helps debugging which
            // bindings a connecting client expects.
            "Core.Supports.Set" => {
                let entry = self.caps.entry(conn_id).or_default();
                if !std::mem::replace(&mut entry.supports_logged, true) {
                    tracing::info!(
                        conn_id,
                        payload = truncate_for_log(payload, GMCP_LOG_MAX),
                        "GMCP Core.Supports.Set"
                    );
                }
                // A (re)negotiating client has lost whatever we sent
                // before; forget it so the next prompt re-sends every
                // change-gated package.
                if let Some(&entity) = self.playing.get(&conn_id) {
                    commands::clear_gmcp_sent(world, entity);
                }
            }
            // External.Discord.Hello — client signals Discord
            // integration is ready. Re-emit ONLY External.Discord.Info
            // (not the whole intro burst) — the connect-time
            // Client.GUI already triggered Mudlet's install prompt;
            // re-emitting it here would cause Mudlet to offer a
            // second install download for the same package.
            "External.Discord.Hello" => {
                self.send_external_discord_info(conn_id, world);
            }
            // External.Discord.Get — client asks for an immediate
            // Status refresh. The next prompt cadence will emit
            // Status anyway; for now we just log and rely on that.
            "External.Discord.Get" => {
                tracing::debug!(
                    conn_id,
                    "GMCP External.Discord.Get (deferred to next prompt)"
                );
            }
            // Char.Skills.Get — client asks for the player's
            // skill list. Handled when the connection is past
            // login (entity exists) — for pre-login connections,
            // skills don't exist yet so we ignore.
            "Char.Skills.Get" => {
                if let Some(&entity) = self.playing.get(&conn_id) {
                    commands::send_char_skills_list(world, entity);
                }
            }
            // Char.Items.Inv / Char.Items.Worn — client asks for
            // a fresh items snapshot. Each GET handler re-emits
            // the matching `Char.Items.List` frame so a client
            // that came up after login (e.g., subsystem setup
            // happened too late to catch the initial push) can
            // ask explicitly. Both handlers also re-push the
            // *other* location since `refresh_player_items_gmcp`
            // is cheap and keeps the client's two panels in sync.
            "Char.Items.Inv" | "Char.Items.Worn" => {
                if let Some(&entity) = self.playing.get(&conn_id) {
                    commands::refresh_player_items_gmcp(world, entity);
                }
            }
            // Room.Mob.Get — click-to-detail on a mob in the
            // current room. Payload `{"id":"<entity_bits>"}`; the
            // handler enforces same-room scope and silently no-ops
            // on mismatch.
            "Room.Mob.Get" => {
                if let Some(&entity) = self.playing.get(&conn_id) {
                    commands::handle_room_mob_get(world, entity, payload);
                }
            }
            other => {
                tracing::debug!(conn_id, package = other, payload, "GMCP unhandled");
            }
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
            commands::note_player_input(world, entity);
            commands::dispatch_with_async(world, entity, pool, &text).await;
            // `quit` flags the player; save, despawn and close the socket.
            if world.get::<commands::Quitting>(entity).is_some() {
                self.on_disconnect(world, conn_id, pool);
                (self.close_conn)(conn_id);
            }
            // dispatch marks the player for prompt at its start; flush
            // sends one prompt each to the player and to everyone else
            // who received output during the turn.
            commands::flush_prompts(world);
        }
    }

    /// Release one waiting input line per playing character whose cast lock
    /// has lifted (`commands::input_queue`). Driven once per tick, so a
    /// queued alias such as `cast 'armor';cast 'bless'` runs its second
    /// spell the tick after the first lands.
    pub async fn drain_input_queues(&mut self, world: &mut World, pool: &PgPool) {
        let waiting: Vec<(ConnId, Entity)> = self
            .playing
            .iter()
            .filter(|(_, e)| commands::has_queued_input(world, **e))
            .map(|(c, e)| (*c, *e))
            .collect();
        for (conn_id, entity) in waiting {
            if !commands::run_queued_input(world, entity, pool).await {
                continue;
            }
            if world.get::<commands::Quitting>(entity).is_some() {
                self.on_disconnect(world, conn_id, pool);
                (self.close_conn)(conn_id);
            }
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
                if !is_valid_login_identifier(trimmed) {
                    // Never echo the rejected input: control bytes in it
                    // (ESC sequences) would act on the player's terminal.
                    let _ = ctx
                        .outbound
                        .try_send(b"Invalid name, please try another.\r\n".to_vec());
                    send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                    return;
                }
                if trimmed.contains('@') {
                    // Email path: no lookup here, and the reply is the same
                    // whether or not the address has an account, so the
                    // prompt cannot be used to probe which emails are
                    // registered. The choice made at the next prompt
                    // (`code` vs creating an account) does the lookup.
                    ctx.stage = Stage::ConfirmCreate {
                        identifier: trimmed.to_string(),
                        is_email: true,
                    };
                    send_confirm_create_prompt(&ctx.outbound, trimmed, true);
                    return;
                }
                // Character-name path. The lookups hit the database, so
                // they run in a task and come back through `AuthDone`
                // (`finish_char_lookup`); an unauthenticated peer must not
                // be able to stall the world loop on Postgres round-trips.
                ctx.stage = Stage::Authenticating;
                let tx = self.auth_tx.clone();
                let pool = pool.clone();
                let identifier = trimmed.to_string();
                tokio::spawn(async move {
                    let result = lookup_character(&pool, &identifier).await;
                    let _ = tx.send(AuthDone {
                        conn_id,
                        kind: AuthDoneKind::CharLookup { identifier, result },
                    });
                });
            }

            Stage::ConfirmCreate {
                identifier,
                is_email,
            } => {
                let answer = trimmed.to_ascii_lowercase();
                if is_email && matches!(answer.as_str(), "code" | "c") {
                    self.email_login_code(conn_id, identifier, pool, world);
                    return;
                }
                let yes = matches!(answer.as_str(), "y" | "yes" | "new");
                let no = matches!(answer.as_str(), "n" | "no" | "");
                if yes {
                    let registration_open = world.resource::<mud_world::RuntimeConfig>().get_bool(
                        "security",
                        "enable_new_player_creation",
                        true,
                    );
                    if !registration_open {
                        let _ = ctx.outbound.try_send(
                            "New character creation is currently closed.\r\n"
                                .as_bytes()
                                .to_vec(),
                        );
                        reprompt_identifier(ctx, world);
                        return;
                    }
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Great — let's set up '{identifier}'. Pick a password \
                             at least {MIN_NEW_PASSWORD_LEN} characters long.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingNewPassword {
                        identifier,
                        is_email,
                    };
                    send_login_prompt(
                        &ctx.outbound,
                        world,
                        "CREATE_PASSWORD",
                        NEW_PASSWORD_PROMPT_FALLBACK,
                    );
                } else if no {
                    let _ = ctx.outbound.try_send(
                        "Okay — please enter an existing email or character name.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    ctx.stage = Stage::AwaitingIdentifier;
                    send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                } else {
                    let _ = ctx.outbound.try_send(
                        if is_email {
                            "Please answer 'new', 'code' or 'no'.\r\n"
                        } else {
                            "Please answer 'yes' or 'no'."
                        }
                        .as_bytes()
                        .to_vec(),
                    );
                    ctx.stage = Stage::ConfirmCreate {
                        identifier,
                        is_email,
                    };
                }
            }

            Stage::AwaitingNewPassword {
                identifier,
                is_email,
            } => {
                if trimmed.len() < MIN_NEW_PASSWORD_LEN {
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Password must be at least {MIN_NEW_PASSWORD_LEN} \
                             characters. Try again.\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingNewPassword {
                        identifier,
                        is_email,
                    };
                    send_login_prompt(
                        &ctx.outbound,
                        world,
                        "CREATE_PASSWORD",
                        NEW_PASSWORD_PROMPT_FALLBACK,
                    );
                    return;
                }
                ctx.stage = Stage::ConfirmNewPassword {
                    identifier,
                    is_email,
                    first_attempt: trimmed.to_string(),
                };
                send_login_prompt(
                    &ctx.outbound,
                    world,
                    "CONFIRM_PASSWORD",
                    CONFIRM_PASSWORD_PROMPT_FALLBACK,
                );
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
                    ctx.stage = Stage::AwaitingNewPassword {
                        identifier,
                        is_email,
                    };
                    send_login_prompt(
                        &ctx.outbound,
                        world,
                        "CREATE_PASSWORD",
                        NEW_PASSWORD_PROMPT_FALLBACK,
                    );
                    return;
                }
                // Password confirmed. Email path needs to collect a
                // character name next; character-name path already
                // has the name (= the identifier they typed).
                if is_email {
                    let _ = ctx.outbound.try_send(
                        format!(
                            "Password set for '{identifier}'. Now choose your \
                             character's name ({MIN_CHARACTER_NAME_LEN}–{MAX_CHARACTER_NAME_LEN} \
                             letters).\r\n"
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingCharacterName {
                        email: identifier,
                        password_plaintext: first_attempt,
                    };
                    send_login_prompt(
                        &ctx.outbound,
                        world,
                        "CREATE_NAME_PROMPT",
                        NEW_CHARACTER_NAME_PROMPT_FALLBACK,
                    );
                } else {
                    // Character-name path: identifier IS the
                    // character name. Advance to race selection.
                    ctx.stage = Stage::AwaitingRace {
                        email: None,
                        character_name: identifier,
                        password_plaintext: first_attempt,
                    };
                    send_race_prompt(&ctx.outbound, world);
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
                    send_login_prompt(
                        &ctx.outbound,
                        world,
                        "CREATE_NAME_PROMPT",
                        NEW_CHARACTER_NAME_PROMPT_FALLBACK,
                    );
                    return;
                }
                match characters::find_by_name(pool, name).await {
                    Ok(Some(_)) => {
                        let _ = ctx.outbound.try_send(
                            format!(
                                "Sorry, the name '{name}' is already taken. \
                                 Pick another.\r\n"
                            )
                            .into_bytes(),
                        );
                        ctx.stage = Stage::AwaitingCharacterName {
                            email,
                            password_plaintext,
                        };
                        send_login_prompt(
                            &ctx.outbound,
                            world,
                            "CREATE_NAME_PROMPT",
                            NEW_CHARACTER_NAME_PROMPT_FALLBACK,
                        );
                    }
                    Ok(None) => {
                        // Name is available. Advance to race
                        // selection.
                        ctx.stage = Stage::AwaitingRace {
                            email: Some(email),
                            character_name: name.to_string(),
                            password_plaintext,
                        };
                        send_race_prompt(&ctx.outbound, world);
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
                        send_login_prompt(
                            &ctx.outbound,
                            world,
                            "CREATE_NAME_PROMPT",
                            NEW_CHARACTER_NAME_PROMPT_FALLBACK,
                        );
                    }
                }
            }

            Stage::AwaitingRace {
                email,
                character_name,
                password_plaintext,
            } => {
                let Some(race) = match_playable_race(world, trimmed) else {
                    let _ = ctx.outbound.try_send(
                        format!("'{trimmed}' isn't one of the available races.\r\n").into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingRace {
                        email,
                        character_name,
                        password_plaintext,
                    };
                    send_race_prompt(&ctx.outbound, world);
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
                        format!("'{trimmed}' isn't one of the available classes.\r\n").into_bytes(),
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
                            "'{trimmed}' isn't a recognized gender — pick one of the listed values.\r\n"
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
                let stats = roll_starting_stats(world.resource::<mud_world::RaceCatalog>(), &race);
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
                    let new_stats =
                        roll_starting_stats(world.resource::<mud_world::RaceCatalog>(), &race);
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
                        "Please answer 'accept' or 'reroll'.\r\n"
                            .as_bytes()
                            .to_vec(),
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
                let draft = NewCharDraft {
                    email,
                    character_name,
                    race,
                    class_id,
                    class_plain_name,
                    gender,
                    stats,
                };
                // bcrypt runs on the blocking pool; `finish_creation`
                // resumes from `on_auth_done`.
                ctx.stage = Stage::Authenticating;
                self.start_hash_job(conn_id, password_plaintext, draft);
            }

            Stage::AwaitingPassword {
                user,
                preselected,
                game_hash,
            } => {
                // `code` instead of a password: log in by approving a
                // short code on the website (no password is sent).
                if trimmed.eq_ignore_ascii_case("code") {
                    // Unlinked legacy characters (empty `user.id`) are
                    // allowed too: the website links the character to
                    // the approving account.
                    self.begin_web_approval(conn_id, user, preselected, false, pool, world)
                        .await;
                    return;
                }
                // Account lockout (website users): password attempts are
                // rejected without verifying and without touching the
                // failure counters; only `code` gets through.
                if let Some(msg) = locked_hint(&user, chrono::Utc::now().naive_utc()) {
                    info!(conn_id, email = %user.email, "password refused: account locked");
                    let _ = ctx.outbound.try_send(msg.into_bytes());
                    ctx.stage = Stage::AwaitingPassword {
                        user,
                        preselected,
                        game_hash,
                    };
                    send_login_prompt(
                        &ctx.outbound,
                        world,
                        "PASSWORD_PROMPT",
                        PASSWORD_PROMPT_FALLBACK,
                    );
                    return;
                }
                // Unlinked legacy characters have no `Users` row to carry
                // `locked_until`; apply the same lockout from the
                // in-memory per-name throttle.
                if user.id.is_empty() {
                    let cfg = world.resource::<mud_world::RuntimeConfig>();
                    let max_attempts = legacy_max_attempts(cfg);
                    let lock_minutes = cfg.get_i32("security", "login_timeout_minutes", 15);
                    let key = LegacyLoginThrottle::key(
                        preselected
                            .as_deref()
                            .map_or(user.email.as_str(), |c| c.name.as_str()),
                    );
                    if let Some(remaining) = self.legacy_throttle.locked_for(
                        &key,
                        Instant::now(),
                        max_attempts,
                        lock_window(lock_minutes),
                    ) {
                        let secs_remaining = remaining.as_secs().max(1);
                        info!(conn_id, character = %key, secs_remaining, "auth refused: legacy character locked");
                        let _ = ctx.outbound.try_send(
                            format!(
                                "Account is temporarily locked after too many failed \
                                 attempts. Try again in {secs_remaining}s.\r\n"
                            )
                            .into_bytes(),
                        );
                        ctx.stage = Stage::AwaitingIdentifier;
                        send_login_prompt(
                            &ctx.outbound,
                            world,
                            "EMAIL_PROMPT",
                            IDENT_PROMPT_FALLBACK,
                        );
                        return;
                    }
                }
                // Verification (bcrypt / legacy crypt) is CPU-heavy; it
                // runs on the blocking pool and the result comes back
                // through `AuthDone`, so the game loop keeps ticking.
                let password = trimmed.to_string();
                self.start_password_check(conn_id, user, preselected, game_hash, password);
            }

            Stage::AwaitingWebApproval(web) => {
                if trimmed.eq_ignore_ascii_case("cancel") {
                    if let Err(e) =
                        mud_db::game_login_code::expire_pending(pool, &web.code_id).await
                    {
                        warn!(conn_id, error = %e, "login code cancel failed");
                    }
                    let _ = ctx
                        .outbound
                        .try_send("Login code cancelled.\r\n".as_bytes().to_vec());
                    reprompt_identifier(ctx, world);
                    return;
                }
                // Anything else (usually just Enter): check right now.
                self.resolve_web_approval(conn_id, *web, pool, world).await;
            }

            Stage::Authenticating => {
                // A job is in flight; swallow input until it resolves.
                ctx.stage = Stage::Authenticating;
            }

            Stage::CharSelect { user, characters } => {
                let pick = trimmed.parse::<usize>().ok();
                let Some(char_row) = pick
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| characters.get(i))
                    .cloned()
                else {
                    send_prompt(
                        &ctx.outbound,
                        format!("Pick 1-{}.\r\n", characters.len()).into_bytes(),
                    );
                    ctx.stage = Stage::CharSelect { user, characters };
                    return;
                };
                // Name-approval gate runs at spawn time, not here —
                // see `complete_login` (NameApprovalPending marker
                // attach + welcome notice).
                self.complete_login(conn_id, world, pool, user, char_row)
                    .await;
            }
        }
    }

    /// Continuation of the creation flow once the off-thread bcrypt
    /// hash of the new password has completed. Persists the `Users`
    /// + `Characters` rows and hands off to `complete_login`.
    #[allow(clippy::too_many_lines)]
    async fn finish_creation(
        &mut self,
        conn_id: ConnId,
        draft: NewCharDraft,
        hashed_result: Result<String, String>,
        pool: &PgPool,
        world: &mut World,
    ) {
        let NewCharDraft {
            email,
            character_name,
            race,
            class_id,
            class_plain_name,
            gender,
            stats,
        } = draft;
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            // Connection dropped while the hash was in flight.
            return;
        };
        let effective_email = email
            .clone()
            .unwrap_or_else(|| format!("{character_name}@local.fierymud-rs"));
        let display_name = effective_email
            .split('@')
            .next()
            .unwrap_or(&effective_email)
            .to_string();
        let hashed = match hashed_result {
            Ok(h) => h,
            Err(e) => {
                warn!(conn_id, error = %e, "bcrypt hash failed");
                let _ = ctx.outbound.try_send(
                    "Server error securing your password. Please try again.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                ctx.stage = Stage::AwaitingIdentifier;
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                return;
            }
        };
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
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                return;
            }
        };
        // `Users.password_hash` (website password) stays NULL; the game
        // password is stored on the character below.
        let user_id = match users::create(&mut *tx, &effective_email, &display_name).await {
            Ok(id) => id,
            Err(e) => {
                warn!(conn_id, error = %e, "user create failed");
                let _ = ctx.outbound.try_send(
                    // Deliberately generic: the database error would say
                    // whether the email / name is already registered.
                    "Couldn't create the account. If you already have one, \
                     type 'code' at the email prompt to log in; otherwise \
                     try a different identifier.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                // Drop the tx — uncommitted, so the user
                // INSERT (if any) gets rolled back.
                drop(tx);
                ctx.stage = Stage::AwaitingIdentifier;
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                return;
            }
        };
        // Name-approval toggle: when `social.name_approval_required`
        // is ON in the live GameConfig, fresh characters land
        // unapproved and get the `NameApprovalPending` marker at
        // spawn — they can play but every social channel is
        // silenced until staff runs `approve_name`. When the
        // toggle is OFF (the default), new characters are
        // auto-approved on creation. Existing characters carry
        // their column value regardless of the toggle's state.
        let name_approval_required = world.resource::<mud_world::RuntimeConfig>().get_bool(
            "social",
            "name_approval_required",
            false,
        );
        let new_character = mud_db::characters::NewCharacter {
            user_id: &user_id,
            name: &character_name,
            race: &race,
            gender,
            class_id,
            strength: stats.strength,
            intelligence: stats.intelligence,
            wisdom: stats.wisdom,
            dexterity: stats.dexterity,
            constitution: stats.constitution,
            charisma: stats.charisma,
            name_approved: !name_approval_required,
            password_hash: &hashed,
        };
        let character_id = match mud_db::characters::create(&mut *tx, &new_character).await {
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
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
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
            send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
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
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
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
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
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
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
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
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                return;
            }
        };
        // Internal IDs (character_id, user_id) intentionally
        // omitted from the player-facing welcome — they're
        // diagnostic and live in `clientinfo` for staff.
        let _ = character_id;
        let _ = user_id;
        let _ = ctx.outbound.try_send(
            format!(
                "Welcome to FieryMUD, {character_name}! Your {gender} {race} \
                 {class_plain_name} is ready. Stepping into the world…\r\n"
            )
            .into_bytes(),
        );
        self.complete_login(conn_id, world, pool, new_user, new_char)
            .await;
    }

    /// Continuation of the password stage once the off-thread
    /// verification (and, for legacy characters, the bcrypt re-hash)
    /// has completed. Everything past the credential check lives
    /// here: throttle bookkeeping, legacy migration, wizlock / ban
    /// gates, then character select or `complete_login`.
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    async fn finish_password(
        &mut self,
        conn_id: ConnId,
        user: User,
        preselected: Option<Box<CharacterRow>>,
        ok: bool,
        migration_hash: Option<Result<String, String>>,
        pool: &PgPool,
        world: &mut World,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            // Connection dropped while verification was in flight.
            return;
        };
        if !ok {
            // Throttle: bump the failed-login counter and lock the
            // account once it crosses `security.max_login_attempts`.
            // Live config; a 0 or missing row disables the throttle
            // (legacy permissive behavior). Unlinked legacy characters
            // have no `Users` row to update, so their strikes are
            // tracked in memory, keyed on the character name.
            let cfg = world.resource::<mud_world::RuntimeConfig>();
            let account_max_attempts = cfg.get_i32("security", "max_login_attempts", 0);
            let legacy_max = legacy_max_attempts(cfg);
            let lock_minutes = cfg.get_i32("security", "login_timeout_minutes", 15);
            let max_attempts = if user.id.is_empty() {
                legacy_max
            } else {
                account_max_attempts
            };
            let (attempts_after, lock_now) = if user.id.is_empty() {
                let key = LegacyLoginThrottle::key(
                    preselected
                        .as_deref()
                        .map_or(user.email.as_str(), |c| c.name.as_str()),
                );
                self.legacy_throttle.record_failure(
                    &key,
                    Instant::now(),
                    max_attempts,
                    lock_window(lock_minutes),
                )
            } else {
                // The count (and lock decision) comes from the row itself,
                // atomically, not from the snapshot read at name entry.
                match mud_db::users::record_failed_login(pool, &user.id, max_attempts, lock_minutes)
                    .await
                {
                    Ok(f) => (f.attempts, f.locked),
                    Err(e) => {
                        warn!(conn_id, error = %e, "record_failed_login failed");
                        (user.failed_login_attempts.saturating_add(1), false)
                    }
                }
            };
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
            ctx.failed_attempts = ctx.failed_attempts.saturating_add(1);
            if ctx.failed_attempts >= MAX_FAILED_PASSWORDS_PER_CONN {
                // Per-connection cap: stop serving this connection so
                // an attacker can't grind passwords on one socket.
                // (Throttling above covers reconnects.)
                let _ = ctx.outbound.try_send(
                    "Too many failed login attempts. Goodbye.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                info!(
                    conn_id,
                    "auth: per-connection failure cap reached; dropping connection"
                );
                self.login.remove(&conn_id);
                self.caps.remove(&conn_id);
                (self.close_conn)(conn_id);
                return;
            }
            ctx.stage = Stage::AwaitingIdentifier;
            send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
            return;
        }
        if user.id.is_empty()
            && let Some(c) = preselected.as_deref()
        {
            self.legacy_throttle
                .clear(&LegacyLoginThrottle::key(&c.name));
        }
        // The game password verified against the character's own hash. If
        // it was still a legacy crypt(3) value, upgrade it to bcrypt now
        // that the plaintext is known. Unlinked legacy characters (no
        // `user_id`) stay unlinked: no placeholder website account is
        // created, so the website's `linkCharacter` can still claim them;
        // account-only features tell the player to link on the website.
        if let Some(c) = preselected.as_deref() {
            match &migration_hash {
                Some(Ok(new_hash)) => {
                    match characters::set_password_hash(pool, &c.id, new_hash).await {
                        Ok(()) => {
                            info!(conn_id, character = %c.name, "game password upgraded to bcrypt");
                        }
                        Err(e) => warn!(conn_id, error = %e, "game password bcrypt upgrade failed"),
                    }
                }
                Some(Err(e)) => warn!(conn_id, error = %e, "game password bcrypt hash failed"),
                None => {}
            }
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
        // A preselected character (name login) is known here, so its level
        // counts toward staff rank (unlinked god characters have no account
        // role). The email path has no character yet and falls back to the
        // account role alone, i.e. it can only be stricter, never looser.
        let wizlock_rank = preselected.as_ref().map_or(user.role, |c| {
            mud_db::enums::effective_rank(c.level, user.role)
        });
        if wizlock_active && !wizlock_rank.at_least(mud_db::enums::UserRole::Builder) {
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
            send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
            return;
        }
        // Ban check. Refuses post-auth so we don't leak
        // whether an email exists pre-password. The conn
        // stays in AwaitingIdentifier (mirrors auth-failure
        // path); player can't proceed past the ban message.
        let active_ban = match mud_db::bans::active_for(pool, &user.id).await {
            Ok(b) => b,
            Err(e) => {
                // Fail closed: never let someone in because the ban
                // lookup failed.
                warn!(conn_id, user_id = %user.id, error = %e, "ban check failed");
                let _ = ctx.outbound.try_send(
                    "Server error, please try again later.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
                return;
            }
        };
        if let Some(ban) = active_ban {
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
                format!("Your account is banned: {}{until}\r\n", ban.reason).into_bytes(),
            );
            ctx.stage = Stage::AwaitingIdentifier;
            send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
            return;
        }
        info!(conn_id, user_id = %user.id, email = %user.email, "auth success");

        // Character-name path: preselected character → spawn it
        // directly without showing the CharSelect menu. The
        // name-approval gate runs at spawn time, not at auth
        // time — players with an unapproved character still
        // log in and can play; social commands are silenced
        // until staff resolves the name.
        if let Some(char_row) = preselected {
            self.complete_login(conn_id, world, pool, user, *char_row)
                .await;
            return;
        }

        // Email path: list all characters and show the menu.
        let chars = match characters::list_for_user(pool, &user.id).await {
            Ok(c) => c,
            Err(e) => {
                warn!(conn_id, error = %e, "character list failed");
                let _ = ctx
                    .outbound
                    .try_send("Server error.\r\n".as_bytes().to_vec());
                ctx.stage = Stage::AwaitingIdentifier;
                send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
                return;
            }
        };
        if chars.is_empty() {
            let _ = ctx
                .outbound
                .try_send("No characters on this account.\r\n".as_bytes().to_vec());
            ctx.stage = Stage::AwaitingIdentifier;
            send_login_prompt(&ctx.outbound, world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
            return;
        }
        let mut menu = String::from("\r\nCharacters:\r\n");
        for (idx, c) in chars.iter().enumerate() {
            menu.push_str(&format!(
                "  {}. {} (level {})\r\n",
                idx + 1,
                c.name,
                c.level
            ));
        }
        menu.push_str("Pick a number: ");
        send_prompt(&ctx.outbound, menu.into_bytes());
        ctx.stage = Stage::CharSelect {
            user,
            characters: chars,
        };
    }

    /// Queue a password verification on the blocking pool and park the
    /// connection in `Authenticating`. For unlinked legacy characters a
    /// successful verification also computes the bcrypt re-hash used by
    /// the migration, in the same job. Result arrives via `auth_tx`.
    fn start_password_check(
        &mut self,
        conn_id: ConnId,
        user: User,
        preselected: Option<Box<CharacterRow>>,
        game_hash: String,
        password: String,
    ) {
        if let Some(ctx) = self.login.get_mut(&conn_id) {
            ctx.stage = Stage::Authenticating;
        }
        let tx = self.auth_tx.clone();
        // Anything that isn't bcrypt is a legacy crypt(3) hash that gets
        // upgraded on a successful login.
        let needs_upgrade = !game_hash.starts_with("$2");
        tokio::spawn(async move {
            let ok = if game_hash.is_empty() {
                false
            } else {
                verify_password_blocking(password.clone(), game_hash).await
            };
            let migration_hash = if ok && needs_upgrade {
                Some(hash_password_blocking(password).await)
            } else {
                None
            };
            let _ = tx.send(AuthDone {
                conn_id,
                kind: AuthDoneKind::Password {
                    user,
                    preselected,
                    ok,
                    migration_hash,
                },
            });
        });
    }

    /// Queue the bcrypt hash for a new account on the blocking pool.
    fn start_hash_job(&self, conn_id: ConnId, password: String, draft: NewCharDraft) {
        let tx = self.auth_tx.clone();
        tokio::spawn(async move {
            let hashed = hash_password_blocking(password).await;
            let _ = tx.send(AuthDone {
                conn_id,
                kind: AuthDoneKind::Create { draft, hashed },
            });
        });
    }

    /// Resolve a finished credential job. Drops the result silently if
    /// the connection went away while the job was running.
    pub async fn on_auth_done(&mut self, done: AuthDone, pool: &PgPool, world: &mut World) {
        let AuthDone { conn_id, kind } = done;
        if !self.login.contains_key(&conn_id) {
            return;
        }
        match kind {
            AuthDoneKind::Password {
                user,
                preselected,
                ok,
                migration_hash,
            } => {
                self.finish_password(conn_id, user, preselected, ok, migration_hash, pool, world)
                    .await;
            }
            AuthDoneKind::Create { draft, hashed } => {
                self.finish_creation(conn_id, draft, hashed, pool, world)
                    .await;
            }
            AuthDoneKind::CharLookup { identifier, result } => {
                self.finish_char_lookup(conn_id, &identifier, result, world);
            }
            AuthDoneKind::EmailLookup { email, result } => {
                self.finish_email_lookup(conn_id, email, result, pool, world)
                    .await;
            }
            AuthDoneKind::SaveSettled {
                user,
                char_row,
                settled,
            } => {
                self.finish_save_wait(conn_id, user, *char_row, settled, pool, world)
                    .await;
            }
            AuthDoneKind::WebApprovalWake { code_id } => {
                let Some(ctx) = self.login.get_mut(&conn_id) else {
                    return;
                };
                // Stale wake (cancelled / replaced code): ignore.
                let current = matches!(
                    &ctx.stage,
                    Stage::AwaitingWebApproval(w) if w.code_id == code_id
                );
                if !current {
                    return;
                }
                if let Stage::AwaitingWebApproval(web) =
                    std::mem::replace(&mut ctx.stage, Stage::AwaitingIdentifier)
                {
                    self.resolve_web_approval(conn_id, *web, pool, world).await;
                }
            }
        }
    }

    /// Continuation of the character-name identifier prompt once the
    /// off-loop lookups finished: password prompt for a known
    /// character, create-offer (or closed notice) for an unknown name.
    fn finish_char_lookup(
        &mut self,
        conn_id: ConnId,
        identifier: &str,
        result: CharLookup,
        world: &World,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        if !matches!(ctx.stage, Stage::Authenticating) {
            return;
        }
        match result {
            CharLookup::Found {
                character,
                user,
                game_hash,
            } => {
                // Legacy-orphan path: imported CircleMUD characters land
                // with no `user_id`. A sentinel user (empty id) marks
                // them; after the game password verifies,
                // `finish_password` only upgrades the hash to bcrypt. No
                // `Users` row is created: the player claims the character
                // on the website.
                let user = user.unwrap_or_else(|| User {
                    id: String::new(),
                    email: identifier.to_string(),
                    display_name: String::new(),
                    role: mud_db::enums::UserRole::Player,
                    failed_login_attempts: 0,
                    locked_until: None,
                    account_wealth: 0,
                });
                ctx.stage = Stage::AwaitingPassword {
                    user,
                    preselected: Some(character),
                    game_hash,
                };
                if !ctx.tls && !ctx.notice_shown {
                    ctx.notice_shown = true;
                    let _ = ctx.outbound.try_send(plain_telnet_notice_bytes(world));
                }
                if let Stage::AwaitingPassword { user, .. } = &ctx.stage
                    && let Some(msg) = locked_hint(user, chrono::Utc::now().naive_utc())
                {
                    let _ = ctx.outbound.try_send(msg.into_bytes());
                }
                send_login_prompt(
                    &ctx.outbound,
                    world,
                    "PASSWORD_PROMPT",
                    PASSWORD_PROMPT_FALLBACK,
                );
            }
            CharLookup::Unknown => {
                // Registration gate: when
                // `security.enable_new_player_creation` is false, the
                // unknown-name path returns a closed-registration message
                // instead of the create-account flow. Default true.
                let registration_open = world.resource::<mud_world::RuntimeConfig>().get_bool(
                    "security",
                    "enable_new_player_creation",
                    true,
                );
                if !registration_open {
                    let _ = ctx.outbound.try_send(
                        "New character creation is currently closed.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    reprompt_identifier(ctx, world);
                    return;
                }
                ctx.stage = Stage::ConfirmCreate {
                    identifier: identifier.to_string(),
                    is_email: false,
                };
                send_confirm_create_prompt(&ctx.outbound, identifier, false);
            }
            CharLookup::Failed => {
                let _ = ctx
                    .outbound
                    .try_send("Server error.\r\n".as_bytes().to_vec());
                reprompt_identifier(ctx, world);
            }
        }
    }

    /// `code` chosen at the email prompt: start a website device-code
    /// login. A registered address gets a real code; an unregistered one
    /// gets a decoy that looks identical and simply never approves, so
    /// the two outcomes cannot be told apart from the game prompt.
    ///
    /// The per-IP code limit is charged first, so a peer past its quota
    /// costs no query; the account lookup then runs in a task and resumes
    /// from `on_auth_done` ([`Self::finish_email_lookup`]) rather than
    /// stalling the world loop on a Postgres round-trip.
    fn email_login_code(
        &mut self,
        conn_id: ConnId,
        email: String,
        pool: &PgPool,
        world: &mut World,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        let ip = ctx.peer.map_or(IpAddr::from([0u8, 0, 0, 0]), |a| a.ip());
        if !self.code_limiter.try_acquire(ip, Instant::now()) {
            info!(conn_id, %ip, "login code rate limit hit");
            let _ = ctx.outbound.try_send(
                "Too many login codes requested; try again later.\r\n"
                    .as_bytes()
                    .to_vec(),
            );
            reprompt_identifier(ctx, world);
            return;
        }
        ctx.stage = Stage::Authenticating;
        let tx = self.auth_tx.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            let result = users::find_by_email(&pool, &email).await;
            let _ = tx.send(AuthDone {
                conn_id,
                kind: AuthDoneKind::EmailLookup { email, result },
            });
        });
    }

    /// Continuation of [`Self::email_login_code`] once the off-loop account
    /// lookup finished. The code quota was already charged there.
    async fn finish_email_lookup(
        &mut self,
        conn_id: ConnId,
        email: String,
        result: mud_db::sqlx::Result<Option<User>>,
        pool: &PgPool,
        world: &mut World,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        match result {
            Ok(Some(user)) => {
                let _ = ctx
                    .outbound
                    .try_send(WEBSITE_PASSWORD_NOTICE.as_bytes().to_vec());
                self.begin_web_approval(conn_id, user, None, true, pool, world)
                    .await;
            }
            Ok(None) => {
                let _ = ctx
                    .outbound
                    .try_send(WEBSITE_PASSWORD_NOTICE.as_bytes().to_vec());
                self.begin_decoy_approval(conn_id, email, world);
            }
            Err(e) => {
                warn!(conn_id, error = %e, "user lookup failed");
                let _ = ctx
                    .outbound
                    .try_send("Server error.\r\n".as_bytes().to_vec());
                reprompt_identifier(ctx, world);
            }
        }
    }

    /// Show a plausible login code for an email with no account. Same
    /// text, prompts and timing as [`Self::begin_web_approval`] (the caller
    /// already charged the code rate limit), but no `GameLoginCode` row
    /// exists, so nothing can approve it.
    fn begin_decoy_approval(&mut self, conn_id: ConnId, email: String, world: &World) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        let (timeout_secs, website_url) = web_approval_settings(world);
        let code = generate_login_code();
        let shown = format_login_code(&code);
        let _ = ctx.outbound.try_send(
            format!(
                "Your login code is {shown}. Approve it at {website_url}/verify?code={shown} \
                 within {}. Press Enter to check, or type cancel.\r\n",
                describe_timeout(timeout_secs)
            )
            .into_bytes(),
        );
        let _ = ctx.outbound.try_send(mud_net::iac_eor());
        let expires_at = Instant::now() + Duration::from_secs(timeout_secs);
        let poller = {
            let tx = self.auth_tx.clone();
            let deadline = expires_at + Duration::from_secs(1);
            let handle = tokio::spawn(async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                let _ = tx.send(AuthDone {
                    conn_id,
                    kind: AuthDoneKind::WebApprovalWake {
                        code_id: String::new(),
                    },
                });
            });
            PollGuard(handle.abort_handle())
        };
        ctx.stage = Stage::AwaitingWebApproval(Box::new(WebLogin {
            code,
            code_id: String::new(),
            expires_at,
            user_id: String::new(),
            user: User {
                id: String::new(),
                email,
                display_name: String::new(),
                role: mud_db::enums::UserRole::Player,
                failed_login_attempts: 0,
                locked_until: None,
                account_wealth: 0,
            },
            preselected: None,
            decoy: true,
            _poller: poller,
        }));
    }

    /// Start a device-code login for `user` (and, on the character-name
    /// path, `preselected`): rate-limit (unless `quota_charged`: the caller
    /// already spent the IP's quota), insert the `GameLoginCode`
    /// row, spawn its poller, tell the player where to approve it.
    /// `user.id` is empty for an unlinked legacy character, in which
    /// case `preselected` is required and the row is inserted with a
    /// NULL `"userId"`.
    #[allow(clippy::too_many_lines)]
    async fn begin_web_approval(
        &mut self,
        conn_id: ConnId,
        user: User,
        preselected: Option<Box<CharacterRow>>,
        quota_charged: bool,
        pool: &PgPool,
        world: &mut World,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        if user.id.is_empty() && preselected.is_none() {
            warn!(
                conn_id,
                "web approval requested without account or character"
            );
            let _ = ctx
                .outbound
                .try_send("Server error.\r\n".as_bytes().to_vec());
            reprompt_identifier(ctx, world);
            return;
        }
        let ip = ctx.peer.map_or(IpAddr::from([0u8, 0, 0, 0]), |a| a.ip());
        if !quota_charged && !self.code_limiter.try_acquire(ip, Instant::now()) {
            info!(conn_id, %ip, "login code rate limit hit");
            let _ = ctx.outbound.try_send(
                "Too many login codes requested; try again later.\r\n"
                    .as_bytes()
                    .to_vec(),
            );
            reprompt_identifier(ctx, world);
            return;
        }
        let (timeout_secs, website_url) = web_approval_settings(world);
        let character_name = preselected.as_deref().map_or("", |c| c.name.as_str());
        let created_at = chrono::Utc::now().naive_utc();
        let expires_naive =
            created_at + chrono::Duration::seconds(i64::try_from(timeout_secs).unwrap_or(120));
        let ip_text = ctx
            .peer
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let client_port = ctx.peer.map(|a| i32::from(a.port()));
        let mut inserted: Option<(String, String)> = None;
        for _ in 0..5 {
            let code = generate_login_code();
            let new = mud_db::game_login_code::NewGameLoginCode {
                code: &code,
                character_name,
                user_id: (!user.id.is_empty()).then_some(user.id.as_str()),
                client_ip: &ip_text,
                client_port,
                tls: ctx.tls,
                created_at,
                expires_at: expires_naive,
            };
            match mud_db::game_login_code::insert(pool, &new).await {
                Ok(id) => {
                    inserted = Some((id, code));
                    break;
                }
                Err(e)
                    if e.as_database_error()
                        .is_some_and(mud_db::sqlx::error::DatabaseError::is_unique_violation) =>
                {
                    // Code collision: draw another.
                }
                Err(e) => {
                    warn!(conn_id, error = %e, "login code insert failed");
                    break;
                }
            }
        }
        let Some((code_id, code)) = inserted else {
            let _ = ctx
                .outbound
                .try_send("Server error.\r\n".as_bytes().to_vec());
            reprompt_identifier(ctx, world);
            return;
        };
        let expires_at = Instant::now() + Duration::from_secs(timeout_secs);
        // Poller: wakes the main loop once the row leaves PENDING (or
        // the deadline passes). Aborted by `PollGuard` on any stage exit.
        let poller = {
            let tx = self.auth_tx.clone();
            let pool = pool.clone();
            let id = code_id.clone();
            let deadline = expires_at + Duration::from_secs(1);
            let handle = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(WEB_APPROVAL_POLL_INTERVAL).await;
                    let changed = match mud_db::game_login_code::state(&pool, &id).await {
                        Ok(Some(s)) => s.status != "PENDING",
                        Ok(None) => true,
                        Err(_) => false,
                    };
                    if changed || Instant::now() >= deadline {
                        let _ = tx.send(AuthDone {
                            conn_id,
                            kind: AuthDoneKind::WebApprovalWake { code_id: id },
                        });
                        return;
                    }
                }
            });
            PollGuard(handle.abort_handle())
        };
        let shown = format_login_code(&code);
        let within = describe_timeout(timeout_secs);
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        let _ = ctx.outbound.try_send(
            format!(
                "Your login code is {shown}. Approve it at {website_url}/verify?code={shown} \
                 within {within}. Press Enter to check, or type cancel.\r\n"
            )
            .into_bytes(),
        );
        if user.id.is_empty() {
            let _ = ctx.outbound.try_send(
                "This character isn't linked to a website account yet; you'll be asked \
                 to link it when you approve.\r\n"
                    .as_bytes()
                    .to_vec(),
            );
        }
        let _ = ctx.outbound.try_send(mud_net::iac_eor());
        ctx.stage = Stage::AwaitingWebApproval(Box::new(WebLogin {
            code,
            code_id,
            expires_at,
            user_id: user.id.clone(),
            user,
            preselected,
            decoy: false,
            _poller: poller,
        }));
    }

    /// Re-read a pending device code and act on its status: keep
    /// waiting, fail back to the identifier prompt, or (APPROVED by the
    /// right account, atomically consumed) finish exactly like a
    /// successful password login.
    #[allow(clippy::too_many_lines)]
    async fn resolve_web_approval(
        &mut self,
        conn_id: ConnId,
        web: WebLogin,
        pool: &PgPool,
        world: &mut World,
    ) {
        if web.decoy {
            let Some(ctx) = self.login.get_mut(&conn_id) else {
                return;
            };
            let now = Instant::now();
            if now >= web.expires_at {
                let _ = ctx.outbound.try_send(
                    "Your login code expired. Please try again.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
            } else {
                let left = (web.expires_at - now).as_secs().max(1);
                send_prompt(
                    &ctx.outbound,
                    format!(
                        "Code {} is still waiting for approval ({left}s left). \
                         Press Enter to check, or type cancel.\r\n",
                        format_login_code(&web.code)
                    )
                    .into_bytes(),
                );
                ctx.stage = Stage::AwaitingWebApproval(Box::new(web));
            }
            return;
        }
        let state = mud_db::game_login_code::state(pool, &web.code_id).await;
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        let state = match state {
            Ok(Some(s)) => s,
            Ok(None) => {
                let _ = ctx.outbound.try_send(
                    "Your login code is no longer valid.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
                return;
            }
            Err(e) => {
                warn!(conn_id, error = %e, "login code check failed");
                // Best effort: close the code so it can't be approved later.
                let _ = mud_db::game_login_code::expire_pending(pool, &web.code_id).await;
                let _ = ctx.outbound.try_send(
                    "Couldn't check your login code. Please try again.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
                return;
            }
        };
        match state.status.as_str() {
            "PENDING" => {
                let now = Instant::now();
                if now >= web.expires_at {
                    let _ = mud_db::game_login_code::expire_pending(pool, &web.code_id).await;
                    let _ = ctx.outbound.try_send(
                        "Your login code expired. Please try again.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    reprompt_identifier(ctx, world);
                } else {
                    let left = (web.expires_at - now).as_secs().max(1);
                    send_prompt(
                        &ctx.outbound,
                        format!(
                            "Code {} is still waiting for approval ({left}s left). \
                             Press Enter to check, or type cancel.\r\n",
                            format_login_code(&web.code)
                        )
                        .into_bytes(),
                    );
                    ctx.stage = Stage::AwaitingWebApproval(Box::new(web));
                }
            }
            "APPROVED" if web.user_id.is_empty() => {
                self.resolve_unlinked_approval(
                    conn_id,
                    web,
                    state.approved_by_user_id,
                    pool,
                    world,
                )
                .await;
            }
            "APPROVED" => {
                if state.approved_by_user_id.as_deref() != Some(web.user_id.as_str()) {
                    warn!(
                        conn_id,
                        "login code approved by a different account; refusing"
                    );
                    let _ = ctx.outbound.try_send(
                        "That code was approved by a different account. Login refused.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    reprompt_identifier(ctx, world);
                    return;
                }
                let consumed = mud_db::game_login_code::consume(
                    pool,
                    &web.code_id,
                    &web.user_id,
                    chrono::Utc::now().naive_utc(),
                )
                .await;
                if matches!(consumed, Ok(true)) {
                    // Device-code approval lifts any password lockout.
                    if let Err(e) = mud_db::users::clear_failed_logins(pool, &web.user_id).await {
                        warn!(conn_id, error = %e, "clear_failed_logins after code login failed");
                    }
                    let _ = ctx
                        .outbound
                        .try_send("Code approved. Logging in...\r\n".as_bytes().to_vec());
                    let WebLogin {
                        user, preselected, ..
                    } = web;
                    self.finish_password(conn_id, user, preselected, true, None, pool, world)
                        .await;
                } else {
                    if let Err(e) = &consumed {
                        warn!(conn_id, error = %e, "login code consume failed");
                    }
                    let _ = ctx.outbound.try_send(
                        "That login code could not be used. Please try again.\r\n"
                            .as_bytes()
                            .to_vec(),
                    );
                    reprompt_identifier(ctx, world);
                }
            }
            "DENIED" => {
                let _ = ctx.outbound.try_send(
                    "Your login code was denied on the website.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
            }
            "EXPIRED" => {
                let _ = ctx.outbound.try_send(
                    "Your login code expired. Please try again.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
            }
            _ => {
                // CONSUMED (already used) or anything unexpected.
                let _ = ctx.outbound.try_send(
                    "That login code has already been used.\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                reprompt_identifier(ctx, world);
            }
        }
    }

    /// APPROVED code for an unlinked legacy character. The website
    /// links the character to the approving account *before* marking
    /// the code APPROVED, so re-read the character and require that its
    /// `user_id` is set and equals `approvedByUserId`. Then consume the
    /// code for that user and log in as the (now linked) character.
    async fn resolve_unlinked_approval(
        &mut self,
        conn_id: ConnId,
        web: WebLogin,
        approved_by: Option<String>,
        pool: &PgPool,
        world: &mut World,
    ) {
        let WebLogin {
            code_id,
            preselected,
            ..
        } = web;
        let linked: Option<(Box<CharacterRow>, User)> = 'check: {
            let Some(name) = preselected.as_deref().map(|c| c.name.clone()) else {
                break 'check None;
            };
            let character = match characters::find_by_name(pool, &name).await {
                Ok(Some(c)) => c,
                Ok(None) => break 'check None,
                Err(e) => {
                    warn!(conn_id, error = %e, "character re-read after approval failed");
                    break 'check None;
                }
            };
            let (Some(uid), Some(approver)) =
                (character.user_id.as_deref(), approved_by.as_deref())
            else {
                break 'check None;
            };
            if uid != approver {
                break 'check None;
            }
            let consumed = mud_db::game_login_code::consume(
                pool,
                &code_id,
                uid,
                chrono::Utc::now().naive_utc(),
            )
            .await;
            if !matches!(consumed, Ok(true)) {
                if let Err(e) = &consumed {
                    warn!(conn_id, error = %e, "login code consume failed");
                }
                break 'check None;
            }
            match users::find_by_id(pool, uid).await {
                Ok(Some(u)) => Some((Box::new(character), u)),
                Ok(None) => None,
                Err(e) => {
                    warn!(conn_id, error = %e, "user lookup after approval failed");
                    None
                }
            }
        };
        let Some((character, user)) = linked else {
            warn!(conn_id, "unlinked-character code approval did not link it");
            // An APPROVED row that failed the check is flipped to
            // EXPIRED so it can never be consumed later.
            if let Err(e) = mud_db::game_login_code::expire_unconsumed(pool, &code_id).await {
                warn!(conn_id, error = %e, "login code expire failed");
            }
            let Some(ctx) = self.login.get_mut(&conn_id) else {
                return;
            };
            let _ = ctx.outbound.try_send(
                "Approval did not link this character.\r\n"
                    .as_bytes()
                    .to_vec(),
            );
            reprompt_identifier(ctx, world);
            return;
        };
        if let Err(e) = users::clear_failed_logins(pool, &user.id).await {
            warn!(conn_id, error = %e, "clear_failed_logins after code login failed");
        }
        if let Some(ctx) = self.login.get(&conn_id) {
            let _ = ctx
                .outbound
                .try_send("Code approved. Logging in...\r\n".as_bytes().to_vec());
        }
        self.finish_password(conn_id, user, Some(character), true, None, pool, world)
            .await;
    }

    /// Classic MUD "take over" for a duplicate login. If a player
    /// entity for `character_id` is already in the world, attach the
    /// new connection to it, tell the old connection it was replaced,
    /// and detach the old connection from the router (so its eventual
    /// disconnect can't save / despawn the live entity). Returns
    /// `true` when a takeover happened — the caller must NOT spawn a
    /// second entity (that would duplicate the inventory on save).
    fn try_takeover(&mut self, world: &mut World, conn_id: ConnId, character_id: &str) -> bool {
        let existing = {
            let mut q = world.query_filtered::<(Entity, &Account), With<Player>>();
            q.iter(world)
                .find(|(_, a)| a.character_id == character_id)
                .map(|(e, _)| e)
        };
        let Some(entity) = existing else {
            return false;
        };
        let Some(LoginCtx { outbound, .. }) = self.login.remove(&conn_id) else {
            return false;
        };
        let old_conn = self.find_conn(entity);
        if let Some(old_conn) = old_conn {
            self.playing.remove(&old_conn);
            self.caps.remove(&old_conn);
            info!(
                conn_id,
                old_conn, "login takeover: detaching previous connection"
            );
        }
        if let Some(old) = world.get::<Connection>(entity) {
            let _ = old.0.try_send(
                "\r\nThis character has been taken over by another connection. \
                 Disconnecting.\r\n"
                    .as_bytes()
                    .to_vec(),
            );
        }
        // Notice is queued; now really close the old socket (the net
        // layer flushes queued output before shutting it down).
        if let Some(old_conn) = old_conn {
            (self.close_conn)(old_conn);
        }
        // Lines queued by the previous connection are not the new one's.
        commands::input_queue::clear(world, entity);
        let was_linkdead = world.get::<commands::Linkdead>(entity).is_some();
        // A character still waiting for its save turn to retire is taken back
        // into play: the quit / camp / kick that parked it is cancelled.
        world
            .entity_mut(entity)
            .remove::<(Retiring, commands::Quitting, commands::Camped)>();
        let _ = outbound.try_send(if was_linkdead {
            b"Reconnecting.\r\n".to_vec()
        } else {
            b"You take over your own body, already in use!\r\n".to_vec()
        });
        // Replacing the component drops the entity's handle to the old
        // channel; the entity itself (items, state) is untouched.
        world.entity_mut(entity).insert(Connection(outbound));
        if was_linkdead {
            // Same in-world character, picked up where the fight left it: no
            // DB read, so nothing here can race an in-flight save.
            world.entity_mut(entity).remove::<commands::Linkdead>();
            commands::note_player_input(world, entity);
            if let Some(room) = world.get::<Located>(entity).map(|l| l.0) {
                let name = commands::name_of(world, entity);
                commands::broadcast_room_visual(
                    world,
                    room,
                    entity,
                    &[entity],
                    &commands::cap_sentence_start(&format!("{name} has reconnected.\r\n")),
                );
            }
        }
        self.bind_player(conn_id, entity, world);
        true
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
        self.complete_login_inner(conn_id, world, pool, user, char_row, false)
            .await;
    }

    /// Hold a relogging character until its previous session's save has
    /// landed. A failed quit-save is retried in the background for minutes
    /// and an autosave may still be in flight; loading from the database
    /// now would hand the player state older than that write, which would
    /// then land on top of the new session (rolled-back stats, duplicated
    /// items). The wait runs in a spawned task so the game loop keeps
    /// ticking; its result comes back through `auth_tx`.
    fn start_save_wait(
        &mut self,
        conn_id: ConnId,
        coordinator: SaveCoordinator,
        user: User,
        char_row: CharacterRow,
    ) {
        let Some(ctx) = self.login.get_mut(&conn_id) else {
            return;
        };
        // Swallow input until the wait resolves.
        ctx.stage = Stage::Authenticating;
        let _ = ctx
            .outbound
            .try_send(b"Saving your previous session, please wait...\r\n".to_vec());
        info!(conn_id, character_id = %char_row.id,
            "login waiting for the previous session's save to land");
        let tx = self.auth_tx.clone();
        let wait = self.save_wait;
        tokio::spawn(async move {
            let settled = coordinator.wait_settled(&char_row.id, wait).await;
            let _ = tx.send(AuthDone {
                conn_id,
                kind: AuthDoneKind::SaveSettled {
                    user,
                    char_row: Box::new(char_row),
                    settled,
                },
            });
        });
    }

    /// The previous-session wait ended: refuse the login if the save is
    /// still failing, else finish logging in (which re-reads the character
    /// row: it was read before the wait, so its stats may predate the save).
    async fn finish_save_wait(
        &mut self,
        conn_id: ConnId,
        user: User,
        char_row: CharacterRow,
        settled: bool,
        pool: &PgPool,
        world: &mut World,
    ) {
        if !settled {
            warn!(conn_id, character_id = %char_row.id,
                "login refused: the previous session's save is still failing");
            self.refuse_login(
                conn_id,
                "Your previous session is still being saved; try again in a minute.",
            );
            return;
        }
        // `complete_login_inner` re-reads the row under the character's turn.
        self.complete_login_inner(conn_id, world, pool, user, char_row, true)
            .await;
    }

    /// Tell the connection why its login was refused, then drop it.
    fn refuse_login(&mut self, conn_id: ConnId, message: &str) {
        if let Some(ctx) = self.login.remove(&conn_id) {
            let _ = ctx.outbound.try_send(format!("{message}\r\n").into_bytes());
        }
        self.caps.remove(&conn_id);
        (self.close_conn)(conn_id);
    }

    #[allow(clippy::too_many_lines)]
    async fn complete_login_inner(
        &mut self,
        conn_id: ConnId,
        world: &mut World,
        pool: &PgPool,
        user: User,
        char_row: CharacterRow,
        save_wait_done: bool,
    ) {
        // Duplicate session: take over the live entity instead of
        // spawning a second copy (and a second inventory).
        if self.try_takeover(world, conn_id, &char_row.id) {
            if let Some(&entity) = self.playing.get(&conn_id) {
                commands::info::cmd_look(world, entity, "");
                commands::refresh_player_items_gmcp(world, entity);
                commands::send_prompt(world, entity);
            }
            info!(conn_id, char_name = %char_row.name, "player reconnected (takeover)");
            return;
        }
        // Relog barrier: nothing below may read the database while the
        // character's previous session still has a save pending.
        if !save_wait_done
            && let Some(coordinator) = world.get_resource::<SaveCoordinator>().cloned()
            && coordinator.has_unsettled_saves(&char_row.id)
        {
            self.start_save_wait(conn_id, coordinator, user, char_row);
            return;
        }
        // The character's write turn, from the row read to the spawned
        // entity. Anything that writes this character's rows from outside
        // the world (an offline quest item grant) takes the same turn, so
        // it lands before the reads below or sees the live session.
        let coordinator = world
            .get_resource::<SaveCoordinator>()
            .cloned()
            .unwrap_or_default();
        let ordered = coordinator.begin_ordered(&char_row.id).await;
        // Updates queued for this (still absent) character were written to
        // the database before they were queued, so the fresh row below
        // already holds them: let the inbox discard them now rather than
        // apply them a second time to the spawned entity. (The row passed in
        // can be minutes old: character select waits on a human.)
        if world.contains_resource::<commands::PlayerUpdateInbox>() {
            commands::drain_player_updates(world);
        }
        let char_row = match reload_character_row(pool, &char_row, self.load_fault).await {
            Ok(fresh) => fresh,
            Err(failure) => {
                failure.log(conn_id, &char_row.id);
                self.refuse_login(conn_id, LOAD_FAILED_MESSAGE);
                return;
            }
        };
        // Every per-character table the save path rewrites is loaded
        // here; a failed load refuses the login instead of continuing
        // with empty state that the next save would write over the
        // real rows (the data-loss mode this guards against).
        let loaded = match load_persisted(pool, &char_row, &user, self.load_fault).await {
            Ok(l) => l,
            Err(failure) => {
                failure.log(conn_id, &char_row.id);
                self.refuse_login(conn_id, LOAD_FAILED_MESSAGE);
                return;
            }
        };
        let PersistedLoad {
            item_rows,
            achievement_rows,
            kill_total,
            drunk,
            recent_tells,
            clan,
            script_vars_json,
            trophy_json,
            spell_cooldowns_json,
            cooldowns_json,
            ignore_list_json,
            command_grants_json,
            effect_instances_json,
            pets_json,
            house_summary,
            ability_rows,
            alias_rows,
            all_chars,
        } = loaded;

        // Stamp last_login exactly once per successful login — split
        // from save_state, which used to overwrite it on every
        // autosave (so the column meant "last save," not "last
        // login"). PreviousLogin was already captured from char_row
        // before this call, so the displayed "Last login" line keeps
        // showing the prior session's start. The same UPDATE clears
        // `last_logout`; if it fails the stale value would grant repose
        // for time played after a crash, so the login is refused (before
        // anything is spawned) rather than continuing.
        if let Err(failure) = guarded(
            self.load_fault,
            "last_login",
            mud_db::characters::update_last_login(pool, &char_row.id),
        )
        .await
        {
            failure.log(conn_id, &char_row.id);
            self.refuse_login(conn_id, LOGIN_STAMP_FAILED_MESSAGE);
            return;
        }

        // Unread-mail flag for the enter-game notice. Mail is
        // account-scoped, so an unlinked character (empty user id) has no
        // mailbox. A failed count only drops the notice; it must not block
        // the login.
        let has_mail = if user.id.is_empty() {
            false
        } else {
            match mud_db::mail::unread_count(pool, &user.id).await {
                Ok(n) => n > 0,
                Err(e) => {
                    warn!(conn_id, error = %e, "unread mail count failed");
                    false
                }
            }
        };

        let LoginCtx { outbound, .. } = self.login.remove(&conn_id).unwrap();
        let entity = spawn_player(world, &user, &char_row, outbound);
        let item_count = spawn_inventory(world, entity, &item_rows);
        // From here the entity is the truth for this character: a grant that
        // takes the turn next must go through the world, not the rows.
        coordinator.set_session_live(&char_row.id, true);
        drop(ordered);
        // Apply gear stat bonuses (ObjectAffects), per-element
        // resistances (ObjectResistance), and wear-granted effects
        // (ObjectEffects) for every item that just respawned with
        // an EquippedSlot. The CombatStats base loaded into
        // spawn_player above is the *unmodified* DB row; this pass
        // stacks the gear-derived deltas on top.
        crate::equip_apply::recompute_equipped_keeping_vitals(world, entity);
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
            // Name-approval gate — attach the `NameApprovalPending`
            // marker when the DB column is `false`. The player can
            // play (move / look / fight) but every social channel
            // (`tell` / `say` / `gossip` / `group invite` / clan)
            // refuses until staff runs `approve_name` or
            // `reject_name <new>`. The welcome notice below mirrors
            // the marker so the player isn't confused why `tell`
            // refuses.
            if !char_row.name_approved {
                e.insert(mud_world::NameApprovalPending);
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
            // ScriptVars — JSON object → BTreeMap. Shape was already
            // validated by `check_json` in `load_persisted`.
            if let Some(json) = script_vars_json {
                insert_loaded_script_vars(&mut e, json, char_row.rest_source);
            }
            // Trophy — JSON list → Trophy (validated in `load_persisted`).
            if let Some(json) = trophy_json
                && let Ok(entries) = serde_json::from_value::<
                    std::collections::VecDeque<mud_world::TrophyEntry>,
                >(json)
                && !entries.is_empty()
            {
                e.insert(mud_world::Trophy { entries });
            }
            // SpellSlots — JSON object {in_flight: [...]} → component.
            // Validated in `load_persisted`; NULL/empty means no slots.
            if let Some(json) = spell_cooldowns_json
                && let Ok(slots) = serde_json::from_value::<mud_world::SpellSlots>(json)
                && !slots.in_flight.is_empty()
            {
                e.insert(slots);
            }
            // IgnoreList — JSON array of lowercased names (validated in
            // `load_persisted`).
            if let Some(json) = ignore_list_json
                && let Ok(list) = serde_json::from_value::<Vec<String>>(json)
                && !list.is_empty()
            {
                e.insert(mud_world::IgnoreList(list));
            }
            // CommandGrants — JSON {grants, revokes} (validated in
            // `load_persisted`).
            if let Some(json) = command_grants_json
                && let Ok(grants) = serde_json::from_value::<mud_world::CommandGrants>(json)
                && !grants.is_empty()
            {
                e.insert(grants);
            }
            // Cooldowns — JSON map of ability_id → unix_secs_ready_at.
            // Convert to Instant by computing the offset from `now`,
            // dropping any keys whose ready_at has already passed.
            // Future-proof: if the system clock jumped backwards
            // since save, the saturating_add keeps the offset
            // non-negative.
            if let Some(json) = cooldowns_json
                && let Ok(map) =
                    serde_json::from_value::<std::collections::HashMap<String, i64>>(json)
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
                        now_inst
                            + std::time::Duration::from_secs(u64::try_from(secs_left).unwrap_or(0)),
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
                            u64::try_from(row.sent_at.and_utc().timestamp().max(0)).unwrap_or(0),
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
                            custom: i.custom(),
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
        // Race-innate permanent effects (RaceEffects), rebuilt on every
        // login rather than restored, so relogging never duplicates them.
        apply_player_race_effects(world, entity);
        // Logging in mid-air without wings: the gravity tick re-checks
        // flight next tick, once worn items have re-applied their effects.
        mud_world::movement::begin_fall_if_unsupported(world, entity);
        // Hired / charmed pets — same 1h cap. Helper drops the
        // whole envelope when elapsed exceeds the cap, otherwise
        // spawns each pet next to the player with HP restored.
        if let Some(json) = pets_json
            && let Ok(persisted) = serde_json::from_value::<PersistedPets>(json)
        {
            restore_persisted_pets(world, entity, persisted);
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
        // Wire the connection's output capabilities before anything is
        // shown, so the MOTD and the auto-look render with the client's
        // colour / charset settings.
        self.bind_player(conn_id, entity, world);
        show_enter_game(
            world,
            entity,
            &char_row.name,
            char_row.last_login.is_none(),
            has_mail,
        );
        // One-shot per login: ship the chat-channel directory so
        // the client can build chat tabs from server data instead
        // of hardcoding the channel list. Role-aware — wiznet
        // only appears for immortals.
        commands::send_comm_channel_list(world, entity);
        // Same idea for inventory + equipment frames: client
        // panels need a baseline view at login. Without this push
        // the floating Inventory / Equipment windows stay empty
        // until the player gets / drops / wears something for the
        // first time, which is misleading.
        commands::refresh_player_items_gmcp(world, entity);
        // Quest trigger: AUTO (Wave 4.1). Any quest with
        // `triggerType = AUTO` is granted at login — the dispatcher
        // skips characters already on the quest, so this is safe
        // to re-fire every login.
        crate::quest_triggers::dispatch_auto_trigger(world, entity);
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

/// What a character sees on entering the game, in legacy `CON_MENU` '1'
/// order: MOTD (and staff `imotd`), welcome line, "$n has entered the game."
/// to the room, then the auto-look through the real `look` command so GMCP
/// Room.Info precedes the room text, then the legacy "You have mail
/// waiting." notice when the account has unread mail.
fn show_enter_game(
    world: &mut World,
    entity: Entity,
    name: &str,
    first_login: bool,
    has_mail: bool,
) {
    // Display MOTD before the spawn-prompt. Pulled from the
    // schema's `SystemText` table (key `"motd"`) via the
    // [`mud_world::SystemTexts`] resource; falls back to the
    // compile-time constant when the row is missing. Skipped
    // entirely when the row exists but is empty so a builder
    // can disable the auto-display by clearing the content
    // without dropping the row. `imotd` (staff-only) is also
    // shown when the viewer's level qualifies — the
    // `min_level` gate makes that automatic.
    let viewer_level = world.get::<Profile>(entity).map_or(0, |p| p.level);
    let motd = world
        .get_resource::<mud_world::SystemTexts>()
        .and_then(|t| t.content("motd", viewer_level))
        .unwrap_or(crate::commands::MOTD_TEXT);
    if !motd.trim().is_empty() {
        commands::send_to(world, entity, motd.to_string());
    }
    // Staff-only `imotd`: shown after `motd` so it sits closer
    // to the prompt where eyes land. No fallback constant —
    // an absent row simply means "no immortal motd today,"
    // which is the right behavior for a fresh DB.
    if let Some(imotd) = world
        .get_resource::<mud_world::SystemTexts>()
        .and_then(|t| t.content("imotd", viewer_level))
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
    {
        commands::send_to(world, entity, imotd);
    }
    // Name-approval notice: when the marker is present (i.e. the
    // `Characters.name_approved` column is `false`), surface a
    // one-line heads-up after the MOTD so the player understands
    // why social channels refuse. The marker itself is what gates
    // the social commands; this just explains the gate.
    if world
        .get::<mud_world::NameApprovalPending>(entity)
        .is_some()
    {
        commands::send_to(
            world,
            entity,
            "\r\n<b:yellow>Your character name is awaiting staff approval.</> \
             You can move, look, and fight; chat channels (tell / say / \
             gossip / group) are silenced until a staff member runs \
             'approve_name' or 'reject_name'. Run 'name_status' for the \
             current state.\r\n",
        );
    }
    // Enter-game sequence, legacy `CON_MENU` '1' order: welcome line,
    // "$n has entered the game." to the room, then the auto-look
    // (`look_at_room`). The look goes through the real `look` command so
    // GMCP Room.Info precedes the room text.
    if let Some(room) = world.get::<Located>(entity).map(|l| l.0) {
        commands::send_to(world, entity, format!("\r\nWelcome, {name}.\r\n\r\n"));
        // First-login guidance: a brand-new character has no prior
        // `last_login` stamp. Suppressed for returning characters so
        // veterans aren't nagged.
        if first_login {
            commands::send_to(
                world,
                entity,
                "Try:  look  ·  exits  ·  score  ·  inventory  ·  help newbie\r\n\r\n",
            );
        }
        // Room.AddPlayer updates the "who's here" panel of anyone
        // already in the room; the text line covers plain telnet.
        commands::broadcast_room_player_diff(world, room, entity, "AddPlayer");
        let player_name = commands::name_of(world, entity);
        commands::broadcast_room_visual(
            world,
            room,
            entity,
            &[entity],
            &commands::cap_sentence_start(&format!("{player_name} has entered the game.\r\n")),
        );
        commands::info::cmd_look(world, entity, "");
    }
    if has_mail {
        commands::send_to(world, entity, "You have mail waiting.\r\n");
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
pub(crate) fn build_achievement_components(
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
                ca.unlocked
                    .insert(row.achievement_id, row.unlocked_at.and_utc());
            }
            if !visited.is_empty() {
                zv.by_zone.insert(n, visited);
            }
        } else {
            ca.unlocked
                .insert(row.achievement_id, row.unlocked_at.and_utc());
        }
    }
    (ca, zv)
}

/// Room a character logs in to. A mortal whose saved room has become
/// off-limits (it carries an entry restriction, or sits in a god zone) logs
/// in at their recall point instead, then the race start room; staff land
/// wherever they saved. A restricted room can't be evaluated before the
/// player entity exists, so any restriction counts as off-limits here.
pub(crate) fn resolve_login_room(
    world: &World,
    wanted: (i32, i32),
    recall: Option<Entity>,
    race_start: Option<(i32, i32)>,
    is_staff: bool,
) -> Option<Entity> {
    let index = world.resource::<WorldKeyIndex>();
    let allowed = |room: Entity| {
        is_staff
            || (world.get::<mud_world::EntryRestriction>(room).is_none()
                && !mud_world::room_in_god_zone(world, room))
    };
    let lookup = |key: (i32, i32)| index.rooms.get(&key).copied();
    lookup(wanted)
        .filter(|r| allowed(*r))
        .or_else(|| recall.filter(|r| allowed(*r)))
        .or_else(|| race_start.and_then(lookup).filter(|r| allowed(*r)))
        .or_else(|| lookup(FALLBACK_START))
}

/// Monks fight unarmed by design: the dice stamped as their
/// `NaturalDamage`. `None` for every other class, and for all classes
/// when the catalog has no Monk. `core` is the boot-resolved
/// [`mud_world::CoreClasses`].
pub(crate) fn monk_natural_damage(
    core: mud_world::CoreClasses,
    class_id: Option<i32>,
    level: i32,
) -> Option<mud_world::NaturalDamage> {
    (class_id.is_some() && class_id == core.monk).then(|| mud_world::NaturalDamage {
        num: (level / 10).max(1),
        size: 6,
        bonus: level / 5,
    })
}

/// Single spawn path for a player entity. The `Located(room_entity)`
/// component is added in a follow-up insert (after spawn) only when
/// the starting room resolved — keeping the core bundle one place
/// avoids the recurring "did I update both branches?" bug we hit
/// three times before consolidating.
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_player(
    world: &mut World,
    user: &User,
    c: &CharacterRow,
    outbound: Outbound,
) -> Entity {
    let race_start = world
        .resource::<mud_world::RaceDefaults>()
        .start_room_by_race
        .get(&c.race)
        .copied();
    // Everyone logs back in where they left, however they left and however
    // long they were gone (issue #58: no penalty for quitting, camping or
    // dropping link).
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let (zone, room) = pick_starting_room(c, race_start);

    // Rest / repose R3: accrue Repose for the elapsed offline window
    // BEFORE the spawn bundle insert, so the RestState component
    // lands with the new pool value. Source / tier round-trip
    // verbatim — they're consumed only on first XP gain (R4), never
    // on login per ADR 0001 §1.
    // Offline window = now - last_logout. `last_login` is the START of the
    // previous session, so measuring from it would count play time as rest.
    let elapsed_secs = offline_elapsed_secs(c.last_logout, now_unix);
    let next_level_xp = repose_next_level_xp(world, c.class_id, c.level);
    let new_repose = accrue_repose(c.repose, c.rest_tier, next_level_xp, elapsed_secs);

    let index = world.resource::<WorldKeyIndex>();
    // Recall point: only set when the row has both coordinates AND the room
    // is loaded. Missing recall is a normal state (`recall` will report it).
    let recall_entity = match (c.recall_room_zone_id, c.recall_room_id) {
        (Some(rz), Some(rr)) => index.rooms.get(&(rz, rr)).copied(),
        _ => None,
    };
    let is_staff = mud_db::enums::effective_rank(c.level, user.role)
        .at_least(mud_db::enums::UserRole::Immortal);
    let room_entity = resolve_login_room(world, (zone, room), recall_entity, race_start, is_staff);

    let health = Health {
        hp: c.hit_points,
        max: c.hit_points_max,
    };
    let stamina = Stamina {
        current: c.stamina,
        max: c.stamina_max,
    };
    // Build CombatStats directly from the new combat columns on
    // Characters (combat redesign complete — see migration-plan.md
    // wave 3). armor_rating + damage_reduction_percent fold into
    // armor_pct (sum clamped to 100); soak renames to armor_flat.
    let armor_pct = c
        .armor_rating
        .saturating_add(c.damage_reduction_percent)
        .clamp(0, 100);
    let combat = CombatStats {
        accuracy: c.accuracy,
        evasion: c.evasion,
        attack_power: c.attack_power,
        spell_power: c.spell_power,
        // Per-character crit chance, seeded from Class.baseCritChance
        // at character creation / import time. Defaults to 5% (combat-tier)
        // for schema rows imported before the column landed.
        crit_chance: c.crit_chance,
        pen_pct: c.penetration_percent,
        pen_flat: c.penetration_flat,
        armor_pct,
        armor_flat: c.soak,
        ward_pct: c.ward_percent,
        hardness: c.hardness,
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

    // No room to land in: say so now. The welcome line, arrival message
    // and auto-look for the normal case are sent by `complete_login_inner`
    // after the MOTD (legacy order), once the entity exists.
    if room_entity.is_none() {
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
            Named {
                name: c.name.clone(),
            },
            Account {
                user_id: user.id.clone(),
                character_id: c.id.clone(),
                // Effective in-game staff rank: max(website account role,
                // role implied by character level). An unlinked legacy god
                // character has no account role, so level is what grants
                // its commands. See `mud_db::enums::effective_rank` for
                // why trusting level is safe (XP capped at 99; staff-only
                // paths set level >= 100).
                role: mud_db::enums::effective_rank(c.level, user.role),
                account_role: user.role,
                perms: c.permissions.clone(),
            },
            Connection(outbound),
            health,
            stamina,
            combat,
            core_stats,
            // Posture restored from the schema's `Position` enum so a
            // player who logged out asleep stays asleep. Ghost is the
            // dead-but-incorporeal state — the marker gets applied
            // separately below since it's a marker rather than data.
            // Unmodeled life-state values (Dead / MortallyWounded /
            // Incapacitated / Stunned) fall through to Standing until
            // we have those states in the runtime.
            Posture(match c.position {
                mud_db::enums::Position::Sleeping => PostureKind::Sleeping,
                mud_db::enums::Position::Resting => PostureKind::Resting,
                mud_db::enums::Position::Sitting => PostureKind::Sitting,
                _ => PostureKind::Standing,
            }),
            PlayerFlags(c.player_flags.clone()),
            Prompt(crate::prompt::sanitize_prompt_template(&c.prompt)),
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
                mud_world::AccountWealth(user.account_wealth),
                mud_world::SkillPoints(c.skill_points),
            ),
        ))
        .id();
    let core_classes = world
        .get_resource::<mud_world::CoreClasses>()
        .copied()
        .unwrap_or_default();
    if let Ok(mut e) = world.get_entity_mut(entity) {
        if let Some(re) = room_entity {
            e.insert(Located(re));
        }
        if let Some(re) = recall_entity {
            e.insert(RecallPoint(re));
        }
        // Hunger/Thirst loaded from the row. `regen::hunger_thirst_tick`
        // increments both gauges every game-hour and emits the
        // crossing flavor lines (hungry/starving/thirsty/parched).
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
        // Rest / repose R3: install the RestState component with
        // the freshly-accrued Repose pool. Source / tier persist
        // verbatim from the row; the wake consumer (R4) clears them
        // on the next XP gain.
        e.insert(mud_world::RestState {
            repose: new_repose,
            source: c.rest_source,
            tier: c.rest_tier,
        });
        // Restore the dead-but-incorporeal marker if the player
        // logged out as a ghost. The matching Posture column was
        // already mapped in the spawn bundle; this side just adds
        // the marker so `release` works and combat skip-paths fire.
        if c.position == mud_db::enums::Position::Ghost {
            e.insert(mud_world::Ghost);
        }
        // Monks fight unarmed by design. Stamp NaturalDamage so the
        // combat-pipeline unarmed branch uses real dice instead of
        // the `unarmed floor = 1` fallback (combat.rs:677). Other
        // classes stay weapon-dependent on purpose — a Brawling
        // skill (or similar buff) is the right place to grant
        // temporary unarmed dice to non-monks. The Monk class id is
        // resolved by plain name at boot (`CoreClasses`); ids differ
        // between databases.
        if let Some(natural) = monk_natural_damage(core_classes, c.class_id, c.level) {
            e.insert(natural);
        }
    }
    // Body metrics — height (inches) + weight (lbs). Rolled fresh
    // from `RaceCatalog::random_{height,weight}` for any character
    // without persisted values yet. Wave 3 wires the
    // `Characters.height` / `weight` columns onto `CharacterRow`;
    // once that lands the spawn path will prefer the persisted
    // values and fall back to the random roll here. Characters
    // whose race or gender band isn't authored in the catalog get
    // skipped — no zero-stamped fallback that would render as
    // "0 lbs" on examine.
    {
        let race_catalog = world.resource::<mud_world::RaceCatalog>();
        let height = race_catalog.random_height(&c.race, &c.gender);
        let weight = race_catalog.random_weight(&c.race, &c.gender);
        if let (Some(h), Some(w)) = (height, weight)
            && let Ok(mut em) = world.get_entity_mut(entity)
        {
            em.insert(mud_world::BodyMetrics {
                height: h,
                weight: w,
            });
        }
    }
    // Race-granted resistances. `Races.resistances` JSON folds
    // into the player's `Resistances` map at spawn time so combat
    // reads a single aggregated lookup. Class resistances stack
    // additively on top — same shape, same map. Empty maps are
    // skipped so we don't allocate a Resistances component just
    // to hold nothing.
    {
        let mut race_map: std::collections::HashMap<mud_db::enums::ElementType, i32> =
            std::collections::HashMap::new();
        if let Some(def) = world.resource::<mud_world::RaceCatalog>().get(&c.race) {
            race_map.clone_from(&def.resistances);
        }
        if let Some(cid) = c.class_id
            && let Some(class_def) = world.resource::<mud_world::ClassCatalog>().by_id.get(&cid)
        {
            for (el, v) in &class_def.resistances {
                *race_map.entry(*el).or_insert(0) += v;
            }
        }
        if !race_map.is_empty()
            && let Ok(mut em) = world.get_entity_mut(entity)
        {
            em.insert(mud_world::Resistances(race_map));
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
    /// On a failed write, the owned snapshot that never reached the
    /// database. Callers about to destroy the player (quit, idle kick,
    /// shutdown) hand it to [`retry_failed_save`] so the state isn't lost.
    pub retry: Option<PlayerSaveSnapshot>,
}

/// Everything one character save writes, captured synchronously from the
/// ECS by [`snapshot_player`]. Owning every value (no `World` borrow, no
/// `Entity` dereference) is what lets the DB write run off the tick: the
/// snapshot is built in microseconds on the game thread, then handed to
/// [`write_snapshot`] wherever it is convenient to await.
#[derive(Debug)]
pub(crate) struct PlayerSaveSnapshot {
    pub(crate) character_id: String,
    pub(crate) entity: Entity,
    /// Monotonic per-character save sequence number, assigned by the
    /// [`SaveCoordinator`](crate::autosave::SaveCoordinator) when the
    /// snapshot is taken. A write whose generation is lower than one
    /// already committed is stale and must not run (see `autosave.rs`).
    pub(crate) generation: u64,
    /// Set only by a session-ending save ([`save_player_final`]): the
    /// instant to stamp into `Characters.last_logout` in the same
    /// transaction. `None` for autosave / `save` / Lua saves, which must
    /// never move it (offline rest accrues from it).
    pub(crate) last_logout: Option<chrono::NaiveDateTime>,
    /// This save consumed a Lua `PendingSave` request; if the write fails
    /// the coordinator re-arms the marker so the request isn't lost.
    pub(crate) resume_pending_save: bool,
    hp: i32,
    stamina: i32,
    zone_id: Option<i32>,
    room_id: Option<i32>,
    recall_zone: Option<i32>,
    recall_room: Option<i32>,
    flags: Vec<mud_db::enums::PlayerFlag>,
    prompt: String,
    title: Option<String>,
    description: Option<String>,
    wealth: i64,
    experience: i32,
    skill_points: i32,
    hunger: i32,
    thirst: i32,
    invis_level: i32,
    freeze_level: Option<i32>,
    wimpy_threshold: i32,
    poof_in: Option<String>,
    poof_out: Option<String>,
    position: mud_db::enums::Position,
    rest_source: mud_db::enums::RestSource,
    rest_tier: i32,
    repose: i32,
    items: Vec<mud_db::character_items::CharacterItemSnap>,
    /// Parallel to `items`: the entity each snap came from, so ids the
    /// diff assigns can be stamped back as `PersistedItemId`.
    entity_for_idx: Vec<Entity>,
    drunk: i32,
    bank: i64,
    script_vars_json: Option<serde_json::Value>,
    trophy_json: Option<serde_json::Value>,
    spell_cooldowns_json: Option<serde_json::Value>,
    cooldowns_json: Option<serde_json::Value>,
    ignore_list_json: Option<serde_json::Value>,
    command_grants_json: Option<serde_json::Value>,
    pub(crate) effect_instances_json: Option<serde_json::Value>,
    pets_json: Option<serde_json::Value>,
    ability_rows: Vec<mud_db::character_abilities::CharacterAbilityRow>,
    alias_rows: Vec<mud_db::character_aliases::CharacterAliasRow>,
    core_stats_payload: Option<mud_db::characters::CoreStatsPayload>,
    now_inst: std::time::Instant,
    new_time_played: Option<i32>,
    /// The player died and the corpse is not committed yet: this write
    /// inserts the `PlayerCorpses` row and files the moved items under it
    /// in the same transaction as the rest of the save.
    pub(crate) death: Option<crate::corpses::DeathPersist>,
    /// Coins taken from player corpses since the last committed save,
    /// debited from those corpses in this write's transaction.
    corpse_coin_takes: Vec<(i32, i64)>,
    /// Corpses a resurrection emptied into this player: deleted in this
    /// write's transaction, after their items are re-homed.
    retired_corpses: Vec<i32>,
    /// `PlayerHouseItem` rows of items this player picked up: deleted in
    /// this write's transaction, the one that inserts the items into the
    /// pack, so a pickup is never persisted twice or not at all.
    house_row_deletes: Vec<i32>,
    /// Corpses this player has looted since their last settled mark:
    /// `(PlayerCorpses.id, loot sequence)`. This commit is what makes those
    /// takes durable, so [`apply_commit`] clears exactly these marks and
    /// decay / retirement of the corpse may proceed.
    corpse_loot_marks: Vec<(i32, u64)>,
    /// `PlayerCorpses.id` the death transaction inserted (0 = none).
    /// Set once the transaction commits; [`apply_commit`] reads it.
    pub(crate) committed_corpse_id: std::sync::atomic::AtomicI32,
}

/// Capture a player's save payload from the ECS without any I/O. Returns
/// `None` when the entity isn't a player (no `Account`, e.g. a mob
/// reached via `switch`).
#[allow(clippy::too_many_lines)]
pub(crate) fn snapshot_player(
    world: &mut World,
    entity: Entity,
    generation: u64,
) -> Option<PlayerSaveSnapshot> {
    let account = world.get::<Account>(entity).cloned()?;
    // Worn gear is re-applied from the equipped items at login, so the
    // character row keeps the values without it (else every relog stacks
    // the bonus again).
    let gear = crate::equip_apply::gear_offsets(world, entity);
    let hp = world.get::<Health>(entity).map_or(0, |h| {
        crate::equip_apply::base_current(h.hp, h.max, gear.max_hp)
    });
    let stamina = world.get::<Stamina>(entity).map_or(0, |s| {
        crate::equip_apply::base_current(s.current, s.max, gear.max_stamina)
    });
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
    let invis_level = world.get::<mud_world::WizInvis>(entity).map_or(0, |w| w.0);
    // Frozen marker → freeze_level. The schema column is
    // nullable; we send None when the marker is absent, so an
    // unfreeze on this session genuinely clears the lock instead
    // of leaving a stale level on the row.
    let freeze_level: Option<i32> = world.get::<mud_world::Frozen>(entity).map(|_| 1);
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

    // Rest / repose R2: stamp `restSource=QUIT, restTier=0` on the
    // way out if the player logs off without a real source acquired.
    // Don't overwrite an existing CAMP / INN / HOUSE — those are the
    // rest the player paid for and must survive the disconnect.
    // TODO(housing): when the housing schema lands, detect "logged
    // out in a HOUSE-owned room" here and stamp restSource=HOUSE,
    // restTier=<house quality> instead of QUIT.
    let rest_snapshot = world
        .get::<mud_world::RestState>(entity)
        .copied()
        .unwrap_or_default();
    let (rest_source, rest_tier) = match rest_snapshot.source {
        mud_db::enums::RestSource::None => (mud_db::enums::RestSource::Quit, 0),
        other => (other, rest_snapshot.tier),
    };
    let repose = rest_snapshot.repose;

    // Snapshot every Item rooted at the player — both directly carried
    // and nested inside any container the player carries. BFS keeps
    // parents before children so save_inventory_diff can resolve
    // `parent_idx` for newly-acquired items inside newly-acquired
    // containers. `entity_for_idx` is the parallel Vec we use to write
    // back assigned PersistedItemId(s) after the diff returns.
    //
    // After a death the corpse's contents are snapshotted too (second
    // root, `in_corpse`), so the death transaction can file those rows
    // under the new corpse instead of the diff deleting them as "no
    // longer carried".
    let death = crate::corpses::pending_death(world, entity);
    let (new_items, entity_for_idx): (
        Vec<mud_db::character_items::CharacterItemSnap>,
        Vec<Entity>,
    ) = {
        use std::collections::HashMap;
        type ItemSnap = (
            Entity,
            Entity,
            WorldKey,
            Option<EquippedSlot>,
            Option<mud_world::PersistedItemId>,
            Option<mud_world::Charges>,
            Option<mud_world::LiquidContainer>,
            bool,
        );
        // BFS from each root (the player, then the corpse) through the
        // `Contents` reverse index of `Located`, so the walk costs
        // O(carried items) rather than a scan over every item in the
        // world. Parents are pushed before their children.
        let mut order: Vec<ItemSnap> = Vec::new();
        let mut entity_to_idx: HashMap<Entity, usize> = HashMap::new();
        let mut roots: Vec<(Entity, bool)> = vec![(entity, false)];
        if let Some(d) = &death {
            roots.push((d.corpse, true));
        }
        for &(root, in_corpse) in &roots {
            let mut frontier: Vec<Entity> = vec![root];
            while let Some(parent) = frontier.pop() {
                let Some(contents) = world.get::<mud_world::Contents>(parent) else {
                    continue;
                };
                for e in contents.iter() {
                    if entity_to_idx.contains_key(&e) || world.get::<Item>(e).is_none() {
                        continue;
                    }
                    // Items without a prototype key can't be reloaded.
                    let Some(wk) = world.get::<WorldKey>(e).copied() else {
                        continue;
                    };
                    // TEMPORARY items vanish on rent / logout — drop them
                    // (and anything inside them) from the snapshot so they
                    // don't round-trip into the next session. Permanent
                    // disappearance is the canonical behavior; the DB row is
                    // also released by the diff (an item not in the snapshot
                    // is deleted).
                    if world
                        .get::<mud_world::ObjectFlags>(e)
                        .is_some_and(|f| f.has(mud_db::enums::ObjectFlag::Temporary))
                    {
                        continue;
                    }
                    entity_to_idx.insert(e, order.len());
                    order.push((
                        e,
                        parent,
                        wk,
                        world.get::<EquippedSlot>(e).copied(),
                        world.get::<mud_world::PersistedItemId>(e).copied(),
                        world.get::<mud_world::Charges>(e).copied(),
                        world.get::<mud_world::LiquidContainer>(e).cloned(),
                        in_corpse,
                    ));
                    frontier.push(e);
                }
            }
        }
        // Pull persisted-id of the parent (if loaded) from the entity
        // map so the diff can set container_id directly without waiting
        // for the parent's INSERT.
        let parent_pid_lookup: HashMap<Entity, Option<i32>> = order
            .iter()
            .map(|(e, _, _, _, pid, _, _, _)| (*e, pid.map(|p| p.0)))
            .collect();

        let mut snaps: Vec<mud_db::character_items::CharacterItemSnap> =
            Vec::with_capacity(order.len());
        let mut ents: Vec<Entity> = Vec::with_capacity(order.len());
        for (e, parent, wk, eq, pid, ch, lc, in_corpse) in &order {
            let is_root = roots.iter().any(|(r, _)| r == parent);
            let parent_persisted_id = if is_root {
                None
            } else {
                parent_pid_lookup.get(parent).copied().flatten()
            };
            let parent_idx = if is_root {
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
                lit: world.get::<mud_world::Lit>(*e).is_some(),
                custom: world.get::<mud_world::ItemCustomization>(*e).map(|c| {
                    mud_db::character_items::ItemCustomSnap {
                        name: c.name.clone(),
                        examine: c.examine.clone(),
                        keywords: c.keywords.clone(),
                        overwrite: c.dirty,
                    }
                }),
                alter: Some(mud_db::character_items::ItemAlterSnap {
                    state: crate::item_alter::snapshot(world, *e),
                    overwrite: world
                        .get::<mud_world::components::ItemAlterDirty>(*e)
                        .is_some(),
                }),
                in_corpse: *in_corpse,
            });
            ents.push(*e);
        }
        (snaps, ents)
    };

    // Pre-collect all snapshot values so the inside-tx block doesn't
    // re-borrow the world. Each helper that takes a JSON blob /
    // counter value reads from these locals; the world only gets
    // re-borrowed at the very end (post-commit) to stamp PersistedItemId
    // and bump TimePlayed/LastPersistedAt.
    let drunk = world
        .get::<mud_world::Drunkenness>(entity)
        .map_or(0, |d| d.0);
    let bank = world.get::<BankWealth>(entity).map_or(0, |b| b.0);
    // The account-shared bank balance is NOT part of the snapshot: it belongs
    // to the user, not this character, and every change goes through a
    // guarded delta in the database (`account_bank`). Writing the in-memory
    // value back here would let a stale sibling overwrite another
    // character's transfer and duplicate the coin.
    let script_vars_json = script_vars_for_save(world, entity);
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
    let cooldowns_json: Option<serde_json::Value> =
        world.get::<mud_world::Cooldowns>(entity).and_then(|cd| {
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
                    Some((
                        id.to_string(),
                        now_unix.saturating_add(i64::try_from(secs_left).unwrap_or(0)),
                    ))
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
    let command_grants_json: Option<serde_json::Value> = world
        .get::<mud_world::CommandGrants>(entity)
        .filter(|g| !g.is_empty())
        .and_then(|g| serde_json::to_value(g).ok());
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
            Option<&mud_world::SpellResistanceDelta>,
            Option<&mud_world::AlignmentProtectionTag>,
            Has<mud_world::InvisibleSource>,
        )>();
        let effects: Vec<PersistedEffectInstance> = q
            .iter(world)
            .filter(|(_, applied, ..)| applied.0 == entity)
            // Race innates are rebuilt from `RaceEffects` at login.
            .filter(|(inst, ..)| !mud_world::mob_effects::is_innate_effect(&inst.source))
            .map(
                |(inst, _, modd, resist, align, invisible)| PersistedEffectInstance {
                    kind: inst.kind,
                    name: inst.name.clone(),
                    strength: inst.strength,
                    remaining_secs: inst.remaining_secs,
                    source: inst.source.clone(),
                    ability_id: inst.ability_id,
                    modify_delta: modd.map(|m| (m.target.clone(), m.amount)),
                    resistance: resist.map(|r| (r.element, r.percent)),
                    align_protect: align.map(|a| {
                        match a {
                            mud_world::AlignmentProtectionTag::Evil => "evil",
                            mud_world::AlignmentProtectionTag::Good => "good",
                        }
                        .to_string()
                    }),
                    invisible_source: invisible,
                },
            )
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
    // Pets — query mob entities with `Follower(player)` AND
    // `PersistentPet` (the durability marker attached at hire / charm
    // sites). Other followers (random tagalong, ephemeral summons)
    // are session-only by intentionally lacking the marker.
    let pets_json: Option<serde_json::Value> = {
        let mut q = world.query_filtered::<(
            &Named,
            &WorldKey,
            &Health,
            &Follower,
        ), (With<Mob>, With<mud_world::PersistentPet>)>();
        let pets: Vec<PersistedPet> = q
            .iter(world)
            .filter(|(_, _, _, follower)| follower.0 == entity)
            .map(|(named, wk, health, _)| PersistedPet {
                proto_zone_id: wk.zone,
                proto_id: wk.id,
                name: named.name.clone(),
                hp: health.hp,
                max_hp: health.max,
            })
            .collect();
        if pets.is_empty() {
            None
        } else {
            let saved_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            serde_json::to_value(&PersistedPets {
                saved_at_unix,
                pets,
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
    let core_stats_payload: Option<mud_db::characters::CoreStatsPayload> =
        crate::equip_apply::base_core_stats(world, entity).map(Into::into);
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

    // Body / life-state for the schema's `Position` enum. Ghost
    // wins — a player who logged out as a ghost (post-death,
    // pre-`release`) stays a ghost on reconnect rather than popping
    // back into a pristine body. Otherwise translate `Posture` onto
    // the schema variant; `Kneeling` collapses onto `Sitting`
    // because the schema doesn't model kneeling separately.
    let position = if world.get::<mud_world::Ghost>(entity).is_some() {
        mud_db::enums::Position::Ghost
    } else {
        match world.get::<Posture>(entity).map(|p| p.0) {
            Some(PostureKind::Sleeping) => mud_db::enums::Position::Sleeping,
            Some(PostureKind::Resting) => mud_db::enums::Position::Resting,
            Some(PostureKind::Sitting | PostureKind::Kneeling) => mud_db::enums::Position::Sitting,
            Some(PostureKind::Standing) | None => mud_db::enums::Position::Standing,
        }
    };

    // === Single transaction wraps every per-character DB write ===
    //
    // All-or-nothing: if any `save_X` fails, the `?` short-circuits,
    // the inner block returns Err, the tx drops without commit (auto-
    // rollback), and the character row is unchanged from the last
    // successful save. Postgres SAVEPOINT is implicit on `?`-bubbling
    // so we don't manage it explicitly. PersistedItemId stamping
    // happens AFTER commit so a rolled-back save can't leave entities
    // pointing at row IDs that don't exist.

    Some(PlayerSaveSnapshot {
        character_id: account.character_id,
        entity,
        generation,
        resume_pending_save: false,
        hp,
        stamina,
        zone_id,
        room_id,
        recall_zone,
        recall_room,
        flags,
        prompt,
        title,
        description,
        wealth,
        experience,
        skill_points,
        hunger,
        thirst,
        invis_level,
        freeze_level,
        wimpy_threshold,
        poof_in,
        poof_out,
        position,
        rest_source,
        rest_tier,
        repose,
        last_logout: None,
        items: new_items,
        entity_for_idx,
        drunk,
        bank,
        script_vars_json,
        trophy_json,
        spell_cooldowns_json,
        cooldowns_json,
        ignore_list_json,
        command_grants_json,
        effect_instances_json,
        pets_json,
        ability_rows,
        alias_rows,
        core_stats_payload,
        now_inst,
        new_time_played,
        death,
        house_row_deletes: world
            .get::<crate::house_items::PendingHouseDeletes>(entity)
            .map(|p| p.0.clone())
            .unwrap_or_default(),
        corpse_coin_takes: world
            .get::<crate::corpses::PendingCorpseCoinTakes>(entity)
            .map(|t| t.0.clone())
            .unwrap_or_default(),
        // A corpse some OTHER player has looted from, and whose save hasn't
        // committed, keeps its row: deleting it now would cascade away
        // their item rows before they are re-homed. It stays pending and
        // the next save retries.
        retired_corpses: world
            .get::<crate::corpses::PendingCorpseRetire>(entity)
            .map(|p| {
                p.0.iter()
                    .copied()
                    .filter(|id| !crate::corpses::has_pending_loot(world, *id, Some(entity)))
                    .collect()
            })
            .unwrap_or_default(),
        corpse_loot_marks: crate::corpses::loot_marks(world, entity),
        committed_corpse_id: std::sync::atomic::AtomicI32::new(0),
    })
}

/// Shown when a per-character table could not be read at login.
const LOAD_FAILED_MESSAGE: &str =
    "The game is having trouble loading your character; please try again in a minute.";

/// Shown when the login-time `last_login` / `last_logout` stamp could not be
/// written.
const LOGIN_STAMP_FAILED_MESSAGE: &str = "Please try again in a moment.";

/// Saved per-character state could not be loaded; carries which table or
/// column for the log. Either way the login is refused, because the next
/// save would write the empty stand-in over the real data.
#[derive(Debug)]
enum LoadFailure {
    /// A table read failed.
    Db {
        table: &'static str,
        source: mud_db::sqlx::Error,
    },
    /// A non-empty JSON column did not parse into its runtime shape.
    Parse {
        column: &'static str,
        source: serde_json::Error,
    },
}

impl LoadFailure {
    /// Log at ERROR naming the character and table/column. Never the raw
    /// column value: it is player data.
    fn log(&self, conn_id: ConnId, character_id: &str) {
        match self {
            Self::Db { table, source } => error!(conn_id, character_id, table, error = %source,
                "login refused: per-character table failed to load"),
            Self::Parse { column, source } => error!(conn_id, character_id, column,
                error = %source,
                "login refused: saved state column failed to parse"),
        }
    }
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db { table, source } => write!(f, "{table}: {source}"),
            Self::Parse { column, source } => write!(f, "{column}: {source}"),
        }
    }
}

/// Refuse a saved JSON column that has content but does not parse as `T`.
/// NULL and empty values (`null`, `""`, `{}`, `[]`) carry nothing to lose
/// and are allowed.
fn check_json<T: serde::de::DeserializeOwned>(
    column: &'static str,
    value: Option<&serde_json::Value>,
) -> Result<(), LoadFailure> {
    let Some(value) = value else { return Ok(()) };
    let empty = match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    };
    if empty {
        return Ok(());
    }
    serde_json::from_value::<T>(value.clone())
        .map(drop)
        .map_err(|source| LoadFailure::Parse { column, source })
}

/// Everything read from per-character tables at login.
struct PersistedLoad {
    item_rows: Vec<mud_db::character_items::CharacterItemRow>,
    achievement_rows: Vec<mud_db::achievements::CharacterAchievementRow>,
    kill_total: i32,
    drunk: i32,
    recent_tells: Vec<mud_db::tell_messages::TellMessageRow>,
    clan: Option<mud_db::clans::ClanMembershipRow>,
    script_vars_json: Option<serde_json::Value>,
    trophy_json: Option<serde_json::Value>,
    spell_cooldowns_json: Option<serde_json::Value>,
    cooldowns_json: Option<serde_json::Value>,
    ignore_list_json: Option<serde_json::Value>,
    command_grants_json: Option<serde_json::Value>,
    effect_instances_json: Option<serde_json::Value>,
    pets_json: Option<serde_json::Value>,
    house_summary: Option<HouseBundle>,
    ability_rows: Vec<mud_db::character_abilities::CharacterAbilityRow>,
    alias_rows: Vec<mud_db::character_aliases::CharacterAliasRow>,
    all_chars: Vec<CharacterRow>,
}

type HouseBundle = (
    mud_db::housing::PlayerHouseRow,
    Vec<mud_db::housing::PlayerHouseRoomRow>,
    Vec<mud_db::housing::PlayerHouseExitRow>,
    Vec<mud_db::housing::PlayerHouseItemRow>,
    Vec<mud_db::housing::PlayerHouseGuestRow>,
);

/// Run one table load, mapping its error to a [`LoadFailure`]. `fault`
/// is the test seam: when it names `table` the load is replaced by an
/// injected pool-timeout error.
async fn guarded<T>(
    fault: Option<&str>,
    table: &'static str,
    load: impl std::future::Future<Output = mud_db::sqlx::Result<T>>,
) -> Result<T, LoadFailure> {
    let result = if fault == Some(table) {
        Err(mud_db::sqlx::Error::PoolTimedOut)
    } else {
        load.await
    };
    result.map_err(|source| LoadFailure::Db { table, source })
}

/// Housing summary: `None` for the typical player who owns no house.
async fn load_house(
    pool: &PgPool,
    character_id: &str,
    fault: Option<&str>,
) -> Result<Option<HouseBundle>, LoadFailure> {
    let Some(h) = guarded(
        fault,
        "player_houses",
        mud_db::housing::for_character(pool, character_id),
    )
    .await?
    else {
        return Ok(None);
    };
    let rooms = guarded(
        fault,
        "player_house_rooms",
        mud_db::housing::rooms_for_house(pool, h.id),
    )
    .await?;
    let exits = guarded(
        fault,
        "player_house_exits",
        mud_db::housing::exits_for_house(pool, h.id),
    )
    .await?;
    let items = guarded(
        fault,
        "player_house_items",
        mud_db::housing::items_for_house(pool, h.id),
    )
    .await?;
    let guests = guarded(
        fault,
        "player_house_guests",
        mud_db::housing::guests_for_house(pool, h.id),
    )
    .await?;
    Ok(Some((h, rooms, exits, items, guests)))
}

/// Load every per-character table for `char_row`. The save path rewrites
/// each of these from in-memory state (delete-not-in-memory semantics),
/// so continuing a login with an empty stand-in for a table that failed to
/// read would let the next save wipe the real rows: any failure here
/// aborts the login. Only data the save path never writes back (recent
/// tells, the account's character list, race innates) falls back softly.
#[allow(clippy::too_many_lines)]
async fn load_persisted(
    pool: &PgPool,
    char_row: &CharacterRow,
    user: &User,
    fault: Option<&str>,
) -> Result<PersistedLoad, LoadFailure> {
    let id = char_row.id.as_str();
    let item_rows = guarded(
        fault,
        "character_items",
        mud_db::character_items::list_for(pool, id),
    )
    .await?;
    let achievement_rows = guarded(
        fault,
        "achievements",
        mud_db::achievements::unlocked_for(pool, id),
    )
    .await?;
    // Lifetime kill counter, persisted in the JSON column on
    // Characters. Defaults to 0 for new characters / null JSON.
    let kill_total: i32 = guarded(
        fault,
        "kill_tracking",
        mud_db::characters::load_kill_tracking(pool, id),
    )
    .await?
    .and_then(|v| v.get("total").and_then(serde_json::Value::as_i64))
    .and_then(|n| i32::try_from(n).ok())
    .unwrap_or(0);
    let drunk = guarded(
        fault,
        "drunkenness",
        mud_db::characters::load_drunkenness(pool, id),
    )
    .await?;
    // Last 10 received tells, newest first. Read-only; cosmetic.
    let recent_tells = mud_db::tell_messages::recent_for(pool, id, 10)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "tell history load failed");
            Vec::new()
        });
    let clan = guarded(
        fault,
        "clan_membership",
        mud_db::clans::membership_for(pool, id),
    )
    .await?;
    // Script-vars / trophy / cooldowns / ignore / effects / pets JSON
    // blobs. NULL is legitimate (never saved); a read error is not.
    let script_vars_json = guarded(
        fault,
        "script_vars",
        mud_db::characters::load_script_vars(pool, id),
    )
    .await?;
    let trophy_json = guarded(fault, "trophy", mud_db::characters::load_trophy(pool, id)).await?;
    let spell_cooldowns_json = guarded(
        fault,
        "spell_cooldowns",
        mud_db::characters::load_spell_cooldowns(pool, id),
    )
    .await?;
    let cooldowns_json = guarded(
        fault,
        "cooldowns",
        mud_db::characters::load_cooldowns(pool, id),
    )
    .await?;
    let ignore_list_json = guarded(
        fault,
        "ignore_list",
        mud_db::characters::load_ignore_list(pool, id),
    )
    .await?;
    let command_grants_json = guarded(
        fault,
        "command_grants",
        mud_db::characters::load_command_grants(pool, id),
    )
    .await?;
    let effect_instances_json = guarded(
        fault,
        "effect_instances",
        mud_db::characters::load_effect_instances(pool, id),
    )
    .await?;
    let pets_json = guarded(fault, "pets", mud_db::characters::load_pets(pool, id)).await?;
    // The component hydration below tolerates nothing: a column that has
    // content but does not parse would be dropped, and the next save
    // would then write NULL over it.
    check_json::<std::collections::BTreeMap<String, String>>(
        "script_vars",
        script_vars_json.as_ref(),
    )?;
    check_json::<std::collections::VecDeque<mud_world::TrophyEntry>>(
        "trophy_data",
        trophy_json.as_ref(),
    )?;
    check_json::<mud_world::SpellSlots>("spell_cooldowns", spell_cooldowns_json.as_ref())?;
    check_json::<Vec<String>>("ignore_list", ignore_list_json.as_ref())?;
    check_json::<mud_world::CommandGrants>("command_grants", command_grants_json.as_ref())?;
    check_json::<std::collections::HashMap<String, i64>>("cooldowns", cooldowns_json.as_ref())?;
    check_json::<PersistedEffects>("effect_instances", effect_instances_json.as_ref())?;
    check_json::<PersistedPets>("pets", pets_json.as_ref())?;
    let house_summary = load_house(pool, id, fault).await?;
    let mut ability_rows = guarded(
        fault,
        "character_abilities",
        mud_db::character_abilities::list_for(pool, id),
    )
    .await?;
    // Race innates (`RaceAbilities`) are part of the character from
    // creation: grant any the saved set lacks. A read failure here only
    // delays the grant (nothing is overwritten), so it falls back softly.
    match mud_db::race_abilities::list_for_race(pool, &char_row.race).await {
        Ok(innates) => {
            let granted = mud_db::race_abilities::merge_innates(&mut ability_rows, &innates);
            if granted > 0 {
                info!(race = %char_row.race, granted, "granted race innates");
            }
        }
        Err(e) => warn!(error = %e, "race innates load failed"),
    }
    let alias_rows = guarded(
        fault,
        "character_aliases",
        mud_db::character_aliases::list_for(pool, id),
    )
    .await?;
    // AccountSummary lists all sibling characters on the account. Empty
    // when the character has no associated user (some legacy imports);
    // the summary then shows just the chosen one. Display-only.
    let all_chars: Vec<CharacterRow> = if user.id.is_empty() {
        vec![char_row.clone()]
    } else {
        characters::list_for_user(pool, &user.id)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "character list failed");
                vec![char_row.clone()]
            })
    };
    Ok(PersistedLoad {
        item_rows,
        achievement_rows,
        kill_total,
        drunk,
        recent_tells,
        clan,
        script_vars_json,
        trophy_json,
        spell_cooldowns_json,
        cooldowns_json,
        ignore_list_json,
        command_grants_json,
        effect_instances_json,
        pets_json,
        house_summary,
        ability_rows,
        alias_rows,
        all_chars,
    })
}

/// Re-read a character's row after waiting on its pending save, so the
/// session starts from what that save wrote. A failed or empty re-read is
/// an error: continuing with the pre-wait row would let the next
/// `save_state` write those older stats over the save that just landed.
async fn reload_character_row(
    pool: &PgPool,
    stale: &CharacterRow,
    fault: Option<&str>,
) -> Result<CharacterRow, LoadFailure> {
    guarded(fault, "character_reload", async {
        match characters::find_by_name(pool, &stale.name).await? {
            Some(fresh) if fresh.id == stale.id => Ok(fresh),
            _ => Err(mud_db::sqlx::Error::RowNotFound),
        }
    })
    .await
}

/// Run every per-character DB write for `snap` inside ONE transaction.
///
/// All-or-nothing: if any `save_X` fails, the `?` short-circuits, the
/// inner block returns Err, the tx drops without commit (auto-rollback),
/// and the character row is unchanged from the last successful save.
/// Returns the ids assigned to newly-INSERTed `CharacterItems` rows,
/// keyed by index into the snapshot's item list; the caller stamps them
/// on the entities (via [`apply_commit`]) only AFTER commit so a
/// rolled-back save can't leave entities pointing at row ids that don't
/// exist.
#[allow(clippy::too_many_lines)]
pub(crate) async fn write_snapshot(
    pool: &PgPool,
    snap: &PlayerSaveSnapshot,
) -> Result<HashMap<usize, i32>, mud_db::sqlx::Error> {
    let cid = snap.character_id.as_str();
    let mut tx = pool.begin().await?;
    characters::save_state(
        &mut *tx,
        cid,
        &mud_db::characters::CharacterStatePayload {
            hit_points: snap.hp,
            stamina: snap.stamina,
            current_room_zone_id: snap.zone_id,
            current_room_id: snap.room_id,
            recall_room_zone_id: snap.recall_zone,
            recall_room_id: snap.recall_room,
            player_flags: &snap.flags,
            prompt: &snap.prompt,
            title: snap.title.as_deref(),
            description: snap.description.as_deref(),
            wealth: snap.wealth,
            experience: snap.experience,
            skill_points: snap.skill_points,
            hunger: snap.hunger,
            thirst: snap.thirst,
            invis_level: snap.invis_level,
            freeze_level: snap.freeze_level,
            wimpy_threshold: snap.wimpy_threshold,
            poof_in: snap.poof_in.as_deref(),
            poof_out: snap.poof_out.as_deref(),
            position: snap.position,
        },
    )
    .await?;
    // Death: the corpse row goes in first so the items can be filed under
    // its id; the wealth zeroed above and the items moving into the corpse
    // commit together or not at all.
    let corpse_id = match &snap.death {
        Some(d) => Some(
            mud_db::player_corpses::insert(
                &mut tx,
                cid,
                d.room_zone,
                d.room_id,
                d.coins,
                d.decay_secs,
            )
            .await?,
        ),
        None => None,
    };
    let assigned =
        mud_db::character_items::save_inventory_diff(&mut tx, cid, &snap.items, corpse_id).await?;
    // Items picked up out of a house enter the pack and leave the house in
    // the same commit.
    for id in &snap.house_row_deletes {
        mud_db::housing::remove_item(&mut *tx, *id).await?;
    }
    // Coins this player took from player corpses leave the corpse in the
    // very commit that credits their wealth (written above).
    for (taken_from, amount) in &snap.corpse_coin_takes {
        mud_db::player_corpses::take_coins(&mut tx, *taken_from, *amount).await?;
    }
    for id in &snap.retired_corpses {
        mud_db::player_corpses::delete_in(&mut tx, *id).await?;
    }
    if let Some(at) = snap.last_logout {
        mud_db::characters::save_last_logout(&mut *tx, cid, at).await?;
    }
    mud_db::characters::save_drunkenness(&mut *tx, cid, snap.drunk).await?;
    mud_db::characters::save_script_vars(&mut *tx, cid, snap.script_vars_json.as_ref()).await?;
    mud_db::characters::save_trophy(&mut *tx, cid, snap.trophy_json.as_ref()).await?;
    mud_db::characters::save_spell_cooldowns(&mut *tx, cid, snap.spell_cooldowns_json.as_ref())
        .await?;
    mud_db::characters::save_cooldowns(&mut *tx, cid, snap.cooldowns_json.as_ref()).await?;
    mud_db::characters::save_ignore_list(&mut *tx, cid, snap.ignore_list_json.as_ref()).await?;
    mud_db::characters::save_command_grants(&mut *tx, cid, snap.command_grants_json.as_ref())
        .await?;
    mud_db::characters::save_effect_instances(&mut *tx, cid, snap.effect_instances_json.as_ref())
        .await?;
    mud_db::characters::save_pets(&mut *tx, cid, snap.pets_json.as_ref()).await?;
    mud_db::characters::save_bank_wealth(&mut *tx, cid, snap.bank).await?;
    mud_db::characters::save_rest_state(
        &mut *tx,
        cid,
        snap.repose,
        snap.rest_source,
        snap.rest_tier,
    )
    .await?;
    if let Some(t) = snap.new_time_played {
        mud_db::characters::save_time_played(&mut *tx, cid, t).await?;
    }
    mud_db::character_abilities::save_for(&mut tx, cid, &snap.ability_rows).await?;
    mud_db::character_aliases::save_for(&mut tx, cid, &snap.alias_rows).await?;
    if let Some(stats) = &snap.core_stats_payload {
        characters::save_core_stats(&mut *tx, cid, stats).await?;
    }
    tx.commit().await?;
    if let Some(id) = corpse_id {
        snap.committed_corpse_id
            .store(id, std::sync::atomic::Ordering::SeqCst);
    }
    info!(
        character_id = %snap.character_id,
        hp = snap.hp,
        zone_id = snap.zone_id,
        room_id = snap.room_id,
        recall_zone = snap.recall_zone,
        recall_room = snap.recall_room,
        flag_count = snap.flags.len(),
        item_count = snap.items.len(),
        alias_count = snap.alias_rows.len(),
        generation = snap.generation,
        "player saved"
    );
    Ok(assigned)
}

/// True when `item` is still directly carried by, or nested (via
/// `Located` / `Contents`) inside something carried by, `holder`.
fn is_held_by(world: &World, item: Entity, holder: Entity) -> bool {
    // Real nesting is a handful of bags; the cap only guards against a
    // malformed `Located` cycle.
    let mut cur = item;
    for _ in 0..32 {
        let Some(loc) = world.get::<Located>(cur) else {
            return false;
        };
        cur = loc.0;
        if cur == holder {
            return true;
        }
    }
    false
}

/// Fold a committed save back into the ECS: stamp newly-INSERTed
/// `CharacterItems` ids onto their entities and advance the time-played
/// anchor. Strictly post-commit ("DB first, then in-memory"). Entities
/// despawned since the snapshot (the player quit) are skipped.
pub(crate) fn apply_commit(
    world: &mut World,
    snap: &PlayerSaveSnapshot,
    assigned: HashMap<usize, i32>,
) {
    for (idx, new_id) in assigned {
        let Some(target) = snap.entity_for_idx.get(idx).copied() else {
            continue;
        };
        // The snapshot is a point-in-time view and a background commit can
        // land several ticks later. Stamping blindly would (a) overwrite an
        // id a newer commit already gave the item, and (b) tag an item that
        // has since left this player (given, dropped) with a row this
        // character's next save is about to delete. In both cases the row
        // just written is reconciled by the next save instead. The id the
        // item had when it was snapshotted (`None` for a fresh INSERT, the
        // old id when its row vanished and was re-inserted) must still be
        // the one it has now.
        let id_at_snapshot = snap.items.get(idx).and_then(|s| s.persisted_id);
        let id_now = world.get::<mud_world::PersistedItemId>(target).map(|p| p.0);
        let held = is_held_by(world, target, snap.entity)
            || snap
                .death
                .as_ref()
                .is_some_and(|d| is_held_by(world, target, d.corpse));
        if id_now != id_at_snapshot || !held {
            continue;
        }
        if let Ok(mut em) = world.get_entity_mut(target) {
            em.insert(mud_world::PersistedItemId(new_id));
        }
    }
    settle_item_customizations(world, snap);
    if let Some(d) = &snap.death {
        // The corpse is committed: let it be looted and dragged, and stop
        // carrying the death transaction in this player's snapshots.
        let corpse_id = snap
            .committed_corpse_id
            .load(std::sync::atomic::Ordering::SeqCst);
        if corpse_id != 0 {
            if let Ok(mut em) = world.get_entity_mut(d.corpse) {
                em.insert(mud_world::PlayerCorpseId(corpse_id));
            }
            // Items that decayed inside the corpse before this commit.
            crate::corpses::flush_unsettled_removals(world, d.corpse, corpse_id);
            if let Ok(mut em) = world.get_entity_mut(snap.entity) {
                em.remove::<crate::corpses::PendingDeath>();
            }
        }
    }
    crate::house_items::settle_deletes(world, snap.entity, &snap.house_row_deletes);
    crate::corpses::settle_coin_takes(world, snap.entity, &snap.corpse_coin_takes);
    crate::corpses::settle_retired(world, snap.entity, &snap.retired_corpses);
    crate::corpses::settle_loot(world, snap.entity, &snap.corpse_loot_marks);
    if let Some(t) = snap.new_time_played
        && let Ok(mut em) = world.get_entity_mut(snap.entity)
    {
        em.insert(mud_world::TimePlayed(t));
        em.insert(mud_world::LastPersistedAt(snap.now_inst));
    }
}

/// The commit wrote every `dirty` item customization (and spell-altered
/// item state, see `item_alter`) it carried, so the
/// overrides are settled: clear their `dirty` flag, otherwise every later
/// save would keep overwriting the row and clobber edits made in the
/// database. An override changed again since the snapshot stays dirty for
/// the next save. A row that had to be re-INSERTed was written regardless
/// of the flag, so this only ever clears.
fn settle_item_customizations(world: &mut World, snap: &PlayerSaveSnapshot) {
    for (idx, item) in snap.items.iter().enumerate() {
        if let Some(alter) = item.alter.as_ref().filter(|a| a.overwrite)
            && let Some(target) = snap.entity_for_idx.get(idx).copied()
        {
            crate::item_alter::settle(world, target, &alter.state);
        }
        let Some(saved) = item.custom.as_ref().filter(|c| c.overwrite) else {
            continue;
        };
        let Some(target) = snap.entity_for_idx.get(idx).copied() else {
            continue;
        };
        let Some(mut now) = world.get_mut::<mud_world::ItemCustomization>(target) else {
            continue;
        };
        if now.dirty
            && now.name == saved.name
            && now.examine == saved.examine
            && now.keywords == saved.keywords
        {
            now.dirty = false;
        }
    }
}

/// Foreground save: snapshot + write + apply, awaited by the caller. Used
/// where the caller needs the outcome or the entity is about to vanish
/// (`save` command, disconnect / idle-kick, shutdown). Periodic saves go
/// through [`spawn_background_save`] instead so the tick never waits on
/// Postgres.
///
/// Ordering: holds the character's save lock for the whole write, so it
/// queues behind any in-flight background save of the same character
/// (whose item-id stamps are applied before this snapshot is taken) and
/// any older background snapshot still waiting for the lock is dropped as
/// stale once this write commits.
pub(crate) async fn save_player(world: &mut World, entity: Entity, pool: &PgPool) -> SaveOutcome {
    save_player_inner(world, entity, pool, false).await
}

/// [`save_player`] for a save that ENDS the session (quit, rent, camp, idle
/// kick, linkdead retirement, dropped link, server shutdown): additionally
/// stamps `Characters.last_logout` in the same transaction. This is the only
/// writer of that column, so offline rest never counts time spent playing.
/// The stamp rides the snapshot, so a failed write retried in the background
/// still records when the session actually ended.
pub(crate) async fn save_player_final(
    world: &mut World,
    entity: Entity,
    pool: &PgPool,
) -> SaveOutcome {
    save_player_inner(world, entity, pool, true).await
}

async fn save_player_inner(
    world: &mut World,
    entity: Entity,
    pool: &PgPool,
    session_end: bool,
) -> SaveOutcome {
    let Some(character_id) = world.get::<Account>(entity).map(|a| a.character_id.clone()) else {
        return SaveOutcome {
            aborted: true,
            ..SaveOutcome::default()
        };
    };
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let mut ordered = coordinator.begin_ordered(&character_id).await;
    // The write we just waited for (if any) finished before we got the
    // lock; fold its stamps into the world BEFORE snapshotting.
    coordinator.apply_completions(world);
    // Updates written to the database but not yet applied to the character
    // (quest gold, experience, skill points) reach it through the update
    // inbox. Apply whatever is waiting, now that the turn is ours and before
    // the snapshot: the save would otherwise write the pre-reward values over
    // them, and a house placement that failed while this save waited for its
    // turn would not yet have put its item back in the pack.
    if world.contains_resource::<commands::PlayerUpdateInbox>() {
        commands::drain_player_updates(world);
    }
    let generation = ordered.next_generation();
    let Some(mut snap) = snapshot_player(world, entity, generation) else {
        return SaveOutcome {
            aborted: true,
            ..SaveOutcome::default()
        };
    };
    if session_end {
        snap.last_logout = Some(chrono::Utc::now().naive_utc());
    }
    let mut outcome = SaveOutcome::default();
    match write_snapshot(pool, &snap).await {
        Ok(assigned) => {
            outcome.committed = true;
            ordered.record_commit(generation);
            apply_commit(world, &snap, assigned);
        }
        Err(e) => {
            warn!(error = %e, character_id = %snap.character_id, "save tx failed; rolled back");
            outcome.error = Some(e.to_string());
            outcome.retry = Some(snap);
        }
    }
    outcome
}

/// Background save for periodic autosave and Lua `actor:save()`:
/// snapshots the player now (cheap, in-memory) and hands the DB write to a
/// spawned task, so the tick never awaits I/O. Returns `false` when the
/// character already has a background save in flight (caller retries
/// later) or isn't a player.
pub(crate) fn spawn_background_save(world: &mut World, entity: Entity, pool: &PgPool) -> bool {
    let Some(character_id) = world.get::<Account>(entity).map(|a| a.character_id.clone()) else {
        return false;
    };
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let had_pending_save = world.get::<mud_world::PendingSave>(entity).is_some();
    let pool = pool.clone();
    let started = coordinator.request_background(
        &character_id,
        |generation| {
            let mut snap = snapshot_player(world, entity, generation)?;
            snap.resume_pending_save = had_pending_save;
            Some(snap)
        },
        move |snap| async move {
            write_snapshot(&pool, &snap)
                .await
                .map_err(|e| e.to_string())
        },
    );
    // The request is satisfied by this write; if the write fails the
    // coordinator puts the marker back (`resume_pending_save`).
    if started
        && had_pending_save
        && let Ok(mut em) = world.get_entity_mut(entity)
    {
        em.remove::<mud_world::PendingSave>();
    }
    started
}

/// Drain Lua-requested saves (`PendingSave` markers). A player whose
/// previous background save is still in flight keeps the marker and is
/// retried next tick. A marker on an entity that isn't a player (no
/// `Account`, e.g. a mob a script called `save()` on) can never be
/// satisfied, so it is removed rather than retried forever.
pub(crate) fn drain_pending_saves(world: &mut World, pool: &PgPool) {
    let pending: Vec<Entity> = world
        .query_filtered::<Entity, With<mud_world::PendingSave>>()
        .iter(world)
        .collect();
    for e in pending {
        if world.get::<Account>(e).is_none() {
            warn!(entity = ?e, "PendingSave on a non-player entity; dropping marker");
            if let Ok(mut em) = world.get_entity_mut(e) {
                em.remove::<mud_world::PendingSave>();
            }
            continue;
        }
        // `false` means "busy, retry next tick"; the marker is cleared
        // inside once the write has actually been handed off.
        spawn_background_save(world, e, pool);
    }
}

/// A foreground save failed and the player is about to be despawned:
/// log it loudly and hand the owned snapshot to the background writer,
/// which keeps retrying (generation-ordered) after the entity is gone.
pub(crate) fn retry_failed_save(world: &World, outcome: SaveOutcome, pool: &PgPool) {
    let Some(snap) = outcome.retry else {
        return;
    };
    error!(
        character_id = %snap.character_id,
        error = outcome.error.as_deref().unwrap_or("unknown"),
        "final save FAILED; handing the snapshot to the background writer for retry"
    );
    let coordinator = world
        .get_resource::<SaveCoordinator>()
        .cloned()
        .unwrap_or_default();
    let pool = pool.clone();
    coordinator.retry_failed_snapshot(
        snap,
        move |snap| {
            let pool = pool.clone();
            async move {
                write_snapshot(&pool, &snap)
                    .await
                    .map_err(|e| e.to_string())
            }
        },
        crate::autosave::QUIT_RETRY_BACKOFF,
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
pub(crate) fn spawn_inventory(
    world: &mut World,
    player: Entity,
    rows: &[CharacterItemRow],
) -> usize {
    use std::collections::HashMap;
    // row.id → spawned Entity. Top-level items spawn first; nested rows
    // wait for their parent to land.
    let mut spawned: HashMap<i32, Entity> = HashMap::new();
    let mut pending: Vec<&CharacterItemRow> = rows.iter().collect();
    // Slots already filled during this load. Old saves can hold two rows
    // with the same paired label (two `EARS`, two `WRIST`); the second one
    // goes to the other side of the pair.
    let mut worn: std::collections::HashSet<Slot> = std::collections::HashSet::new();

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
                Named {
                    name: proto.name.clone(),
                },
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
            // Per-instance attribute flags carry along through
            // login rehydration the same as proto-spawned items
            // — without this, a quest item flagged NO_DROP would
            // lose its restriction on logout/login.
            if !proto.flags.is_empty() {
                bundle.insert(mud_world::ObjectFlags(proto.flags.clone()));
            }
            if !proto.restrictions.is_empty() {
                bundle.insert(mud_world::ObjectRestrictions(proto.restrictions.clone()));
            }
            let item_entity = bundle.id();
            crate::item_decay::attach_timer_if_decaying(world, item_entity, &proto);
            // Stamp the row's id so save_inventory_diff knows to UPDATE
            // this row instead of issuing a delete-and-reinsert that
            // would clobber DB columns the runtime doesn't own
            // (condition, instance_flags, custom_name, etc.).
            if let Ok(mut e) = world.get_entity_mut(item_entity) {
                e.insert(mud_world::PersistedItemId(row.id));
            }
            if let Some(slot_str) = row.equipped_location.as_deref()
                && let Some(label_slot) = Slot::from_label_warn(slot_str)
                // Both sides of a pair taken: keep the label's own slot
                // (item stays worn, never dropped). A single-slot
                // position already taken leaves the item carried.
                && let Some(slot) = label_slot.first_free(|s| worn.contains(&s)).or(
                    (label_slot.group().len() > 1).then_some(label_slot),
                )
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                worn.insert(slot);
                e.insert(EquippedSlot(slot));
            }
            // Charges: the proto's starting pool first (so a row left at the
            // schema default `-1` still gets one), then the persisted
            // per-instance value when the row has one (>= 0). Wands that were
            // half-spent before logout come back half-spent.
            mud_world::attach_proto_charges(world, item_entity, proto.zone_id, proto.id);
            if row.charges >= 0
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                e.insert(mud_world::Charges(row.charges));
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
            // Lit state: a light the player lit stays lit across logout.
            // A burnt-out light (fuel 0) never comes back lit; permanent
            // lights need no marker (they are always lit).
            if row.lit
                && world
                    .get::<mud_world::LightFuel>(item_entity)
                    .is_some_and(|f| f.remaining > 0)
                && let Ok(mut e) = world.get_entity_mut(item_entity)
            {
                e.insert(mud_world::Lit);
            }
            if row.custom_name.is_some()
                || row.custom_examine_description.is_some()
                || row.custom_keywords.is_some()
            {
                crate::item_custom::install(
                    world,
                    item_entity,
                    mud_world::ItemCustomization {
                        name: row.custom_name.clone(),
                        examine: row.custom_examine_description.clone(),
                        keywords: row.custom_keywords.clone(),
                        dirty: false,
                    },
                );
            }
            if let Some(alter) = row.alter() {
                crate::item_alter::restore(world, item_entity, &alter, false);
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

/// Races a new character may pick: every `Races` row flagged `playable`
/// in the database, as the `Characters.race` enum text (`HUMAN`,
/// `HALF_ELF`, ...), sorted by that text so the listing is stable.
fn playable_races(world: &World) -> Vec<String> {
    let Some(catalog) = world.get_resource::<mud_world::RaceCatalog>() else {
        return Vec::new();
    };
    let mut races: Vec<String> = catalog
        .by_race
        .values()
        .filter(|r| r.playable)
        .map(|r| r.race.clone())
        .collect();
    races.sort_unstable();
    races
}

/// Render the race-selection prompt. Lists the playable races in a
/// single comma-joined line; players type any of the names back.
fn send_race_prompt(outbound: &Outbound, world: &World) {
    let mut msg = String::from("Available races: ");
    msg.push_str(&playable_races(world).join(", "));
    msg.push_str("\r\nRace: ");
    send_prompt(outbound, msg.into_bytes());
}

/// Match a freshly-typed race against the playable list. Returns
/// the canonical SHOUTCASE form on success — used as both the
/// pretty echo and the `Characters.race` enum value at INSERT
/// time. Case-insensitive equality only; partial-match would be
/// ambiguous between e.g. ELF and `HALF_ELF`.
fn match_playable_race(world: &World, input: &str) -> Option<String> {
    let needle = input.trim().to_ascii_uppercase();
    playable_races(world).into_iter().find(|r| *r == needle)
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
    send_prompt(outbound, msg.into_bytes());
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
    send_prompt(outbound, msg.into_bytes());
}

/// Match a freshly-typed gender against the accepted list.
/// Case-insensitive equality; returns the canonical lowercase
/// form so the persisted `Characters.gender` value is consistent
/// regardless of how the player typed it.
fn match_playable_gender(input: &str) -> Option<&'static str> {
    let needle = input.trim().to_ascii_lowercase();
    PLAYABLE_GENDERS.iter().copied().find(|g| *g == needle)
}

/// Classic 3d6-per-stat roll for a brand-new character, clamped
/// at the race's per-attribute max from `Races.max_*` columns.
/// 3d6 tops out at 18 and the schema default cap is 76, so the
/// clamp is normally a no-op — but a race that lowers a specific
/// cap (e.g. low-INT brutes capped at 12) will see freshly-rolled
/// stats respect the schema authoring. Returns six stats in the
/// canonical order STR / INT / WIS / DEX / CON / CHA.
fn roll_starting_stats(race_catalog: &mud_world::RaceCatalog, race: &str) -> CoreStats {
    let roll = || -> i32 {
        (0..3)
            .map(|_| i32::try_from(rand::random_range(1u32..=6)).unwrap_or(1))
            .sum()
    };
    let cap = |stat: &str, val: i32| val.min(race_catalog.stat_cap(race, stat, i32::MAX));
    CoreStats {
        strength: cap("strength", roll()),
        intelligence: cap("intelligence", roll()),
        wisdom: cap("wisdom", roll()),
        dexterity: cap("dexterity", roll()),
        constitution: cap("constitution", roll()),
        charisma: cap("charisma", roll()),
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
    send_prompt(outbound, line.into_bytes());
}

/// Shared prompt for the `ConfirmCreate` doorway. `is_email`
/// switches the noun so the player sees "account" / "character"
/// matching the identifier they typed.
fn send_confirm_create_prompt(outbound: &Outbound, identifier: &str, is_email: bool) {
    if is_email {
        // Identical for registered and unregistered addresses.
        send_prompt(
            outbound,
            format!(
                "'{identifier}': type 'new' to create an account, or 'code' to log in \
                 with a website login code (new/code/no): "
            )
            .into_bytes(),
        );
        return;
    }
    let kind_label = "character";
    send_prompt(
        outbound,
        format!("I don't see a {kind_label} for '{identifier}'. Create a new one? (yes/no): ")
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

/// Seconds spent offline: `now - last_logout`, floored at 0. `None` (no clean
/// logout on record: first login after the column was added, or a crash)
/// yields 0 so it can never fall back to `last_login` and re-open the
/// play-time-counts-as-rest exploit.
fn offline_elapsed_secs(last_logout: Option<chrono::NaiveDateTime>, now_unix: i64) -> i64 {
    last_logout.map_or(0, |ts| {
        now_unix.saturating_sub(ts.and_utc().timestamp()).max(0)
    })
}

/// Rest / repose: XP needed to advance from `level` to the next level
/// (class-scaled, from the live `LevelTable`; the same bracket
/// `level_progress` shows). `None` when there is no next level to
/// accrue against: staff levels, the mortal cap (99), or a level the
/// table has no rows for.
#[must_use]
fn repose_next_level_xp(world: &World, class_id: Option<i32>, level: i32) -> Option<i64> {
    if !(1..mud_db::enums::MAX_MORTAL_LEVEL).contains(&level) {
        return None;
    }
    let floor = i64::from(mud_world::exp_to_reach(world, class_id, level).unwrap_or(0));
    let ceiling = i64::from(mud_world::exp_to_reach(world, class_id, level + 1)?);
    Some((ceiling - floor).max(1))
}

/// Rest / repose: accrue Repose for the elapsed offline window.
/// Returns the new pool value. Pure-fn for unit testability: no DB or
/// component writes.
///
/// `next_level_xp` is the XP bracket to the next level (see
/// [`repose_next_level_xp`]; `None` accrues nothing). Gain is
/// `hours * fill% * next_level_xp`, added to `existing_repose` and
/// capped at `cap% * next_level_xp`, all rounded down to whole XP. An
/// existing pool already above the cap (earned at a higher tier, or
/// before a level-up changed the bracket) is never reduced.
#[must_use]
fn accrue_repose(
    existing_repose: i32,
    tier: i32,
    next_level_xp: Option<i64>,
    elapsed_secs: i64,
) -> i32 {
    let (Some(next_level_xp), true) = (next_level_xp, elapsed_secs > 0) else {
        return existing_repose;
    };
    let Some((rate_bp, cap_bp)) = usize::try_from(tier)
        .ok()
        .and_then(|i| Some((REPOSE_FILL_BP_PER_HOUR.get(i)?, REPOSE_CAP_BP.get(i)?)))
    else {
        return existing_repose;
    };
    let xp = i128::from(next_level_xp.max(0));
    // Exact integer math: no float rounding at the cap boundary.
    let gained = i128::from(elapsed_secs) * i128::from(*rate_bp) * xp / (3600 * BASIS_POINTS);
    let cap = i128::from(*cap_bp) * xp / BASIS_POINTS;
    let existing = i128::from(existing_repose);
    let total = existing.saturating_add(gained).min(cap).max(existing);
    i32::try_from(total).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cur: Option<(i32, i32)>, recall: Option<(i32, i32)>) -> CharacterRow {
        CharacterRow {
            id: "c".into(),
            name: "Tester".into(),
            user_id: Some("u".into()),
            level: 1,
            hit_points: 10,
            hit_points_max: 10,
            stamina: 10,
            stamina_max: 10,
            alignment: 0,
            accuracy: 0,
            attack_power: 0,
            spell_power: 0,
            penetration_flat: 0,
            penetration_percent: 0,
            crit_chance: 5,
            evasion: 0,
            armor_rating: 0,
            damage_reduction_percent: 0,
            soak: 0,
            hardness: 0,
            ward_percent: 0,
            perception: 0,
            concealment: 0,
            resistances: serde_json::json!({}),
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
            last_logout: None,
            invis_level: 0,
            freeze_level: None,
            wimpy_threshold: 0,
            poof_in: None,
            poof_out: None,
            position: mud_db::enums::Position::Standing,
            name_approved: true,
            repose: 0,
            rest_source: mud_db::enums::RestSource::None,
            rest_tier: 0,
        }
    }

    #[test]
    fn verify_password_any_handles_bcrypt() {
        let h = bcrypt::hash("hunter2", 4).unwrap();
        assert!(verify_password_any("hunter2", &h));
        assert!(!verify_password_any("wrong", &h));
    }

    #[test]
    fn verify_password_any_handles_legacy_truncated_crypt() {
        // Mirror the FieryMUD C++ creation path: crypt(password, name)
        // truncated to MAX_PWD_LENGTH (10). The first 2 chars of the
        // name become the salt embedded at the head of the hash.
        let plaintext = "hunter2";
        #[allow(deprecated)]
        let full = pwhash::unix_crypt::hash_with("St", plaintext).unwrap();
        let stored = &full[..10];
        assert!(verify_password_any(plaintext, stored));
        assert!(!verify_password_any("wrong", stored));
    }

    #[test]
    fn verify_password_any_rejects_empty_and_short() {
        assert!(!verify_password_any("anything", ""));
        assert!(!verify_password_any("anything", "x"));
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

    // --- Rest / repose spawn + accrue tests ---

    #[test]
    fn login_room_is_the_saved_room_for_every_rest_source_and_offline_time() {
        use mud_db::enums::RestSource;
        for source in [
            RestSource::None,
            RestSource::Quit,
            RestSource::Camp,
            RestSource::Inn,
            RestSource::House,
        ] {
            let mut r = row(Some((30, 5)), Some((10, 1)));
            r.rest_source = source;
            // Even a year offline, no source routes the player to recall.
            assert_eq!(pick_starting_room(&r, Some((50, 1))), (30, 5), "{source:?}");
        }
    }

    /// L10 -> L11 bracket used by the accrual tests.
    const L10_NEXT_XP: i64 = 69_000;
    const HOUR: i64 = 3600;

    fn accrue_l10(existing: i32, tier: i32, hours_secs: i64) -> i32 {
        accrue_repose(existing, tier, Some(L10_NEXT_XP), hours_secs)
    }

    #[test]
    fn accrue_repose_tier1_four_hours_is_exactly_the_cap() {
        // 4 h * 2.5% = 10% of 69,000.
        assert_eq!(accrue_l10(0, 1, 4 * HOUR), 6_900);
    }

    #[test]
    fn accrue_repose_tier1_one_hour_is_a_quarter_of_the_cap() {
        assert_eq!(accrue_l10(0, 1, HOUR), 1_725);
    }

    #[test]
    fn accrue_repose_tier1_beyond_cap_stays_at_cap() {
        assert_eq!(accrue_l10(0, 1, 365 * 24 * HOUR), 6_900);
    }

    #[test]
    fn accrue_repose_higher_tiers_use_their_own_rate_and_cap() {
        assert_eq!(accrue_l10(0, 2, HOUR), 3_450);
        assert_eq!(accrue_l10(0, 2, 5 * HOUR), 17_250);
        assert_eq!(accrue_l10(0, 3, HOUR), 6_900);
        assert_eq!(accrue_l10(0, 3, 5 * HOUR), 34_500);
        assert_eq!(accrue_l10(0, 3, 100 * HOUR), 34_500);
    }

    #[test]
    fn accrue_repose_zero_hours_gains_nothing() {
        assert_eq!(accrue_l10(0, 1, 0), 0);
        assert_eq!(accrue_l10(123, 3, 0), 123);
    }

    #[test]
    fn accrue_repose_zero_for_negative_elapsed() {
        assert_eq!(accrue_l10(100, 2, -1), 100);
    }

    #[test]
    fn accrue_repose_zero_for_tier_zero() {
        assert_eq!(accrue_l10(0, 0, 10 * HOUR), 0);
        assert_eq!(accrue_l10(100, 0, 10 * HOUR), 100);
    }

    #[test]
    fn accrue_repose_unknown_tier_gains_nothing() {
        assert_eq!(accrue_l10(5, 4, 10 * HOUR), 5);
        assert_eq!(accrue_l10(5, -1, 10 * HOUR), 5);
    }

    #[test]
    fn accrue_repose_rounds_down_to_whole_xp() {
        // 1 s at tier 1: 69,000 * 2.5% / 3600 = 0.479 XP.
        assert_eq!(accrue_l10(0, 1, 1), 0);
        // 100 s: 47.9 XP.
        assert_eq!(accrue_l10(0, 1, 100), 47);
    }

    #[test]
    fn accrue_repose_adds_to_existing_pool_up_to_cap() {
        assert_eq!(accrue_l10(1_000, 1, HOUR), 2_725);
        assert_eq!(accrue_l10(6_000, 1, HOUR), 6_900);
    }

    #[test]
    fn accrue_repose_never_reduces_pool_above_cap() {
        assert_eq!(accrue_l10(999_999, 1, HOUR), 999_999);
        assert_eq!(accrue_l10(7_000, 1, 4 * HOUR), 7_000);
    }

    #[test]
    fn accrue_repose_without_next_level_gains_nothing() {
        assert_eq!(accrue_repose(0, 3, None, 100 * HOUR), 0);
        assert_eq!(accrue_repose(50, 3, None, 100 * HOUR), 50);
    }

    fn level_world() -> World {
        let mut world = World::new();
        let rows = [
            (10, 600_000),
            (11, 669_000),
            (99, 9_000_000),
            (100, 9_500_000),
        ]
        .into_iter()
        .map(|(level, exp_required)| mud_world::LevelRow {
            level,
            name: None,
            exp_required,
            hp_gain: 1,
            stamina_gain: 1,
            is_immortal: level >= 100,
            permissions: Vec::new(),
        })
        .collect();
        world.insert_resource(mud_world::LevelTable { rows });
        world
    }

    #[test]
    fn repose_next_level_xp_is_the_level_bracket() {
        let world = level_world();
        assert_eq!(repose_next_level_xp(&world, None, 10), Some(69_000));
    }

    #[test]
    fn repose_next_level_xp_none_at_mortal_cap_and_for_gods() {
        let world = level_world();
        assert_eq!(repose_next_level_xp(&world, None, 99), None);
        assert_eq!(repose_next_level_xp(&world, None, 104), None);
        assert_eq!(repose_next_level_xp(&world, None, 0), None);
        // No table row for level 13 / 14 at all.
        assert_eq!(repose_next_level_xp(&world, None, 12), None);
    }

    #[test]
    fn l99_and_l104_accrue_nothing_end_to_end() {
        let world = level_world();
        for level in [99, 104] {
            let next = repose_next_level_xp(&world, None, level);
            assert_eq!(accrue_repose(0, 3, next, 100 * HOUR), 0, "level {level}");
        }
    }

    #[test]
    fn l10_tier1_four_hours_end_to_end() {
        let world = level_world();
        let next = repose_next_level_xp(&world, None, 10);
        assert_eq!(accrue_repose(0, 1, next, 4 * HOUR), 6_900);
        assert_eq!(accrue_repose(0, 1, next, HOUR), 1_725);
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

    /// A world whose `RaceCatalog` holds `(race, playable)` rows.
    fn world_with_races(rows: &[(&str, bool)]) -> World {
        let mut world = World::new();
        let mut catalog = mud_world::RaceCatalog::default();
        for (race, playable) in rows {
            catalog.by_race.insert(
                (*race).to_string(),
                mud_world::RaceDef {
                    race: (*race).to_string(),
                    playable: *playable,
                    ..Default::default()
                },
            );
        }
        world.insert_resource(catalog);
        world
    }

    #[test]
    fn race_match_is_case_insensitive() {
        let world = world_with_races(&[("HUMAN", true), ("HALF_ELF", true), ("ELF", true)]);
        assert_eq!(
            match_playable_race(&world, "human").as_deref(),
            Some("HUMAN")
        );
        assert_eq!(
            match_playable_race(&world, "Half_Elf").as_deref(),
            Some("HALF_ELF")
        );
        assert_eq!(match_playable_race(&world, "ELF").as_deref(), Some("ELF"));
    }

    #[test]
    fn race_match_rejects_partial_or_unknown() {
        let world = world_with_races(&[("HUMAN", true), ("ELF", true), ("HALF_ELF", true)]);
        // No prefix matching — ELF and HALF_ELF would collide.
        assert_eq!(match_playable_race(&world, "hu"), None);
        // Not in the catalog at all.
        assert_eq!(match_playable_race(&world, "DRAGON_FIRE"), None);
    }

    #[test]
    fn race_picker_follows_the_playable_column() {
        // GNOLL is playable in the data though the old const never listed
        // it; GOLIATH was in the const but is flagged non-playable here.
        let world = world_with_races(&[
            ("HUMAN", true),
            ("GNOLL", true),
            ("GOLIATH", false),
            ("DEMON", false),
        ]);
        assert_eq!(playable_races(&world), vec!["GNOLL", "HUMAN"]);
        assert_eq!(
            match_playable_race(&world, "gnoll").as_deref(),
            Some("GNOLL")
        );
        assert_eq!(match_playable_race(&world, "GOLIATH"), None);
        assert_eq!(match_playable_race(&world, "DEMON"), None);
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        send_race_prompt(&tx, &world);
        let text = String::from_utf8_lossy(&rx.try_recv().unwrap()).into_owned();
        assert!(
            text.contains("Available races: GNOLL, HUMAN\r\nRace: "),
            "{text}"
        );
        assert!(
            !text.contains("GOLIATH") && !text.contains("DEMON"),
            "{text}"
        );
    }

    #[test]
    fn race_picker_is_empty_without_a_catalog() {
        let world = World::new();
        assert!(playable_races(&world).is_empty());
        assert_eq!(match_playable_race(&world, "HUMAN"), None);
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
    // ---- login hardening: hashing, throttle, takeover ---------------

    fn legacy_hash(plaintext: &str) -> String {
        #[allow(deprecated)]
        let full = pwhash::unix_crypt::hash_with("Li", plaintext).unwrap();
        full[..10].to_string()
    }

    #[test]
    fn verify_password_any_legacy_compare_is_case_sensitive() {
        let stored = legacy_hash("hunter2");
        assert!(verify_password_any("hunter2", &stored));
        // Password differing only in letter case.
        assert!(!verify_password_any("Hunter2", &stored));
        assert!(!verify_password_any("HUNTER2", &stored));
        // Stored hash differing only in letter case ("Li" salt has an
        // uppercase letter, so lowercasing always changes it).
        let lowered = stored.to_ascii_lowercase();
        assert_ne!(lowered, stored);
        assert!(!verify_password_any("hunter2", &lowered));
        let uppered = stored.to_ascii_uppercase();
        assert_ne!(uppered, stored);
        assert!(!verify_password_any("hunter2", &uppered));
    }

    #[test]
    fn verify_password_any_rejects_salt_only_legacy_hash() {
        // A stored value of just the salt would otherwise match any
        // password (crypt output begins with the salt).
        assert!(!verify_password_any("anything", "Li"));
        assert!(!verify_password_any("anything", "Li1234567"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_verify_and_hash_roundtrip() {
        let hashed = hash_password_blocking("hunter2".to_string()).await.unwrap();
        assert!(hashed.starts_with("$2"));
        assert!(verify_password_blocking("hunter2".into(), hashed.clone()).await);
        assert!(!verify_password_blocking("hunter3".into(), hashed).await);
        let legacy = legacy_hash("hunter2");
        assert!(verify_password_blocking("hunter2".into(), legacy.clone()).await);
        assert!(!verify_password_blocking("Hunter2".into(), legacy).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_verify_does_not_stall_current_thread_runtime() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        // Cost-12 hash (~250 ms+ per verify) -- the production cost.
        let hashed = hash_password_blocking("hunter2".to_string()).await.unwrap();
        let ticks = Arc::new(AtomicU32::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let (t2, d2) = (ticks.clone(), done.clone());
        // Stand-in for the game tick: must keep running while the
        // verification is in flight on the single-threaded runtime.
        let ticker = tokio::spawn(async move {
            while !d2.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
                t2.fetch_add(1, Ordering::SeqCst);
            }
        });
        let ok = verify_password_blocking("hunter2".into(), hashed).await;
        done.store(true, Ordering::SeqCst);
        ticker.await.unwrap();
        assert!(ok);
        assert!(
            ticks.load(Ordering::SeqCst) >= 3,
            "ticker starved during verification: {} ticks",
            ticks.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn legacy_throttle_locks_at_threshold_and_expires() {
        let mut th = LegacyLoginThrottle::default();
        let win = Duration::from_secs(900);
        let t0 = Instant::now();
        assert_eq!(th.record_failure("bob", t0, 3, win), (1, false));
        assert_eq!(th.record_failure("bob", t0, 3, win), (2, false));
        assert!(th.locked_for("bob", t0, 3, win).is_none());
        assert_eq!(th.record_failure("bob", t0, 3, win), (3, true));
        assert!(th.locked_for("bob", t0, 3, win).is_some());
        // Other names are unaffected.
        assert!(th.locked_for("alice", t0, 3, win).is_none());
        // Still locked just inside the window, free just past it.
        assert!(
            th.locked_for("bob", t0 + Duration::from_secs(899), 3, win)
                .is_some()
        );
        assert!(
            th.locked_for("bob", t0 + Duration::from_secs(901), 3, win)
                .is_none()
        );
        // A failure after expiry starts a fresh count.
        assert_eq!(
            th.record_failure("bob", t0 + Duration::from_secs(901), 3, win),
            (1, false)
        );
        // Disabled when max <= 0.
        assert!(th.locked_for("bob", t0, 0, win).is_none());
        th.clear("bob");
        assert!(th.locked_for("bob", t0, 3, win).is_none());
    }

    fn lazy_pool() -> PgPool {
        // Never connects: the paths under test don't touch the DB.
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    fn auth_world(max_attempts: i64) -> World {
        let mut world = World::new();
        let mut cfg = mud_world::RuntimeConfig::default();
        cfg.by_key.insert(
            ("security".into(), "max_login_attempts".into()),
            mud_world::ConfigValue::Int(max_attempts),
        );
        cfg.by_key.insert(
            ("security".into(), "login_timeout_minutes".into()),
            mud_world::ConfigValue::Int(15),
        );
        world.insert_resource(cfg);
        world
    }

    fn legacy_sentinel() -> (User, Option<Box<CharacterRow>>) {
        let user = User {
            id: String::new(),
            email: "Tester".into(),
            display_name: String::new(),
            role: mud_db::enums::UserRole::Player,
            failed_login_attempts: 0,
            locked_until: None,
            account_wealth: 0,
        };
        let mut c = row(None, None);
        c.user_id = None;
        (user, Some(Box::new(c)))
    }

    fn park_at_password(router: &mut ConnRouter, conn: ConnId, hash: &str) {
        let (user, preselected) = legacy_sentinel();
        router.login.get_mut(&conn).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected,
            game_hash: hash.to_string(),
        };
    }

    /// Type one password at a parked connection and drive the
    /// off-thread job to completion (the main loop's select arm).
    async fn attempt(
        router: &mut ConnRouter,
        rx: &mut UnboundedReceiver<AuthDone>,
        world: &mut World,
        pool: &PgPool,
        conn: ConnId,
        pw: &str,
    ) {
        router.on_line(conn, pw.to_string(), pool, world).await;
        if let Ok(done) = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
            router.on_auth_done(done.unwrap(), pool, world).await;
        }
    }

    /// First output frame after `on_connect_with` is the banner.
    fn banner_for(world: &World, handle: mud_net::OutputHandle) -> (String, mud_net::OutputHandle) {
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(8);
        router.on_connect_with(1, tx, None, handle.clone(), world);
        let first = orx.try_recv().expect("banner frame");
        (String::from_utf8(first).unwrap(), handle)
    }

    fn messages(rows: &[(&str, &str, &str)]) -> mud_world::LoginMessages {
        mud_world::LoginMessages {
            by_key: rows
                .iter()
                .map(|(stage, variant, text)| {
                    (
                        ((*stage).to_string(), (*variant).to_string()),
                        (*text).to_string(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn banner_fallback_is_ascii_for_plain_clients_and_blocks_for_utf8() {
        let world = World::new();
        let (plain, _) = banner_for(&world, mud_net::OutputHandle::new());
        assert!(plain.is_ascii(), "plain client got non-ASCII: {plain:?}");
        assert!(plain.contains("forged in fire"));

        let utf8 = mud_net::OutputHandle::new();
        utf8.apply_mtts(1 | 4 | 8);
        let (fancy, _) = banner_for(&world, utf8);
        assert!(fancy.contains('\u{2588}'), "UTF-8 client lost the logo");
    }

    /// Frames queued so far, as raw byte vectors.
    fn frames(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn identifier_prompt_at_connect_ends_with_iac_eor() {
        let world = World::new();
        let mut router = ConnRouter::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        router.on_connect_with(1, tx, None, mud_net::OutputHandle::new(), &world);
        let got = frames(&mut rx);
        // banner, prompt text, then the marker as its own frame.
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[1], b"Email or character name: ");
        assert_eq!(got[2], [0xFF, 0xEF]);
    }

    #[test]
    fn reprompts_and_selection_prompts_end_with_iac_eor() {
        let world = World::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut ctx = LoginCtx {
            outbound: tx,
            stage: Stage::AwaitingIdentifier,
            failed_attempts: 0,
            peer: None,
            tls: false,
            notice_shown: false,
        };
        reprompt_identifier(&mut ctx, &world);
        send_race_prompt(&ctx.outbound, &world);
        send_gender_prompt(&ctx.outbound);
        send_confirm_create_prompt(&ctx.outbound, "Bob", false);
        send_login_prompt(
            &ctx.outbound,
            &world,
            "PASSWORD_PROMPT",
            PASSWORD_PROMPT_FALLBACK,
        );
        let got = frames(&mut rx);
        assert_eq!(got.len(), 10, "{got:?}");
        for pair in got.chunks(2) {
            assert!(!pair[0].ends_with(&[0xFF, 0xEF]), "marker glued to text");
            assert_eq!(pair[1], [0xFF, 0xEF]);
        }
        assert_eq!(got[8], b"Password: ");
    }

    #[test]
    fn banner_prefers_an_ascii_variant_row_for_plain_clients() {
        let mut world = World::new();
        world.insert_resource(messages(&[
            ("WELCOME_BANNER", "default", "FANCY \u{2588}\u{2588}"),
            ("WELCOME_BANNER", "ascii", "PLAIN ##"),
        ]));
        let (plain, _) = banner_for(&world, mud_net::OutputHandle::new());
        assert_eq!(plain, "PLAIN ##");
        let utf8 = mud_net::OutputHandle::new();
        utf8.mark_utf8();
        let (fancy, _) = banner_for(&world, utf8);
        assert_eq!(fancy, "FANCY \u{2588}\u{2588}");
    }

    #[test]
    fn banner_default_row_is_transliterated_for_plain_clients_when_no_variant() {
        let mut world = World::new();
        world.insert_resource(messages(&[(
            "WELCOME_BANNER",
            "default",
            "<c196>\u{2588}\u{2557}</> \u{2014} hi\n",
        )]));
        let (text, handle) = banner_for(&world, mud_net::OutputHandle::new());
        // The router hands over the data-driven row; the connection
        // writer encodes it for the client.
        assert!(text.contains('\u{2588}'));
        let wire = String::from_utf8(handle.encode(text.into_bytes())).unwrap();
        assert!(wire.is_ascii(), "{wire:?}");
        assert!(wire.contains("#+") && wire.contains("--"), "{wire:?}");
        // 256-colour tag downgraded to 16 colours.
        assert!(wire.contains("\x1b[91m"), "{wire:?}");
        assert!(
            wire.ends_with("\r\n") || wire.contains("hi\r\n"),
            "{wire:?}"
        );
    }

    #[test]
    fn plain_telnet_notice_is_line_terminated_so_the_prompt_starts_a_new_line() {
        // DB row as seeded: one paragraph, no trailing newline (#49).
        let mut world = World::new();
        world.insert_resource(messages(&[(
            "PLAIN_TELNET_NOTICE",
            "default",
            "Not encrypted. Use TLS port {tls_port}.",
        )]));
        let notice = String::from_utf8(plain_telnet_notice_bytes(&world)).unwrap();
        assert_eq!(notice, "Not encrypted. Use TLS port 4443.\r\n");

        // Compiled fallback and an already-terminated row are untouched.
        let fallback = plain_telnet_notice_bytes(&World::new());
        assert!(fallback.ends_with(b"\r\n"));
        world.insert_resource(messages(&[("PLAIN_TELNET_NOTICE", "default", "Hi\n")]));
        assert_eq!(plain_telnet_notice_bytes(&world), b"Hi\n");

        // The prompt itself stays unterminated, like every other prompt.
        let prompt = login_message_bytes(&world, "PASSWORD_PROMPT", PASSWORD_PROMPT_FALLBACK);
        assert_eq!(prompt, b"Password: ");
        let ident = login_message_bytes(&world, "EMAIL_PROMPT", IDENT_PROMPT_FALLBACK);
        assert!(ident.ends_with(b": "));
    }

    fn drain(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> String {
        let mut s = String::new();
        while let Ok(b) = rx.try_recv() {
            s.push_str(&String::from_utf8_lossy(&b));
        }
        s
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlinked_legacy_character_is_locked_out_after_n_failures() {
        let mut world = auth_world(3);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let hash = legacy_hash("hunter2");
        // Account threshold is 3 but the legacy one is separate (default
        // 10), so attempts 3..9 must NOT lock the character.
        // Fresh connection per attempt: the lock must follow the
        // character, not the socket.
        let legacy_max = ConnId::try_from(DEFAULT_LEGACY_MAX_LOGIN_ATTEMPTS).unwrap();
        for conn in 1..=legacy_max {
            let (tx, mut orx) = tokio::sync::mpsc::channel(64);
            router.on_connect(conn, tx, None, &world);
            park_at_password(&mut router, conn, &hash);
            attempt(&mut router, &mut rx, &mut world, &pool, conn, "wrong").await;
            let out = drain(&mut orx);
            assert!(out.contains("Invalid credentials"), "attempt {conn}: {out}");
            if conn < legacy_max {
                assert!(
                    !out.contains("locked"),
                    "attempt {conn} must not lock: {out}"
                );
            } else {
                assert!(out.contains("locked"), "last failure should lock: {out}");
            }
        }
        // Correct password is now refused without ever verifying.
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        let next = legacy_max + 1;
        router.on_connect(next, tx, None, &world);
        park_at_password(&mut router, next, &hash);
        router
            .on_line(next, "hunter2".into(), &pool, &mut world)
            .await;
        let out = drain(&mut orx);
        assert!(out.contains("temporarily locked"), "{out}");
        assert!(matches!(
            router.login.get(&next).unwrap().stage,
            Stage::AwaitingIdentifier
        ));
        assert!(
            rx.try_recv().is_err(),
            "no verification job should be queued"
        );
        // Window expiry releases the lock.
        let key = LegacyLoginThrottle::key("Tester");
        let later = Instant::now() + lock_window(15) + Duration::from_secs(1);
        assert!(
            router
                .legacy_throttle
                .locked_for(
                    &key,
                    later,
                    DEFAULT_LEGACY_MAX_LOGIN_ATTEMPTS,
                    lock_window(15)
                )
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connection_is_dropped_after_too_many_wrong_passwords() {
        // Throttle disabled (max 0) so only the per-connection cap acts.
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        thread_local! {
            static CLOSED: std::cell::RefCell<Vec<ConnId>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        router.close_conn = |c| {
            CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let mut rx = router.take_auth_rx().unwrap();
        let hash = legacy_hash("hunter2");
        let (tx, mut orx) = tokio::sync::mpsc::channel(256);
        router.on_connect(1, tx, None, &world);
        for i in 1..=MAX_FAILED_PASSWORDS_PER_CONN {
            assert!(
                router.login.contains_key(&1),
                "dropped early at attempt {i}"
            );
            park_at_password(&mut router, 1, &hash);
            attempt(&mut router, &mut rx, &mut world, &pool, 1, "wrong").await;
        }
        assert!(!router.login.contains_key(&1));
        assert_eq!(router.live_connections(), 0);
        assert!(drain(&mut orx).contains("Too many failed login attempts"));
        // ...and the socket was actually asked to close.
        assert_eq!(CLOSED.with(|v| v.borrow().clone()), vec![1]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn other_connection_input_is_processed_while_verification_in_flight() {
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let (tx_a, _orx_a) = tokio::sync::mpsc::channel(64);
        let (tx_b, mut orx_b) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx_a, None, &world);
        router.on_connect(2, tx_b, None, &world);
        drain(&mut orx_b);
        // A: linked account with a real bcrypt hash.
        let user = User {
            id: "u1".into(),
            email: "a@example.com".into(),
            display_name: "a".into(),
            role: mud_db::enums::UserRole::Player,
            failed_login_attempts: 0,
            locked_until: None,
            account_wealth: 0,
        };
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: None,
            game_hash: bcrypt::hash("pw-a-long", 4).unwrap(),
        };
        router.login.get_mut(&2).unwrap().stage = Stage::ConfirmCreate {
            identifier: "Newbie".into(),
            is_email: false,
        };
        router
            .on_line(1, "pw-a-long".into(), &pool, &mut world)
            .await;
        // A is parked, job queued but not resolved yet.
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::Authenticating
        ));
        assert!(rx.try_recv().is_err());
        // B is served immediately, before A's result is applied.
        router.on_line(2, "no".into(), &pool, &mut world).await;
        assert!(drain(&mut orx_b).contains("existing email or character name"));
        // A's input while authenticating is swallowed, stage preserved.
        router.on_line(1, "junk".into(), &pool, &mut world).await;
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::Authenticating
        ));
        // The job resolves with the right answer.
        let done = rx.recv().await.unwrap();
        match done.kind {
            AuthDoneKind::Password {
                ok, migration_hash, ..
            } => {
                assert!(ok);
                assert!(migration_hash.is_none());
            }
            _ => panic!("wrong job kind"),
        }
    }

    /// Issue #2: a NAWS report of 212 columns reaches the player and is the
    /// width prose wraps at, with or without a stale `columns 250` saved.
    #[test]
    fn naws_212_sets_the_wrap_width_even_over_a_stale_columns_setting() {
        let mut world = World::new();
        let mut router = ConnRouter::new();
        let entity = world.spawn(Player).id();
        router.playing.insert(1, entity);
        router.on_window_size(1, 212, 50, &mut world);
        assert_eq!(world.get::<mud_world::ClientWidth>(entity).unwrap().0, 212);
        assert_eq!(crate::layout::wrap_width(&world, entity), 212);

        let mut vars = mud_world::ScriptVars::default();
        vars.0
            .insert(mud_world::PREF_COLUMNS_KEY.to_string(), "250".to_string());
        world.entity_mut(entity).insert(vars);
        assert_eq!(crate::layout::wrap_width(&world, entity), 212);
        // A narrower saved preference still wins; a resize is followed.
        world
            .get_mut::<mud_world::ScriptVars>(entity)
            .unwrap()
            .0
            .insert(mud_world::PREF_COLUMNS_KEY.to_string(), "160".to_string());
        assert_eq!(crate::layout::wrap_width(&world, entity), 160);
        router.on_window_size(1, 100, 50, &mut world);
        assert_eq!(crate::layout::wrap_width(&world, entity), 100);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn second_login_of_same_character_takes_over_single_entity() {
        let mut world = World::new();
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        thread_local! {
            static CLOSED: std::cell::RefCell<Vec<ConnId>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        router.close_conn = |c| {
            CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let (tx1, mut rx1) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        // First connection is already playing the character.
        let entity = world
            .spawn((
                Player,
                Account {
                    user_id: "u".into(),
                    character_id: "c".into(),
                    role: mud_db::enums::UserRole::Player,
                    account_role: mud_db::enums::UserRole::Player,
                    perms: vec![],
                },
                Connection(tx1),
            ))
            .id();
        router.playing.insert(1, entity);
        // Second connection authenticates as the same character.
        router.on_connect(2, tx2, None, &world);
        drain(&mut rx2);
        assert!(router.try_takeover(&mut world, 2, "c"));

        // Exactly one entity for that character; it is the original.
        let owners: Vec<Entity> = world
            .query_filtered::<(Entity, &Account), With<Player>>()
            .iter(&world)
            .filter(|(_, a)| a.character_id == "c")
            .map(|(e, _)| e)
            .collect();
        assert_eq!(owners, vec![entity]);
        // First connection: told why, detached, and its channel is closed.
        assert!(drain(&mut rx1).contains("taken over"));
        assert!(matches!(
            rx1.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
        assert!(!router.playing.contains_key(&1));
        // ...and the old socket was actually asked to close (only it).
        assert_eq!(CLOSED.with(|v| v.borrow().clone()), vec![1]);
        // Second connection owns the entity and was told so.
        assert_eq!(router.playing.get(&2), Some(&entity));
        assert_eq!(router.find_conn(entity), Some(2));
        assert!(!router.login.contains_key(&2));
        assert!(drain(&mut rx2).contains("You take over your own body, already in use!"));
        world
            .get::<Connection>(entity)
            .unwrap()
            .0
            .try_send(b"hi".to_vec())
            .unwrap();
        assert_eq!(rx2.try_recv().unwrap(), b"hi");
        // The old socket's late disconnect must not save/despawn the entity.
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get_entity(entity).is_ok());
        // A different character is not a takeover.
        let (tx3, _rx3) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        router.on_connect(3, tx3, None, &world);
        assert!(!router.try_takeover(&mut world, 3, "other"));
    }

    /// What `complete_login_inner` does for the new connection right after
    /// a takeover: look, item frames, prompt.
    fn finish_takeover(world: &mut World, entity: Entity) {
        commands::info::cmd_look(world, entity, "");
        commands::refresh_player_items_gmcp(world, entity);
        commands::send_prompt(world, entity);
    }

    fn gmcp_packages(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> Vec<String> {
        let bytes = commands::gmcp_tests::drain_bytes(rx);
        commands::gmcp_tests::frames(&bytes)
            .into_iter()
            .map(|(pkg, _)| pkg)
            .collect()
    }

    /// A takeover must not inherit the old client's change-gated GMCP
    /// cache: the new client gets the whole package set on the next prompt
    /// even though nothing changed since the old connection saw it.
    #[tokio::test(flavor = "current_thread")]
    async fn takeover_of_a_linkdead_character_resends_all_gmcp() {
        let (mut fx, p, mut old_rx) = commands::gmcp_tests::world_with_skills();
        let mut router = ConnRouter::new();
        router.close_conn = |_| true;
        // The old session saw everything.
        commands::send_prompt(&mut fx.world, p);
        gmcp_packages(&mut old_rx);
        commands::send_prompt(&mut fx.world, p);
        assert!(
            !gmcp_packages(&mut old_rx).contains(&"Char.Name".to_string()),
            "change-gated frames are not repeated"
        );
        // Socket dropped mid-fight: no connection, still in the world.
        fx.world
            .entity_mut(p)
            .remove::<Connection>()
            .insert(commands::Linkdead { since_tick: 0 });

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
        router.on_connect(2, tx, None, &fx.world);
        assert!(router.try_takeover(&mut fx.world, 2, "c-Kicker"));
        finish_takeover(&mut fx.world, p);

        let got = gmcp_packages(&mut rx);
        for pkg in [
            "Char.Name",
            "Char.StatusVars",
            "Char.Status",
            "Char.Skills",
            "Char.Vitals",
            "Char.Effects",
            "Room.Info",
        ] {
            assert!(got.contains(&pkg.to_string()), "{pkg} missing: {got:?}");
        }
    }

    /// Mudlet sends Core.Hello / Core.Supports.Set on the login screen,
    /// before the connection is `playing`; they cannot clear the cache
    /// then, so binding to the entity must.
    #[tokio::test(flavor = "current_thread")]
    async fn core_hello_on_the_login_screen_then_login_gets_a_full_send() {
        let (mut fx, p, mut old_rx) = commands::gmcp_tests::world_with_skills();
        let mut router = ConnRouter::new();
        router.close_conn = |_| true;
        router.playing.insert(1, p);
        commands::send_prompt(&mut fx.world, p);
        gmcp_packages(&mut old_rx);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
        router.on_connect(2, tx, None, &fx.world);
        router
            .on_gmcp(2, "Core.Hello", r#"{"client":"Mudlet"}"#, &mut fx.world)
            .await;
        router
            .on_gmcp(2, "Core.Supports.Set", r#"["Char 1"]"#, &mut fx.world)
            .await;
        gmcp_packages(&mut rx);
        assert!(router.try_takeover(&mut fx.world, 2, "c-Kicker"));
        finish_takeover(&mut fx.world, p);

        let got = gmcp_packages(&mut rx);
        for pkg in ["Char.Name", "Char.Skills", "Char.Vitals", "Room.Info"] {
            assert!(got.contains(&pkg.to_string()), "{pkg} missing: {got:?}");
        }
    }

    /// A connected, playing character standing in `room` on connection `conn`.
    fn playing_in(
        router: &mut ConnRouter,
        world: &mut World,
        room: Entity,
        conn: ConnId,
        name: &str,
    ) -> (Entity, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let entity = world
            .spawn((
                Player,
                Named {
                    name: name.to_string(),
                },
                Located(room),
                Health { hp: 10, max: 10 },
                Account {
                    user_id: format!("u-{name}"),
                    character_id: format!("c-{name}"),
                    role: mud_db::enums::UserRole::Player,
                    account_role: mud_db::enums::UserRole::Player,
                    perms: vec![],
                },
                Connection(tx),
            ))
            .id();
        router.playing.insert(conn, entity);
        (entity, rx)
    }

    thread_local! {
        static QUIT_CLOSED: std::cell::RefCell<Vec<ConnId>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quit_goes_through_disconnect_closes_socket_and_despawns() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        let pool = failing_pool();
        world.insert_resource(SaveCoordinator::default());
        let mut router = ConnRouter::new();
        QUIT_CLOSED.with(|v| v.borrow_mut().clear());
        router.close_conn = |c| {
            QUIT_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let room = world.spawn(mud_world::Room).id();
        let (leaver, mut rx_leaver) = playing_in(&mut router, &mut world, room, 1, "Leaver");
        let (watcher, mut rx_watcher) = playing_in(&mut router, &mut world, room, 2, "Watcher");

        router.on_line(1, "quit".into(), &pool, &mut world).await;

        // Same teardown as a dropped link: saved (the failed write is handed
        // to the background retry), despawned, detached, socket closed.
        assert!(world.get_entity(leaver).is_err());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 1);
        assert!(!router.playing.contains_key(&1));
        assert_eq!(QUIT_CLOSED.with(|v| v.borrow().clone()), vec![1]);
        let out = drain(&mut rx_leaver);
        assert!(out.contains("Goodbye, friend.  Come back soon!"), "{out}");
        // Bystanders get the legacy departure line, not the link-drop one.
        let seen = drain(&mut rx_watcher);
        assert!(seen.contains("Leaver has left the game."), "{seen}");
        assert!(!seen.contains("fades from view"), "{seen}");
        // Everyone else is untouched.
        assert!(world.get_entity(watcher).is_ok());
        assert_eq!(router.playing.get(&2), Some(&watcher));
        // The late Disconnected event from the closed socket is a no-op.
        router.on_disconnect(&mut world, 1, &pool);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refused_quit_keeps_the_connection_open() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        QUIT_CLOSED.with(|v| v.borrow_mut().clear());
        router.close_conn = |c| {
            QUIT_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let room = world.spawn(mud_world::Room).id();
        let (fighter, mut rx) = playing_in(&mut router, &mut world, room, 1, "Fighter");
        let foe = world.spawn((mud_world::Mob, Located(room))).id();
        world.entity_mut(fighter).insert(mud_world::Fighting(foe));

        router.on_line(1, "quit".into(), &pool, &mut world).await;

        assert!(world.get_entity(fighter).is_ok());
        assert_eq!(router.playing.get(&1), Some(&fighter));
        assert!(QUIT_CLOSED.with(|v| v.borrow().is_empty()));
        assert!(drain(&mut rx).contains("No way!  You're fighting for your life!"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rent_at_a_receptionist_takes_the_quit_save_and_disconnect_path() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        let mut protos = mud_world::MobPrototypes::default();
        protos.by_key.insert(
            (1, 5),
            crate::commands::test_support::mob_proto(
                1,
                5,
                mud_db::enums::MobProfession::Receptionist,
            ),
        );
        world.insert_resource(protos);
        let pool = failing_pool();
        world.insert_resource(SaveCoordinator::default());
        let mut router = ConnRouter::new();
        QUIT_CLOSED.with(|v| v.borrow_mut().clear());
        router.close_conn = |c| {
            QUIT_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let room = world.spawn(mud_world::Room).id();
        world.spawn((
            mud_world::Mob,
            mud_world::WorldKey { zone: 1, id: 5 },
            Located(room),
            mud_world::Named {
                name: "the receptionist".into(),
            },
        ));
        let (guest, mut rx) = playing_in(&mut router, &mut world, room, 1, "Guest");

        router.on_line(1, "rent".into(), &pool, &mut world).await;

        // Identical teardown to `quit`: saved (failed write handed to the
        // background retry), despawned, detached, socket closed.
        assert!(world.get_entity(guest).is_err());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 1);
        assert!(!router.playing.contains_key(&1));
        assert_eq!(QUIT_CLOSED.with(|v| v.borrow().clone()), vec![1]);
        let out = drain(&mut rx);
        assert!(out.contains("private chamber"), "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rent_away_from_a_receptionist_keeps_the_connection_open() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        QUIT_CLOSED.with(|v| v.borrow_mut().clear());
        router.close_conn = |c| {
            QUIT_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let room = world.spawn(mud_world::Room).id();
        let (guest, mut rx) = playing_in(&mut router, &mut world, room, 1, "Guest");

        router.on_line(1, "rent".into(), &pool, &mut world).await;

        assert!(world.get_entity(guest).is_ok());
        assert_eq!(router.playing.get(&1), Some(&guest));
        assert!(QUIT_CLOSED.with(|v| v.borrow().is_empty()));
        assert!(drain(&mut rx).contains("nothing to rent"));
    }

    // ---- linkdead (dropped link mid-fight), quit / camp refusals, camp logout ----

    /// A connected fighter in `room` swinging at a fresh mob.
    fn fighter_in(
        router: &mut ConnRouter,
        world: &mut World,
        room: Entity,
        conn: ConnId,
    ) -> (Entity, Entity, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        let (fighter, rx) = playing_in(router, world, room, conn, "Fighter");
        let foe = world
            .spawn((mud_world::Mob, Located(room), Health { hp: 50, max: 50 }))
            .id();
        world.entity_mut(fighter).insert(mud_world::Fighting(foe));
        (fighter, foe, rx)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_link_mid_fight_leaves_the_character_fighting_in_the_world() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(7));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (fighter, foe, _rx) = fighter_in(&mut router, &mut world, room, 1);
        let (_watcher, mut rx_watcher) = playing_in(&mut router, &mut world, room, 2, "Watcher");

        router.on_disconnect(&mut world, 1, &pool);

        // Still in the world, still fighting, no socket, not saved/despawned.
        assert!(world.get_entity(fighter).is_ok());
        assert!(world.get::<Connection>(fighter).is_none());
        assert_eq!(world.get::<mud_world::Fighting>(fighter).unwrap().0, foe);
        assert_eq!(
            world.get::<commands::Linkdead>(fighter).unwrap().since_tick,
            7
        );
        assert!(!router.playing.contains_key(&1));
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 0);
        let seen = drain(&mut rx_watcher);
        assert!(seen.contains("Fighter has lost their link."), "{seen}");
        // Output to a socketless character is a silent no-op.
        commands::send_to(&world, fighter, "nobody hears this\r\n");
        // The autosave / shutdown save still covers it.
        assert!(router.online_entities(&mut world).contains(&fighter));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn linkdeath_and_reconnect_drop_the_queued_input() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(7));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        router.close_conn = |_| true;
        let room = world.spawn(mud_world::Room).id();
        let (fighter, _foe, _rx) = fighter_in(&mut router, &mut world, room, 1);
        world.entity_mut(fighter).insert(Account {
            user_id: "u".into(),
            character_id: "c-Fighter".into(),
            role: mud_db::enums::UserRole::Player,
            account_role: mud_db::enums::UserRole::Player,
            perms: vec![],
        });
        commands::input_queue::seed_for_test(&mut world, fighter, &["cast 'fireball' orc"]);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get::<commands::Linkdead>(fighter).is_some());
        assert_eq!(commands::input_queue::queued_len(&world, fighter), 0);

        // Lines that slipped in while linkdead are gone on reconnect too.
        commands::input_queue::seed_for_test(&mut world, fighter, &["get all corpse"]);
        let (tx, _rx2) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        router.on_connect(2, tx, None, &world);
        assert!(router.try_takeover(&mut world, 2, "c-Fighter"));
        assert_eq!(commands::input_queue::queued_len(&world, fighter), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stunned_player_dropping_link_stays_linkdead() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (victim, foe, _rx) = fighter_in(&mut router, &mut world, room, 1);
        // Combat clears a stunned player's own target; the mob still swings.
        world.entity_mut(victim).remove::<mud_world::Fighting>();
        world.entity_mut(foe).insert(mud_world::Fighting(victim));

        router.on_disconnect(&mut world, 1, &pool);

        assert!(world.get::<commands::Linkdead>(victim).is_some());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn linkdead_player_still_attacked_by_another_holds_the_timer() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (player, target_a, _rx) = fighter_in(&mut router, &mut world, room, 1);
        let attacker_b = world
            .spawn((mud_world::Mob, Located(room), Health { hp: 50, max: 50 }))
            .id();
        router.on_disconnect(&mut world, 1, &pool);
        // Target A dies (player's own Fighting cleared, no retarget); B keeps
        // swinging at the player.
        world.despawn(target_a);
        world.entity_mut(player).remove::<mud_world::Fighting>();
        world
            .entity_mut(attacker_b)
            .insert(mud_world::Fighting(player));

        world.insert_resource(crate::TickCount(LINKDEAD_TIMEOUT_TICKS + 5));
        ConnRouter::drain_linkdead(&mut world, &pool);
        assert!(world.get_entity(player).is_ok(), "held while B swings");
        assert_eq!(
            world.get::<commands::Linkdead>(player).unwrap().since_tick,
            LINKDEAD_TIMEOUT_TICKS + 5
        );

        // B stops: after the timeout from the last round, the player goes.
        world.entity_mut(attacker_b).remove::<mud_world::Fighting>();
        world.insert_resource(crate::TickCount(2 * LINKDEAD_TIMEOUT_TICKS + 5));
        ConnRouter::drain_linkdead(&mut world, &pool);
        assert!(world.get_entity(player).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_link_out_of_combat_still_saves_and_despawns() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (idler, _rx) = playing_in(&mut router, &mut world, room, 1, "Idler");

        router.on_disconnect(&mut world, 1, &pool);

        assert!(world.get_entity(idler).is_err());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 1);
        assert!(!router.playing.contains_key(&1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn relogging_a_linkdead_character_rebinds_the_same_entity_without_the_database() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = enter_game_room(&mut world);
        let (fighter, foe, _rx) = fighter_in(&mut router, &mut world, room, 1);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get::<commands::Linkdead>(fighter).is_some());

        // The player comes back on connection 2. The pool is unreachable, so
        // any attempt to reload the character from the database would refuse
        // the login instead of re-binding.
        let (tx2, mut rx2) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        router.on_connect(2, tx2, None, &world);
        drain(&mut rx2);
        let mut char_row = row(None, None);
        char_row.id = "c-Fighter".into();
        router
            .complete_login_inner(2, &mut world, &pool, linked_user(), char_row, false)
            .await;

        assert_eq!(router.playing.get(&2), Some(&fighter));
        assert!(!router.login.contains_key(&2));
        assert!(world.get::<commands::Linkdead>(fighter).is_none());
        assert!(world.get::<Connection>(fighter).is_some());
        assert_eq!(world.get::<mud_world::Fighting>(fighter).unwrap().0, foe);
        // Exactly one copy of the character, no relog wait, nothing saved.
        let owners = world
            .query_filtered::<&Account, With<Player>>()
            .iter(&world)
            .filter(|a| a.character_id == "c-Fighter")
            .count();
        assert_eq!(owners, 1);
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 0);
        let out = drain(&mut rx2);
        assert!(out.contains("Reconnecting."), "{out}");
        // Legacy `perform_dupe_check`: reconnecting is followed by a look.
        let reconnecting = out.find("Reconnecting.").unwrap();
        let hall = out.find("The Grand Hall").expect(&out);
        let info = out.find("Room.Info").expect(&out);
        assert!(reconnecting < info && info < hall, "{out}");
        // One prompt for the takeover, not a second from the next flush.
        commands::flush_prompts(&mut world);
        assert_eq!(drain(&mut rx2), "", "no repeated prompt after login");
    }

    /// World resources `look` needs, plus a named, described room.
    fn enter_game_room(world: &mut World) -> Entity {
        world.insert_resource(mud_script::LuaHost::default());
        world.insert_resource(WorldKeyIndex::default());
        world.insert_resource(mud_world::WeatherCatalog::default());
        world.insert_resource(mud_world::AbilityCatalog::default());
        world.insert_resource(mud_world::EffectCatalog::default());
        world.insert_resource(mud_world::RaceCatalog::default());
        world.insert_resource(mud_world::RuntimeConfig::default());
        world.init_resource::<mud_world::MudClock>();
        world.insert_resource(commands::PromptState::default());
        world
            .spawn((
                mud_world::Room,
                Named {
                    name: "The Grand Hall".into(),
                },
                mud_world::Description("Banners hang from the rafters.".into()),
                mud_world::Exits::default(),
            ))
            .id()
    }

    fn enter_game_player(
        world: &mut World,
        room: Entity,
        name: &str,
    ) -> (Entity, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        let e = world
            .spawn((
                Player,
                mud_world::Online,
                Named { name: name.into() },
                Located(room),
                Connection(tx),
                Health { hp: 10, max: 10 },
                Account {
                    user_id: format!("u-{name}"),
                    character_id: format!("c-{name}"),
                    role: mud_db::enums::UserRole::Player,
                    account_role: mud_db::enums::UserRole::Player,
                    perms: vec![],
                },
                Profile {
                    level: 20,
                    class_id: None,
                    race: "Human".into(),
                    experience: 0,
                    gender: "neutral".into(),
                },
            ))
            .id();
        (e, rx)
    }

    #[test]
    fn entering_the_game_shows_welcome_then_room_look() {
        let mut world = World::new();
        let room = enter_game_room(&mut world);
        let (me, mut rx) = enter_game_player(&mut world, room, "Tester");

        show_enter_game(&mut world, me, "Tester", false, false);

        let out = drain(&mut rx);
        let welcome = out.find("Welcome, Tester.").expect(&out);
        let info = out.find("Room.Info").expect(&out);
        // The room text is the last mention (Room.Info JSON names it too).
        let title = out.rfind("The Grand Hall").expect(&out);
        let desc = out.rfind("Banners hang").expect(&out);
        // Welcome before the look; GMCP Room.Info ahead of the room text.
        assert!(welcome < info && info < title && title < desc, "{out}");
        assert!(!out.contains("You appear in"), "{out}");
        assert!(!out.contains("Try:"), "returning character: {out}");
    }

    #[test]
    fn unread_mail_notice_follows_the_look() {
        let mut world = World::new();
        let room = enter_game_room(&mut world);
        let (me, mut rx) = enter_game_player(&mut world, room, "Tester");

        show_enter_game(&mut world, me, "Tester", false, true);

        let out = drain(&mut rx);
        let desc = out.rfind("Banners hang").expect(&out);
        let notice = out.find("You have mail waiting.").expect(&out);
        assert!(desc < notice, "{out}");
        assert_eq!(out.matches("You have mail waiting.").count(), 1, "{out}");
    }

    #[test]
    fn no_mail_notice_without_unread_mail() {
        let mut world = World::new();
        let room = enter_game_room(&mut world);
        let (me, mut rx) = enter_game_player(&mut world, room, "Tester");

        show_enter_game(&mut world, me, "Tester", false, false);

        let out = drain(&mut rx);
        assert!(!out.contains("mail waiting"), "{out}");
    }

    #[test]
    fn first_login_hint_comes_before_the_look() {
        let mut world = World::new();
        let room = enter_game_room(&mut world);
        let (me, mut rx) = enter_game_player(&mut world, room, "Tester");

        show_enter_game(&mut world, me, "Tester", true, false);

        let out = drain(&mut rx);
        let hint = out.find("Try:").expect(&out);
        let title = out.find("The Grand Hall").expect(&out);
        assert!(
            out.find("Welcome, Tester.").unwrap() < hint && hint < title,
            "{out}"
        );
    }

    #[test]
    fn login_look_ignores_room_command_triggers_and_aliases() {
        use mud_world::{TriggerAttach, TriggerCatalog, TriggerDef, TriggerEvent};
        let mut world = World::new();
        let room = enter_game_room(&mut world);
        // A mob in the room whose COMMAND trigger swallows every command.
        let mut catalog = TriggerCatalog::default();
        catalog.by_key.insert(
            (99, 1),
            TriggerDef {
                zone_id: 99,
                id: 1,
                name: "swallow".to_string(),
                attach_type: TriggerAttach::Mob,
                commands: "return false".to_string(),
                flags: vec![TriggerEvent::Command],
                arg_list: vec![],
                num_args: 0,
            },
        );
        world.insert_resource(catalog);
        world.insert_resource(mud_script::LuaHost::new());
        world.spawn((
            mud_world::Mob,
            Located(room),
            mud_world::AttachedTriggers(vec![(99, 1)]),
        ));
        let (me, mut rx) = enter_game_player(&mut world, room, "Tester");
        world.entity_mut(me).insert(mud_world::Aliases {
            entries: vec![("look".into(), "say aliased".into())],
        });

        // Control: a typed `look` is eaten by the trigger.
        commands::dispatch(&mut world, me, "look");
        let typed = drain(&mut rx);
        assert!(!typed.contains("Banners hang"), "control: {typed}");

        show_enter_game(&mut world, me, "Tester", false, false);
        let out = drain(&mut rx);
        assert!(out.contains("Banners hang"), "look was swallowed: {out}");
        assert!(!out.contains("aliased"), "alias expanded at login: {out}");
    }

    #[test]
    fn room_sees_the_arrival_message_once_and_after_the_welcome() {
        let mut world = World::new();
        let room = enter_game_room(&mut world);
        let (me, mut my_rx) = enter_game_player(&mut world, room, "Tester");
        let (_watcher, mut watcher_rx) = enter_game_player(&mut world, room, "Watcher");

        show_enter_game(&mut world, me, "Tester", false, false);

        let seen = drain(&mut watcher_rx);
        assert_eq!(
            seen.matches("Tester has entered the game.").count(),
            1,
            "{seen}"
        );
        let mine = drain(&mut my_rx);
        assert!(!mine.contains("has entered the game"), "{mine}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_linkdead_removes_ghosts_and_expired_but_not_active_fighters() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (fighting, _foe, _rx1) = fighter_in(&mut router, &mut world, room, 1);
        let (dead, _foe2, _rx2) = fighter_in(&mut router, &mut world, room, 2);
        let (resting, _foe3, _rx3) = fighter_in(&mut router, &mut world, room, 3);
        // Separate characters: retirements of one character queue on its
        // single save turn (see `retire_player`), which is not under test.
        for (e, cid) in [(fighting, "c-1"), (dead, "c-2"), (resting, "c-3")] {
            world.get_mut::<Account>(e).unwrap().character_id = cid.to_string();
        }
        for c in [1, 2, 3] {
            router.on_disconnect(&mut world, c, &pool);
        }
        world.entity_mut(dead).insert(Ghost);
        world.entity_mut(resting).remove::<mud_world::Fighting>();

        // Just under the timeout: the ghost goes, the other two stay.
        world.insert_resource(crate::TickCount(LINKDEAD_TIMEOUT_TICKS - 1));
        ConnRouter::drain_linkdead(&mut world, &pool);
        assert!(world.get_entity(dead).is_err());
        assert!(world.get_entity(fighting).is_ok());
        assert!(world.get_entity(resting).is_ok());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 1);

        // At the timeout the idle one is saved and removed; the one still in
        // a fight has its clock held back and stays.
        world.insert_resource(crate::TickCount(LINKDEAD_TIMEOUT_TICKS));
        ConnRouter::drain_linkdead(&mut world, &pool);
        assert!(world.get_entity(resting).is_err());
        assert!(world.get_entity(fighting).is_ok());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 2);
        assert_eq!(
            world
                .get::<commands::Linkdead>(fighting)
                .unwrap()
                .since_tick,
            LINKDEAD_TIMEOUT_TICKS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rent_and_camp_are_refused_mid_fight_and_keep_the_connection() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(crate::TickCount(0));
        let mut protos = mud_world::MobPrototypes::default();
        protos.by_key.insert(
            (1, 5),
            crate::commands::test_support::mob_proto(
                1,
                5,
                mud_db::enums::MobProfession::Receptionist,
            ),
        );
        world.insert_resource(protos);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        QUIT_CLOSED.with(|v| v.borrow_mut().clear());
        router.close_conn = |c| {
            QUIT_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let room = world
            .spawn((
                mud_world::Room,
                mud_world::RoomSector(mud_db::enums::Sector::Field),
            ))
            .id();
        world.spawn((
            mud_world::Mob,
            mud_world::WorldKey { zone: 1, id: 5 },
            Located(room),
            mud_world::Named {
                name: "the receptionist".into(),
            },
        ));
        let (fighter, _foe, mut rx) = fighter_in(&mut router, &mut world, room, 1);

        router.on_line(1, "rent".into(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("No way!  You're fighting for your life!"));
        router.on_line(1, "camp".into(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("You are too busy to do this!"));

        assert!(world.get_entity(fighter).is_ok());
        assert!(world.get::<mud_world::Camping>(fighter).is_none());
        assert!(world.get::<commands::Quitting>(fighter).is_none());
        assert_eq!(router.playing.get(&1), Some(&fighter));
        assert!(QUIT_CLOSED.with(|v| v.borrow().is_empty()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn camp_is_refused_where_camping_is_not_allowed_and_starts_where_it_is() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(crate::TickCount(0));
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let city = world
            .spawn((
                mud_world::Room,
                mud_world::RoomSector(mud_db::enums::Sector::City),
            ))
            .id();
        let tent = world
            .spawn((
                mud_world::Room,
                mud_world::RoomSector(mud_db::enums::Sector::Field),
                mud_world::IndoorRoom,
            ))
            .id();
        let field = world
            .spawn((
                mud_world::Room,
                mud_world::RoomSector(mud_db::enums::Sector::Field),
            ))
            .id();
        let (camper, mut rx) = playing_in(&mut router, &mut world, city, 1, "Camper");

        router.on_line(1, "camp".into(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("This isn't a place to camp"));
        assert!(world.get::<mud_world::Camping>(camper).is_none());

        world.entity_mut(camper).insert(Located(tent));
        router.on_line(1, "camp".into(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("You always pitch a tent indoors?"));
        assert!(world.get::<mud_world::Camping>(camper).is_none());

        world.entity_mut(camper).insert(Located(field));
        router.on_line(1, "camp".into(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("You start setting up camp."));
        assert!(world.get::<mud_world::Camping>(camper).is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_camp_saves_logs_out_and_closes_the_socket() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(mud_world::MudClock::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        QUIT_CLOSED.with(|v| v.borrow_mut().clear());
        router.close_conn = |c| {
            QUIT_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let room = world
            .spawn((
                mud_world::Room,
                mud_world::RoomSector(mud_db::enums::Sector::Field),
            ))
            .id();
        let (camper, mut rx) = playing_in(&mut router, &mut world, room, 1, "Camper");
        let (_watcher, mut rx_watcher) = playing_in(&mut router, &mut world, room, 2, "Watcher");
        router.on_line(1, "camp".into(), &pool, &mut world).await;
        assert!(world.get::<mud_world::Camping>(camper).is_some());

        // Countdown not over: still here.
        world.insert_resource(crate::TickCount(crate::camp::CAMP_DURATION_TICKS - 1));
        crate::camp::camp_tick(&mut world);
        router.drain_quitting(&mut world, &pool);
        assert!(world.get_entity(camper).is_ok());

        // Countdown over: the camp tick flags the player, the drain logs out.
        world.insert_resource(crate::TickCount(crate::camp::CAMP_DURATION_TICKS));
        crate::camp::camp_tick(&mut world);
        let rest = *world.get::<mud_world::RestState>(camper).unwrap();
        assert_eq!(rest.source, mud_db::enums::RestSource::Camp);
        router.drain_quitting(&mut world, &pool);

        assert!(world.get_entity(camper).is_err());
        assert_eq!(world.resource::<SaveCoordinator>().pending(), 1);
        assert!(!router.playing.contains_key(&1));
        assert_eq!(QUIT_CLOSED.with(|v| v.borrow().clone()), vec![1]);
        let out = drain(&mut rx);
        assert!(
            out.contains("You complete your campsite, and leave this world for a while."),
            "{out}"
        );
        let seen = drain(&mut rx_watcher);
        assert!(
            seen.contains("Camper rolls up their bedroll and tunes out the world."),
            "{seen}"
        );
    }

    // ---- device-code / game-password-only login ----

    #[test]
    fn login_code_alphabet_and_format() {
        assert_eq!(CODE_ALPHABET.len(), 32);
        for banned in [b'0', b'O', b'1', b'I'] {
            assert!(!CODE_ALPHABET.contains(&banned));
        }
        for _ in 0..500 {
            let code = generate_login_code();
            assert_eq!(code.len(), CODE_LEN);
            assert!(
                code.bytes().all(|b| CODE_ALPHABET.contains(&b)),
                "bad code {code}"
            );
            assert!(!code.contains('-'));
        }
        assert_eq!(format_login_code("ABCDEFGH"), "ABCD-EFGH");
        assert_eq!(
            format_login_code(&generate_login_code())
                .matches('-')
                .count(),
            1
        );
    }

    #[test]
    fn code_rate_limiter_caps_per_ip_per_window() {
        let mut rl = CodeRateLimiter::default();
        let a = IpAddr::from([10, 0, 0, 1]);
        let b = IpAddr::from([10, 0, 0, 2]);
        let t0 = Instant::now();
        for i in 0..CODE_RATE_MAX {
            assert!(rl.try_acquire(a, t0 + Duration::from_secs(i as u64)), "{i}");
        }
        assert!(!rl.try_acquire(a, t0 + Duration::from_secs(30)));
        // Another IP has its own quota.
        assert!(rl.try_acquire(b, t0 + Duration::from_secs(30)));
        // A refused attempt isn't recorded: once the first hit ages out
        // of the window exactly one slot frees up.
        let later = t0 + CODE_RATE_WINDOW;
        assert!(rl.try_acquire(a, later));
        assert!(!rl.try_acquire(a, later));
        // Long after, everything has expired.
        assert!(rl.try_acquire(a, t0 + CODE_RATE_WINDOW * 3));
    }

    #[test]
    fn tls_detection_uses_conn_id_bit() {
        assert!(!conn_is_tls(7));
        assert!(conn_is_tls((1u64 << 40) | 7));
    }

    #[test]
    fn plain_telnet_notice_open_colour_tag_after_crlf_gets_no_extra_line_break() {
        let mut world = World::new();
        world.insert_resource(messages(&[(
            "PLAIN_TELNET_NOTICE",
            "default",
            "<red>Not encrypted.\r\n",
        )]));
        let notice = String::from_utf8(plain_telnet_notice_bytes(&world)).unwrap();
        assert!(notice.contains("Not encrypted.\r\n"), "{notice:?}");
        assert_eq!(notice.matches('\n').count(), 1, "{notice:?}");
        assert!(notice.ends_with("\x1b[0m"), "{notice:?}");

        // Unterminated text with an open tag still gets exactly one.
        world.insert_resource(messages(&[(
            "PLAIN_TELNET_NOTICE",
            "default",
            "<red>Not encrypted.",
        )]));
        let notice = String::from_utf8(plain_telnet_notice_bytes(&world)).unwrap();
        assert_eq!(notice.matches('\n').count(), 1, "{notice:?}");
    }

    #[test]
    fn login_identifier_rejects_control_bytes_and_bad_shapes() {
        assert!(!is_valid_login_identifier("\x1b[2J"));
        assert!(!is_valid_login_identifier("Bob\x07"));
        assert!(!is_valid_login_identifier("\x7fBob"));
        assert!(!is_valid_login_identifier("a"));
        assert!(!is_valid_login_identifier(""));
        assert!(!is_valid_login_identifier("Bob2"));
        assert!(!is_valid_login_identifier("o'neil"));
        assert!(!is_valid_login_identifier("two words"));
        let too_long = "a".repeat(MAX_CHARACTER_NAME_LEN + 1);
        assert!(!is_valid_login_identifier(&too_long));
    }

    #[test]
    fn login_identifier_accepts_names_and_emails() {
        assert!(is_valid_login_identifier("Strider"));
        assert!(is_valid_login_identifier("ab"));
        assert!(is_valid_login_identifier(
            &"a".repeat(MAX_CHARACTER_NAME_LEN)
        ));
        assert!(is_valid_login_identifier("user@example.com"));
        assert!(!is_valid_login_identifier("us er@example.com"));
        assert!(!is_valid_login_identifier("user@exa\x1bmple.com"));
    }

    #[test]
    fn plain_telnet_notice_mentions_tls_port_and_code() {
        let world = auth_world(0);
        let text = String::from_utf8(plain_telnet_notice_bytes(&world)).unwrap();
        assert!(text.contains("unencrypted"), "{text}");
        assert!(text.contains("4443"), "{text}");
        assert!(text.contains("code"), "{text}");
        let mut world = auth_world(0);
        world
            .resource_mut::<mud_world::RuntimeConfig>()
            .by_key
            .insert(
                ("server".into(), "tls_port".into()),
                mud_world::ConfigValue::Int(5555),
            );
        let text = String::from_utf8(plain_telnet_notice_bytes(&world)).unwrap();
        assert!(text.contains("5555"), "{text}");
    }

    fn linked_user() -> User {
        User {
            id: "u1".into(),
            email: "a@example.com".into(),
            display_name: "a".into(),
            role: mud_db::enums::UserRole::Player,
            failed_login_attempts: 0,
            locked_until: None,
            account_wealth: 0,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn website_password_is_rejected_and_game_password_accepted() {
        // The linked account's website password is not even loadable
        // (`User` has no password field); the only hash in play is the
        // character's game hash.
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let (tx, _orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        let game_hash = bcrypt::hash("game-pass", 4).unwrap();
        let park = |router: &mut ConnRouter| {
            let mut c = row(None, None);
            c.user_id = Some("u1".into());
            router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
                user: linked_user(),
                preselected: Some(Box::new(c)),
                game_hash: game_hash.clone(),
            };
        };
        park(&mut router);
        router
            .on_line(1, "website-pass".into(), &pool, &mut world)
            .await;
        match rx.recv().await.unwrap().kind {
            AuthDoneKind::Password { ok, .. } => assert!(!ok, "website password accepted"),
            _ => panic!("wrong job kind"),
        }
        park(&mut router);
        router
            .on_line(1, "game-pass".into(), &pool, &mut world)
            .await;
        match rx.recv().await.unwrap().kind {
            AuthDoneKind::Password {
                ok, migration_hash, ..
            } => {
                assert!(ok, "game password rejected");
                assert!(migration_hash.is_none(), "bcrypt hash needs no upgrade");
            }
            _ => panic!("wrong job kind"),
        }
        // A character with no game password at all can't be entered
        // with any typed password.
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user: linked_user(),
            preselected: None,
            game_hash: String::new(),
        };
        router.on_line(1, String::new(), &pool, &mut world).await;
        match rx.recv().await.unwrap().kind {
            AuthDoneKind::Password { ok, .. } => assert!(!ok),
            _ => panic!("wrong job kind"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn legacy_crypt_game_hash_is_accepted_and_flagged_for_upgrade() {
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let (tx, _orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        let mut c = row(None, None);
        c.user_id = Some("u1".into());
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user: linked_user(),
            preselected: Some(Box::new(c)),
            game_hash: legacy_hash("hunter2"),
        };
        router.on_line(1, "hunter2".into(), &pool, &mut world).await;
        match rx.recv().await.unwrap().kind {
            AuthDoneKind::Password {
                ok, migration_hash, ..
            } => {
                assert!(ok);
                assert!(matches!(migration_hash, Some(Ok(h)) if h.starts_with("$2")));
            }
            _ => panic!("wrong job kind"),
        }
    }

    /// A correct crypt(3) password on an unlinked legacy character
    /// upgrades the character's hash to bcrypt but must NOT create a
    /// placeholder web user or link the character (the website's
    /// `linkCharacter` has to stay able to claim it). Wizlock stops the
    /// flow right after the upgrade so no player spawn is needed.
    #[tokio::test(flavor = "current_thread")]
    async fn legacy_login_upgrades_hash_without_creating_user() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("LegacyT{suffix}");
        let tmp_email = format!("{}@tmp.invalid", name.to_ascii_lowercase());
        let placeholder = format!("{}@legacy.fierymud.local", name.to_ascii_lowercase());
        let tmp_user = mud_db::users::create(&pool, &tmp_email, &name)
            .await
            .unwrap();
        let char_id = mud_db::characters::create(
            &pool,
            &mud_db::characters::NewCharacter {
                user_id: &tmp_user,
                name: &name,
                race: "HUMAN",
                gender: "neutral",
                class_id: 1,
                strength: 13,
                intelligence: 13,
                wisdom: 13,
                dexterity: 13,
                constitution: 13,
                charisma: 13,
                name_approved: true,
                password_hash: "",
            },
        )
        .await
        .unwrap();
        // Make it a legacy orphan with a crypt(3) game hash.
        let hash = legacy_hash("hunter2");
        mud_db::sqlx::query(
            "UPDATE \"Characters\" SET user_id = NULL, password_hash = $1 WHERE id = $2",
        )
        .bind(&hash)
        .bind(&char_id)
        .execute(&pool)
        .await
        .unwrap();
        mud_db::sqlx::query("DELETE FROM \"Users\" WHERE id = $1")
            .bind(&tmp_user)
            .execute(&pool)
            .await
            .unwrap();
        let char_row = characters::find_by_name(&pool, &name)
            .await
            .unwrap()
            .unwrap();
        assert!(char_row.user_id.is_none());

        let mut world = auth_world(0);
        world.insert_resource(mud_world::WizLock { active: true });
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        drain(&mut orx);
        let mut user = legacy_sentinel().0;
        user.email = name.clone();
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: Some(Box::new(char_row)),
            game_hash: hash,
        };
        attempt(&mut router, &mut rx, &mut world, &pool, 1, "hunter2").await;
        let out = drain(&mut orx);

        let (user_id, new_hash): (Option<String>, String) = mud_db::sqlx::query_as(
            "SELECT user_id, password_hash FROM \"Characters\" WHERE id = $1",
        )
        .bind(&char_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let placeholders: i64 =
            mud_db::sqlx::query_scalar("SELECT COUNT(*) FROM \"Users\" WHERE email = $1")
                .bind(&placeholder)
                .fetch_one(&pool)
                .await
                .unwrap();
        mud_db::sqlx::query("DELETE FROM \"Characters\" WHERE id = $1")
            .bind(&char_id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(out.contains("locked for staff only"), "{out}");
        assert!(user_id.is_none(), "character must stay unlinked");
        assert_eq!(placeholders, 0, "no placeholder web user may be created");
        assert!(new_hash.starts_with("$2"), "hash upgraded to bcrypt");
        assert!(bcrypt::verify("hunter2", &new_hash).unwrap());
    }

    /// Connect to the dev database for the flow tests that exercise
    /// the real `GameLoginCode` table; `None` (test skipped) when it
    /// isn't reachable.
    async fn live_pool() -> Option<(PgPool, tokio::sync::MutexGuard<'static, ()>)> {
        let db_lock = crate::commands::test_support::db_test_lock().await;
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let pool = tokio::time::timeout(
            Duration::from_secs(3),
            mud_db::connect_with(&url, crate::commands::test_support::db_test_pool_settings()),
        )
        .await
        .ok()?
        .ok()?;
        mud_db::sqlx::query("SELECT 1 FROM \"GameLoginCode\" LIMIT 1")
            .execute(&pool)
            .await
            .ok()?;
        Some((pool, db_lock))
    }

    #[test]
    fn gmcp_log_payload_is_truncated_on_a_char_boundary() {
        assert_eq!(truncate_for_log("short", 256), "short");
        let long = "a".repeat(1000);
        assert_eq!(truncate_for_log(&long, GMCP_LOG_MAX).len(), GMCP_LOG_MAX);
        // 'é' is two bytes: cutting at 3 must not split it.
        assert_eq!(truncate_for_log("aéé", 4), "aé");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gmcp_hello_is_logged_once_per_connection() {
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        for _ in 0..3 {
            router.on_gmcp(1, "Core.Hello", "{}", &mut world).await;
            router
                .on_gmcp(1, "Core.Supports.Set", "[]", &mut world)
                .await;
        }
        let caps = router.caps.get(&1).unwrap();
        assert!(caps.hello_logged && caps.supports_logged);
    }

    /// The email prompt answers identically whatever the address, and
    /// does not touch the database (the pool here cannot connect).
    #[tokio::test(flavor = "current_thread")]
    async fn email_identifier_gets_one_neutral_prompt_without_a_lookup() {
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let mut outputs = Vec::new();
        for (conn, email) in [(1, "known@example.com"), (2, "nobody-here@example.org")] {
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            router.on_connect(conn, tx, None, &world);
            drain(&mut rx);
            router.on_line(conn, email.into(), &pool, &mut world).await;
            assert!(matches!(
                router.login.get(&conn).unwrap().stage,
                Stage::ConfirmCreate { is_email: true, .. }
            ));
            // Same text once the address itself is blanked out.
            outputs.push(drain(&mut rx).replace(email, "<email>"));
        }
        assert_eq!(outputs[0], outputs[1]);
        assert!(outputs[0].contains("'code'"), "{}", outputs[0]);
    }

    /// Character-name lookups run off the loop: the stage parks in
    /// `Authenticating` and the answer arrives as an `AuthDone`.
    #[tokio::test(flavor = "current_thread")]
    async fn character_lookup_runs_off_the_loop_and_resumes_via_auth_done() {
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        drain(&mut rx);
        router
            .on_line(1, "Somebody".into(), &pool, &mut world)
            .await;
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::Authenticating
        ));
        // Input while the lookup is in flight is swallowed.
        router.on_line(1, "again".into(), &pool, &mut world).await;
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::Authenticating
        ));
        // Resolve it by hand (the lazy pool's own lookup would fail).
        router
            .on_auth_done(
                AuthDone {
                    conn_id: 1,
                    kind: AuthDoneKind::CharLookup {
                        identifier: "Somebody".into(),
                        result: CharLookup::Unknown,
                    },
                },
                &pool,
                &mut world,
            )
            .await;
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::ConfirmCreate {
                is_email: false,
                ..
            }
        ));
        assert!(drain(&mut rx).contains("Create a new one?"));
    }

    /// `code` for an email with no account still shows a code and waits,
    /// exactly like a real one; nothing exists to approve it.
    #[tokio::test(flavor = "current_thread")]
    async fn unregistered_email_code_request_is_a_decoy() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        drain(&mut rx);
        let email = format!("nobody-{}@example.invalid", std::process::id());
        router.on_line(1, email.clone(), &pool, &mut world).await;
        drain(&mut rx);
        router.on_line(1, "code".into(), &pool, &mut world).await;
        // The account lookup runs off the world loop; swallow input until
        // it resolves (here: no such account).
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::Authenticating
        ));
        router
            .on_auth_done(
                AuthDone {
                    conn_id: 1,
                    kind: AuthDoneKind::EmailLookup {
                        email,
                        result: Ok(None),
                    },
                },
                &pool,
                &mut world,
            )
            .await;
        let out = drain(&mut rx);
        assert!(out.contains("Your login code is"), "{out}");
        assert!(
            out.contains("does not accept your website password"),
            "{out}"
        );
        assert!(
            matches!(
                &router.login.get(&1).unwrap().stage,
                Stage::AwaitingWebApproval(w) if w.decoy
            ),
            "stage"
        );
        // Enter: still waiting, like a pending real code.
        router.on_line(1, String::new(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("still waiting for approval"));
        router.on_line(1, "cancel".into(), &pool, &mut world).await;
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::AwaitingIdentifier
        ));
    }

    /// A peer past its login-code quota is refused before any account
    /// lookup is started.
    #[tokio::test(flavor = "current_thread")]
    async fn email_code_over_quota_starts_no_lookup() {
        let mut world = auth_world(0);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        drain(&mut rx);
        let ip = IpAddr::from([0u8, 0, 0, 0]);
        for _ in 0..CODE_RATE_MAX {
            assert!(router.code_limiter.try_acquire(ip, Instant::now()));
        }
        router
            .on_line(1, "who@example.invalid".into(), &pool, &mut world)
            .await;
        drain(&mut rx);
        router.on_line(1, "code".into(), &pool, &mut world).await;
        assert!(drain(&mut rx).contains("Too many login codes requested"));
        assert!(
            !matches!(router.login.get(&1).unwrap().stage, Stage::Authenticating),
            "no lookup may be started"
        );
    }

    fn pending_web(router: &ConnRouter, conn: ConnId) -> (String, String) {
        match &router.login.get(&conn).unwrap().stage {
            Stage::AwaitingWebApproval(w) => (w.code_id.clone(), w.code.clone()),
            _ => panic!("not awaiting web approval"),
        }
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test(flavor = "current_thread")]
    async fn code_at_password_prompt_enters_web_approval_and_resolves() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        // Own throwaway account: borrowing "the first Users row" races
        // with other tests that create and delete users (FK violation on
        // the code insert -> "not awaiting web approval").
        let Some(uid) = temp_user(&pool, "dctest").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, Some("127.0.0.1:40123".parse().unwrap()), &world);
        drain(&mut orx);
        let mut user = linked_user();
        user.id = uid.clone();
        let c = row(None, None);
        let char_name = c.name.clone();
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: Some(Box::new(c)),
            game_hash: String::new(),
        };
        router.on_line(1, "code".into(), &pool, &mut world).await;
        let out = drain(&mut orx);
        let (code_id, code) = pending_web(&router, 1);
        let shown = format_login_code(&code);
        assert!(
            out.contains(&format!("Your login code is {shown}.")),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "https://muditor.utaboshi.com/verify?code={shown} within 2 minutes"
            )),
            "{out}"
        );
        assert!(
            out.contains("Press Enter to check, or type cancel."),
            "{out}"
        );
        // Row contents.
        let (status, user_id, ip, port, tls, name): (
            String,
            Option<String>,
            String,
            Option<i32>,
            bool,
            String,
        ) = mud_db::sqlx::query_as(
            "SELECT status::text, \"userId\", \"clientIp\", \"clientPort\", tls, \"characterName\" \
             FROM \"GameLoginCode\" WHERE id = $1",
        )
        .bind(&code_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "PENDING");
        assert_eq!(user_id.as_deref(), Some(uid.as_str()));
        assert_eq!((ip.as_str(), port, tls), ("127.0.0.1", Some(40123), false));
        assert_eq!(name, char_name);

        // Still pending: Enter keeps waiting.
        router.on_line(1, String::new(), &pool, &mut world).await;
        assert!(drain(&mut orx).contains("still waiting"));
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::AwaitingWebApproval(_)
        ));

        // Approved by somebody else: refused, never consumed.
        mud_db::sqlx::query(
            "UPDATE \"GameLoginCode\" SET status = 'APPROVED', \"approvedByUserId\" = 'someone-else' \
             WHERE id = $1",
        )
        .bind(&code_id)
        .execute(&pool)
        .await
        .unwrap();
        router.on_line(1, String::new(), &pool, &mut world).await;
        assert!(drain(&mut orx).contains("different account"));
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::AwaitingIdentifier
        ));
        let st = mud_db::game_login_code::state(&pool, &code_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(st.status, "APPROVED");
        // ...and the consume SQL itself enforces the approver.
        assert!(
            !mud_db::game_login_code::consume(
                &pool,
                &code_id,
                &uid,
                chrono::Utc::now().naive_utc()
            )
            .await
            .unwrap()
        );
        mud_db::game_login_code::delete(&pool, &code_id)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[], &[&uid]).await;
    }

    /// Temp website account for the unlinked-character flow tests.
    async fn temp_user(pool: &PgPool, tag: &str) -> Option<String> {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        mud_db::users::create(
            pool,
            &format!("{tag}{suffix}@example.invalid"),
            &format!("{tag}{suffix}"),
        )
        .await
        .ok()
    }

    /// An account created at the game prompt carries an unproven email;
    /// Muditor's Google login must not auto-link to it.
    #[tokio::test(flavor = "current_thread")]
    async fn game_created_user_is_marked_email_unverified() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(uid) = temp_user(&pool, "unverif").await else {
            return;
        };
        let flag: Option<String> = mud_db::sqlx::query_scalar(
            "SELECT preferences->>'emailVerified' FROM \"Users\" WHERE id = $1",
        )
        .bind(&uid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(flag.as_deref(), Some("false"));
        temp_cleanup(&pool, &[], &[], &[&uid]).await;
    }

    /// Temp unlinked (NULL `user_id`) legacy-style character; returns
    /// the sentinel-user + preselected pair the password prompt holds.
    async fn temp_unlinked_char(pool: &PgPool, tag: &str) -> (User, Box<CharacterRow>) {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("Zz{tag}{}", suffix % 1_000_000_000_000);
        let id = format!("zz-{tag}-{suffix}");
        mud_db::sqlx::query(
            "INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())",
        )
        .bind(&id)
        .bind(&name)
        .execute(pool)
        .await
        .unwrap();
        let (user, _) = legacy_sentinel();
        let mut c = row(None, None);
        c.id = id;
        c.name = name;
        c.user_id = None;
        (user, Box::new(c))
    }

    /// End-to-end through the real tables: the foreground path and the
    /// background (autosave) path both write the snapshot, stamp the
    /// assigned `CharacterItems` ids back, and a second save updates
    /// rather than duplicates the item rows.
    #[tokio::test(flavor = "current_thread")]
    async fn foreground_and_background_saves_round_trip() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "sv").await;
        let count = |pool: PgPool, cid: String| async move {
            mud_db::sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM \"CharacterItems\" WHERE character_id = $1",
            )
            .bind(cid)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        let hp_of = |pool: PgPool, cid: String| async move {
            mud_db::sqlx::query_scalar::<_, i32>(
                "SELECT hit_points FROM \"Characters\" WHERE id = $1",
            )
            .bind(cid)
            .fetch_one(&pool)
            .await
            .unwrap()
        };

        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let player = world
            .spawn((
                Account {
                    user_id: String::new(),
                    character_id: c.id.clone(),
                    role: mud_db::enums::UserRole::Player,
                    account_role: mud_db::enums::UserRole::Player,
                    perms: vec![],
                },
                Health { hp: 7, max: 20 },
                Located(room),
            ))
            .id();
        let item = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(player)))
            .id();

        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert_eq!(hp_of(pool.clone(), c.id.clone()).await, 7);
        assert_eq!(count(pool.clone(), c.id.clone()).await, 1);
        let pid = world.get::<mud_world::PersistedItemId>(item).unwrap().0;

        // Background path: changed hp lands via the spawned writer, and
        // the already-stamped item is updated in place (same row id).
        world.get_mut::<Health>(player).unwrap().hp = 9;
        assert!(spawn_background_save(&mut world, player, &pool));
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
        assert_eq!(hp_of(pool.clone(), c.id.clone()).await, 9);
        assert_eq!(count(pool.clone(), c.id.clone()).await, 1);
        assert_eq!(
            world.get::<mud_world::PersistedItemId>(item).unwrap().0,
            pid
        );

        // A new item acquired between saves is inserted by the background
        // write and stamped back when the tick folds the completion in.
        let item2 = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(player)))
            .id();
        assert!(spawn_background_save(&mut world, player, &pool));
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
        assert_eq!(count(pool.clone(), c.id.clone()).await, 2);
        assert!(world.get::<mud_world::PersistedItemId>(item2).is_some());

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// Issue #56: a re-acquired item (old row id, newest arrival) must
    /// reload as the newest item, so `list_for` orders by the arrival
    /// stamp `save_inventory_diff` writes, not by row id.
    #[tokio::test]
    async fn reacquired_item_reloads_as_newest_arrival() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "ord").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);
        let first = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(player)))
            .id();
        let second = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(player)))
            .id();
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let first_id = world.get::<mud_world::PersistedItemId>(first).unwrap().0;
        let second_id = world.get::<mud_world::PersistedItemId>(second).unwrap().0;
        assert!(first_id < second_id);

        // Put `first` down and pick it up again: it is now the newest.
        world.entity_mut(first).insert(Located(room));
        world.entity_mut(first).insert(Located(player));
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);

        let rows = mud_db::character_items::list_for(&pool, &c.id)
            .await
            .unwrap();
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![second_id, first_id], "oldest arrival first");

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    async fn save(world: &mut World, player: Entity, pool: &PgPool) {
        let out = save_player(world, player, pool).await;
        assert!(out.committed, "{:?}", out.error);
    }

    /// Issue #77: Curse on a carried weapon (`NO_DROP` + a smaller die) and
    /// Remove Curse both survive a save and relog; a clean save never
    /// clobbers a delta an admin wrote straight into the row.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn item_curse_survives_save_and_reload() {
        use mud_db::enums::ObjectRestriction::{NoDrop, NoSell};
        use mud_world::components::{ItemAlterDirty, WeaponDiceSizeAdjust};
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, ch) = temp_unlinked_char(&pool, "curse").await;
        let mut protos = mud_world::ObjectPrototypes::default();
        let mut proto =
            crate::commands::test_support::object_proto(oz, oid, mud_db::enums::ObjectType::Weapon);
        proto.weapon_dice_num = 2;
        proto.weapon_dice_size = 6;
        proto.restrictions = vec![NoSell];
        protos.by_key.insert((oz, oid), proto);
        let by_key = protos.by_key.clone();
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(protos);
        world.insert_resource(mud_world::TriggerCatalog::default());
        world.insert_resource(mud_world::ObjectAbilityCatalog::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &ch.id, room);
        let sword = world
            .spawn((
                Item,
                Named {
                    name: "a test object".into(),
                },
                WorldKey { zone: oz, id: oid },
                mud_world::ObjectRestrictions(vec![NoSell]),
                Located(player),
            ))
            .id();
        let curse_key = |pool: PgPool, id: i32| async move {
            mud_db::sqlx::query_scalar::<_, Option<serde_json::Value>>(
                "SELECT custom_values -> 'curse' FROM \"CharacterItems\" WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        let reload = |pool: PgPool, by_key: std::collections::HashMap<(i32, i32), _>| {
            let cid = ch.id.clone();
            async move {
                let rows = mud_db::character_items::list_for(&pool, &cid)
                    .await
                    .unwrap();
                let mut fresh = World::new();
                fresh.insert_resource(mud_world::ObjectPrototypes { by_key });
                fresh.insert_resource(mud_world::TriggerCatalog::default());
                fresh.insert_resource(mud_world::ObjectAbilityCatalog::default());
                let fresh_room = fresh.spawn_empty().id();
                let who = spawn_player_for(&mut fresh, &cid, fresh_room);
                assert_eq!(spawn_inventory(&mut fresh, who, &rows), 1);
                let item = fresh
                    .query_filtered::<Entity, With<Item>>()
                    .iter(&fresh)
                    .next()
                    .unwrap();
                (fresh, item)
            }
        };

        // A clean first save writes no curse key.
        save(&mut world, player, &pool).await;
        let pid = world.get::<mud_world::PersistedItemId>(sword).unwrap().0;
        assert_eq!(curse_key(pool.clone(), pid).await, None);

        // Curse: NO_DROP added, die shrunk, marked dirty (as the spell does).
        world
            .entity_mut(sword)
            .insert(mud_world::ObjectRestrictions(vec![NoSell, NoDrop]));
        world.entity_mut(sword).insert(WeaponDiceSizeAdjust(-1));
        crate::item_alter::mark_dirty(&mut world, sword);
        save(&mut world, player, &pool).await;
        assert!(
            world.get::<ItemAlterDirty>(sword).is_none(),
            "a committed save settles the dirty marker"
        );
        let stored = curse_key(pool.clone(), pid).await.expect("curse key");
        assert_eq!(stored["restrictions_added"], serde_json::json!(["NO_DROP"]));
        assert_eq!(stored["weapon_dice_size"], -1);
        let (w2, loaded) = reload(pool.clone(), by_key.clone()).await;
        let r = w2.get::<mud_world::ObjectRestrictions>(loaded).unwrap();
        assert!(r.has(NoDrop) && r.has(NoSell), "{:?}", r.0);
        assert_eq!(w2.get::<WeaponDiceSizeAdjust>(loaded).unwrap().0, -1);
        assert!(w2.get::<ItemAlterDirty>(loaded).is_none());

        // A relogged, unchanged item saves without rewriting the key: an
        // admin edit made meanwhile survives.
        mud_db::sqlx::query(
            "UPDATE \"CharacterItems\" SET custom_values = \
             '{\"curse\": {\"weapon_dice_size\": -3}}' WHERE id = $1",
        )
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
        save(&mut world, player, &pool).await;
        assert_eq!(
            curse_key(pool.clone(), pid).await.unwrap()["weapon_dice_size"],
            -3
        );

        // Remove Curse: back to the prototype, dirty again, key cleared.
        world
            .entity_mut(sword)
            .insert(mud_world::ObjectRestrictions(vec![NoSell]));
        world.entity_mut(sword).remove::<WeaponDiceSizeAdjust>();
        crate::item_alter::mark_dirty(&mut world, sword);
        save(&mut world, player, &pool).await;
        assert_eq!(curse_key(pool.clone(), pid).await, None);
        let (w3, loaded) = reload(pool.clone(), by_key).await;
        let r = w3.get::<mud_world::ObjectRestrictions>(loaded).unwrap();
        assert!(!r.has(NoDrop) && r.has(NoSell), "{:?}", r.0);
        assert!(w3.get::<WeaponDiceSizeAdjust>(loaded).is_none());

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&ch.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&ch.id], &[]).await;
    }

    /// Enchant Weapon's per-instance applies, `MAGIC` flag and barred
    /// alignment ride in the `curse` key of `custom_values`, reload with the
    /// item, and are re-granted on wield rather than baked into any saved
    /// character stat.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn item_enchantment_survives_save_and_reload() {
        use mud_db::enums::{Alignment, ObjectFlag};
        use mud_world::components::{ItemApplies, ItemBarredAlignments};
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, ch) = temp_unlinked_char(&pool, "enchant").await;
        let mut protos = mud_world::ObjectPrototypes::default();
        let mut proto =
            crate::commands::test_support::object_proto(oz, oid, mud_db::enums::ObjectType::Weapon);
        proto.weapon_dice_num = 2;
        proto.weapon_dice_size = 6;
        protos.by_key.insert((oz, oid), proto);
        let by_key = protos.by_key.clone();
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(protos);
        world.insert_resource(mud_world::TriggerCatalog::default());
        world.insert_resource(mud_world::ObjectAbilityCatalog::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &ch.id, room);
        let sword = world
            .spawn((
                Item,
                Named {
                    name: "a test object".into(),
                },
                WorldKey { zone: oz, id: oid },
                Located(player),
            ))
            .id();
        world.entity_mut(sword).insert((
            ItemApplies(vec![("accuracy".into(), 4), ("attack_power".into(), 10)]),
            ItemBarredAlignments(vec![Alignment::Evil]),
        ));
        // Enchant records the flag it adds; only that is persisted.
        crate::item_alter::add_flag(&mut world, sword, ObjectFlag::Magic);
        crate::item_alter::mark_dirty(&mut world, sword);
        save(&mut world, player, &pool).await;
        let pid = world.get::<mud_world::PersistedItemId>(sword).unwrap().0;
        let stored: serde_json::Value = mud_db::sqlx::query_scalar(
            "SELECT custom_values -> 'curse' FROM \"CharacterItems\" WHERE id = $1",
        )
        .bind(pid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored["applies"][0]["target"], "accuracy");
        assert_eq!(stored["applies"][0]["amount"], 4);
        assert_eq!(stored["applies"][1]["target"], "attack_power");
        assert_eq!(stored["flags_added"], serde_json::json!(["Magic"]));
        assert_eq!(stored["alignments_barred"], serde_json::json!(["Evil"]));

        // Relog: the enchantment is back, and wielding grants it exactly once.
        let rows = mud_db::character_items::list_for(&pool, &ch.id)
            .await
            .unwrap();
        let mut fresh = World::new();
        fresh.insert_resource(mud_world::ObjectPrototypes { by_key });
        fresh.insert_resource(mud_world::TriggerCatalog::default());
        fresh.insert_resource(mud_world::ObjectAbilityCatalog::default());
        let fresh_room = fresh.spawn_empty().id();
        let who = spawn_player_for(&mut fresh, &ch.id, fresh_room);
        assert_eq!(spawn_inventory(&mut fresh, who, &rows), 1);
        let loaded = fresh
            .query_filtered::<Entity, With<Item>>()
            .iter(&fresh)
            .next()
            .unwrap();
        assert_eq!(
            fresh.get::<ItemApplies>(loaded).unwrap().0,
            vec![
                ("accuracy".to_string(), 4),
                ("attack_power".to_string(), 10)
            ]
        );
        assert!(
            fresh
                .get::<mud_world::ObjectFlags>(loaded)
                .unwrap()
                .has(ObjectFlag::Magic)
        );
        assert_eq!(
            fresh.get::<ItemBarredAlignments>(loaded).unwrap().0,
            vec![Alignment::Evil]
        );
        assert!(
            fresh
                .get::<mud_world::components::ItemAlterDirty>(loaded)
                .is_none()
        );
        fresh
            .entity_mut(who)
            .insert(mud_world::CombatStats::default());
        let before = fresh.get::<mud_world::CombatStats>(who).map(|c| c.accuracy);
        fresh
            .entity_mut(loaded)
            .insert(mud_world::EquippedSlot(mud_world::Slot::Wield));
        crate::equip_apply::apply_object_to_wearer(&mut fresh, loaded, who);
        let after = fresh.get::<mud_world::CombatStats>(who).map(|c| c.accuracy);
        assert_eq!(after.unwrap_or(0) - before.unwrap_or(0), 4);
        assert_eq!(
            crate::equip_apply::gear_offsets(&fresh, who),
            crate::equip_apply::GearOffsets::default(),
            "the enchantment is not part of any persisted stat"
        );

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&ch.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&ch.id], &[]).await;
    }

    /// Issues #67/#68: a custom name, examine text and keyword override
    /// written through the normal save reload on the next login, set the
    /// mirroring instance flags, and clearing them clears the columns.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn item_customization_survives_save_and_reload() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "cust").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let mut protos = mud_world::ObjectPrototypes::default();
        let mut proto = crate::commands::test_support::object_proto(
            oz,
            oid,
            mud_db::enums::ObjectType::Container,
        );
        proto.name = "a cloth sack".into();
        proto.keywords = vec!["sack".into()];
        protos.by_key.insert((oz, oid), proto);
        let by_key = protos.by_key.clone();
        world.insert_resource(protos);
        world.insert_resource(mud_world::TriggerCatalog::default());
        world.insert_resource(mud_world::ObjectAbilityCatalog::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);
        let bag = world
            .spawn((
                Item,
                Named {
                    name: "a cloth sack".into(),
                },
                WorldKey { zone: oz, id: oid },
                Located(player),
            ))
            .id();
        crate::item_custom::edit(&mut world, bag, |cu| {
            cu.name = Some("Daedela's cloth sack".into());
            cu.examine = Some("Stitched with care.".into());
            cu.keywords = Some(vec!["sack".into(), "gems".into()]);
        });
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);

        let row_flags = |pool: PgPool, id: i32| async move {
            mud_db::sqlx::query_as::<
                _,
                (
                    Option<String>,
                    Option<String>,
                    Vec<String>,
                    serde_json::Value,
                ),
            >(
                "SELECT custom_name, custom_examine_description, \
                        COALESCE(instance_flags::text[], '{}'), custom_values \
                 FROM \"CharacterItems\" WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        let pid = world.get::<mud_world::PersistedItemId>(bag).unwrap().0;
        let (name, examine, flags, values) = row_flags(pool.clone(), pid).await;
        assert_eq!(name.as_deref(), Some("Daedela's cloth sack"));
        assert_eq!(examine.as_deref(), Some("Stitched with care."));
        assert!(flags.contains(&"CUSTOM_NAMED".to_string()), "{flags:?}");
        assert!(flags.contains(&"CUSTOM_DESCRIBED".to_string()), "{flags:?}");
        assert_eq!(values["keywords"], serde_json::json!(["sack", "gems"]));

        // Reload into a fresh world through the real loader.
        let rows = mud_db::character_items::list_for(&pool, &c.id)
            .await
            .unwrap();
        let mut world2 = World::new();
        world2.insert_resource(mud_world::ObjectPrototypes { by_key });
        world2.insert_resource(mud_world::TriggerCatalog::default());
        world2.insert_resource(mud_world::ObjectAbilityCatalog::default());
        let room2 = world2.spawn_empty().id();
        let player2 = spawn_player_for(&mut world2, &c.id, room2);
        assert_eq!(spawn_inventory(&mut world2, player2, &rows), 1);
        let loaded = world2
            .query_filtered::<Entity, With<Item>>()
            .iter(&world2)
            .next()
            .unwrap();
        assert_eq!(
            world2.get::<Named>(loaded).unwrap().name,
            "Daedela's cloth sack"
        );
        assert_eq!(
            world2.get::<mud_world::Description>(loaded).unwrap().0,
            "Stitched with care."
        );
        let kw = world2.get::<mud_world::Keywords>(loaded).unwrap().0.clone();
        assert!(kw.contains(&"gems".to_string()) && kw.contains(&"daedela's".to_string()));

        // Clearing the overrides clears the columns and flags.
        crate::item_custom::edit(&mut world, bag, |cu| {
            cu.name = None;
            cu.examine = None;
            cu.keywords = None;
        });
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let (name, examine, flags, values) = row_flags(pool.clone(), pid).await;
        assert!(name.is_none() && examine.is_none(), "{name:?} {examine:?}");
        assert!(flags.is_empty(), "{flags:?}");
        assert!(values.get("keywords").is_none(), "{values}");

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A holder's save must not overwrite customization an admin wrote
    /// straight into the database: only a customization the runtime changed
    /// this session (`dirty`) is written over an existing row.
    #[tokio::test]
    async fn clean_customization_does_not_clobber_database_edits() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "clean").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);
        let plain = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(player)))
            .id();
        let loaded = world
            .spawn((
                Item,
                WorldKey { zone: oz, id: oid },
                Located(player),
                mud_world::ItemCustomization {
                    name: Some("Old".into()),
                    ..Default::default()
                },
            ))
            .id();
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let plain_id = world.get::<mud_world::PersistedItemId>(plain).unwrap().0;
        let loaded_id = world.get::<mud_world::PersistedItemId>(loaded).unwrap().0;
        // A fresh INSERT writes the customization even when not dirty.
        let name_of_row = |pool: PgPool, id: i32| async move {
            mud_db::sqlx::query_scalar::<_, Option<String>>(
                "SELECT custom_name FROM \"CharacterItems\" WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(
            name_of_row(pool.clone(), loaded_id).await.as_deref(),
            Some("Old")
        );

        mud_db::sqlx::query(
            "UPDATE \"CharacterItems\" SET custom_name = 'Admin set' WHERE id = ANY($1)",
        )
        .bind(vec![plain_id, loaded_id])
        .execute(&pool)
        .await
        .unwrap();
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert_eq!(
            name_of_row(pool.clone(), plain_id).await.as_deref(),
            Some("Admin set")
        );
        assert_eq!(
            name_of_row(pool.clone(), loaded_id).await.as_deref(),
            Some("Admin set")
        );

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A committed save settles the customizations it wrote: `dirty` clears,
    /// unless the override changed again after the snapshot was taken.
    #[test]
    fn apply_commit_clears_dirty_only_for_what_it_wrote() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let a = spawn_player_for(&mut world, "ac-dirty", room);
        let custom = |name: &str, dirty: bool| mud_world::ItemCustomization {
            name: Some(name.into()),
            dirty,
            ..Default::default()
        };
        let written = world
            .spawn((
                Item,
                WorldKey { zone: 1, id: 1 },
                Located(a),
                custom("Written", true),
            ))
            .id();
        let edited_since = world
            .spawn((
                Item,
                WorldKey { zone: 1, id: 2 },
                Located(a),
                custom("Before", true),
            ))
            .id();
        let clean = world
            .spawn((
                Item,
                WorldKey { zone: 1, id: 3 },
                Located(a),
                custom("Clean", false),
            ))
            .id();
        let snap = snapshot_player(&mut world, a, 1).unwrap();
        world.entity_mut(edited_since).insert(custom("After", true));
        apply_commit(&mut world, &snap, HashMap::new());
        let dirty = |e: Entity| world.get::<mud_world::ItemCustomization>(e).unwrap().dirty;
        assert!(!dirty(written), "written customization is settled");
        assert!(dirty(edited_since), "a newer edit stays dirty");
        assert!(!dirty(clean));
    }

    /// After the save that wrote a customization commits, later saves leave
    /// the row alone, so an edit made straight in the database survives.
    #[tokio::test]
    async fn dirty_customization_is_written_once_then_left_alone() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let object: Option<(i32, i32)> =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some((oz, oid)) = object else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "dirtyonce").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);
        let item = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(player)))
            .id();
        // Save once so the item has a row, then rename it.
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let row_id = world.get::<mud_world::PersistedItemId>(item).unwrap().0;
        world.entity_mut(item).insert(mud_world::ItemCustomization {
            name: Some("Mine".into()),
            dirty: true,
            ..Default::default()
        });
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let row_name = || async {
            mud_db::sqlx::query_scalar::<_, Option<String>>(
                "SELECT custom_name FROM \"CharacterItems\" WHERE id = $1",
            )
            .bind(row_id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(row_name().await.as_deref(), Some("Mine"));
        assert!(
            !world
                .get::<mud_world::ItemCustomization>(item)
                .unwrap()
                .dirty,
            "dirty cleared once the write committed"
        );
        mud_db::sqlx::query(
            "UPDATE \"CharacterItems\" SET custom_name = 'Muditor set' WHERE id = $1",
        )
        .bind(row_id)
        .execute(&pool)
        .await
        .unwrap();
        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert_eq!(row_name().await.as_deref(), Some("Muditor set"));

        mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    // --- last_logout: offline rest measures from session END ---

    fn naive_at(unix: i64) -> chrono::NaiveDateTime {
        chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .naive_utc()
    }

    #[test]
    fn four_hour_session_then_immediate_relog_grants_no_repose() {
        // Logout stamped at the moment the session ends (4 h after it began):
        // relogging right away leaves ~0 s offline, whatever last_login says.
        let now = 1_900_000_000;
        let elapsed = offline_elapsed_secs(Some(naive_at(now)), now + 2);
        assert_eq!(accrue_l10(0, 1, elapsed), 0);
    }

    #[test]
    fn quit_then_four_hours_offline_fills_tier1_cap() {
        let now = 1_900_000_000;
        let elapsed = offline_elapsed_secs(Some(naive_at(now - 4 * HOUR)), now);
        assert_eq!(elapsed, 4 * HOUR);
        assert_eq!(accrue_l10(0, 1, elapsed), accrue_l10(0, 1, 40 * HOUR));
        assert!(accrue_l10(0, 1, elapsed) > 0);
    }

    #[test]
    fn null_last_logout_grants_nothing() {
        assert_eq!(offline_elapsed_secs(None, 1_900_000_000), 0);
        // Even with a stale last_login on the row, only last_logout counts.
        let mut r = row(None, None);
        r.last_login = Some(naive_at(1_900_000_000 - 100 * HOUR));
        assert_eq!(offline_elapsed_secs(r.last_logout, 1_900_000_000), 0);
    }

    #[test]
    fn logout_in_the_future_clamps_to_zero() {
        assert_eq!(offline_elapsed_secs(Some(naive_at(2_000)), 1_000), 0);
    }

    /// Wait for the spawned session-ending writes (`retire_player` never
    /// awaits its save) and fold their completions into the world.
    async fn settle_saves(world: &mut World) {
        let c = world.resource::<SaveCoordinator>().clone();
        assert!(
            c.flush(world, Duration::from_secs(10)).await,
            "saves settle"
        );
    }

    async fn last_logout_of(pool: &PgPool, cid: &str) -> Option<chrono::NaiveDateTime> {
        mud_db::sqlx::query_scalar::<_, Option<chrono::NaiveDateTime>>(
            "SELECT last_logout FROM \"Characters\" WHERE id = $1",
        )
        .bind(cid)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// `save` / autosave / background saves never move `last_logout`; the
    /// session-ending save stamps it; the next login clears it again.
    #[tokio::test(flavor = "current_thread")]
    async fn only_session_end_saves_stamp_last_logout() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "lo1").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);

        let out = save_player(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert!(spawn_background_save(&mut world, player, &pool));
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
        assert_eq!(last_logout_of(&pool, &c.id).await, None, "autosave stamped");

        let before = chrono::Utc::now().naive_utc();
        let out = save_player_final(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let stamped = last_logout_of(&pool, &c.id)
            .await
            .expect("final save stamps");
        assert!(stamped >= before - chrono::Duration::seconds(1));
        assert!(stamped <= chrono::Utc::now().naive_utc() + chrono::Duration::seconds(1));

        // A later autosave leaves the stamp alone.
        assert!(spawn_background_save(&mut world, player, &pool));
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
        assert_eq!(last_logout_of(&pool, &c.id).await, Some(stamped));

        // Next login consumes the window and clears it (a crash can't re-accrue).
        mud_db::characters::update_last_login(&pool, &c.id)
            .await
            .unwrap();
        assert_eq!(last_logout_of(&pool, &c.id).await, None);
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// Round trip through the real column: quit, push `last_logout` back 4 h,
    /// reload the row exactly as login does -> full tier-1 cap.
    #[tokio::test(flavor = "current_thread")]
    async fn quit_then_four_hours_offline_row_round_trip() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "lo2").await;
        // Sampled *after* the row is read back each time: the logout stamp is
        // written during the save, so a clock read taken before it can land a
        // second earlier than the stamp and under-count by 1 s.
        let now_secs = || chrono::Utc::now().timestamp();
        let load = |pool: PgPool, name: String| async move {
            mud_db::characters::find_by_name(&pool, &name)
                .await
                .unwrap()
                .unwrap()
        };

        // NULL last_logout (first login after deploy) -> nothing.
        let r = load(pool.clone(), c.name.clone()).await;
        assert_eq!(r.last_logout, None);
        assert_eq!(offline_elapsed_secs(r.last_logout, now_secs()), 0);

        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);
        let out = save_player_final(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);

        // Just quit and relogged: ~0 offline.
        let r = load(pool.clone(), c.name.clone()).await;
        assert!(offline_elapsed_secs(r.last_logout, now_secs()) <= 5);

        mud_db::sqlx::query(
            "UPDATE \"Characters\" SET last_logout = last_logout - interval '4 hours' WHERE id = $1",
        )
        .bind(&c.id)
        .execute(&pool)
        .await
        .unwrap();
        let r = load(pool.clone(), c.name.clone()).await;
        let elapsed = offline_elapsed_secs(r.last_logout, now_secs());
        assert!((4 * HOUR..=4 * HOUR + 5).contains(&elapsed), "{elapsed}");
        assert_eq!(accrue_l10(0, 1, elapsed), accrue_l10(0, 1, 40 * HOUR));
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// Every exit that goes through `retire_player` stamps `last_logout`:
    /// a dropped link out of combat, and a linkdead character timing out.
    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_and_linkdead_retirement_stamp_last_logout() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (_u1, a) = temp_unlinked_char(&pool, "lo3").await;
        let (_u2, b) = temp_unlinked_char(&pool, "lo4").await;
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();

        // Plain disconnect (not fighting): on_disconnect -> retire_player.
        let pa = spawn_player_for(&mut world, &a.id, room);
        router.playing.insert(1, pa);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get_entity(pa).is_err());
        settle_saves(&mut world).await;
        assert!(last_logout_of(&pool, &a.id).await.is_some(), "disconnect");

        // Linkdead character retired by the timeout.
        let pb = spawn_player_for(&mut world, &b.id, room);
        world
            .entity_mut(pb)
            .insert(commands::Linkdead { since_tick: 0 });
        world.insert_resource(crate::TickCount(LINKDEAD_TIMEOUT_TICKS));
        ConnRouter::drain_linkdead(&mut world, &pool);
        assert!(world.get_entity(pb).is_err());
        settle_saves(&mut world).await;
        assert!(last_logout_of(&pool, &b.id).await.is_some(), "linkdead");
        temp_cleanup(&pool, &[], &[&a.id, &b.id], &[]).await;
    }

    /// The world loop never waits on Postgres to retire a player. The pool's
    /// only connection is held, so any database round-trip would hang the
    /// test: `drain_idle_kicks` and `drain_quitting` are plain functions, the
    /// player is gone and the write is a pending task when they return, and
    /// the state still lands once the database frees up.
    #[tokio::test(flavor = "current_thread")]
    async fn idle_kick_and_quit_retire_without_waiting_for_the_database() {
        let Some((_shared, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
        let pool = mud_db::connect_with(
            &url,
            mud_db::PoolSettings {
                max_connections: 1,
                acquire_timeout: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();
        let (_u1, a) = temp_unlinked_char(&pool, "ik1").await;
        let (_u2, b) = temp_unlinked_char(&pool, "ik2").await;
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let mut router = ConnRouter::new();
        router.close_conn = |_| true;
        let room = world.spawn(mud_world::Room).id();
        let kicked = spawn_player_for(&mut world, &a.id, room);
        let quitter = spawn_player_for(&mut world, &b.id, room);
        router.playing.insert(1, kicked);
        router.playing.insert(2, quitter);
        world
            .entity_mut(kicked)
            .insert(crate::idle::IdleKickPending);
        world.entity_mut(quitter).insert(commands::Quitting);

        // The slow database: nothing can run a query until this is dropped.
        let held = pool.acquire().await.unwrap();
        crate::idle::drain_idle_kicks(&mut world, &mut router, &pool);
        router.drain_quitting(&mut world, &pool);

        assert!(world.get_entity(kicked).is_err(), "idle-kicked player gone");
        assert!(world.get_entity(quitter).is_err(), "quitter gone");
        let c = world.resource::<SaveCoordinator>().clone();
        assert_eq!(c.pending(), 2, "both writes are spawned tasks, not done");
        assert!(c.has_unsettled_saves(&a.id) && c.has_unsettled_saves(&b.id));

        drop(held);
        settle_saves(&mut world).await;
        assert!(last_logout_of(&pool, &a.id).await.is_some(), "idle kick");
        assert!(last_logout_of(&pool, &b.id).await.is_some(), "quit");
        temp_cleanup(&pool, &[], &[&a.id, &b.id], &[]).await;
    }

    /// A busy save turn (a background write ahead) must not make the world
    /// wait: the player stays in the world, marked `Retiring`, and the next
    /// `drain_retiring` after the turn frees retires and saves it. Nothing is
    /// broadcast or spawned in the meantime.
    #[tokio::test(flavor = "current_thread")]
    async fn busy_save_turn_defers_the_retirement_instead_of_waiting() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (player, _rx) = playing_in(&mut router, &mut world, room, 1, "Busy");
        let (_watcher, mut watcher_rx) = playing_in(&mut router, &mut world, room, 2, "Watcher");
        let c = world.resource::<SaveCoordinator>().clone();

        let turn = c.try_begin_ordered("c-Busy").expect("turn is free");
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get::<Retiring>(player).is_some());
        assert_eq!(c.pending(), 0, "no write without the turn");
        assert!(!drain(&mut watcher_rx).contains("fades from view"));
        ConnRouter::drain_retiring(&mut world, &pool);
        assert!(world.get_entity(player).is_ok(), "still busy: still here");

        drop(turn);
        ConnRouter::drain_retiring(&mut world, &pool);
        assert!(world.get_entity(player).is_err());
        assert_eq!(c.pending(), 1, "the save is a spawned task");
        assert!(drain(&mut watcher_rx).contains("Busy fades from view"));
    }

    /// A relog that lands while the retirement is parked cancels it (and
    /// the quit that caused it) instead of being kicked by the stale marker.
    #[tokio::test(flavor = "current_thread")]
    async fn takeover_cancels_a_parked_retirement() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (player, _rx) = playing_in(&mut router, &mut world, room, 1, "Back");
        let c = world.resource::<SaveCoordinator>().clone();
        let _turn = c.try_begin_ordered("c-Back").expect("turn is free");
        world.entity_mut(player).insert(commands::Quitting);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get::<Retiring>(player).is_some());

        let (tx, _rx2) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        router.on_connect(2, tx, None, &world);
        assert!(router.try_takeover(&mut world, 2, "c-Back"));
        assert!(world.get::<Retiring>(player).is_none());
        assert!(world.get::<commands::Quitting>(player).is_none());
        router.drain_quitting(&mut world, &pool);
        ConnRouter::drain_retiring(&mut world, &pool);
        assert!(world.get_entity(player).is_ok());
        assert_eq!(router.find_conn(player), Some(2));
    }

    /// A final save that fails (unreachable database) is handed to the retry
    /// writer without a gap: the character never looks settled in between,
    /// so a relog keeps waiting rather than loading stale rows.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_final_save_stays_unsettled_through_its_retry() {
        let mut world = World::new();
        world.insert_resource(mud_world::SocialRegistry::default());
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(crate::TickCount(0));
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        let room = world.spawn(mud_world::Room).id();
        let (player, _rx) = playing_in(&mut router, &mut world, room, 1, "Flaky");
        let c = world.resource::<SaveCoordinator>().clone();
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get_entity(player).is_err());
        assert!(c.has_unsettled_saves("c-Flaky"));
        // The first attempt dies at the 200 ms acquire timeout; the retry
        // backs off for a second after that.
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(c.has_unsettled_saves("c-Flaky"), "settled early");
        }
        assert_eq!(c.pending(), 1);
    }

    fn failing_pool() -> PgPool {
        // Connects nowhere, and gives up fast so a "failed save" test
        // doesn't sit in sqlx's default 30s acquire timeout.
        mud_db::sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .unwrap()
    }

    fn spawn_player_for(world: &mut World, cid: &str, room: Entity) -> Entity {
        world
            .spawn((
                Player,
                Account {
                    user_id: String::new(),
                    character_id: cid.to_string(),
                    role: mud_db::enums::UserRole::Player,
                    account_role: mud_db::enums::UserRole::Player,
                    perms: vec![],
                },
                Health { hp: 10, max: 10 },
                Located(room),
            ))
            .id()
    }

    /// `(id, character_id)` of every `CharacterItems` row owned by any of
    /// `cids`, ordered by id.
    async fn item_rows(pool: &PgPool, cids: &[&str]) -> Vec<(i32, String)> {
        let cids: Vec<String> = cids.iter().map(ToString::to_string).collect();
        mud_db::sqlx::query_as(
            "SELECT id, character_id FROM \"CharacterItems\" \
             WHERE character_id = ANY($1) ORDER BY id",
        )
        .bind(cids)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn first_object(pool: &PgPool) -> Option<(i32, i32)> {
        mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Objects\" LIMIT 1")
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    /// Quest rewards queued for a character reach the database even when the
    /// character quits before the tick drains them: the gold the grant
    /// already wrote must not be overwritten by the quit save, and the reward
    /// item (which has no row yet) must be saved with the pack.
    #[tokio::test(flavor = "current_thread")]
    async fn queued_quest_rewards_survive_the_quit_save() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "qrq").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(commands::DbPool(pool.clone()));
        let mut protos = mud_world::ObjectPrototypes::default();
        protos.by_key.insert(
            (oz, oid),
            crate::commands::test_support::object_proto(oz, oid, mud_db::enums::ObjectType::Other),
        );
        world.insert_resource(protos);
        let (tx, inbox_rx) = tokio::sync::mpsc::channel(8);
        world.insert_resource(commands::PlayerUpdateInbox(std::sync::Mutex::new(inbox_rx)));
        let room = world.spawn_empty().id();
        let player = spawn_player_for(&mut world, &c.id, room);
        world.entity_mut(player).insert(mud_world::Wealth(100));
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = 100 WHERE id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();

        // grant_simple_rewards wrote +50 gold to the row; the in-memory half
        // and the reward item are still waiting in the inbox when the player
        // quits.
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = wealth + 50 WHERE id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        tx.try_send(commands::PendingPlayerUpdate::WealthDelta {
            character_id: c.id.clone(),
            amount: 50,
        })
        .unwrap();
        tx.try_send(commands::PendingPlayerUpdate::SpawnItem {
            character_id: c.id.clone(),
            object_zone: oz,
            object_id: oid,
            quantity: 1,
        })
        .unwrap();
        let out = save_player_final(&mut world, player, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);

        let wealth: i64 =
            mud_db::sqlx::query_scalar("SELECT wealth FROM \"Characters\" WHERE id = $1")
                .bind(&c.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(wealth, 150, "the quit save overwrote the quest gold");
        assert_eq!(item_rows(&pool, &[&c.id]).await.len(), 1, "reward item");
        drop_item_rows(&pool, &[&c.id]).await;
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A reward item whose recipient is already gone when the inbox drains is
    /// written to their pack rows instead of being dropped.
    #[tokio::test(flavor = "current_thread")]
    async fn reward_item_for_a_departed_character_is_not_dropped() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "qrg").await;
        let mut world = World::new();
        world.insert_resource(commands::DbPool(pool.clone()));
        let (tx, inbox_rx) = tokio::sync::mpsc::channel(8);
        world.insert_resource(commands::PlayerUpdateInbox(std::sync::Mutex::new(inbox_rx)));
        tx.try_send(commands::PendingPlayerUpdate::SpawnItem {
            character_id: c.id.clone(),
            object_zone: oz,
            object_id: oid,
            quantity: 2,
        })
        .unwrap();
        commands::drain_player_updates(&mut world);
        let mut rows = 0;
        for _ in 0..60 {
            rows = item_rows(&pool, &[&c.id]).await.len();
            if rows == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(rows, 2, "reward items were dropped with the player");
        drop_item_rows(&pool, &[&c.id]).await;
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    async fn drop_item_rows(pool: &PgPool, cids: &[&str]) {
        for c in cids {
            mud_db::sqlx::query("DELETE FROM \"CharacterItems\" WHERE character_id = $1")
                .bind(c)
                .execute(pool)
                .await
                .unwrap();
        }
        temp_cleanup(pool, &[], cids, &[]).await;
    }

    /// P0 regression: an item handed from A to B must end up as exactly
    /// one row owned by B, whichever character saves first - and when only
    /// B saves (A "crashed").
    #[tokio::test(flavor = "current_thread")]
    async fn given_item_ends_as_one_row_owned_by_receiver_in_every_save_order() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        for order in ["ab", "ba", "b"] {
            let (_u, ca) = temp_unlinked_char(&pool, "ga").await;
            let (_u, cb) = temp_unlinked_char(&pool, "gb").await;
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            let room = world.spawn_empty().id();
            let a = spawn_player_for(&mut world, &ca.id, room);
            let b = spawn_player_for(&mut world, &cb.id, room);
            let item = world
                .spawn((Item, WorldKey { zone: oz, id: oid }, Located(a)))
                .id();
            assert!(save_player(&mut world, a, &pool).await.committed);
            assert!(world.get::<mud_world::PersistedItemId>(item).is_some());
            // give A -> B.
            world.entity_mut(item).insert(Located(b));
            for who in order.chars() {
                let e = if who == 'a' { a } else { b };
                let out = save_player(&mut world, e, &pool).await;
                assert!(out.committed, "{order}: {:?}", out.error);
            }
            let rows = item_rows(&pool, &[&ca.id, &cb.id]).await;
            assert_eq!(rows.len(), 1, "order {order}: {rows:?}");
            assert_eq!(rows[0].1, cb.id, "order {order}: {rows:?}");
            // Receiver's later saves keep it at one row; the id is stamped.
            assert!(save_player(&mut world, b, &pool).await.committed);
            assert_eq!(item_rows(&pool, &[&ca.id, &cb.id]).await.len(), 1);
            assert!(world.get::<mud_world::PersistedItemId>(item).is_some());
            drop_item_rows(&pool, &[&ca.id, &cb.id]).await;
        }
    }

    /// A container handed over with its contents moves as a unit, even
    /// when the giver's save deletes the old rows first (children are
    /// re-inserted under the re-inserted parent).
    #[tokio::test(flavor = "current_thread")]
    async fn given_container_with_contents_survives_either_save_order() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        for order in ["ab", "ba"] {
            let (_u, ca) = temp_unlinked_char(&pool, "ca").await;
            let (_u, cb) = temp_unlinked_char(&pool, "cb").await;
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            let room = world.spawn_empty().id();
            let a = spawn_player_for(&mut world, &ca.id, room);
            let b = spawn_player_for(&mut world, &cb.id, room);
            let bag = world
                .spawn((Item, WorldKey { zone: oz, id: oid }, Located(a)))
                .id();
            let inner = world
                .spawn((Item, WorldKey { zone: oz, id: oid }, Located(bag)))
                .id();
            assert!(save_player(&mut world, a, &pool).await.committed);
            world.entity_mut(bag).insert(Located(b));
            for who in order.chars() {
                let e = if who == 'a' { a } else { b };
                assert!(save_player(&mut world, e, &pool).await.committed);
            }
            let rows = item_rows(&pool, &[&ca.id, &cb.id]).await;
            assert_eq!(rows.len(), 2, "order {order}: {rows:?}");
            assert!(rows.iter().all(|r| r.1 == cb.id), "{rows:?}");
            let bag_id = world.get::<mud_world::PersistedItemId>(bag).unwrap().0;
            let inner_id = world.get::<mud_world::PersistedItemId>(inner).unwrap().0;
            let container: Option<i32> = mud_db::sqlx::query_scalar(
                "SELECT container_id FROM \"CharacterItems\" WHERE id = $1",
            )
            .bind(inner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(container, Some(bag_id), "order {order}");
            drop_item_rows(&pool, &[&ca.id, &cb.id]).await;
        }
    }

    /// An item dropped on the ground keeps its row id (nothing strips it):
    /// the owner's next save deletes the row (ground items are not
    /// persisted), and the next person to pick it up INSERTs a fresh one.
    #[tokio::test(flavor = "current_thread")]
    async fn dropped_item_row_is_deleted_and_pickup_inserts_fresh() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_u, ca) = temp_unlinked_char(&pool, "da").await;
        let (_u, cb) = temp_unlinked_char(&pool, "db").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let a = spawn_player_for(&mut world, &ca.id, room);
        let b = spawn_player_for(&mut world, &cb.id, room);
        let item = world
            .spawn((Item, WorldKey { zone: oz, id: oid }, Located(a)))
            .id();
        assert!(save_player(&mut world, a, &pool).await.committed);
        let old_id = world.get::<mud_world::PersistedItemId>(item).unwrap().0;

        world.entity_mut(item).insert(Located(room));
        assert_eq!(
            world.get::<mud_world::PersistedItemId>(item).unwrap().0,
            old_id,
            "ids are never stripped"
        );
        assert!(save_player(&mut world, a, &pool).await.committed);
        assert!(item_rows(&pool, &[&ca.id, &cb.id]).await.is_empty());

        world.entity_mut(item).insert(Located(b));
        assert!(save_player(&mut world, b, &pool).await.committed);
        let rows = item_rows(&pool, &[&ca.id, &cb.id]).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].1, cb.id);
        assert_ne!(rows[0].0, old_id, "a fresh row, not the dead one");
        drop_item_rows(&pool, &[&ca.id, &cb.id]).await;
    }

    /// The id stays on a dropped item, so a pick-up before the dropper's
    /// save re-homes the row and a pick-up after it re-inserts: exactly one
    /// row either way, whoever saves when, including when only the picker
    /// ever saves (the dropper "crashed") - no duplicate window.
    #[tokio::test(flavor = "current_thread")]
    async fn ground_item_pickup_ends_with_exactly_one_row() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        // (a saves between drop and pick-up, a saves after b, a saves at all)
        for (a_before, a_after) in [(true, true), (false, true), (false, false), (true, false)] {
            let (_u, ca) = temp_unlinked_char(&pool, "sa").await;
            let (_u, cb) = temp_unlinked_char(&pool, "sb").await;
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            let room = world.spawn_empty().id();
            let a = spawn_player_for(&mut world, &ca.id, room);
            let b = spawn_player_for(&mut world, &cb.id, room);
            let item = world
                .spawn((Item, WorldKey { zone: oz, id: oid }, Located(a)))
                .id();
            assert!(save_player(&mut world, a, &pool).await.committed);
            world.entity_mut(item).insert(Located(room));
            if a_before {
                assert!(save_player(&mut world, a, &pool).await.committed);
            }
            world.entity_mut(item).insert(Located(b));
            assert!(save_player(&mut world, b, &pool).await.committed);
            // Crash-equivalent: nothing else ran, so the row must already
            // be exactly B's.
            let rows = item_rows(&pool, &[&ca.id, &cb.id]).await;
            assert_eq!(rows.len(), 1, "a_before={a_before}: {rows:?}");
            assert_eq!(rows[0].1, cb.id);
            if a_after {
                assert!(save_player(&mut world, a, &pool).await.committed);
                let rows = item_rows(&pool, &[&ca.id, &cb.id]).await;
                assert_eq!(rows.len(), 1, "a_before={a_before}: {rows:?}");
                assert_eq!(rows[0].1, cb.id);
            }
            drop_item_rows(&pool, &[&ca.id, &cb.id]).await;
        }
    }

    /// Corpse looting: the dead player's items move into a corpse and then
    /// to the looter. One row, owned by the looter, whichever of the two
    /// saves first or whether only the looter saves.
    #[tokio::test(flavor = "current_thread")]
    async fn corpse_looting_ends_with_one_row_owned_by_the_looter() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        for order in ["ab", "ba", "b"] {
            let (_u, ca) = temp_unlinked_char(&pool, "ka").await;
            let (_u, cb) = temp_unlinked_char(&pool, "kb").await;
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            let room = world.spawn_empty().id();
            let a = spawn_player_for(&mut world, &ca.id, room);
            let b = spawn_player_for(&mut world, &cb.id, room);
            let item = world
                .spawn((Item, WorldKey { zone: oz, id: oid }, Located(a)))
                .id();
            assert!(save_player(&mut world, a, &pool).await.committed);
            let pid = world.get::<mud_world::PersistedItemId>(item).unwrap().0;
            // A dies: the item goes into a corpse container in the room.
            let corpse = world.spawn((Item, Located(room))).id();
            world.entity_mut(item).insert(Located(corpse));
            // B loots the corpse.
            world.entity_mut(item).insert(Located(b));
            assert_eq!(
                world.get::<mud_world::PersistedItemId>(item).unwrap().0,
                pid
            );
            for who in order.chars() {
                let e = if who == 'a' { a } else { b };
                assert!(save_player(&mut world, e, &pool).await.committed);
            }
            let rows = item_rows(&pool, &[&ca.id, &cb.id]).await;
            assert_eq!(rows.len(), 1, "order {order}: {rows:?}");
            assert_eq!(rows[0].1, cb.id, "order {order}: {rows:?}");
            drop_item_rows(&pool, &[&ca.id, &cb.id]).await;
        }
    }

    /// Items that are destroyed (sold, junked, decayed, consumed: all just
    /// despawn) lose their row at the last owner's next save, and items
    /// left on the ground or in a corpse forever do too.
    #[tokio::test(flavor = "current_thread")]
    async fn destroyed_and_abandoned_items_lose_their_rows_at_the_owners_next_save() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let (_u, ca) = temp_unlinked_char(&pool, "xa").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let a = spawn_player_for(&mut world, &ca.id, room);
        let spawn_item = |world: &mut World| {
            world
                .spawn((Item, WorldKey { zone: oz, id: oid }, Located(a)))
                .id()
        };
        let destroyed = spawn_item(&mut world);
        let abandoned = spawn_item(&mut world);
        let kept = spawn_item(&mut world);
        assert!(save_player(&mut world, a, &pool).await.committed);
        assert_eq!(item_rows(&pool, &[&ca.id]).await.len(), 3);
        world.despawn(destroyed);
        world.entity_mut(abandoned).insert(Located(room));
        assert!(save_player(&mut world, a, &pool).await.committed);
        let rows = item_rows(&pool, &[&ca.id]).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].0,
            world.get::<mud_world::PersistedItemId>(kept).unwrap().0
        );
        drop_item_rows(&pool, &[&ca.id]).await;
    }

    /// A commit that lands after the item moved (or after a newer commit
    /// stamped it) must not stamp the row id onto the entity.
    #[test]
    fn apply_commit_skips_moved_and_already_stamped_items() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let a = spawn_player_for(&mut world, "ac-a", room);
        let b = spawn_player_for(&mut world, "ac-b", room);
        let bag = world
            .spawn((Item, WorldKey { zone: 1, id: 1 }, Located(a)))
            .id();
        let in_bag = world
            .spawn((Item, WorldKey { zone: 1, id: 2 }, Located(bag)))
            .id();
        let moved = world
            .spawn((Item, WorldKey { zone: 1, id: 3 }, Located(a)))
            .id();
        let stamped = world
            .spawn((Item, WorldKey { zone: 1, id: 4 }, Located(a)))
            .id();
        let plain = world
            .spawn((Item, WorldKey { zone: 1, id: 5 }, Located(a)))
            .id();
        let snap = snapshot_player(&mut world, a, 1).unwrap();
        let idx = |e: Entity| snap.entity_for_idx.iter().position(|x| *x == e).unwrap();
        // After the snapshot: one item goes to B, one gets a newer id.
        world.entity_mut(moved).insert(Located(b));
        world
            .entity_mut(stamped)
            .insert(mud_world::PersistedItemId(99));
        let assigned = HashMap::from([
            (idx(bag), 10),
            (idx(in_bag), 11),
            (idx(moved), 12),
            (idx(stamped), 13),
            (idx(plain), 14),
        ]);
        apply_commit(&mut world, &snap, assigned);
        let pid = |e: Entity| world.get::<mud_world::PersistedItemId>(e).map(|p| p.0);
        assert_eq!(pid(bag), Some(10));
        assert_eq!(pid(in_bag), Some(11), "nested items are still reachable");
        assert_eq!(pid(moved), None, "moved off the saver: not stamped");
        assert_eq!(pid(stamped), Some(99), "existing id is not overwritten");
        assert_eq!(pid(plain), Some(14));
    }

    /// A `PendingSave` marker on something that is not a player can never
    /// be satisfied: it is removed, not retried forever.
    #[tokio::test(flavor = "current_thread")]
    async fn pending_save_on_non_player_is_dropped() {
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let mob = world.spawn(mud_world::PendingSave).id();
        drain_pending_saves(&mut world, &lazy_pool());
        assert!(world.get::<mud_world::PendingSave>(mob).is_none());
    }

    /// A failed background write re-arms the Lua `PendingSave` marker and
    /// schedules the character for a quick retry instead of a full
    /// autosave interval later.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_background_save_rearms_marker_and_retries_soon() {
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let p = spawn_player_for(&mut world, "pend-1", room);
        world.entity_mut(p).insert(mud_world::PendingSave);
        let pool = failing_pool();
        drain_pending_saves(&mut world, &pool);
        // Consumed once the write is handed off...
        assert!(world.get::<mud_world::PendingSave>(p).is_none());
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
        // ...and restored because the write failed.
        assert!(world.get::<mud_world::PendingSave>(p).is_some());
        assert!(
            crate::autosave::FAILED_SAVE_RETRY <= Duration::from_secs(10),
            "failed saves must retry within seconds"
        );
    }

    /// A failed quit-save is not lost with the despawned player: the owned
    /// snapshot is handed to the background writer.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_quit_save_is_retried_in_background_before_despawn() {
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let p = spawn_player_for(&mut world, "quit-1", room);
        let pool = failing_pool();
        let mut router = ConnRouter::new();
        router.playing.insert(1, p);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get_entity(p).is_err(), "player despawned");
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert_eq!(coordinator.pending(), 1, "retry task owns the snapshot");
    }

    /// Every exit funnels through `retire_player`: a leader who quits hands
    /// the group on, a member who quits just drops out.
    #[tokio::test(flavor = "current_thread")]
    async fn quitting_hands_the_group_on_or_drops_the_member() {
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let lead = spawn_player_for(&mut world, "grp-lead", room);
        let m1 = spawn_player_for(&mut world, "grp-m1", room);
        let m2 = spawn_player_for(&mut world, "grp-m2", room);
        for m in [m1, m2] {
            world.entity_mut(m).insert(mud_world::GroupMember(lead));
        }
        let pool = failing_pool();
        let mut router = ConnRouter::new();

        router.playing.insert(1, lead);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get_entity(lead).is_err());
        let still: Vec<Entity> = [m1, m2]
            .into_iter()
            .filter(|m| world.get::<mud_world::GroupMember>(*m).is_some())
            .collect();
        assert_eq!(still.len(), 1, "one member now leads, the other follows");
        let new_lead = if still[0] == m1 { m2 } else { m1 };
        assert_eq!(mud_world::group_root(&world, still[0]), new_lead);

        // The remaining member quits: the two-person group is gone.
        router.playing.insert(2, still[0]);
        router.on_disconnect(&mut world, 2, &pool);
        assert_eq!(
            mud_world::group_members(&mut world, new_lead),
            vec![new_lead]
        );
    }

    /// Quitting saves the persistent pets and mounts, then removes them from
    /// the world: login respawns them from the save, so leaving them standing
    /// would leave a duplicate beside the respawned one on every relog.
    #[tokio::test(flavor = "current_thread")]
    async fn quitting_removes_the_saved_pets_and_mounts_and_nothing_else() {
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let me = spawn_player_for(&mut world, "pet-me", room);
        let other = spawn_player_for(&mut world, "pet-other", room);
        let mob = |world: &mut World, name: &str| {
            world
                .spawn((
                    Mob,
                    Named { name: name.into() },
                    WorldKey { zone: 30, id: 1 },
                    Health { hp: 20, max: 20 },
                    Located(room),
                ))
                .id()
        };
        let mount = mob(&mut world, "a mare");
        world
            .entity_mut(mount)
            .insert((Follower(me), mud_world::PersistentPet));
        world.entity_mut(mount).insert(mud_world::RiddenBy(me));
        world.entity_mut(me).insert(mud_world::Mounted(mount));
        let wolf = mob(&mut world, "a wolf");
        world
            .entity_mut(wolf)
            .insert((Follower(me), mud_world::PersistentPet));
        let tagalong = mob(&mut world, "a tagalong");
        world.entity_mut(tagalong).insert(Follower(me));
        let others_pet = mob(&mut world, "another's cat");
        world
            .entity_mut(others_pet)
            .insert((Follower(other), mud_world::PersistentPet));

        let snap = snapshot_player(&mut world, me, 1).expect("snapshot");
        let saved: PersistedPets =
            serde_json::from_value(snap.pets_json.expect("pets recorded")).unwrap();
        assert_eq!(saved.pets.len(), 2, "mount and wolf are saved");

        let pool = failing_pool();
        let mut router = ConnRouter::new();
        router.playing.insert(1, me);
        router.on_disconnect(&mut world, 1, &pool);
        assert!(world.get_entity(me).is_err());
        assert!(world.get_entity(mount).is_err(), "saved mount despawned");
        assert!(world.get_entity(wolf).is_err(), "saved pet despawned");
        assert!(world.get_entity(tagalong).is_ok(), "unsaved follower stays");
        assert!(world.get_entity(others_pet).is_ok(), "other's pet stays");
    }

    /// Gear handed to a pet (carried or worn) must not die with the pet when
    /// its owner quits: it moves into the owner's pack before the final save,
    /// so the next login finds it there.
    #[tokio::test(flavor = "current_thread")]
    async fn gear_on_a_pet_is_saved_with_the_owner_on_quit() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "petgear").await;
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let me = spawn_player_for(&mut world, &c.id, room);
        let wolf = world
            .spawn((
                Mob,
                Named {
                    name: "a wolf".into(),
                },
                WorldKey { zone: 30, id: 1 },
                Health { hp: 20, max: 20 },
                Located(room),
                Follower(me),
                mud_world::PersistentPet,
            ))
            .id();
        world.spawn((Item, WorldKey { zone: oz, id: oid }, Located(wolf)));
        world.spawn((
            Item,
            WorldKey { zone: oz, id: oid },
            Located(wolf),
            EquippedSlot(mud_world::Slot::Wield),
        ));

        let mut router = ConnRouter::new();
        router.playing.insert(1, me);
        router.on_disconnect(&mut world, 1, &pool);
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);

        assert!(world.get_entity(wolf).is_err(), "pet despawned");
        assert!(world.get_entity(me).is_err(), "owner despawned");
        let rows: Vec<(i32, Option<String>)> = mud_db::sqlx::query_as(
            "SELECT id, equipped_location::text FROM \"CharacterItems\" \
             WHERE character_id = $1 ORDER BY id",
        )
        .bind(&c.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2, "both the carried and the worn item saved");
        drop_item_rows(&pool, &[&c.id]).await;
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A reward item for an offline character is granted under their save
    /// turn. While a login holds that turn (reading the pack) the grant
    /// waits; once the character is live it goes back through the world
    /// instead of inserting a row the session's first save would delete.
    #[tokio::test(flavor = "current_thread")]
    async fn offline_item_grant_waits_for_login_and_then_goes_through_the_world() {
        let mut world = World::new();
        let coordinator = SaveCoordinator::default();
        world.insert_resource(coordinator.clone());
        world.insert_resource(commands::DbPool(failing_pool()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        world.insert_resource(commands::PlayerUpdateTx(tx));

        // Login mid-load: holds the turn.
        let login_turn = coordinator.begin_ordered("grant-cid").await;
        commands::grant_item_to_offline(&world, "grant-cid".to_string(), 3, 4, 2);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "the grant must wait for the turn");
        // Login finishes: the entity exists.
        coordinator.set_session_live("grant-cid", true);
        drop(login_turn);
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("re-sent")
            .unwrap();
        assert!(matches!(
            msg,
            commands::PendingPlayerUpdate::SpawnItem {
                ref character_id,
                object_zone: 3,
                object_id: 4,
                quantity: 2,
            } if character_id == "grant-cid"
        ));
        assert!(coordinator.flush(&mut world, Duration::from_secs(5)).await);
    }

    /// Login re-reads the character row under their turn and discards
    /// updates queued for the (absent) character: the reward gold was
    /// written to the database before it was queued, so a row read after it
    /// already holds it and applying the queued delta would pay it twice.
    #[tokio::test(flavor = "current_thread")]
    async fn login_reads_a_fresh_row_and_does_not_replay_queued_rewards() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "freshrow").await;
        let stale = (*c).clone();
        // The reward is written, queued, and then the player (whose row was
        // read at auth time, before the write) finishes logging in.
        mud_db::sqlx::query("UPDATE \"Characters\" SET wealth = wealth + 250 WHERE id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        let mut world = auth_world(3);
        enter_game_room(&mut world);
        world.insert_resource(mud_world::RaceDefaults::default());
        world.insert_resource(SaveCoordinator::default());
        let (tx, inbox_rx) = tokio::sync::mpsc::channel(8);
        world.insert_resource(commands::PlayerUpdateInbox(std::sync::Mutex::new(inbox_rx)));
        tx.try_send(commands::PendingPlayerUpdate::WealthDelta {
            character_id: c.id.clone(),
            amount: 250,
        })
        .unwrap();
        let (mut router, _orx) = load_guard_router(&world);
        router
            .complete_login(1, &mut world, &pool, user, stale.clone())
            .await;
        let entity = *router.playing.get(&1).expect("logged in");
        assert_eq!(
            world.get::<Wealth>(entity).unwrap().0,
            stale.wealth + 250,
            "stale row or double-applied reward"
        );
        // The queued delta was consumed by the login, not left for the tick.
        commands::drain_player_updates(&mut world);
        assert_eq!(world.get::<Wealth>(entity).unwrap().0, stale.wealth + 250);
        assert!(
            world.resource::<SaveCoordinator>().session_live(&c.id),
            "login marks the character live"
        );
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A throwaway house (one foyer) for `cid`. Returns `(house_id,
    /// foyer_row_id)`; `None` (test skipped) without a Rooms row to hang the
    /// entrance on.
    async fn temp_house(pool: &PgPool, cid: &str) -> Option<(i32, i32)> {
        let (zone, id): (i32, i32) =
            mud_db::sqlx::query_as("SELECT zone_id, id FROM \"Room\" LIMIT 1")
                .fetch_optional(pool)
                .await
                .unwrap()?;
        Some(
            mud_db::housing::create_house(pool, cid, zone, id)
                .await
                .unwrap(),
        )
    }

    async fn house_item_count(pool: &PgPool, foyer_row_id: i32) -> i64 {
        mud_db::sqlx::query_scalar("SELECT COUNT(*) FROM player_house_items WHERE room_id = $1")
            .bind(foyer_row_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// What `house place` does to the world before it hands the write off.
    fn place_in_world(
        world: &mut World,
        item: Entity,
        house_room: Entity,
        character_id: &str,
        foyer_row_id: i32,
        key: (i32, i32),
    ) {
        let inventory_row_id = world.get::<mud_world::PersistedItemId>(item).map(|p| p.0);
        let placement = mud_world::HousePlacement::pending();
        world
            .entity_mut(item)
            .insert(Located(house_room))
            .remove::<mud_world::PersistedItemId>()
            .insert(placement.clone());
        crate::house_items::persist_placement(
            world,
            crate::house_items::PlacementWrite {
                character_id: character_id.to_string(),
                item,
                room_row_id: foyer_row_id,
                object_zone_id: key.0,
                object_id: key.1,
                custom: mud_db::housing::HouseItemCustom::default(),
                inventory_row_id,
                placement,
            },
        );
    }

    /// `house place` followed at once by `quit` (and a shutdown right
    /// after): the placement's turn was queued before the quit save asked
    /// for its own, so the quit waits for the house row instead of dropping
    /// the pack row first and leaving the item in neither place. A `get` in
    /// between ends with one pack row and no house row.
    #[tokio::test(flavor = "current_thread")]
    async fn place_then_quit_never_loses_the_item() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(key) = first_object(&pool).await else {
            return;
        };
        for take_back in [false, true] {
            let (_user, c) = temp_unlinked_char(&pool, "plq").await;
            let Some((house_id, foyer)) = temp_house(&pool, &c.id).await else {
                temp_cleanup(&pool, &[], &[&c.id], &[]).await;
                return;
            };
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            world.insert_resource(commands::DbPool(pool.clone()));
            crate::house_items::register_observers(&mut world);
            let room = world.spawn_empty().id();
            let house_room = world
                .spawn(mud_world::HouseRoom {
                    house_id,
                    local_index: 0,
                })
                .id();
            let me = spawn_player_for(&mut world, &c.id, room);
            let item = world
                .spawn((
                    Item,
                    WorldKey {
                        zone: key.0,
                        id: key.1,
                    },
                    Located(me),
                ))
                .id();
            assert!(save_player(&mut world, me, &pool).await.committed);

            place_in_world(&mut world, item, house_room, &c.id, foyer, key);
            if take_back {
                world.entity_mut(item).insert(Located(me));
            }
            let out = save_player_final(&mut world, me, &pool).await;
            assert!(out.committed, "{:?}", out.error);
            // Shutdown right behind the quit.
            let coordinator = world.resource::<SaveCoordinator>().clone();
            assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);

            let pack = item_rows(&pool, &[&c.id]).await.len();
            let house = house_item_count(&pool, foyer).await;
            if take_back {
                assert_eq!((pack, house), (1, 0), "taken back: one pack row only");
            } else {
                assert_eq!((pack, house), (0, 1), "placed: the house row only");
            }
            mud_db::housing::delete_house(&pool, house_id)
                .await
                .unwrap();
            drop_item_rows(&pool, &[&c.id]).await;
        }
    }

    /// A house row for `foyer` holding prototype `key`, returned with the
    /// in-world item standing in `house_room` the way a boot load builds it.
    async fn house_item_in_world(
        pool: &PgPool,
        world: &mut World,
        foyer: i32,
        house_room: Entity,
        key: (i32, i32),
    ) -> (i32, Entity) {
        let row = mud_db::housing::place_item(
            pool,
            foyer,
            key.0,
            key.1,
            &mud_db::housing::HouseItemCustom::default(),
            None,
        )
        .await
        .unwrap();
        let item = world
            .spawn((
                Item,
                WorldKey {
                    zone: key.0,
                    id: key.1,
                },
                mud_world::HouseItem(row),
                Located(house_room),
            ))
            .id();
        (row, item)
    }

    /// Picking a placed item up deletes its house row in the transaction that
    /// saves the pack: a save that fails leaves the item in the house (and
    /// out of the pack) in the database, and the save that lands writes one
    /// pack row and no house row. Never both, never neither.
    #[tokio::test(flavor = "current_thread")]
    async fn house_pickup_and_pack_save_commit_together() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(key) = first_object(&pool).await else {
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "hpk").await;
        let Some((house_id, foyer)) = temp_house(&pool, &c.id).await else {
            temp_cleanup(&pool, &[], &[&c.id], &[]).await;
            return;
        };
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(commands::DbPool(pool.clone()));
        crate::house_items::register_observers(&mut world);
        let room = world.spawn_empty().id();
        let house_room = world
            .spawn(mud_world::HouseRoom {
                house_id,
                local_index: 0,
            })
            .id();
        let me = spawn_player_for(&mut world, &c.id, room);
        let (row, item) = house_item_in_world(&pool, &mut world, foyer, house_room, key).await;

        // `get`: the observer hands the row to the player's next save.
        world.entity_mut(item).insert(Located(me));
        assert_eq!(
            world
                .get::<crate::house_items::PendingHouseDeletes>(me)
                .map(|p| p.0.clone()),
            Some(vec![row])
        );
        assert!(world.get::<mud_world::HouseItem>(item).is_none());

        // The database is down for the first save: nothing changed there.
        let out = save_player(&mut world, me, &failing_pool()).await;
        assert!(!out.committed);
        assert_eq!(
            house_item_count(&pool, foyer).await,
            1,
            "still in the house"
        );
        assert!(
            item_rows(&pool, &[&c.id]).await.is_empty(),
            "not in the pack"
        );
        assert_eq!(
            world
                .get::<crate::house_items::PendingHouseDeletes>(me)
                .map(|p| p.0.clone()),
            Some(vec![row]),
            "the failed save keeps the delete queued"
        );

        // The next save lands pack row and delete together.
        let out = save_player(&mut world, me, &pool).await;
        assert!(out.committed, "{:?}", out.error);
        assert_eq!(house_item_count(&pool, foyer).await, 0, "left the house");
        assert_eq!(item_rows(&pool, &[&c.id]).await.len(), 1, "in the pack");
        assert!(
            world
                .get::<crate::house_items::PendingHouseDeletes>(me)
                .is_none(),
            "committed deletes are settled"
        );
        mud_db::housing::delete_house(&pool, house_id)
            .await
            .unwrap();
        drop_item_rows(&pool, &[&c.id]).await;
    }

    /// An `order`ed pet picks a placed item up and the owner quits: the
    /// item moves into the owner's pack and the house row goes in the same
    /// commit as that pack row.
    #[tokio::test(flavor = "current_thread")]
    async fn pet_pickup_of_a_house_item_survives_the_owners_quit() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(key) = first_object(&pool).await else {
            return;
        };
        let (_user, c) = temp_unlinked_char(&pool, "hpp").await;
        let Some((house_id, foyer)) = temp_house(&pool, &c.id).await else {
            temp_cleanup(&pool, &[], &[&c.id], &[]).await;
            return;
        };
        let mut world = World::new();
        world.insert_resource(SaveCoordinator::default());
        world.insert_resource(commands::DbPool(pool.clone()));
        crate::house_items::register_observers(&mut world);
        let room = world.spawn_empty().id();
        let house_room = world
            .spawn(mud_world::HouseRoom {
                house_id,
                local_index: 0,
            })
            .id();
        let me = spawn_player_for(&mut world, &c.id, room);
        let wolf = world
            .spawn((
                Mob,
                Named {
                    name: "a wolf".into(),
                },
                WorldKey { zone: 30, id: 1 },
                Health { hp: 20, max: 20 },
                Located(house_room),
                Follower(me),
                mud_world::PersistentPet,
            ))
            .id();
        let (_row, item) = house_item_in_world(&pool, &mut world, foyer, house_room, key).await;
        world.entity_mut(item).insert(Located(wolf));

        let mut router = ConnRouter::new();
        router.playing.insert(1, me);
        router.on_disconnect(&mut world, 1, &pool);
        let coordinator = world.resource::<SaveCoordinator>().clone();
        assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);

        assert_eq!(house_item_count(&pool, foyer).await, 0, "left the house");
        assert_eq!(item_rows(&pool, &[&c.id]).await.len(), 1, "owner has it");
        mud_db::housing::delete_house(&pool, house_id)
            .await
            .unwrap();
        drop_item_rows(&pool, &[&c.id]).await;
    }

    /// A picker with no save to carry the delete (a scavenger mob) gets a
    /// tracked delete that keeps retrying through failures.
    #[tokio::test(flavor = "current_thread")]
    async fn row_delete_retries_until_it_succeeds() {
        let attempts = std::sync::atomic::AtomicU32::new(0);
        crate::house_items::retry_until_ok("test delete", 7, || async {
            if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                Err("db blip")
            } else {
                Ok(())
            }
        })
        .await;
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    /// A mob (no `Account`) taking a placed item queues no player delete.
    #[test]
    fn mob_pickup_of_a_house_item_is_not_queued_on_a_player() {
        let mut world = World::new();
        crate::house_items::register_observers(&mut world);
        let room = world.spawn_empty().id();
        let house_room = world
            .spawn(mud_world::HouseRoom {
                house_id: 1,
                local_index: 0,
            })
            .id();
        let player = spawn_player_for(&mut world, "pc-x", room);
        let mob = world.spawn((Mob, Located(room))).id();
        let item = world
            .spawn((Item, mud_world::HouseItem(9), Located(house_room)))
            .id();
        world.entity_mut(item).insert(Located(mob));
        assert!(
            world.get::<mud_world::HouseItem>(item).is_none(),
            "released"
        );
        assert!(
            world
                .get::<crate::house_items::PendingHouseDeletes>(player)
                .is_none()
        );
    }

    /// A placement whose insert fails does not strand the item in the house
    /// room with no row behind it: it goes back to the player's pack with a
    /// message, even when the player quits while the insert is still failing
    /// (the quit save waits for the placement's turn and drains the hand-back
    /// before its snapshot).
    #[tokio::test(flavor = "current_thread")]
    async fn failed_house_placement_returns_the_item_to_the_pack() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(key) = first_object(&pool).await else {
            return;
        };
        for quit in [false, true] {
            let (_user, c) = temp_unlinked_char(&pool, "hpf").await;
            let mut world = World::new();
            world.insert_resource(SaveCoordinator::default());
            world.insert_resource(commands::DbPool(pool.clone()));
            let (tx, inbox_rx) = tokio::sync::mpsc::channel(8);
            world.insert_resource(commands::PlayerUpdateTx(tx));
            world.insert_resource(commands::PlayerUpdateInbox(std::sync::Mutex::new(inbox_rx)));
            crate::house_items::register_observers(&mut world);
            let room = world.spawn_empty().id();
            let house_room = world
                .spawn(mud_world::HouseRoom {
                    house_id: 1,
                    local_index: 0,
                })
                .id();
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(16);
            let me = spawn_player_for(&mut world, &c.id, room);
            world.entity_mut(me).insert(Connection(out_tx));
            let item = world
                .spawn((
                    Item,
                    Named {
                        name: "a plain sword".into(),
                    },
                    WorldKey {
                        zone: key.0,
                        id: key.1,
                    },
                    Located(me),
                ))
                .id();
            assert!(save_player(&mut world, me, &pool).await.committed);

            // A room row that does not exist: the insert is refused.
            place_in_world(&mut world, item, house_room, &c.id, -1, key);
            assert_eq!(world.get::<Located>(item).unwrap().0, house_room);
            let coordinator = world.resource::<SaveCoordinator>().clone();
            if quit {
                let out = save_player_final(&mut world, me, &pool).await;
                assert!(out.committed, "{:?}", out.error);
            } else {
                assert!(coordinator.flush(&mut world, Duration::from_secs(10)).await);
                commands::drain_player_updates(&mut world);
                assert!(
                    drain(&mut out_rx).contains("could not be stored in your house"),
                    "player told"
                );
            }
            assert_eq!(
                world.get::<Located>(item).unwrap().0,
                me,
                "back in the pack"
            );
            assert!(world.get::<mud_world::HousePlacement>(item).is_none());
            if !quit {
                assert!(save_player(&mut world, me, &pool).await.committed);
            }
            assert_eq!(
                item_rows(&pool, &[&c.id]).await.len(),
                1,
                "saved in the pack"
            );
            drop_item_rows(&pool, &[&c.id]).await;
        }
    }

    static RELOG_RETRY: &[Duration] = &[Duration::from_millis(150)];
    static RELOG_NEVER: &[Duration] = &[Duration::from_secs(30)];

    /// Relog while the previous session's quit-save is still being retried:
    /// login waits (without blocking the loop), then reloads the character
    /// so it starts from what that save wrote, not the stale pre-save row.
    #[tokio::test(flavor = "current_thread")]
    async fn relog_waits_for_the_pending_quit_save_and_loads_its_state() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "rl").await;
        let stale = (*c).clone();
        assert_eq!(stale.hit_points, 10, "stale row as read at auth time");
        let mut world = auth_world(3);
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let p = spawn_player_for(&mut world, &c.id, room);
        world.get_mut::<Health>(p).unwrap().hp = 3;
        let snap = snapshot_player(&mut world, p, 1).unwrap();
        world.despawn(p);
        // The quit-save failed once; its retry lands ~150 ms later.
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_w = std::sync::Arc::clone(&attempts);
        let wpool = pool.clone();
        let coordinator = world.resource::<SaveCoordinator>().clone();
        coordinator.retry_failed_snapshot(
            snap,
            move |snap| {
                let n = attempts_w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let wpool = wpool.clone();
                async move {
                    if n == 0 {
                        Err("db blip".to_string())
                    } else {
                        write_snapshot(&wpool, &snap)
                            .await
                            .map_err(|e| e.to_string())
                    }
                }
            },
            RELOG_RETRY,
        );

        let mut router = ConnRouter::new();
        let mut arx = router.take_auth_rx().unwrap();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        drain(&mut orx);
        router
            .complete_login(1, &mut world, &pool, user, (*c).clone())
            .await;
        // Returns immediately: the wait runs off the loop.
        assert!(drain(&mut orx).contains("Saving your previous session, please wait..."));
        assert!(matches!(
            router.login.get(&1).unwrap().stage,
            Stage::Authenticating
        ));
        let done = tokio::time::timeout(Duration::from_secs(10), arx.recv())
            .await
            .expect("wait resolves")
            .unwrap();
        let AuthDoneKind::SaveSettled { settled, .. } = done.kind else {
            panic!("expected SaveSettled");
        };
        assert!(settled, "the retry landed within the wait");
        assert!(attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        let fresh = reload_character_row(&pool, &stale, None).await.unwrap();
        assert_eq!(fresh.hit_points, 3, "login must see the saved state");
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// If the previous session's save is still failing when the wait runs
    /// out, login is refused rather than loading stale state.
    #[tokio::test(flavor = "current_thread")]
    async fn relog_is_refused_while_the_previous_save_keeps_failing() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "rf").await;
        let mut world = auth_world(3);
        world.insert_resource(SaveCoordinator::default());
        let room = world.spawn_empty().id();
        let p = spawn_player_for(&mut world, &c.id, room);
        let snap = snapshot_player(&mut world, p, 1).unwrap();
        world.despawn(p);
        let coordinator = world.resource::<SaveCoordinator>().clone();
        coordinator.retry_failed_snapshot(
            snap,
            |_| async { Err::<HashMap<usize, i32>, _>("db down".to_string()) },
            RELOG_NEVER,
        );

        let mut router = ConnRouter::new();
        router.save_wait = Duration::from_millis(120);
        thread_local! {
            static CLOSED: std::cell::RefCell<Vec<ConnId>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        router.close_conn = |c| {
            CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let mut arx = router.take_auth_rx().unwrap();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, &world);
        drain(&mut orx);
        router
            .complete_login(1, &mut world, &pool, user, (*c).clone())
            .await;
        assert!(drain(&mut orx).contains("Saving your previous session"));
        let done = tokio::time::timeout(Duration::from_secs(10), arx.recv())
            .await
            .expect("wait resolves")
            .unwrap();
        router.on_auth_done(done, &pool, &mut world).await;
        let text = drain(&mut orx);
        assert!(
            text.contains("Your previous session is still being saved; try again in a minute."),
            "{text}"
        );
        assert!(!router.login.contains_key(&1));
        assert_eq!(CLOSED.with(|v| v.borrow().clone()), vec![1]);
        assert!(
            world.query::<&Player>().iter(&world).next().is_none(),
            "no session was spawned"
        );
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    async fn temp_cleanup(pool: &PgPool, code_ids: &[&str], char_ids: &[&str], user_ids: &[&str]) {
        for id in code_ids {
            mud_db::game_login_code::delete(pool, id).await.unwrap();
        }
        for id in char_ids {
            mud_db::sqlx::query("DELETE FROM \"Characters\" WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
        for id in user_ids {
            mud_db::sqlx::query("DELETE FROM \"BanRecords\" WHERE user_id = $1 OR banned_by = $1")
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
            mud_db::sqlx::query("DELETE FROM \"Users\" WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_for_unlinked_character_inserts_null_user_row_and_ignores_throttles() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "nul").await;
        let (char_id, char_name) = (c.id.clone(), c.name.clone());
        let mut world = auth_world(3);
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, Some("198.51.100.20:3000".parse().unwrap()), &world);
        drain(&mut orx);
        // The per-name throttle is tripped and one more wrong password
        // would drop the connection: `code` is blocked by neither.
        let key = LegacyLoginThrottle::key(&char_name);
        for _ in 0..3 {
            router
                .legacy_throttle
                .record_failure(&key, Instant::now(), 3, lock_window(15));
        }
        assert!(
            router
                .legacy_throttle
                .locked_for(&key, Instant::now(), 3, lock_window(15))
                .is_some()
        );
        router.login.get_mut(&1).unwrap().failed_attempts = MAX_FAILED_PASSWORDS_PER_CONN - 1;
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: Some(c),
            game_hash: "irrelevant".into(),
        };
        router.on_line(1, "code".into(), &pool, &mut world).await;
        let out = drain(&mut orx);
        let (code_id, code) = pending_web(&router, 1);
        assert!(
            out.contains(&format!("Your login code is {}.", format_login_code(&code))),
            "{out}"
        );
        assert!(
            out.contains(
                "This character isn't linked to a website account yet; you'll be asked \
                 to link it when you approve."
            ),
            "{out}"
        );
        assert_eq!(
            router.login.get(&1).unwrap().failed_attempts,
            MAX_FAILED_PASSWORDS_PER_CONN - 1
        );
        let (status, user_id, name): (String, Option<String>, String) = mud_db::sqlx::query_as(
            "SELECT status::text, \"userId\", \"characterName\" FROM \"GameLoginCode\" \
             WHERE id = $1",
        )
        .bind(&code_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        temp_cleanup(&pool, &[&code_id], &[&char_id], &[]).await;
        assert_eq!(status, "PENDING");
        assert_eq!(user_id, None);
        assert_eq!(name, char_name);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlinked_code_approved_after_link_logs_in_as_linked_user() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(uid) = temp_user(&pool, "unlink").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        // Ban the temp account so a proceeding login stops at the ban
        // gate in `finish_password` (a bare test World can't spawn a
        // player); that gate only runs for the account the login
        // continued as.
        mud_db::bans::ban(&pool, &uid, &uid, "unlinked-code test", None)
            .await
            .unwrap();
        let (user, c) = temp_unlinked_char(&pool, "lnk").await;
        let char_id = c.id.clone();
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, Some("198.51.100.21:3001".parse().unwrap()), &world);
        drain(&mut orx);
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: Some(c),
            game_hash: String::new(),
        };
        router.on_line(1, "code".into(), &pool, &mut world).await;
        let (code_id, _) = pending_web(&router, 1);
        drain(&mut orx);
        // Website side: link the character, then approve as that user.
        mud_db::sqlx::query("UPDATE \"Characters\" SET user_id = $2 WHERE id = $1")
            .bind(&char_id)
            .bind(&uid)
            .execute(&pool)
            .await
            .unwrap();
        mud_db::sqlx::query(
            "UPDATE \"GameLoginCode\" SET status = 'APPROVED', \"approvedByUserId\" = $2 \
             WHERE id = $1",
        )
        .bind(&code_id)
        .bind(&uid)
        .execute(&pool)
        .await
        .unwrap();
        router.on_line(1, String::new(), &pool, &mut world).await;
        let out = drain(&mut orx);
        let st = mud_db::game_login_code::state(&pool, &code_id)
            .await
            .unwrap()
            .unwrap();
        let stage_is_ident = matches!(
            router.login.get(&1).map(|c| &c.stage),
            Some(Stage::AwaitingIdentifier)
        );
        temp_cleanup(&pool, &[&code_id], &[&char_id], &[&uid]).await;
        assert!(out.contains("Code approved"), "{out}");
        assert!(out.contains("Your account is banned"), "{out}");
        assert!(!out.contains("did not link"), "{out}");
        assert!(stage_is_ident);
        assert_eq!(st.status, "CONSUMED");
        assert_eq!(st.approved_by_user_id.as_deref(), Some(uid.as_str()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlinked_code_approved_without_link_is_rejected_and_expired() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some(uid) = temp_user(&pool, "nolink").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let Some(other) = temp_user(&pool, "nolinkb").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let mut code_ids = Vec::new();
        let mut char_ids = Vec::new();
        // Conn 1: character never linked. Conn 2: linked to a different
        // account than the approver.
        for (conn, port, link_to) in [(1, 3002u16, None), (2, 3003, Some(&other))] {
            let (tx, mut orx) = tokio::sync::mpsc::channel(64);
            let peer = format!("198.51.100.{}:{port}", 30 + conn);
            router.on_connect(conn, tx, Some(peer.parse().unwrap()), &world);
            drain(&mut orx);
            let (user, c) = temp_unlinked_char(&pool, "nlk").await;
            char_ids.push(c.id.clone());
            if let Some(owner) = link_to {
                mud_db::sqlx::query("UPDATE \"Characters\" SET user_id = $2 WHERE id = $1")
                    .bind(&c.id)
                    .bind(owner)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            router.login.get_mut(&conn).unwrap().stage = Stage::AwaitingPassword {
                user,
                preselected: Some(c),
                game_hash: String::new(),
            };
            router.on_line(conn, "code".into(), &pool, &mut world).await;
            let (code_id, _) = pending_web(&router, conn);
            code_ids.push(code_id.clone());
            drain(&mut orx);
            mud_db::sqlx::query(
                "UPDATE \"GameLoginCode\" SET status = 'APPROVED', \"approvedByUserId\" = $2 \
                 WHERE id = $1",
            )
            .bind(&code_id)
            .bind(&uid)
            .execute(&pool)
            .await
            .unwrap();
            router.on_line(conn, String::new(), &pool, &mut world).await;
            let out = drain(&mut orx);
            let st = mud_db::game_login_code::state(&pool, &code_id)
                .await
                .unwrap()
                .unwrap();
            let at_ident = matches!(
                router.login.get(&conn).map(|c| &c.stage),
                Some(Stage::AwaitingIdentifier)
            );
            if !(out.contains("Approval did not link this character.")
                && !out.contains("Code approved")
                && at_ident
                && st.status == "EXPIRED")
            {
                let ids: Vec<&str> = code_ids.iter().map(String::as_str).collect();
                let cids: Vec<&str> = char_ids.iter().map(String::as_str).collect();
                temp_cleanup(&pool, &ids, &cids, &[&uid, &other]).await;
                panic!(
                    "conn {conn}: out={out:?} ident={at_ident} status={}",
                    st.status
                );
            }
        }
        let ids: Vec<&str> = code_ids.iter().map(String::as_str).collect();
        let cids: Vec<&str> = char_ids.iter().map(String::as_str).collect();
        temp_cleanup(&pool, &ids, &cids, &[&uid, &other]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_approval_denied_cancel_and_wake() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        // Own throwaway account: borrowing "the first Users row" races
        // with other tests that create and delete users (FK violation on
        // the code insert -> "not awaiting web approval").
        let Some(uid) = temp_user(&pool, "dctest").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        // TLS connection id, no peer address.
        let conn: ConnId = (1u64 << 40) | 1;
        router.on_connect(conn, tx, None, &world);
        drain(&mut orx);
        let mut user = linked_user();
        user.id = uid.clone();

        // Denied while waiting: the poller wakes the loop.
        router.login.get_mut(&conn).unwrap().stage = Stage::AwaitingPassword {
            user: user.clone(),
            preselected: None,
            game_hash: String::new(),
        };
        router.on_line(conn, "CODE".into(), &pool, &mut world).await;
        let (code_id, _) = pending_web(&router, conn);
        assert!(
            !drain(&mut orx).contains("Security notice"),
            "no notice on TLS"
        );
        mud_db::sqlx::query("UPDATE \"GameLoginCode\" SET status = 'DENIED' WHERE id = $1")
            .bind(&code_id)
            .execute(&pool)
            .await
            .unwrap();
        let done = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("poller should wake on status change")
            .unwrap();
        router.on_auth_done(done, &pool, &mut world).await;
        assert!(drain(&mut orx).contains("denied"));
        assert!(matches!(
            router.login.get(&conn).unwrap().stage,
            Stage::AwaitingIdentifier
        ));
        let st = mud_db::game_login_code::state(&pool, &code_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(st.status, "DENIED");
        mud_db::game_login_code::delete(&pool, &code_id)
            .await
            .unwrap();

        // Cancel marks the row EXPIRED and returns to the identifier.
        router.login.get_mut(&conn).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: None,
            game_hash: String::new(),
        };
        router.on_line(conn, "code".into(), &pool, &mut world).await;
        let (code_id, _) = pending_web(&router, conn);
        router
            .on_line(conn, "Cancel".into(), &pool, &mut world)
            .await;
        assert!(matches!(
            router.login.get(&conn).unwrap().stage,
            Stage::AwaitingIdentifier
        ));
        let st = mud_db::game_login_code::state(&pool, &code_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(st.status, "EXPIRED");
        mud_db::game_login_code::delete(&pool, &code_id)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[], &[&uid]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_requests_are_rate_limited_per_ip() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        // Own throwaway account: borrowing "the first Users row" races
        // with other tests that create and delete users (FK violation on
        // the code insert -> "not awaiting web approval").
        let Some(uid) = temp_user(&pool, "dctest").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let mut world = auth_world(0);
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(256);
        router.on_connect(1, tx, Some("203.0.113.9:1000".parse().unwrap()), &world);
        drain(&mut orx);
        let mut user = linked_user();
        user.id = uid.clone();
        let mut ids = Vec::new();
        for i in 0..=CODE_RATE_MAX {
            router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
                user: user.clone(),
                preselected: None,
                game_hash: String::new(),
            };
            router.on_line(1, "code".into(), &pool, &mut world).await;
            let out = drain(&mut orx);
            if i < CODE_RATE_MAX {
                ids.push(pending_web(&router, 1).0);
            } else {
                assert!(
                    out.contains("Too many login codes requested; try again later."),
                    "{out}"
                );
                assert!(matches!(
                    router.login.get(&1).unwrap().stage,
                    Stage::AwaitingIdentifier
                ));
            }
        }
        for id in ids {
            mud_db::game_login_code::delete(&pool, &id).await.unwrap();
        }
        temp_cleanup(&pool, &[], &[], &[&uid]).await;
    }

    // ---- lockout interplay with device-code login ----

    fn locked_user(failed: i32) -> User {
        let mut u = linked_user();
        u.failed_login_attempts = failed;
        u.locked_until = Some(chrono::Utc::now().naive_utc() + chrono::Duration::minutes(10));
        u
    }

    #[tokio::test(flavor = "current_thread")]
    async fn locked_account_password_is_rejected_with_hint_and_no_counting() {
        let mut world = auth_world(3);
        let pool = lazy_pool();
        let mut router = ConnRouter::new();
        let mut rx = router.take_auth_rx().unwrap();
        let (tx, mut orx) = tokio::sync::mpsc::channel(256);
        router.on_connect(1, tx, None, &world);
        drain(&mut orx);
        let game_hash = bcrypt::hash("game-pass", 4).unwrap();
        // Well past the per-connection failure cap: none of these count.
        for _ in 0..(MAX_FAILED_PASSWORDS_PER_CONN * 2) {
            router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
                user: locked_user(3),
                preselected: None,
                game_hash: game_hash.clone(),
            };
            // Even the CORRECT game password is refused while locked.
            router
                .on_line(1, "game-pass".into(), &pool, &mut world)
                .await;
            let out = drain(&mut orx);
            assert!(out.contains("This account is locked until"), "{out}");
            assert!(
                out.contains("UTC after too many failed passwords."),
                "{out}"
            );
            assert!(out.contains("typing 'code'"), "{out}");
            assert!(out.contains("Password: "), "re-prompts: {out}");
            let ctx = router.login.get(&1).expect("connection must stay open");
            assert_eq!(ctx.failed_attempts, 0);
            match &ctx.stage {
                Stage::AwaitingPassword { user, .. } => {
                    assert_eq!(user.failed_login_attempts, 3, "counter untouched");
                }
                _ => panic!("should stay at the password prompt"),
            }
            assert!(rx.try_recv().is_err(), "no verification job queued");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn locked_account_code_proceeds_and_ignores_connection_failure_cap() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        // Own throwaway account: borrowing "the first Users row" races
        // with other tests that create and delete users (FK violation on
        // the code insert -> "not awaiting web approval").
        let Some(uid) = temp_user(&pool, "dctest").await else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let mut world = auth_world(3);
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, Some("198.51.100.7:2000".parse().unwrap()), &world);
        drain(&mut orx);
        // One more wrong password would drop the connection...
        router.login.get_mut(&1).unwrap().failed_attempts = MAX_FAILED_PASSWORDS_PER_CONN - 1;
        let mut user = locked_user(3);
        user.id = uid.clone();
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: None,
            game_hash: String::new(),
        };
        // ...but `code` neither counts nor is blocked.
        router.on_line(1, "code".into(), &pool, &mut world).await;
        let (code_id, _) = pending_web(&router, 1);
        let ctx = router.login.get(&1).expect("connection still open");
        assert_eq!(ctx.failed_attempts, MAX_FAILED_PASSWORDS_PER_CONN - 1);
        assert!(drain(&mut orx).contains("Your login code is "));
        mud_db::game_login_code::delete(&pool, &code_id)
            .await
            .unwrap();
        temp_cleanup(&pool, &[], &[], &[&uid]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consumed_code_clears_account_lockout() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let Ok(uid) = mud_db::users::create(
            &pool,
            &format!("locktest{suffix}@example.invalid"),
            &format!("locktest{suffix}"),
        )
        .await
        else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        mud_db::sqlx::query(
            "UPDATE \"Users\" SET failed_login_attempts = 4, \
             locked_until = (NOW() AT TIME ZONE 'UTC') + interval '10 minutes' WHERE id = $1",
        )
        .bind(&uid)
        .execute(&pool)
        .await
        .unwrap();
        let user = mud_db::users::find_by_id(&pool, &uid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.failed_login_attempts, 4);
        assert!(user.locked_until.is_some());

        let mut world = auth_world(3);
        let mut router = ConnRouter::new();
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, Some("198.51.100.8:2001".parse().unwrap()), &world);
        drain(&mut orx);
        router.login.get_mut(&1).unwrap().stage = Stage::AwaitingPassword {
            user,
            preselected: None,
            game_hash: String::new(),
        };
        router.on_line(1, "code".into(), &pool, &mut world).await;
        let (code_id, _) = pending_web(&router, 1);
        mud_db::sqlx::query(
            "UPDATE \"GameLoginCode\" SET status = 'APPROVED', \"approvedByUserId\" = $2 \
             WHERE id = $1",
        )
        .bind(&code_id)
        .bind(&uid)
        .execute(&pool)
        .await
        .unwrap();
        // Enter re-checks: consume succeeds, lock is lifted. The temp
        // account has no characters, so the flow ends back at the prompt.
        router.on_line(1, String::new(), &pool, &mut world).await;
        let out = drain(&mut orx);
        assert!(out.contains("Code approved"), "{out}");
        let (attempts, locked): (i32, Option<chrono::NaiveDateTime>) = mud_db::sqlx::query_as(
            "SELECT failed_login_attempts, locked_until FROM \"Users\" WHERE id = $1",
        )
        .bind(&uid)
        .fetch_one(&pool)
        .await
        .unwrap();
        let st = mud_db::game_login_code::state(&pool, &code_id)
            .await
            .unwrap()
            .unwrap();
        mud_db::game_login_code::delete(&pool, &code_id)
            .await
            .unwrap();
        mud_db::sqlx::query("DELETE FROM \"Users\" WHERE id = $1")
            .bind(&uid)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(st.status, "CONSUMED");
        assert_eq!(attempts, 0);
        assert!(locked.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn freshly_recorded_lock_and_ban_read_back_as_naive_utc() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let Ok(uid) = mud_db::users::create(
            &pool,
            &format!("tztest{suffix}@example.invalid"),
            &format!("tztest{suffix}"),
        )
        .await
        else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let f = mud_db::users::record_failed_login(&pool, &uid, 1, 15)
            .await
            .unwrap();
        assert_eq!(
            f,
            mud_db::users::FailedLogin {
                attempts: 1,
                locked: true
            }
        );
        let user = mud_db::users::find_by_id(&pool, &uid)
            .await
            .unwrap()
            .unwrap();
        let now = chrono::Utc::now().naive_utc();
        let ban = mud_db::bans::ban(&pool, &uid, &uid, "tz test", Some(600)).await;
        let active = mud_db::bans::active_for(&pool, &uid).await;
        let _ = mud_db::sqlx::query("DELETE FROM \"BanRecords\" WHERE user_id = $1")
            .bind(&uid)
            .execute(&pool)
            .await;
        mud_db::sqlx::query("DELETE FROM \"Users\" WHERE id = $1")
            .bind(&uid)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(user.failed_login_attempts, 1);
        let locked_until = user.locked_until.expect("lock recorded");
        assert!(locked_until > now, "{locked_until} !> {now}");
        assert!(
            locked_until < now + chrono::Duration::minutes(16),
            "lock too far ahead: {locked_until} vs {now}"
        );
        assert!(locked_hint(&user, now).is_some());
        if ban.is_ok() {
            assert!(active.unwrap().is_some(), "10-minute ban must be active");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_now_is_naive_utc() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let db_now: chrono::NaiveDateTime = mud_db::sqlx::query_scalar("SELECT NOW()::timestamp")
            .fetch_one(&pool)
            .await
            .unwrap();
        let skew = (db_now - chrono::Utc::now().naive_utc())
            .num_seconds()
            .abs();
        assert!(skew <= 5, "NOW()::timestamp is {skew}s off naive UTC");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_failed_login_counts_in_row_and_resets_after_expired_lock() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let Ok(uid) = mud_db::users::create(
            &pool,
            &format!("rfl{suffix}@example.invalid"),
            &format!("rfl{suffix}"),
        )
        .await
        else {
            eprintln!("skipping: could not create temp user");
            return;
        };
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(mud_db::users::record_failed_login(&pool, &uid, 3, 15).await);
        }
        // Lock expires; the next failure restarts the count at 1 and
        // doesn't re-lock.
        mud_db::sqlx::query(
            "UPDATE \"Users\" SET locked_until = (NOW() AT TIME ZONE 'UTC') - interval '1 minute' \
             WHERE id = $1",
        )
        .bind(&uid)
        .execute(&pool)
        .await
        .unwrap();
        let after_expiry = mud_db::users::record_failed_login(&pool, &uid, 3, 15).await;
        let row = mud_db::users::find_by_id(&pool, &uid)
            .await
            .unwrap()
            .unwrap();
        mud_db::sqlx::query("DELETE FROM \"Users\" WHERE id = $1")
            .bind(&uid)
            .execute(&pool)
            .await
            .unwrap();
        let f = |attempts, locked| Some(mud_db::users::FailedLogin { attempts, locked });
        assert_eq!(seen[0].as_ref().ok().copied(), f(1, false));
        assert_eq!(seen[1].as_ref().ok().copied(), f(2, false));
        assert_eq!(seen[2].as_ref().ok().copied(), f(3, true));
        assert_eq!(after_expiry.ok(), f(1, false));
        assert_eq!(row.failed_login_attempts, 1);
        assert!(row.locked_until.is_none());
    }

    thread_local! {
        static LOAD_CLOSED: std::cell::RefCell<Vec<ConnId>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Router wired so `close_conn` is observable; connection 1 is parked
    /// mid-login and its output drained.
    fn load_guard_router(world: &World) -> (ConnRouter, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        LOAD_CLOSED.with(|v| v.borrow_mut().clear());
        let mut router = ConnRouter::new();
        router.close_conn = |c| {
            LOAD_CLOSED.with(|v| v.borrow_mut().push(c));
            true
        };
        let (tx, mut orx) = tokio::sync::mpsc::channel(64);
        router.on_connect(1, tx, None, world);
        drain(&mut orx);
        (router, orx)
    }

    fn assert_load_refused(
        router: &ConnRouter,
        world: &mut World,
        orx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
        what: &str,
    ) {
        let text = drain(orx);
        assert!(
            text.contains(
                "The game is having trouble loading your character; \
                 please try again in a minute."
            ),
            "{what}: {text}"
        );
        assert!(!router.login.contains_key(&1), "{what}: ctx removed");
        assert_eq!(
            LOAD_CLOSED.with(|v| v.borrow().clone()),
            vec![1],
            "{what}: connection closed"
        );
        assert!(
            world.query::<&Player>().iter(world).next().is_none(),
            "{what}: no session was spawned"
        );
    }

    /// A real (not injected) DB failure while loading the character's
    /// tables refuses the login instead of starting an empty session.
    #[tokio::test(flavor = "current_thread")]
    async fn login_is_refused_when_the_database_is_unreachable() {
        let mut world = auth_world(3);
        let (mut router, mut orx) = load_guard_router(&world);
        let (user, c) = legacy_sentinel();
        router
            .complete_login(1, &mut world, &failing_pool(), user, *c.unwrap())
            .await;
        assert_load_refused(&router, &mut world, &mut orx, "unreachable db");
    }

    /// `abilities_loaded` on a virtual session is just the character's
    /// `CharacterAbilities` row count. A seeded mortal (`TestWarrior`, class
    /// Warrior) must load some. Read-only; skips when the dev DB or that
    /// character is absent.
    #[tokio::test(flavor = "current_thread")]
    async fn seeded_mortal_loads_abilities() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let count = |name: &'static str| {
            let pool = pool.clone();
            async move {
                let id: Option<String> =
                    mud_db::sqlx::query_scalar("SELECT id FROM \"Characters\" WHERE name = $1")
                        .bind(name)
                        .fetch_optional(&pool)
                        .await
                        .unwrap();
                match id {
                    Some(id) => Some(
                        mud_db::character_abilities::list_for(&pool, &id)
                            .await
                            .unwrap()
                            .len(),
                    ),
                    None => None,
                }
            }
        };
        match count("TestWarrior").await {
            Some(n) => assert!(n > 0, "TestWarrior should load abilities, got {n}"),
            None => eprintln!("skipping: TestWarrior not seeded"),
        }
    }

    /// For every per-character table, an injected load failure refuses the
    /// login and leaves the character's `CharacterAbilities` and
    /// `CharacterItems` rows untouched (nothing exists to save over them).
    /// A control load without a fault returns both.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_table_load_refuses_login_and_keeps_rows() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let Some((oz, oid)) = first_object(&pool).await else {
            eprintln!("skipping: no Objects rows");
            return;
        };
        let ability: Option<i32> =
            mud_db::sqlx::query_scalar("SELECT id FROM \"Ability\" ORDER BY id LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap();
        let Some(ability_id) = ability else {
            eprintln!("skipping: no Ability rows");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "lg").await;
        mud_db::sqlx::query(
            "INSERT INTO \"CharacterAbilities\" (character_id, ability_id, known, proficiency) \
             VALUES ($1, $2, true, 77)",
        )
        .bind(&c.id)
        .bind(ability_id)
        .execute(&pool)
        .await
        .unwrap();
        mud_db::sqlx::query(
            "INSERT INTO \"CharacterItems\" (character_id, object_zone_id, object_id, updated_at) \
             VALUES ($1, $2, $3, NOW())",
        )
        .bind(&c.id)
        .bind(oz)
        .bind(oid)
        .execute(&pool)
        .await
        .unwrap();
        let abilities = |pool: PgPool, cid: String| async move {
            mud_db::character_abilities::list_for(&pool, &cid)
                .await
                .unwrap()
                .into_iter()
                .map(|r| (r.ability_id, r.proficiency))
                .collect::<Vec<_>>()
        };
        let before_items = item_rows(&pool, &[&c.id]).await;
        assert_eq!(before_items.len(), 1);
        let before_abilities = abilities(pool.clone(), c.id.clone()).await;
        assert_eq!(before_abilities, vec![(ability_id, 77)]);

        for table in [
            "character_abilities",
            "character_items",
            "character_aliases",
            "achievements",
            "kill_tracking",
            "drunkenness",
            "clan_membership",
            "script_vars",
            "trophy",
            "spell_cooldowns",
            "cooldowns",
            "ignore_list",
            "command_grants",
            "effect_instances",
            "pets",
            "player_houses",
        ] {
            let mut world = auth_world(3);
            let (mut router, mut orx) = load_guard_router(&world);
            router.load_fault = Some(table);
            router
                .complete_login(1, &mut world, &pool, user.clone(), (*c).clone())
                .await;
            assert_load_refused(&router, &mut world, &mut orx, table);
            assert_eq!(item_rows(&pool, &[&c.id]).await, before_items, "{table}");
            assert_eq!(
                abilities(pool.clone(), c.id.clone()).await,
                before_abilities,
                "{table}"
            );
        }

        // Control: without a fault the loader returns both tables.
        let ok = load_persisted(&pool, &c, &user, None)
            .await
            .unwrap_or_else(|f| panic!("control load failed at {f}"));
        assert_eq!(ok.item_rows.len(), 1);
        assert!(
            ok.ability_rows
                .iter()
                .any(|r| r.ability_id == ability_id && r.proficiency == 77)
        );
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A non-empty saved-state column that does not parse refuses the
    /// login and leaves the stored bytes untouched; NULL and empty values
    /// still log in (checked via the loader).
    #[tokio::test(flavor = "current_thread")]
    async fn unparseable_saved_state_refuses_login_and_keeps_bytes() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "pj").await;
        let columns = [
            "script_vars",
            "trophy_data",
            "spell_cooldowns",
            "ignore_list",
            "command_grants",
            "cooldowns",
            "effect_instances",
            "pets",
        ];
        for column in columns {
            mud_db::sqlx::query(
                "UPDATE \"Characters\" SET script_vars = NULL, trophy_data = NULL, \
                 spell_cooldowns = NULL, ignore_list = NULL, command_grants = NULL, \
                 cooldowns = NULL, effect_instances = NULL, pets = NULL WHERE id = $1",
            )
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
            mud_db::sqlx::query(&format!(
                "UPDATE \"Characters\" SET {column} = to_jsonb('corrupt'::text) WHERE id = $1"
            ))
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
            let read = || async {
                mud_db::sqlx::query_scalar::<_, Option<String>>(&format!(
                    "SELECT {column}::text FROM \"Characters\" WHERE id = $1"
                ))
                .bind(&c.id)
                .fetch_one(&pool)
                .await
                .unwrap()
            };
            let before = read().await;
            assert_eq!(before.as_deref(), Some("\"corrupt\""));

            let mut world = auth_world(3);
            let (mut router, mut orx) = load_guard_router(&world);
            router
                .complete_login(1, &mut world, &pool, user.clone(), (*c).clone())
                .await;
            assert_load_refused(&router, &mut world, &mut orx, column);
            assert_eq!(read().await, before, "{column}: bytes unchanged");
            match load_persisted(&pool, &c, &user, None).await {
                Err(LoadFailure::Parse { column: got, .. }) => assert_eq!(got, column),
                Err(other) => panic!("{column}: wrong failure {other}"),
                Ok(_) => panic!("{column}: corrupt value accepted"),
            }
        }

        // Empty values are allowed.
        mud_db::sqlx::query(
            "UPDATE \"Characters\" SET script_vars = '{}', trophy_data = '[]', \
             spell_cooldowns = '{}', ignore_list = '[]', command_grants = '{}', \
             cooldowns = '{}', effect_instances = '{}', pets = NULL WHERE id = $1",
        )
        .bind(&c.id)
        .execute(&pool)
        .await
        .unwrap();
        load_persisted(&pool, &c, &user, None)
            .await
            .unwrap_or_else(|f| panic!("empty columns refused: {f}"));
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// `grant` / `revoke` lists survive the save and load path, and an
    /// empty list is stored as NULL.
    #[tokio::test(flavor = "current_thread")]
    async fn command_grants_round_trip_through_the_column() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "cg").await;
        let grants = mud_world::CommandGrants {
            grants: vec![mud_world::GrantEntry {
                command: "send".into(),
                grantor: "Boss".into(),
                level: 105,
            }],
            revokes: vec![mud_world::GrantEntry {
                command: "goto".into(),
                grantor: "Boss".into(),
                level: 105,
            }],
        };
        let json = serde_json::to_value(&grants).unwrap();
        mud_db::characters::save_command_grants(&pool, &c.id, Some(&json))
            .await
            .unwrap();
        let loaded = load_persisted(&pool, &c, &user, None)
            .await
            .unwrap_or_else(|f| panic!("load failed at {f}"));
        let back: mud_world::CommandGrants =
            serde_json::from_value(loaded.command_grants_json.unwrap()).unwrap();
        assert_eq!(back, grants);

        mud_db::characters::save_command_grants(&pool, &c.id, None)
            .await
            .unwrap();
        let loaded = load_persisted(&pool, &c, &user, None)
            .await
            .unwrap_or_else(|f| panic!("load failed at {f}"));
        assert!(loaded.command_grants_json.is_none());
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A failed character re-read after the save-wait refuses the login
    /// instead of continuing on the pre-wait row.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_reload_after_save_wait_refuses_login() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "rl").await;
        let mut world = auth_world(3);
        let (mut router, mut orx) = load_guard_router(&world);
        router.load_fault = Some("character_reload");
        router
            .finish_save_wait(1, user.clone(), (*c).clone(), true, &pool, &mut world)
            .await;
        assert_load_refused(&router, &mut world, &mut orx, "character_reload");

        // A row that vanished during the wait is refused the same way.
        let mut gone = (*c).clone();
        gone.name = "ZzNoSuchCharacterZz".to_string();
        assert!(reload_character_row(&pool, &gone, None).await.is_err());
        // Control: the real row reloads.
        assert!(reload_character_row(&pool, &c, None).await.is_ok());
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }

    /// A failed `last_login` / `last_logout` stamp refuses the login before
    /// anything spawns, and the stale `last_logout` is left as it was.
    #[tokio::test(flavor = "current_thread")]
    async fn failed_last_login_stamp_refuses_login() {
        let Some((pool, _db_lock)) = live_pool().await else {
            eprintln!("skipping: dev database unavailable");
            return;
        };
        let (user, c) = temp_unlinked_char(&pool, "ll").await;
        mud_db::sqlx::query("UPDATE \"Characters\" SET last_logout = NOW() WHERE id = $1")
            .bind(&c.id)
            .execute(&pool)
            .await
            .unwrap();
        let mut world = auth_world(3);
        let (mut router, mut orx) = load_guard_router(&world);
        router.load_fault = Some("last_login");
        router
            .complete_login(1, &mut world, &pool, user.clone(), (*c).clone())
            .await;
        let text = drain(&mut orx);
        assert!(text.contains("Please try again in a moment."), "{text}");
        assert!(!router.login.contains_key(&1), "ctx removed");
        assert_eq!(LOAD_CLOSED.with(|v| v.borrow().clone()), vec![1]);
        assert!(
            world.query::<&Player>().iter(&world).next().is_none(),
            "no session was spawned"
        );
        let still_set: bool = mud_db::sqlx::query_scalar(
            "SELECT last_logout IS NOT NULL FROM \"Characters\" WHERE id = $1",
        )
        .bind(&c.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(still_set, "last_logout untouched by the refused login");
        temp_cleanup(&pool, &[], &[&c.id], &[]).await;
    }
}
