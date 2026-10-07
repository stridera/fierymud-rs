use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{debug, info, warn};

pub mod output;
pub mod telnet;
pub use output::{Charset, ColorDepth, OutputHandle};
pub use telnet::{
    Event as TelnetEvent, Parser as TelnetParser, charset_request_utf8, do_, dont,
    environ_send_query, iac_eor, iac_ga, mccp2_start, negotiate, opt, parse_environ, parse_mtts,
    parse_naws, subneg, ttype_send, will, wont,
};

pub type ConnId = u64;
/// Outbound message to a connected client. Bytes (not String) so the
/// channel can carry telnet IAC framing for GMCP / MSSP / option
/// negotiation alongside ordinary UTF-8 text.
///
/// Bounded — see `OUTBOUND_QUEUE_CAP`. Senders use `try_send` and drop
/// silently on Full; the bounded channel itself caps server-side
/// memory growth from a slow client.
pub type Outbound = mpsc::Sender<Vec<u8>>;

/// Per-connection outbound queue cap. Sized for steady-state burst:
/// a multi-line render (room look + occupants + exits, large prompt
/// with color tags expanded) lands as several dozen messages; an
/// AOE damage broadcast can fan a hundred lines to one observer.
/// 1024 leaves headroom for those bursts without unbounded growth.
pub const OUTBOUND_QUEUE_CAP: usize = 1024;
pub type InboundTx = mpsc::Sender<Inbound>;
pub type InboundRx = mpsc::Receiver<Inbound>;

/// Cap for the global inbound channel that carries
/// `Connected` / `Line` / `Disconnected` events from every accepted
/// connection into the world tick. Sized for steady-state burst:
/// ~50 connected players × ~80 ops/sec headroom on a slow tick. When
/// full, `send().await` blocks the connection's read task — natural
/// backpressure to the slow client.
pub const INBOUND_QUEUE_CAP: usize = 4096;

#[derive(Debug)]
pub struct Inbound {
    pub conn: ConnId,
    pub kind: InboundKind,
}

#[derive(Debug)]
pub enum InboundKind {
    /// Delivered once capability negotiation has settled (or
    /// `Limits::negotiation_window` has passed), so the greeting the
    /// server sends in reply can be rendered for what the client
    /// reported. Negotiation events that arrived first follow it.
    Connected {
        peer: SocketAddr,
        outbound: Outbound,
        /// The connection's output capabilities (colour depth,
        /// charset). The writer applies them to every frame; the
        /// server can read them and layer player overrides on top.
        output: OutputHandle,
    },
    Line(String),
    /// Client reported its window size via NAWS (RFC 1073).
    /// Forwarded so the world can size who/score/look output to
    /// the actual viewport. Resizes mid-session resend NAWS, so
    /// this event can fire repeatedly per connection.
    WindowSize {
        cols: u16,
        rows: u16,
    },
    /// Client reported a terminal-type response. The MTTS cycle
    /// produces three responses on consecutive `IAC SB TTYPE
    /// SEND`s — the first is the client name (e.g. `"Mudlet"`),
    /// the second a TERM-style name (e.g. `"XTERM-256COLOR"`),
    /// the third an MTTS bitmap (`"MTTS 285"`). Sequence-ordered
    /// in `index` so the receiver can map them.
    Terminal {
        index: u8,
        value: String,
    },
    /// Client confirmed a capability with `IAC DO <option>` (we
    /// said WILL first) or `IAC WILL <option>` (we said DO first).
    /// Tracked at the world layer so commands can gate behavior
    /// (e.g. `setOR` only emits when EOR is on, MXP `<send>` only
    /// when MXP is confirmed).
    Capability {
        name: &'static str,
        on: bool,
    },
    /// GMCP subnegotiation arrived from the client. `package` is
    /// the dotted path (`Core.Hello`, `Char.Login`, ...); `payload`
    /// is the raw JSON string remainder (may be empty for
    /// content-less packages). The world layer parses + dispatches.
    Gmcp {
        package: String,
        payload: String,
    },
    Disconnected,
}

/// Tunable admission limits and timeouts shared by the plain-TCP and
/// TLS listeners. `usize::MAX` for a cap means "no limit".
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Total accepted-and-still-open connections across both listeners.
    pub max_connections: usize,
    /// Concurrent open connections from a single source IP, so one
    /// host can't fill every slot in `max_connections`.
    pub max_per_ip: usize,
    /// Budget for the TLS handshake. A peer that opens a socket and
    /// never speaks TLS is dropped when this elapses.
    pub handshake_timeout: Duration,
    /// A not-yet-logged-in connection that completes no input line for
    /// this long is closed. Telnet negotiation bytes don't count.
    pub pre_login_idle: Duration,
    /// Absolute ceiling on how long a connection may stay pre-login,
    /// so a peer drip-feeding one line per `pre_login_idle` can't hold
    /// a slot forever.
    pub pre_login_total: Duration,
    /// How long the connect greeting waits for the client's terminal
    /// capabilities (TTYPE / MTTS) before giving up and assuming the
    /// safe defaults (16-colour ASCII).
    pub negotiation_window: Duration,
}

/// Default per-IP open-connection cap.
pub const DEFAULT_MAX_PER_IP: usize = 10;
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_PRE_LOGIN_IDLE: Duration = Duration::from_secs(120);
const DEFAULT_PRE_LOGIN_TOTAL: Duration = Duration::from_secs(15 * 60);
const DEFAULT_NEGOTIATION_WINDOW: Duration = Duration::from_millis(500);

impl Limits {
    /// Build limits with the given caps and the default timeouts.
    #[must_use]
    pub fn new(max_connections: usize, max_per_ip: usize) -> Self {
        Self {
            max_connections,
            max_per_ip,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            pre_login_idle: DEFAULT_PRE_LOGIN_IDLE,
            pre_login_total: DEFAULT_PRE_LOGIN_TOTAL,
            negotiation_window: DEFAULT_NEGOTIATION_WINDOW,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new(usize::MAX, DEFAULT_MAX_PER_IP)
    }
}

/// Per-IP connection-rate window.
const THROTTLE_WINDOW: Duration = Duration::from_secs(60);
const MAX_CONNECTS_PER_MIN: usize = 10;
/// Hard cap on tracked IPs in the throttle map. Past this the map is
/// swept of expired entries; if it's still full, new IPs are admitted
/// untracked (fail-open — the open-connection caps still apply).
const THROTTLE_MAP_CAP: usize = 8192;
/// Periodic sweep of expired throttle entries, piggy-backed on inserts.
const THROTTLE_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Minimum gap between emergency sweeps triggered by a full map, so a
/// flood of fresh IPs can't make every accept an O(cap) scan.
const THROTTLE_FULL_SWEEP_GAP: Duration = Duration::from_secs(1);

/// Per-IP throttle state: connection timestamps in the rolling
/// window plus a `warned` latch so a sustained flood logs a single
/// WARN on the transition into throttled state rather than one per
/// rejected connection. The latch clears once the peer falls back
/// under the limit (window slides / flood stops).
#[derive(Default)]
struct ThrottleEntry {
    times: VecDeque<Instant>,
    warned: bool,
}

/// Outcome of a throttle check. `RejectFirst` is the transition into
/// throttled state (worth a WARN); `RejectRepeat` is a continuing
/// flood (debug — the operator already saw the first WARN).
enum Throttle {
    Allow,
    RejectFirst,
    RejectRepeat,
}

/// Why [`Gate::admit`] refused a connection.
#[derive(Debug, PartialEq, Eq)]
enum Reject {
    Throttled { first: bool },
    MaxConnections,
    PerIp,
}

struct GateState {
    active: usize,
    /// Open-connection count per source IP; entries removed at zero.
    per_ip: HashMap<IpAddr, usize>,
    throttle: HashMap<IpAddr, ThrottleEntry>,
    last_sweep: Instant,
    /// Per-connection shared state (login flag, close signal); see
    /// [`mark_authenticated`] and [`close_connection`].
    conns: HashMap<ConnId, Arc<ConnShared>>,
}

/// State shared between the admission gate (and so the server-facing
/// API) and a connection's reader/writer tasks.
struct ConnShared {
    /// Set once the server reports the connection has finished login.
    authenticated: AtomicBool,
    /// Flipped to `true` by [`close_connection`]; both tasks watch it.
    close: watch::Sender<bool>,
}

/// Admission state shared by both listeners: live-connection count,
/// per-IP open counts, the connect-rate throttle and the pre-login
/// auth flags. One process-wide instance backs the real listeners;
/// tests build their own.
struct Gate {
    state: Mutex<GateState>,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState {
                active: 0,
                per_ip: HashMap::new(),
                throttle: HashMap::new(),
                last_sweep: Instant::now(),
                conns: HashMap::new(),
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run every admission check and, on success, reserve a slot. The
    /// returned guard releases the slot on drop (whether the task ends
    /// gracefully, errors out, or is cancelled).
    fn admit(
        self: &Arc<Self>,
        ip: IpAddr,
        conn_id: ConnId,
        limits: &Limits,
        now: Instant,
    ) -> Result<ConnGuard, Reject> {
        let mut st = self.lock();
        match throttle_allow(&mut st, ip, now) {
            Throttle::Allow => {}
            Throttle::RejectFirst => return Err(Reject::Throttled { first: true }),
            Throttle::RejectRepeat => return Err(Reject::Throttled { first: false }),
        }
        if st.active >= limits.max_connections {
            return Err(Reject::MaxConnections);
        }
        if st.per_ip.get(&ip).copied().unwrap_or(0) >= limits.max_per_ip {
            return Err(Reject::PerIp);
        }
        st.active += 1;
        *st.per_ip.entry(ip).or_default() += 1;
        let shared = Arc::new(ConnShared {
            authenticated: AtomicBool::new(false),
            close: watch::Sender::new(false),
        });
        st.conns.insert(conn_id, Arc::clone(&shared));
        drop(st);
        Ok(ConnGuard {
            gate: Arc::clone(self),
            ip,
            conn_id,
            shared,
        })
    }

    fn mark_authenticated(&self, conn_id: ConnId) {
        if let Some(c) = self.lock().conns.get(&conn_id) {
            c.authenticated.store(true, Ordering::Release);
        }
    }

    fn close_connection(&self, conn_id: ConnId) -> bool {
        let shared = self.lock().conns.get(&conn_id).cloned();
        shared.is_some_and(|c| {
            c.close.send_replace(true);
            true
        })
    }
}

fn global_gate() -> Arc<Gate> {
    static GATE: LazyLock<Arc<Gate>> = LazyLock::new(Gate::new);
    Arc::clone(&GATE)
}

/// Tell the network layer that `conn` has completed login. Until this
/// is called a connection is subject to the pre-login idle and total
/// timeouts in [`Limits`]; afterwards it's exempt (the game's own idle
/// kicker takes over). Unknown / already-closed ids are ignored.
pub fn mark_authenticated(conn: ConnId) {
    global_gate().mark_authenticated(conn);
}

/// Ask the network layer to close `conn` from the server side (idle
/// kick, "take over your own body" reconnect, ...). Output already
/// queued on the connection's [`Outbound`] channel is flushed to the
/// socket first, then the socket is shut down; the reader task exits
/// and a final [`InboundKind::Disconnected`] is delivered, same as a
/// client-initiated drop. Returns `false` if the id is unknown or the
/// connection already ended. Safe to call more than once.
#[allow(clippy::must_use_candidate)] // the "was it open" flag is informational
pub fn close_connection(conn: ConnId) -> bool {
    global_gate().close_connection(conn)
}

/// RAII guard for one admitted connection: holds the global and
/// per-IP slots plus the shared per-connection state. Owned by the spawned
/// per-connection task so Drop fires however the task ends — without
/// it a failure path would leak a permanent +1 against the caps.
struct ConnGuard {
    gate: Arc<Gate>,
    ip: IpAddr,
    conn_id: ConnId,
    shared: Arc<ConnShared>,
}

impl ConnGuard {
    fn is_authenticated(&self) -> bool {
        self.shared.authenticated.load(Ordering::Acquire)
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut st = self.gate.lock();
        st.active = st.active.saturating_sub(1);
        if let Some(n) = st.per_ip.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                st.per_ip.remove(&self.ip);
            }
        }
        st.conns.remove(&self.conn_id);
    }
}

/// Drop throttle entries whose whole window has expired.
fn sweep_throttle(st: &mut GateState, now: Instant) {
    st.throttle.retain(|_, e| {
        while e
            .times
            .front()
            .is_some_and(|t| now.duration_since(*t) > THROTTLE_WINDOW)
        {
            e.times.pop_front();
        }
        !e.times.is_empty()
    });
    st.last_sweep = now;
}

/// Records the attempt and decides whether the IP is over its rate.
/// Expired entries are evicted by periodic sweeps and the map is
/// bounded by [`THROTTLE_MAP_CAP`], so it can't grow without limit.
fn throttle_allow(st: &mut GateState, ip: IpAddr, now: Instant) -> Throttle {
    if now.duration_since(st.last_sweep) >= THROTTLE_SWEEP_INTERVAL {
        sweep_throttle(st, now);
    }
    if !st.throttle.contains_key(&ip) && st.throttle.len() >= THROTTLE_MAP_CAP {
        if now.duration_since(st.last_sweep) >= THROTTLE_FULL_SWEEP_GAP {
            sweep_throttle(st, now);
        }
        if st.throttle.len() >= THROTTLE_MAP_CAP {
            // Still full of live entries: admit untracked rather than
            // evict someone mid-flood or refuse legitimate players.
            return Throttle::Allow;
        }
    }
    let entry = st.throttle.entry(ip).or_default();
    while entry
        .times
        .front()
        .is_some_and(|t| now.duration_since(*t) > THROTTLE_WINDOW)
    {
        entry.times.pop_front();
    }
    if entry.times.len() >= MAX_CONNECTS_PER_MIN {
        // Latch the warn so a sustained flood logs once, not per
        // rejected connection.
        if entry.warned {
            return Throttle::RejectRepeat;
        }
        entry.warned = true;
        return Throttle::RejectFirst;
    }
    // Back under the limit — clear the latch so the next burst that
    // crosses the threshold warns afresh.
    entry.warned = false;
    entry.times.push_back(now);
    Throttle::Allow
}

/// Why a freshly accepted socket was turned away.
enum Refusal {
    /// Dropped silently (banned or rate-throttled peers get no courtesy).
    Silent,
    /// Dropped after a one-line explanation of a capacity limit.
    Notice(&'static str),
}

const NOTICE_PER_IP: &str =
    "Too many connections from your address. Please close other sessions and try again.\r\n";
const NOTICE_FULL: &str = "The server is full. Please try again later.\r\n";
/// Budget for writing a refusal notice before the socket is dropped.
const REFUSAL_WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// Best-effort write of a refusal notice, then close. Runs detached so
/// a slow peer can't stall the accept loop; bounded by
/// [`REFUSAL_WRITE_TIMEOUT`].
fn send_refusal<S>(mut stream: S, notice: &'static str)
where
    S: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let _ = tokio::time::timeout(REFUSAL_WRITE_TIMEOUT, async {
            let _ = stream.write_all(notice.as_bytes()).await;
            let _ = stream.shutdown().await;
        })
        .await;
    });
}

/// Ban + admission check for a freshly accepted socket, logging the
/// refusal reason. `kind` is `""` or `" TLS"` for log text.
fn admit_or_log(
    gate: &Arc<Gate>,
    peer: SocketAddr,
    conn_id: ConnId,
    limits: &Limits,
    kind: &str,
) -> Result<ConnGuard, Refusal> {
    if banned(peer.ip()) {
        warn!(%peer, "banlist: refusing{kind} connection");
        return Err(Refusal::Silent);
    }
    match gate.admit(peer.ip(), conn_id, limits, Instant::now()) {
        Ok(guard) => Ok(guard),
        Err(Reject::Throttled { first: true }) => {
            warn!(%peer, "throttle: rejecting{kind} connection — over rate limit");
            Err(Refusal::Silent)
        }
        Err(Reject::Throttled { first: false }) => {
            debug!(%peer, "throttle: rejecting{kind} connection (continuing flood)");
            Err(Refusal::Silent)
        }
        Err(Reject::MaxConnections) => {
            warn!(%peer, max = limits.max_connections, "max_connections reached; refusing{kind}");
            Err(Refusal::Notice(NOTICE_FULL))
        }
        Err(Reject::PerIp) => {
            warn!(%peer, max = limits.max_per_ip, "per-IP connection cap reached; refusing{kind}");
            Err(Refusal::Notice(NOTICE_PER_IP))
        }
    }
}

/// Bind a plain-TCP listener and forward every accepted connection's
/// lines into `inbound`. Returns only on listener error; runs forever
/// otherwise.
///
/// `limits.max_connections` caps the *total* accepted-and-still-open
/// count across this listener and the TLS sibling — they share one
/// admission gate, so a flood that fills the plain-TCP channel can't
/// leave the TLS listener wide open. `limits.max_per_ip` caps open
/// connections from a single source IP.
pub async fn serve(bind_addr: &str, inbound: InboundTx, limits: Limits) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    info!(
        addr = %listener.local_addr()?,
        max_connections = limits.max_connections,
        max_per_ip = limits.max_per_ip,
        "telnet listener accepting connections"
    );
    accept_plain(listener, inbound, global_gate(), limits).await
}

async fn accept_plain(
    listener: TcpListener,
    inbound: InboundTx,
    gate: Arc<Gate>,
    limits: Limits,
) -> std::io::Result<()> {
    let mut next_id: ConnId = 1;
    loop {
        let (stream, peer) = listener.accept().await?;
        let conn_id = next_id;
        next_id += 1;
        // On refusal the socket is closed (after a short notice for
        // capacity limits).
        let guard = match admit_or_log(&gate, peer, conn_id, &limits, "") {
            Ok(g) => g,
            Err(Refusal::Notice(n)) => {
                send_refusal(stream, n);
                continue;
            }
            Err(Refusal::Silent) => continue,
        };
        let inbound = inbound.clone();
        tokio::spawn(async move {
            handle_connection(conn_id, peer, stream, inbound, guard, limits).await;
        });
    }
}

/// Hard cap on a single inbound command line. A peer streaming bytes
/// without a newline can otherwise grow the line buffer unboundedly.
/// 4 KiB is well above any plausible MUD command (longest legitimate
/// inputs are emote / who-tag / mail-body lines on the order of a
/// few hundred bytes); going over indicates either a buggy client
/// or an attacker. On overflow we drop the connection.
const MAX_LINE_LEN: usize = 4096;

/// Per-read chunk size for the inbound socket. Sized to comfortably
/// hold a typical telnet round-trip (input line + IAC negotiation +
/// occasional GMCP heartbeat) without forcing many tiny reads.
const READ_CHUNK: usize = 4096;

/// Hard ban list — IPs that should never connect, regardless of
/// rate. Initialized once from the `MUD_BANLIST` env var (comma-
/// separated `1.2.3.4` entries). Empty by default.
static BANLIST: std::sync::OnceLock<HashSet<IpAddr>> = std::sync::OnceLock::new();

fn banlist() -> &'static HashSet<IpAddr> {
    BANLIST.get_or_init(|| {
        std::env::var("MUD_BANLIST")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .filter_map(|s| s.trim().parse::<IpAddr>().ok())
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn banned(ip: IpAddr) -> bool {
    banlist().contains(&ip)
}

/// Like [`serve`] but wraps every accepted connection in TLS using the
/// supplied PEM-encoded cert chain + private key. `ConnId` space is
/// shared with `serve` via a high-bit offset so logs can tell the two
/// listeners apart.
///
/// `cert_path` is a chain (server cert first, then any intermediates).
/// `key_path` may be PKCS#8 or RSA / SEC1 PEM; we try them in order.
pub async fn serve_tls(
    bind_addr: &str,
    cert_path: &str,
    key_path: &str,
    inbound: InboundTx,
    limits: Limits,
) -> std::io::Result<()> {
    let certs = load_cert_chain(cert_path)?;
    let key = load_private_key(key_path)?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| std::io::Error::other(format!("rustls config: {e}")))?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind(bind_addr).await?;
    info!(addr = %listener.local_addr()?, "TLS listener accepting connections");

    let gate = global_gate();
    // High bit set so TLS conn ids never collide with plain ones.
    let mut next_id: ConnId = 1u64 << 40;
    loop {
        // The same gate guards both listeners — limits ARE shared
        // (a flood that exhausts plain TCP shouldn't fall through
        // to TLS, and vice versa).
        let (stream, peer) = listener.accept().await?;
        let conn_id = next_id;
        next_id += 1;
        // The notice goes out in plaintext, before any TLS handshake.
        let guard = match admit_or_log(&gate, peer, conn_id, &limits, " TLS") {
            Ok(g) => g,
            Err(Refusal::Notice(n)) => {
                send_refusal(stream, n);
                continue;
            }
            Err(Refusal::Silent) => continue,
        };
        let acceptor = acceptor.clone();
        let inbound = inbound.clone();
        tokio::spawn(async move {
            // Guard is taken before the handshake so a handshake
            // failure or timeout also releases the slot.
            serve_tls_conn(
                acceptor.accept(stream),
                conn_id,
                peer,
                inbound,
                guard,
                limits,
            )
            .await;
        });
    }
}

/// Drive one TLS connection: run `accept` (the handshake) under
/// `limits.handshake_timeout`, then hand the stream to
/// [`handle_connection`]. Generic over the handshake future so the
/// timeout path is testable without certificates.
async fn serve_tls_conn<F, S, E>(
    accept: F,
    conn_id: ConnId,
    peer: SocketAddr,
    inbound: InboundTx,
    guard: ConnGuard,
    limits: Limits,
) where
    F: Future<Output = Result<S, E>>,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    E: std::fmt::Display,
{
    match tokio::time::timeout(limits.handshake_timeout, accept).await {
        Ok(Ok(tls)) => handle_connection(conn_id, peer, tls, inbound, guard, limits).await,
        // A handshake failure from a random peer is expected
        // background noise on a public TLS port — port
        // scanners and non-TLS clients probe 4443 constantly
        // and rustls rejects them with "corrupt message".
        // That's not operator-actionable, so log at debug
        // rather than spamming the WARN-level operational log.
        Ok(Err(e)) => debug!(conn_id, peer = %peer, error = %e, "TLS accept failed"),
        Err(_) => debug!(conn_id, peer = %peer, "TLS handshake timed out"),
    }
}

fn load_cert_chain(path: &str) -> std::io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let f = std::fs::File::open(path)
        .map_err(|e| std::io::Error::other(format!("open {path}: {e}")))?;
    let mut r = std::io::BufReader::new(f);
    rustls_pemfile::certs(&mut r).collect::<Result<Vec<_>, _>>()
}

fn load_private_key(path: &str) -> std::io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let f = std::fs::File::open(path)
        .map_err(|e| std::io::Error::other(format!("open {path}: {e}")))?;
    let mut r = std::io::BufReader::new(f);
    let key = rustls_pemfile::private_key(&mut r)?
        .ok_or_else(|| std::io::Error::other(format!("no private key in {path}")))?;
    Ok(key)
}

/// Build an MSSP subnegotiation frame: `IAC SB 70 (MSSP_VAR name
/// MSSP_VAL value)+ IAC SE`. Vars / values are framed per the spec
/// with their leading 1 / 2 bytes. Standard variable names per the
/// MSSP spec: NAME, PLAYERS, UPTIME, CODEBASE, FAMILY, CONTACT,
/// GENRE, LANGUAGE — the caller picks which to send.
#[must_use]
pub fn mssp_packet(vars: &[(&str, &str)]) -> Vec<u8> {
    /// MSSP variable-name marker per the spec (1 = `MSSP_VAR`).
    const MSSP_VAR: u8 = 0x01;
    /// MSSP value marker per the spec (2 = `MSSP_VAL`).
    const MSSP_VAL: u8 = 0x02;
    let mut payload = Vec::with_capacity(
        2 + vars
            .iter()
            .map(|(k, v)| k.len() + v.len() + 2)
            .sum::<usize>(),
    );
    for (name, value) in vars {
        payload.push(MSSP_VAR);
        payload.extend_from_slice(name.as_bytes());
        payload.push(MSSP_VAL);
        payload.extend_from_slice(value.as_bytes());
    }
    subneg(opt::MSSP, &payload)
}

/// Build a GMCP subnegotiation frame:
/// `IAC SB 201 <package_name> <space?> <json_payload> IAC SE`.
///
/// `package` is the dotted package name like `Char.Vitals` or
/// `Room.Info`. `payload` is a JSON literal — pass an empty string
/// for packages that don't carry data.
#[must_use]
pub fn gmcp_packet(package: &str, payload: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(package.len() + 1 + payload.len());
    body.extend_from_slice(package.as_bytes());
    if !payload.is_empty() {
        body.push(b' ');
        body.extend_from_slice(payload.as_bytes());
    }
    subneg(opt::GMCP, &body)
}

// -----------------------------------------------------------------
// Compatibility shims for callers that built negotiation frames
// directly. New code should call `telnet::will`, `telnet::do_`, etc.
// -----------------------------------------------------------------

#[must_use]
pub fn iac_will_gmcp() -> Vec<u8> {
    will(opt::GMCP)
}

#[must_use]
pub fn iac_will_mssp() -> Vec<u8> {
    will(opt::MSSP)
}

/// Per-connection negotiation state. Tracks which optional
/// capabilities the client has accepted so the connection task
/// can gate dependent behavior (EOR emission, MXP tags, MCCP2
/// compression) and holds the shared output capabilities. Reset on
/// disconnect — every connect re-negotiates from scratch since
/// clients may differ between sessions.
// Independent negotiated-capability flags, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone)]
struct CapsLocal {
    gmcp: bool,
    eor: bool,
    mxp: bool,
    naws: bool,
    ttype: bool,
    charset_utf8: bool,
    /// We already answered a client-initiated `DO SGA` with `WILL SGA`.
    sga_will_sent: bool,
    /// We already answered a client-initiated `WILL SGA` with `DO SGA`.
    sga_do_sent: bool,
    /// We already sent the `CHARSET REQUEST` after the client's `DO CHARSET`
    /// (RFC 1143: don't re-answer an option that is already enabled).
    charset_asked: bool,
    /// We already sent the `NEW-ENVIRON SEND` query after the client's
    /// `WILL NEW-ENVIRON`.
    environ_asked: bool,
    /// Number of `IAC SB TTYPE SEND` polls we've sent. Mudlet's
    /// MTTS cycle yields name → terminal → bitmap on the first
    /// three; further polls return the same bitmap. We poll up
    /// to three times then stop.
    ttype_polls: u8,
    /// Previous TTYPE reply; a repeat means the client has run out
    /// of distinct answers (the MTTS end-of-list convention).
    last_ttype: Option<String>,
    /// Colour depth / charset shared with the writer and the server.
    output: OutputHandle,
}

/// Queue the full negotiation advertisement on connect. Each
/// `will`/`do_` is 3 bytes; the whole burst is well under a
/// single TCP segment so clients see it as one round-trip and
/// reply in order. Options we send WILL for: GMCP (we'll push
/// JSON state), MSSP (we'll respond with server metadata),
/// MCCP2 (the client opts in to compression by replying DO),
/// EOR (prompt boundary marker), CHARSET (for UTF-8 confirmation),
/// MXP (clickable links — optional). Options we send DO for:
/// NAWS (window size), TTYPE (terminal type / MTTS), NEW-ENVIRON
/// (env vars). Suppress-Go-Ahead is deliberately NOT offered: a
/// plain line-mode telnet client that sees `WILL SGA` switches to
/// character-at-a-time mode and loses local echo and line editing.
/// If the client itself asks for SGA we agree (see `handle_negotiate`).
fn queue_negotiation(out_tx: &Outbound) {
    let _ = out_tx.try_send(will(opt::GMCP));
    let _ = out_tx.try_send(will(opt::MSSP));
    let _ = out_tx.try_send(will(opt::MCCP2));
    let _ = out_tx.try_send(will(opt::EOR));
    let _ = out_tx.try_send(will(opt::CHARSET));
    let _ = out_tx.try_send(will(opt::MXP));
    let _ = out_tx.try_send(do_(opt::NAWS));
    let _ = out_tx.try_send(do_(opt::TTYPE));
    let _ = out_tx.try_send(do_(opt::NEW_ENVIRON));
    // MSSP advertised + payload pushed inline. MUD list scrapers
    // parse the SB frame whether or not they replied with DO;
    // sending it unconditionally costs ~80 bytes and reaches every
    // scraper in one round-trip.
    let _ = out_tx.try_send(mssp_packet(&[
        ("NAME", "fierymud-rs"),
        ("CODEBASE", "fierymud-rs"),
        ("FAMILY", "Custom"),
        ("GENRE", "Fantasy"),
        ("LANGUAGE", "English"),
        ("DEFAULT_PORT", "4003"),
        ("SSL", "4443"),
        ("HOSTNAME", "minastirith.utaboshi.com"),
    ]));
}

/// Writer task: drains the outbound queue onto the socket.
///
/// MCCP2 — server-to-client zlib compression. Stays at `None` until
/// the read task observes `IAC DO 86` and pushes the
/// start-of-compression marker (`IAC SB 86 IAC SE`) through the
/// channel. The marker frame itself is sent *uncompressed*; the next
/// byte after the marker begins the zlib stream. We detect the marker
/// by exact-match on the outgoing Vec — the protocol guarantees it
/// arrives as a standalone 5-byte frame (built via `mccp2_start()`),
/// never split or concatenated with other content.
///
/// The compressor is a `flate2::Compress` rather than a
/// `ZlibEncoder<Vec<u8>>` because the streaming pattern requires
/// per-frame `Sync` flushes against a long-lived zlib context.
/// `compress_vec` with `FlushCompress::Sync` emits a flush marker
/// after each chunk so the client can decompress incrementally
/// without buffering whole messages.
///
/// The channel itself is bounded (`OUTBOUND_QUEUE_CAP`), so there's
/// no per-connection memory exhaustion risk. Senders use `try_send`
/// and drop on Full; this task just drains as fast as the socket
/// accepts.
fn spawn_writer<W>(
    mut write_half: W,
    mut out_rx: mpsc::Receiver<Vec<u8>>,
    mut close: watch::Receiver<bool>,
    output: OutputHandle,
) -> tokio::task::JoinHandle<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut compressor: Option<flate2::Compress> = None;
        loop {
            tokio::select! {
                // Queued frames win over the close signal so output
                // sent just before a close is never skipped.
                biased;
                msg = out_rx.recv() => {
                    let Some(bytes) = msg else { break };
                    if !write_frame(&mut write_half, &mut compressor, &output, bytes).await {
                        break;
                    }
                }
                () = close_requested(&mut close) => {
                    // Server-initiated close: flush whatever is
                    // already queued, then FIN the socket.
                    while let Ok(bytes) = out_rx.try_recv() {
                        if !write_frame(&mut write_half, &mut compressor, &output, bytes).await {
                            break;
                        }
                    }
                    let _ = write_half.shutdown().await;
                    break;
                }
            }
        }
    })
}

/// Resolve once a server-initiated close has been requested (or the
/// sender is gone). Returns `()` rather than watch's `Ref` guard, which
/// isn't `Send` and would poison the spawned tasks' futures.
async fn close_requested(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|c| *c).await;
}

/// Write one outbound frame, compressing if MCCP2 is active. Returns
/// false when the connection should be torn down (write or
/// compression failure).
async fn write_frame<W: AsyncWrite + Unpin>(
    write_half: &mut W,
    compressor: &mut Option<flate2::Compress>,
    output: &OutputHandle,
    bytes: Vec<u8>,
) -> bool {
    // The single output choke point: colour depth / charset / newline
    // encoding for text frames (telnet frames pass through untouched).
    let bytes = output.encode(bytes);
    let written = if let Some(z) = compressor.as_mut() {
        let mut out = Vec::with_capacity(bytes.len() + 16);
        if z.compress_vec(&bytes, &mut out, flate2::FlushCompress::Sync)
            .is_err()
        {
            // Compression error is fatal — the stream is now out of
            // sync with the client's decoder. Drop the connection
            // rather than corrupt the wire.
            return false;
        }
        write_half.write_all(&out).await
    } else {
        write_half.write_all(&bytes).await
    };
    if written.is_err() {
        return false;
    }
    // Detect the MCCP2 start marker AFTER writing — the marker itself
    // must reach the client uncompressed, and only subsequent frames
    // are deflated.
    if compressor.is_none() && bytes_are_mccp2_marker(&bytes) {
        *compressor = Some(flate2::Compress::new(
            flate2::Compression::default(),
            true, // zlib header
        ));
    }
    true
}

/// Log a read error: a client dropping its socket abruptly (closed
/// laptop, network blip, killed client) surfaces as `ConnectionReset` /
/// `BrokenPipe` / `ConnectionAborted` / `UnexpectedEof` — normal disconnect
/// noise, not operator-actionable. Those go to debug; WARN is kept
/// for genuinely unexpected I/O errors that might signal a real
/// problem.
fn log_read_error(conn_id: ConnId, e: &std::io::Error) {
    use std::io::ErrorKind;
    if matches!(
        e.kind(),
        ErrorKind::ConnectionReset
            | ErrorKind::BrokenPipe
            | ErrorKind::ConnectionAborted
            | ErrorKind::UnexpectedEof
    ) {
        debug!(conn_id, error = %e, "client disconnected (read)");
    } else {
        warn!(conn_id, error = %e, "read error");
    }
}

/// Outcome of one socket read under the pre-login deadline.
enum ReadOutcome {
    Data(usize),
    Closed,
    /// The server asked for this connection to be closed.
    ServerClose,
    Failed(std::io::Error),
    /// Pre-login idle / total timeout elapsed.
    TimedOut,
}

/// Read one chunk. While the connection hasn't logged in, the read
/// races a deadline of `min(last_line + pre_login_idle, start +
/// pre_login_total)`; once the server marks it authenticated the read
/// is unbounded (the in-game idle kicker owns it from there).
async fn read_chunk<R: AsyncRead + Unpin>(
    read_half: &mut R,
    chunk: &mut [u8],
    guard: &ConnGuard,
    limits: &Limits,
    started: Instant,
    last_line: Instant,
    close: &mut watch::Receiver<bool>,
) -> ReadOutcome {
    loop {
        // `wait_for` returns immediately if close was already requested.
        let close_fut = close_requested(close);
        if guard.is_authenticated() {
            return tokio::select! {
                biased;
                () = close_fut => ReadOutcome::ServerClose,
                r = read_half.read(chunk) => match r {
                    Ok(0) => ReadOutcome::Closed,
                    Ok(n) => ReadOutcome::Data(n),
                    Err(e) => ReadOutcome::Failed(e),
                },
            };
        }
        let deadline = (last_line + limits.pre_login_idle).min(started + limits.pre_login_total);
        let read = tokio::time::timeout_at(deadline, read_half.read(chunk));
        let res = tokio::select! {
            biased;
            () = close_fut => return ReadOutcome::ServerClose,
            r = read => r,
        };
        match res {
            Ok(Ok(0)) => return ReadOutcome::Closed,
            Ok(Ok(n)) => return ReadOutcome::Data(n),
            Ok(Err(e)) => return ReadOutcome::Failed(e),
            // Login may have completed while we were waiting; only a
            // still-unauthenticated peer is timed out.
            Err(_) if guard.is_authenticated() => {}
            Err(_) => return ReadOutcome::TimedOut,
        }
    }
}

/// Splits the post-telnet byte stream into command lines. CR, LF and
/// CRLF all terminate a line (a bare CR is what some clients send for
/// Enter); the LF of a CRLF pair — and the NUL of a telnet CR NUL —
/// is swallowed so one Enter yields exactly one line.
#[derive(Default)]
struct LineSplitter {
    buf: Vec<u8>,
    after_cr: bool,
}

/// A line grew past [`MAX_LINE_LEN`] without a terminator.
struct LineTooLong;

impl LineSplitter {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
            after_cr: false,
        }
    }

    /// Feed bytes; completed lines are appended to `lines`. Lines
    /// completed before an overflow are still delivered.
    fn push(&mut self, data: &[u8], lines: &mut Vec<String>) -> Result<(), LineTooLong> {
        for &b in data {
            if std::mem::take(&mut self.after_cr) && (b == b'\n' || b == 0) {
                continue;
            }
            match b {
                // Character-at-a-time clients (and `telnet` once the
                // server stops echoing) send the erase key as BS or
                // DEL instead of editing locally. Drop the whole last
                // character, not one byte of a multi-byte one.
                0x08 | 0x7f => {
                    while let Some(last) = self.buf.pop() {
                        if last & 0xC0 != 0x80 {
                            break;
                        }
                    }
                }
                // Ctrl-U kills the line being typed.
                0x15 => self.buf.clear(),
                b'\r' | b'\n' => {
                    // A blank line still gets forwarded — players use
                    // `<enter>` to dismiss prompts; the dispatcher
                    // treats it as a no-op and refreshes the prompt.
                    lines.push(String::from_utf8_lossy(&self.buf).into_owned());
                    self.buf.clear();
                    self.after_cr = b == b'\r';
                }
                _ if self.buf.len() < MAX_LINE_LEN => self.buf.push(b),
                _ => return Err(LineTooLong),
            }
        }
        Ok(())
    }
}

/// Most negotiation events buffered before `Connected` is released.
const MAX_PENDING_EVENTS: usize = 256;
/// Most payload bytes buffered before `Connected` is released, so a peer
/// can't park large GMCP frames in memory while negotiation is pending.
const MAX_PENDING_BYTES: usize = 64 * 1024;

/// Approximate heap weight of a buffered event, for the pending byte cap.
fn event_bytes(kind: &InboundKind) -> usize {
    match kind {
        InboundKind::Line(line) => line.len(),
        InboundKind::Terminal { value, .. } => value.len(),
        InboundKind::Gmcp { package, payload } => package.len() + payload.len(),
        _ => 0,
    }
}

/// Where a connection's events go. Until the connect greeting is
/// released ([`Sink::connect`]) events are buffered, so the server sees
/// `Connected` first and the negotiation results right after it.
struct Sink<'a> {
    conn_id: ConnId,
    inbound: &'a InboundTx,
    pending: Vec<InboundKind>,
    /// Sum of [`event_bytes`] over `pending`.
    pending_bytes: usize,
    /// `Some` until `Connected` has been sent.
    hello: Option<(SocketAddr, Outbound, OutputHandle)>,
}

impl Sink<'_> {
    fn connected(&self) -> bool {
        self.hello.is_none()
    }

    /// Deliver (or buffer) one event. False when the server is gone.
    async fn send(&mut self, kind: InboundKind) -> bool {
        if self.connected() {
            return self
                .inbound
                .send(Inbound {
                    conn: self.conn_id,
                    kind,
                })
                .await
                .is_ok();
        }
        self.pending_bytes = self.pending_bytes.saturating_add(event_bytes(&kind));
        self.pending.push(kind);
        if self.pending.len() >= MAX_PENDING_EVENTS || self.pending_bytes >= MAX_PENDING_BYTES {
            return self.connect().await;
        }
        true
    }

    /// Release `Connected` and the buffered events. Idempotent.
    async fn connect(&mut self) -> bool {
        let Some((peer, outbound, output)) = self.hello.take() else {
            return true;
        };
        let first = Inbound {
            conn: self.conn_id,
            kind: InboundKind::Connected {
                peer,
                outbound,
                output,
            },
        };
        if self.inbound.send(first).await.is_err() {
            return false;
        }
        self.pending_bytes = 0;
        for kind in std::mem::take(&mut self.pending) {
            let msg = Inbound {
                conn: self.conn_id,
                kind,
            };
            if self.inbound.send(msg).await.is_err() {
                return false;
            }
        }
        true
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_connection<S>(
    conn_id: ConnId,
    peer: SocketAddr,
    stream: S,
    inbound: InboundTx,
    guard: ConnGuard,
    limits: Limits,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read_half, write_half) = tokio::io::split(stream);
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_CAP);
    queue_negotiation(&out_tx);

    let mut caps = CapsLocal::default();
    let mut sink = Sink {
        conn_id,
        inbound: &inbound,
        pending: Vec::new(),
        pending_bytes: 0,
        hello: Some((peer, out_tx.clone(), caps.output.clone())),
    };

    let mut close_rx = guard.shared.close.subscribe();
    let writer = spawn_writer(
        write_half,
        out_rx,
        guard.shared.close.subscribe(),
        caps.output.clone(),
    );

    let mut parser = TelnetParser::new();
    let mut splitter = LineSplitter::new();
    let mut chunk = [0u8; READ_CHUNK];
    let started = Instant::now();
    let mut last_line = started;
    // The greeting waits for the client's capabilities, but not forever
    // (raw TCP clients never answer).
    let connect_deadline = started + limits.negotiation_window;

    // Set when the client half-closes (read EOF): the session ends but
    // the writer is allowed to drain what is already queued.
    let mut drain = false;

    'conn: loop {
        let outcome = if sink.connected() {
            read_chunk(
                &mut read_half,
                &mut chunk,
                &guard,
                &limits,
                started,
                last_line,
                &mut close_rx,
            )
            .await
        } else {
            tokio::select! {
                o = read_chunk(
                    &mut read_half,
                    &mut chunk,
                    &guard,
                    &limits,
                    started,
                    last_line,
                    &mut close_rx,
                ) => o,
                () = tokio::time::sleep_until(connect_deadline) => {
                    if !sink.connect().await {
                        break 'conn;
                    }
                    continue 'conn;
                }
            }
        };
        let n = match outcome {
            ReadOutcome::Data(n) => n,
            ReadOutcome::Closed => {
                drain = true;
                // A bare probe that connects and closes is never
                // announced, but a client that sent input before its
                // half-close (`echo look | nc`) must still reach the
                // world layer and get its reply.
                if !sink.connected()
                    && sink
                        .pending
                        .iter()
                        .any(|k| matches!(k, InboundKind::Line(_)))
                {
                    let _ = sink.connect().await;
                }
                break;
            }
            ReadOutcome::ServerClose => {
                debug!(conn_id, %peer, "server-initiated close");
                break;
            }
            ReadOutcome::Failed(e) => {
                log_read_error(conn_id, &e);
                break;
            }
            ReadOutcome::TimedOut => {
                debug!(conn_id, %peer, "pre-login timeout; closing connection");
                break;
            }
        };
        let (data, events) = parser.feed(&chunk[..n]);

        // Handle telnet events first so any negotiation reply lands
        // before the line we forward upstream.
        for event in events {
            if !handle_telnet_event(conn_id, &out_tx, &mut sink, &mut caps, event).await {
                break 'conn;
            }
        }

        let mut lines = Vec::new();
        let overflow = splitter.push(&data, &mut lines).is_err();
        if !lines.is_empty() {
            last_line = Instant::now();
            // A client already typing doesn't need to be waited for.
            if !sink.connect().await {
                break 'conn;
            }
        }
        for line in lines {
            if !sink.send(InboundKind::Line(line)).await {
                break 'conn;
            }
        }
        if overflow {
            warn!(
                conn_id,
                cap = MAX_LINE_LEN,
                "line exceeded max length; dropping connection"
            );
            break;
        }
    }

    // A connection that never got its `Connected` was never announced,
    // so it has nothing to disconnect either.
    let announced = sink.connected();
    finish_connection(
        conn_id,
        &inbound,
        writer,
        &close_rx,
        announced,
        drain.then_some(out_tx),
    )
    .await;
}

/// Tear-down shared by every exit from the read loop: report the
/// disconnect, then stop the writer. After a server-initiated close the
/// writer is given time to flush queued output and shut the socket down
/// (bounded, in case the client has stopped reading); otherwise it's
/// aborted immediately.
///
/// `drain_tx` is `Some` after a client half-close (read EOF): its sender
/// is dropped so the writer ends on its own once the world layer has
/// released its clone, having written everything queued first (bounded
/// by [`EOF_DRAIN_TIMEOUT`]). Write errors still end the writer at once.
async fn finish_connection(
    conn_id: ConnId,
    inbound: &InboundTx,
    mut writer: tokio::task::JoinHandle<()>,
    close_rx: &watch::Receiver<bool>,
    announced: bool,
    drain_tx: Option<Outbound>,
) {
    if announced {
        let _ = inbound
            .send(Inbound {
                conn: conn_id,
                kind: InboundKind::Disconnected,
            })
            .await;
    }

    if drain_tx.is_some() {
        drop(drain_tx);
        if tokio::time::timeout(EOF_DRAIN_TIMEOUT, &mut writer)
            .await
            .is_err()
        {
            writer.abort();
        }
    } else if *close_rx.borrow() {
        if tokio::time::timeout(CLOSE_FLUSH_TIMEOUT, &mut writer)
            .await
            .is_err()
        {
            writer.abort();
        }
    } else {
        writer.abort();
    }
}

/// How long a server-initiated close waits for the writer to flush
/// pending output to a client that may have stopped reading.
const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the writer may keep draining queued output after the client
/// half-closes its side (e.g. `nc` hitting stdin EOF).
const EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// True if `bytes` is exactly the MCCP2 start-of-compression
/// marker — `IAC SB 86 IAC SE`, 5 bytes — produced by
/// [`mccp2_start`]. Used by the writer task to detect when to
/// flip into compressed mode AFTER passing the marker through
/// uncompressed. Exact-match is safe because we only build this
/// frame in one place; nothing else queues this byte sequence.
fn bytes_are_mccp2_marker(bytes: &[u8]) -> bool {
    bytes
        == [
            telnet::IAC,
            telnet::SB,
            telnet::opt::MCCP2,
            telnet::IAC,
            telnet::SE,
        ]
}

/// Handle one parsed telnet event — respond locally where the
/// answer is purely protocol (option negotiation acks, TTYPE
/// SENDs), forward to the world layer where the data is meaningful
/// (NAWS sizes, GMCP packages). Returns false on a connection-
/// fatal event so the read loop can break; today every event is
/// non-fatal and this is always true.
async fn handle_telnet_event(
    conn_id: ConnId,
    out_tx: &Outbound,
    sink: &mut Sink<'_>,
    caps: &mut CapsLocal,
    event: TelnetEvent,
) -> bool {
    match event {
        TelnetEvent::Negotiate { command, option } => {
            handle_negotiate(out_tx, sink, caps, command, option).await
        }
        TelnetEvent::Subneg { option, payload } => {
            handle_subneg(conn_id, out_tx, sink, caps, option, &payload).await
        }
        TelnetEvent::GoAhead | TelnetEvent::EndOfRecord => {
            // Inbound GA / EOR is unusual — modern MUD clients
            // don't send these to the server. Log and ignore.
            debug!(conn_id, ?event, "inbound IAC marker (ignored)");
            true
        }
    }
}

async fn handle_negotiate(
    out_tx: &Outbound,
    sink: &mut Sink<'_>,
    caps: &mut CapsLocal,
    command: u8,
    option: u8,
) -> bool {
    use telnet::{DO, DONT, WILL, WONT};
    match (command, option) {
        // Client confirms our WILLs (it agrees we may speak this).
        (DO, opt::GMCP) => {
            caps.gmcp = true;
            return forward_capability(sink, "gmcp", true).await;
        }
        (DO, opt::EOR) => {
            caps.eor = true;
            return forward_capability(sink, "eor", true).await;
        }
        (DO, opt::MXP) => {
            caps.mxp = true;
            return forward_capability(sink, "mxp", true).await;
        }
        (DO, opt::CHARSET) => {
            // Client agreed we may negotiate charset; send the
            // request now. ACCEPTED comes back as a SB CHARSET
            // ACCEPTED frame (handled in handle_subneg). Once only: a
            // repeated DO is a no-op for an already-enabled option.
            if !std::mem::replace(&mut caps.charset_asked, true) {
                let _ = out_tx.try_send(charset_request_utf8());
            }
        }
        // The client has no terminal type to report: nothing more to
        // wait for, so keep the safe 16-colour ASCII defaults.
        (WONT, opt::TTYPE) => return sink.connect().await,
        // Deliberate no-ops, merged into one arm:
        //  * DO MSSP — payload was already sent unconditionally on
        //    connect.
        //  * DO MSSP is covered here; DO SGA / WILL SGA are answered
        //    in their own arms below (we never offer SGA ourselves).
        //  * DONT / WONT anything else — the client refuses; the
        //    tracking flag stays false. We don't reply (the protocol
        //    says we could send the opposite but most clients don't
        //    care and Mudlet's negotiation history is already settled).
        (DO, opt::MSSP) | (DONT | WONT, _) => {}
        (WILL, opt::NEW_ENVIRON) => {
            // Ask for the variables that reveal charset / colour
            // depth; the reply arrives as SB NEW-ENVIRON IS. Once only.
            if !std::mem::replace(&mut caps.environ_asked, true) {
                let _ = out_tx.try_send(environ_send_query());
            }
        }
        // The client asked for SGA itself; agree once. The one-shot
        // flags keep a client that re-announces from looping us.
        (DO, opt::SGA) => {
            if !std::mem::replace(&mut caps.sga_will_sent, true) {
                let _ = out_tx.try_send(will(opt::SGA));
            }
        }
        (WILL, opt::SGA) => {
            if !std::mem::replace(&mut caps.sga_do_sent, true) {
                let _ = out_tx.try_send(do_(opt::SGA));
            }
        }
        (DO, opt::MCCP2) => {
            // Client confirmed MCCP2. Push the start-of-compression
            // marker through the outbound channel — the writer
            // task detects it (last frame to be sent uncompressed)
            // and flips into zlib-deflate mode for everything that
            // follows. Subsequent frames go on the wire compressed
            // automatically; nothing else needs to know.
            let _ = out_tx.try_send(mccp2_start());
            debug!("MCCP2 enabled (marker queued)");
        }
        // Client offers a capability we asked DO for.
        (WILL, opt::NAWS) => {
            caps.naws = true;
            // Subneg payload carries the size; arrives next.
        }
        (WILL, opt::TTYPE) => {
            caps.ttype = true;
            // Start the MTTS cycle: poll once now; subsequent
            // polls happen as we receive SUBNEG responses.
            let _ = out_tx.try_send(ttype_send());
            caps.ttype_polls = 1;
        }
        _ => {
            debug!(command, option, "unhandled IAC negotiate");
        }
    }
    true
}

/// Client's CHARSET REQUEST (`<sep> name <sep> name ...`): accept
/// UTF-8 if it is on offer, otherwise reject.
fn answer_charset_request(out_tx: &Outbound, caps: &mut CapsLocal, list: &[u8]) {
    let utf8 = list.split_first().is_some_and(|(&sep, rest)| {
        !rest.starts_with(b"[TTABLE]")
            && rest
                .split(|&b| b == sep)
                .any(|name| name.eq_ignore_ascii_case(b"UTF-8"))
    });
    if utf8 {
        caps.charset_utf8 = true;
        caps.output.mark_utf8();
        let mut body = vec![telnet::charset::ACCEPTED];
        body.extend_from_slice(b"UTF-8");
        let _ = out_tx.try_send(subneg(opt::CHARSET, &body));
    } else {
        let _ = out_tx.try_send(subneg(opt::CHARSET, &[telnet::charset::REJECTED]));
    }
}

async fn handle_subneg(
    conn_id: ConnId,
    out_tx: &Outbound,
    sink: &mut Sink<'_>,
    caps: &mut CapsLocal,
    option: u8,
    payload: &[u8],
) -> bool {
    match option {
        opt::NAWS => {
            if let Some((cols, rows)) = parse_naws(payload) {
                return sink.send(InboundKind::WindowSize { cols, rows }).await;
            }
        }
        opt::TTYPE => {
            // Payload format: `IS <name>` — first byte 0x00, rest
            // is the value. We forward the value upstream and
            // poll for the next response in the MTTS cycle.
            if payload.first() == Some(&telnet::ttype::IS) {
                let value = String::from_utf8_lossy(&payload[1..]).into_owned();
                let mtts = parse_mtts(&value);
                if let Some(bits) = mtts {
                    caps.output.apply_mtts(bits);
                } else {
                    caps.output.apply_term_hint(&value);
                }
                let repeat = caps.last_ttype.as_deref() == Some(value.as_str());
                caps.last_ttype = Some(value.clone());
                if !sink
                    .send(InboundKind::Terminal {
                        index: caps.ttype_polls,
                        value,
                    })
                    .await
                {
                    return false;
                }
                // Cycle up to 3 polls (name → term → MTTS bitmap), stopping
                // early at the bitmap or when the client starts repeating
                // itself (a plain `telnet` only knows $TERM).
                if mtts.is_some() || repeat || caps.ttype_polls >= 3 {
                    return sink.connect().await;
                }
                caps.ttype_polls += 1;
                let _ = out_tx.try_send(ttype_send());
            }
        }
        opt::NEW_ENVIRON => {
            for (name, value) in parse_environ(payload) {
                match name.to_ascii_uppercase().as_str() {
                    "LANG" | "LC_ALL" | "LC_CTYPE" | "CHARSET" if output::is_utf8_name(&value) => {
                        caps.output.mark_utf8();
                    }
                    "MTTS" => {
                        if let Ok(bits) = value.trim().parse::<u32>() {
                            caps.output.apply_mtts(bits);
                        }
                    }
                    "TERMINAL_TYPE" | "TERM" | "COLORTERM" => caps.output.apply_term_hint(&value),
                    _ => {}
                }
            }
        }
        opt::CHARSET => {
            // First byte: REQUEST (1) / ACCEPTED (2) / REJECTED (3).
            match payload.first() {
                Some(&telnet::charset::ACCEPTED) => {
                    caps.charset_utf8 = true;
                    caps.output.mark_utf8();
                    return forward_capability(sink, "utf8", true).await;
                }
                Some(&telnet::charset::REJECTED) => {
                    return forward_capability(sink, "utf8", false).await;
                }
                Some(&telnet::charset::REQUEST) => {
                    answer_charset_request(out_tx, caps, &payload[1..]);
                }
                _ => {}
            }
        }
        opt::GMCP => {
            // Payload shape: "<Package.Name>[ <json>]". Split on
            // the first space; everything after is the JSON body.
            let s = String::from_utf8_lossy(payload);
            let (package, body) = match s.find(' ') {
                Some(i) => (s[..i].to_string(), s[i + 1..].to_string()),
                None => (s.to_string(), String::new()),
            };
            return sink
                .send(InboundKind::Gmcp {
                    package,
                    payload: body,
                })
                .await;
        }
        _ => {
            debug!(conn_id, option, len = payload.len(), "unhandled subneg");
        }
    }
    true
}

async fn forward_capability(sink: &mut Sink<'_>, name: &'static str, on: bool) -> bool {
    sink.send(InboundKind::Capability { name, on }).await
}

#[cfg(test)]
mod iac_tests {
    use super::*;

    #[test]
    fn mssp_packet_has_iac_sb_se_envelope() {
        let frame = mssp_packet(&[("NAME", "test"), ("PORT", "4003")]);
        assert_eq!(frame[0], telnet::IAC);
        assert_eq!(frame[1], telnet::SB);
        assert_eq!(frame[2], opt::MSSP);
        assert_eq!(&frame[frame.len() - 2..], &[telnet::IAC, telnet::SE]);
    }

    #[test]
    fn gmcp_packet_includes_space_separator_when_payload_present() {
        let frame = gmcp_packet("Char.Vitals", r#"{"hp":50}"#);
        // After IAC SB OPT comes "Char.Vitals" then ' ' then JSON.
        let body_start = 3;
        let space_pos = body_start + b"Char.Vitals".len();
        assert_eq!(frame[space_pos], b' ');
    }

    #[test]
    fn gmcp_packet_omits_separator_for_empty_payload() {
        let frame = gmcp_packet("Core.Hello", "");
        // Body is just the package name; immediately followed by
        // IAC SE — no space.
        let body_end = 3 + b"Core.Hello".len();
        assert_eq!(frame[body_end], telnet::IAC);
    }

    #[test]
    fn mccp2_marker_round_trips() {
        // The start-of-compression marker must be exactly 5 bytes
        // and exact-match against `bytes_are_mccp2_marker`.
        let marker = mccp2_start();
        assert_eq!(marker.len(), 5);
        assert!(bytes_are_mccp2_marker(&marker));
    }

    #[test]
    fn mccp2_marker_does_not_match_other_subneg() {
        // GMCP and MSSP subneg frames share the IAC SB / IAC SE
        // envelope but use different option bytes — must not be
        // mistaken for the MCCP2 marker.
        assert!(!bytes_are_mccp2_marker(&gmcp_packet("Core.Hello", "")));
        assert!(!bytes_are_mccp2_marker(&mssp_packet(&[("X", "Y")])));
        // 5-byte sequence with the wrong option also doesn't match.
        let fake = [telnet::IAC, telnet::SB, opt::GMCP, telnet::IAC, telnet::SE];
        assert!(!bytes_are_mccp2_marker(&fake));
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use std::net::Ipv6Addr;
    use tokio::net::{TcpSocket, TcpStream};

    const WAIT: Duration = Duration::from_secs(5);

    fn fast_limits() -> Limits {
        Limits {
            max_connections: usize::MAX,
            max_per_ip: 5,
            handshake_timeout: Duration::from_millis(100),
            pre_login_idle: Duration::from_millis(200),
            pre_login_total: Duration::from_secs(30),
            negotiation_window: Duration::from_millis(50),
        }
    }

    /// Start a plain listener on loopback with a private gate.
    async fn start(limits: Limits) -> (SocketAddr, Arc<Gate>, InboundRx) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(64);
        let gate = Gate::new();
        tokio::spawn(accept_plain(listener, tx, Arc::clone(&gate), limits));
        (addr, gate, rx)
    }

    /// Connect from a specific loopback source IP (127.0.0.0/8 is all
    /// local on Linux), so tests can exercise per-IP logic.
    async fn connect_from(src: &str, dst: SocketAddr) -> TcpStream {
        let sock = TcpSocket::new_v4().unwrap();
        sock.bind(format!("{src}:0").parse().unwrap()).unwrap();
        sock.connect(dst).await.unwrap()
    }

    /// Read until EOF; true if the peer closed within `WAIT`.
    async fn closed_by_peer(s: &mut TcpStream) -> bool {
        let mut buf = [0u8; 1024];
        tokio::time::timeout(WAIT, async {
            loop {
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        })
        .await
        .is_ok()
    }

    async fn next_event(rx: &mut InboundRx) -> Inbound {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("timed out waiting for inbound event")
            .expect("inbound closed")
    }

    fn active(gate: &Gate) -> usize {
        gate.lock().active
    }

    async fn wait_active(gate: &Gate, want: usize) {
        for _ in 0..200 {
            if active(gate) == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("active never reached {want}, is {}", active(gate));
    }

    #[tokio::test]
    async fn stalled_tls_handshake_is_dropped_after_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (server, peer) = listener.accept().await.unwrap();

        let gate = Gate::new();
        let limits = fast_limits();
        let guard = gate.admit(peer.ip(), 7, &limits, Instant::now()).unwrap();
        assert_eq!(active(&gate), 1);
        let (tx, _rx) = mpsc::channel(8);

        // A handshake that holds the socket and never completes.
        let stalled = async move {
            let _held = server;
            std::future::pending::<Result<TcpStream, std::io::Error>>().await
        };
        let started = std::time::Instant::now();
        serve_tls_conn(stalled, 7, peer, tx, guard, limits).await;

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout did not fire"
        );
        assert_eq!(active(&gate), 0, "slot must be released");
        assert!(closed_by_peer(&mut client).await, "socket must be closed");
    }

    #[tokio::test]
    async fn pre_login_connection_closed_after_idle_timeout() {
        let (addr, gate, mut rx) = start(fast_limits()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Connected { .. }
        ));
        assert!(
            closed_by_peer(&mut client).await,
            "idle pre-login conn must be closed"
        );
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Disconnected
        ));
        wait_active(&gate, 0).await;
    }

    #[tokio::test]
    async fn authenticated_connection_survives_pre_login_timeout() {
        let (addr, gate, mut rx) = start(fast_limits()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        let conn = match next_event(&mut rx).await {
            Inbound {
                conn,
                kind: InboundKind::Connected { .. },
            } => conn,
            other => panic!("unexpected {other:?}"),
        };
        gate.mark_authenticated(conn);
        // Well past `pre_login_idle` (200 ms): still open and talking.
        tokio::time::sleep(Duration::from_millis(600)).await;
        client.write_all(b"look\r\n").await.unwrap();
        match next_event(&mut rx).await.kind {
            InboundKind::Line(l) => assert_eq!(l, "look"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn sixth_connection_from_one_ip_refused_other_ip_allowed() {
        let (addr, gate, _rx) = start(Limits {
            max_per_ip: 5,
            ..fast_limits()
        })
        .await;
        let mut held = Vec::new();
        for _ in 0..5 {
            let mut c = connect_from("127.0.0.2", addr).await;
            // Accepted connections get the negotiation preamble.
            let mut b = [0u8; 16];
            assert!(c.read(&mut b).await.unwrap() > 0);
            held.push(c);
        }
        wait_active(&gate, 5).await;

        let mut sixth = connect_from("127.0.0.2", addr).await;
        let mut notice = Vec::new();
        assert!(
            tokio::time::timeout(WAIT, sixth.read_to_end(&mut notice))
                .await
                .is_ok(),
            "6th connection from same IP must be refused"
        );
        assert_eq!(
            String::from_utf8_lossy(&notice),
            "Too many connections from your address. Please close other sessions and try again.\r\n"
        );

        let mut other = connect_from("127.0.0.3", addr).await;
        let mut b = [0u8; 16];
        assert!(
            other.read(&mut b).await.unwrap() > 0,
            "different IP must connect"
        );

        // Closing one frees a slot for the original IP.
        drop(held.pop());
        wait_active(&gate, 5).await; // 4 from .2 + 1 from .3
        let mut again = connect_from("127.0.0.2", addr).await;
        assert!(
            again.read(&mut b).await.unwrap() > 0,
            "slot should be reusable"
        );
    }

    #[tokio::test]
    async fn server_close_flushes_pending_output_then_eof() {
        let (addr, gate, mut rx) = start(fast_limits()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (conn, out) = match next_event(&mut rx).await {
            Inbound {
                conn,
                kind: InboundKind::Connected { outbound, .. },
            } => (conn, outbound),
            other => panic!("unexpected {other:?}"),
        };
        // Authenticated so the pre-login timeout can't be what ends it.
        gate.mark_authenticated(conn);
        out.try_send(b"goodbye, traveller\r\n".to_vec()).unwrap();
        out.try_send(b"second line\r\n".to_vec()).unwrap();
        assert!(gate.close_connection(conn));

        // Everything queued before the close arrives, then EOF.
        let mut got = Vec::new();
        tokio::time::timeout(WAIT, client.read_to_end(&mut got))
            .await
            .expect("client never saw EOF")
            .unwrap();
        let text = String::from_utf8_lossy(&got);
        assert!(
            text.contains("goodbye, traveller\r\nsecond line\r\n"),
            "got {text:?}"
        );
        // The reader side reports a disconnect and the slot is freed.
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Disconnected
        ));
        wait_active(&gate, 0).await;
        assert!(!gate.close_connection(conn), "already closed");
    }

    #[tokio::test]
    async fn server_close_works_before_login_and_while_client_is_silent() {
        let (addr, gate, mut rx) = start(Limits {
            pre_login_idle: Duration::from_secs(60),
            ..fast_limits()
        })
        .await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        let conn = match next_event(&mut rx).await {
            Inbound {
                conn,
                kind: InboundKind::Connected { .. },
            } => conn,
            other => panic!("unexpected {other:?}"),
        };
        assert!(gate.close_connection(conn));
        assert!(closed_by_peer(&mut client).await);
    }

    #[tokio::test]
    async fn cr_only_and_crlf_input_yield_one_line_each() {
        let (addr, _gate, mut rx) = start(fast_limits()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Connected { .. }
        ));
        client.write_all(b"north\rsouth\r\neast\n").await.unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            match next_event(&mut rx).await.kind {
                InboundKind::Line(l) => got.push(l),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(got, ["north", "south", "east"]);
        // No phantom empty line from the LF of the CRLF.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err()
        );
    }

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut sp = LineSplitter::new();
        let mut lines = Vec::new();
        for c in chunks {
            assert!(sp.push(c, &mut lines).is_ok());
        }
        lines
    }

    #[test]
    fn splitter_backspace_and_del_remove_previous_char() {
        assert_eq!(split(&[b"helo\x08lo\r"]), ["hello"]);
        assert_eq!(split(&[b"abcx\x7f\r"]), ["abc"]);
        // Editing works across read boundaries.
        assert_eq!(split(&[b"nort", b"\x08\x08rth\r"]), ["north"]);
        // Several in a row.
        assert_eq!(split(&[b"abc\x08\x7f\x08\r"]), [""]);
    }

    #[test]
    fn splitter_backspace_removes_whole_multibyte_char() {
        // 'é' is 2 bytes, '€' 3, '😀' 4.
        assert_eq!(split(&["caf\u{e9}\x08\r".as_bytes()]), ["caf"]);
        assert_eq!(split(&["a\u{20ac}\x7f\r".as_bytes()]), ["a"]);
        assert_eq!(split(&["a\u{1f600}\x08b\r".as_bytes()]), ["ab"]);
        // Multibyte char split across reads, then erased.
        let e = "\u{e9}".as_bytes();
        assert_eq!(split(&[&e[..1], &[e[1], 0x08, b'x', b'\r']]), ["x"]);
    }

    #[test]
    fn splitter_backspace_on_empty_buffer_is_noop() {
        assert_eq!(split(&[b"\x08\x7f\x08hi\r"]), ["hi"]);
        // Does not reach back past a completed line.
        assert_eq!(split(&[b"ab\r\x08\x7fcd\r"]), ["ab", "cd"]);
        assert_eq!(split(&[b"\x7f\r"]), [""]);
    }

    #[test]
    fn connect_negotiation_does_not_offer_sga_but_keeps_other_options() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_CAP);
        queue_negotiation(&tx);
        drop(tx);
        let mut wire = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            wire.extend_from_slice(&frame);
        }
        let has = |needle: &[u8]| wire.windows(needle.len()).any(|w| w == needle);
        assert!(!has(&[0xff, 0xfb, 0x03]), "IAC WILL SGA offered");
        assert!(!has(&[0xff, 0xfd, 0x03]), "IAC DO SGA requested");
        for (cmd, option) in [
            (telnet::WILL, opt::GMCP),
            (telnet::WILL, opt::MSSP),
            (telnet::WILL, opt::MCCP2),
            (telnet::WILL, opt::EOR),
            (telnet::WILL, opt::CHARSET),
            (telnet::WILL, opt::MXP),
            (telnet::DO, opt::NAWS),
            (telnet::DO, opt::TTYPE),
            (telnet::DO, opt::NEW_ENVIRON),
        ] {
            assert!(has(&[telnet::IAC, cmd, option]), "missing {cmd} {option}");
        }
        assert!(has(&[telnet::IAC, telnet::SB, opt::MSSP]), "MSSP payload");
    }

    #[tokio::test]
    async fn pending_bytes_cap_releases_connected_early() {
        let (in_tx, mut in_rx) = mpsc::channel::<Inbound>(64);
        let (out_tx, _out_rx) = mpsc::channel::<Vec<u8>>(8);
        let mut sink = Sink {
            conn_id: 1,
            inbound: &in_tx,
            pending: Vec::new(),
            pending_bytes: 0,
            hello: Some(("127.0.0.1:1".parse().unwrap(), out_tx, OutputHandle::new())),
        };
        let big = "x".repeat(MAX_PENDING_BYTES / 2);
        let gmcp = |p: &str| InboundKind::Gmcp {
            package: "Core.Hello".into(),
            payload: p.into(),
        };
        assert!(sink.send(gmcp(&big)).await);
        assert!(!sink.connected(), "under the cap: still buffering");
        assert!(sink.send(gmcp(&big)).await);
        assert!(sink.connected(), "byte cap reached: Connected released");
        assert_eq!(sink.pending_bytes, 0);
        assert!(matches!(
            in_rx.recv().await.unwrap().kind,
            InboundKind::Connected { .. }
        ));
        assert!(matches!(
            in_rx.recv().await.unwrap().kind,
            InboundKind::Gmcp { .. }
        ));
        assert!(matches!(
            in_rx.recv().await.unwrap().kind,
            InboundKind::Gmcp { .. }
        ));
    }

    #[tokio::test]
    async fn repeated_will_environ_and_do_charset_query_once() {
        let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(8);
        let (in_tx, _in_rx) = mpsc::channel::<Inbound>(8);
        let mut sink = Sink {
            conn_id: 1,
            inbound: &in_tx,
            pending: Vec::new(),
            pending_bytes: 0,
            hello: None,
        };
        let mut caps = CapsLocal::default();
        for _ in 0..3 {
            handle_negotiate(
                &out_tx,
                &mut sink,
                &mut caps,
                telnet::WILL,
                opt::NEW_ENVIRON,
            )
            .await;
            handle_negotiate(&out_tx, &mut sink, &mut caps, telnet::DO, opt::CHARSET).await;
        }
        drop(out_tx);
        let mut frames = Vec::new();
        while let Some(f) = out_rx.recv().await {
            frames.push(f);
        }
        assert_eq!(frames, [environ_send_query(), charset_request_utf8()]);
    }

    #[tokio::test]
    async fn client_initiated_sga_is_accepted_once_without_looping() {
        let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(8);
        let (in_tx, _in_rx) = mpsc::channel::<Inbound>(8);
        let mut sink = Sink {
            conn_id: 1,
            inbound: &in_tx,
            pending: Vec::new(),
            pending_bytes: 0,
            hello: None,
        };
        let mut caps = CapsLocal::default();
        for _ in 0..3 {
            handle_negotiate(&out_tx, &mut sink, &mut caps, telnet::DO, opt::SGA).await;
            handle_negotiate(&out_tx, &mut sink, &mut caps, telnet::WILL, opt::SGA).await;
        }
        drop(out_tx);
        let mut frames = Vec::new();
        while let Some(f) = out_rx.recv().await {
            frames.push(f);
        }
        assert_eq!(
            frames,
            [
                vec![telnet::IAC, telnet::WILL, opt::SGA],
                vec![telnet::IAC, telnet::DO, opt::SGA],
            ]
        );
    }

    #[tokio::test]
    async fn client_half_close_still_receives_all_queued_output() {
        let (addr, gate, mut rx) = start(fast_limits()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        // Send a command then half-close, like `echo look | nc`. (A
        // client that closes without sending anything is never
        // announced, since the greeting is deferred.)
        client.write_all(b"look\r\n").await.unwrap();
        client.shutdown().await.unwrap();
        let (conn, out) = match next_event(&mut rx).await {
            Inbound {
                conn,
                kind: InboundKind::Connected { outbound, .. },
            } => (conn, outbound),
            other => panic!("unexpected {other:?}"),
        };
        // Queue a large banner (stand-in for the world layer's reply),
        // well past what a single write/segment would carry.
        let banner = "x".repeat(2900) + "\r\nWelcome\r\n";
        out.try_send(banner.clone().into_bytes()).unwrap();
        // The buffered input is delivered, and the session still ends...
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Line(ref l) if l == "look"
        ));
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Disconnected
        ));
        // ...and the world layer releases its sender afterwards.
        out.try_send(b"late reply\r\n".to_vec()).unwrap();
        drop(out);

        let mut got = Vec::new();
        tokio::time::timeout(WAIT, client.read_to_end(&mut got))
            .await
            .expect("client never saw EOF")
            .unwrap();
        let text = String::from_utf8_lossy(&got);
        assert!(
            text.contains(&banner),
            "banner truncated: {} bytes",
            got.len()
        );
        assert!(text.contains("late reply\r\n"), "late reply lost");
        // Negotiation burst was delivered too.
        assert!(
            got.windows(3)
                .any(|w| w == [telnet::IAC, telnet::WILL, opt::GMCP])
        );
        wait_active(&gate, 0).await;
        let _ = conn;
    }

    #[test]
    fn splitter_cr_terminates_line() {
        assert_eq!(split(&[b"hello\r"]), ["hello"]);
    }

    #[test]
    fn splitter_crlf_is_exactly_one_line() {
        assert_eq!(split(&[b"hello\r\n"]), ["hello"]);
        // CR and LF split across reads.
        assert_eq!(split(&[b"hello\r", b"\nworld\n"]), ["hello", "world"]);
        // Telnet CR NUL.
        assert_eq!(split(&[b"hello\r\0next\r"]), ["hello", "next"]);
    }

    #[test]
    fn splitter_blank_lines_are_forwarded() {
        assert_eq!(split(&[b"\r\n\r\n"]), ["", ""]);
        assert_eq!(split(&[b"\n\n"]), ["", ""]);
    }

    #[test]
    fn splitter_rejects_overlong_line() {
        let mut sp = LineSplitter::new();
        let mut lines = Vec::new();
        assert!(sp.push(&vec![b'x'; MAX_LINE_LEN], &mut lines).is_ok());
        assert!(sp.push(b"x", &mut lines).is_err());
    }

    #[test]
    fn splitter_handles_backspace_and_delete() {
        // BS and DEL erase the previous character (#10: char-mode clients).
        assert_eq!(split(&[b"helx\x08lo\r"]), ["hello"]);
        assert_eq!(split(&[b"helx\x7flo\r\n"]), ["hello"]);
        // Erasing past the start is harmless.
        assert_eq!(split(&[b"\x7f\x08ab\r"]), ["ab"]);
        // A multi-byte character is erased whole, even across reads.
        assert_eq!(split(&["caf\u{e9}".as_bytes(), b"\x7f\r"]), ["caf"]);
        // Ctrl-U kills the line.
        assert_eq!(split(&[b"oops\x15north\r"]), ["north"]);
    }

    /// Limits with a window long enough to observe the deferral.
    fn slow_negotiation() -> Limits {
        Limits {
            negotiation_window: Duration::from_millis(400),
            pre_login_idle: Duration::from_secs(5),
            ..fast_limits()
        }
    }

    fn ttype_is(value: &str) -> Vec<u8> {
        let mut p = vec![telnet::ttype::IS];
        p.extend_from_slice(value.as_bytes());
        subneg(opt::TTYPE, &p)
    }

    /// Wait for `Connected` and return its output handle.
    async fn connected_output(rx: &mut InboundRx) -> OutputHandle {
        match next_event(rx).await.kind {
            InboundKind::Connected { output, .. } => output,
            other => panic!("expected Connected first, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_waits_out_the_window_for_a_silent_client() {
        let (addr, _gate, mut rx) = start(slow_negotiation()).await;
        let t0 = std::time::Instant::now();
        let _client = TcpStream::connect(addr).await.unwrap();
        let output = connected_output(&mut rx).await;
        assert!(
            t0.elapsed() >= Duration::from_millis(350),
            "greeting released after only {:?}",
            t0.elapsed()
        );
        // Nothing was learned: safe defaults.
        assert_eq!(output.color(), ColorDepth::Ansi16);
        assert_eq!(output.charset(), Charset::Ascii);
    }

    #[tokio::test]
    async fn mtts_reply_releases_connect_early_with_capabilities() {
        let (addr, _gate, mut rx) = start(slow_negotiation()).await;
        let t0 = std::time::Instant::now();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut hello = will(opt::TTYPE);
        hello.extend(ttype_is("MTTS 2349"));
        hello.extend(telnet::subneg(opt::NAWS, &[0, 100, 0, 30]));
        client.write_all(&hello).await.unwrap();
        let output = connected_output(&mut rx).await;
        assert!(
            t0.elapsed() < Duration::from_millis(300),
            "should not wait the full window: {:?}",
            t0.elapsed()
        );
        assert_eq!(output.color(), ColorDepth::TrueColor);
        assert_eq!(output.charset(), Charset::Utf8);
        // Events that raced ahead of Connected are replayed after it.
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::Terminal { .. }
        ));
        assert!(matches!(
            next_event(&mut rx).await.kind,
            InboundKind::WindowSize {
                cols: 100,
                rows: 30
            }
        ));
    }

    #[tokio::test]
    async fn plain_telnet_term_name_gives_256_colour_ascii() {
        // GNU telnet answers every TTYPE poll with $TERM.
        let (addr, _gate, mut rx) = start(slow_negotiation()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut hello = will(opt::TTYPE);
        hello.extend(ttype_is("XTERM-256COLOR"));
        hello.extend(ttype_is("XTERM-256COLOR"));
        client.write_all(&hello).await.unwrap();
        let output = connected_output(&mut rx).await;
        assert_eq!(output.color(), ColorDepth::Ansi256);
        assert_eq!(output.charset(), Charset::Ascii);
    }

    #[tokio::test]
    async fn refusing_ttype_releases_connect_with_defaults() {
        let (addr, _gate, mut rx) = start(slow_negotiation()).await;
        let t0 = std::time::Instant::now();
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&wont(opt::TTYPE)).await.unwrap();
        let output = connected_output(&mut rx).await;
        assert!(t0.elapsed() < Duration::from_millis(300));
        assert_eq!(output.color(), ColorDepth::Ansi16);
    }

    #[tokio::test]
    async fn new_environ_reply_reveals_utf8_locale() {
        let (addr, _gate, mut rx) = start(slow_negotiation()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut env = vec![telnet::environ::IS, telnet::environ::VAR];
        env.extend_from_slice(b"LANG");
        env.push(telnet::environ::VALUE);
        env.extend_from_slice(b"en_US.UTF-8");
        let mut hello = will(opt::NEW_ENVIRON);
        hello.extend(subneg(opt::NEW_ENVIRON, &env));
        hello.extend(will(opt::TTYPE));
        hello.extend(ttype_is("MTTS 1"));
        client.write_all(&hello).await.unwrap();
        let output = connected_output(&mut rx).await;
        assert_eq!(output.charset(), Charset::Utf8);
        assert_eq!(output.color(), ColorDepth::Ansi16);
    }

    #[tokio::test]
    async fn writer_encodes_text_for_the_negotiated_client() {
        let (addr, _gate, mut rx) = start(fast_limits()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        // A plain client: nothing negotiated -> 16-colour ASCII.
        let (outbound, output) = match next_event(&mut rx).await.kind {
            InboundKind::Connected {
                outbound, output, ..
            } => (outbound, output),
            other => panic!("{other:?}"),
        };
        outbound
            .send(
                "A \u{2014} B \x1b[38;5;196mred\x1b[0m\n"
                    .as_bytes()
                    .to_vec(),
            )
            .await
            .unwrap();
        // Telnet framing must pass through byte for byte.
        let gmcp = gmcp_packet("Core.Hello", "{\"x\":\"\u{2014}\"}");
        outbound.send(gmcp.clone()).await.unwrap();
        let want_text = b"A -- B \x1b[91mred\x1b[0m\r\n";
        let mut got = Vec::new();
        let mut buf = [0u8; 2048];
        while !contains_seq(&got, &gmcp) {
            let n = tokio::time::timeout(WAIT, client.read(&mut buf))
                .await
                .expect("timed out reading")
                .unwrap();
            assert!(n > 0, "closed early");
            got.extend_from_slice(&buf[..n]);
        }
        assert!(
            contains_seq(&got, want_text),
            "got {:?}",
            String::from_utf8_lossy(&got)
        );

        // A player override flips the same connection to colour off / UTF-8.
        output.set_color_override(Some(ColorDepth::None));
        output.set_charset_override(Some(Charset::Utf8));
        outbound
            .send("\x1b[31m\u{2605}\x1b[0m\r\n".as_bytes().to_vec())
            .await
            .unwrap();
        let want = "\u{2605}\r\n".as_bytes();
        let mut got2 = Vec::new();
        while !contains_seq(&got2, want) {
            let n = tokio::time::timeout(WAIT, client.read(&mut buf))
                .await
                .expect("timed out reading")
                .unwrap();
            assert!(n > 0, "closed early");
            got2.extend_from_slice(&buf[..n]);
        }
        assert!(!got2.contains(&0x1b), "colour should be stripped");
    }

    fn contains_seq(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[tokio::test]
    async fn disconnect_before_the_greeting_is_never_announced() {
        let (addr, _gate, mut rx) = start(slow_negotiation()).await;
        let client = TcpStream::connect(addr).await.unwrap();
        drop(client);
        assert!(
            tokio::time::timeout(Duration::from_millis(800), rx.recv())
                .await
                .is_err(),
            "a connection that never completed negotiation produced events"
        );
    }

    fn v6(i: u128) -> IpAddr {
        IpAddr::V6(Ipv6Addr::from((0x2001_0db8u128 << 96) | i))
    }

    #[test]
    fn throttle_map_stays_capped_under_distinct_ip_flood() {
        let gate = Gate::new();
        let mut st = gate.lock();
        let now = Instant::now();
        for i in 0..100_000u128 {
            // Every address is new and inside the window, so nothing
            // is evictable; the map must still not exceed the cap.
            assert!(matches!(
                throttle_allow(&mut st, v6(i), now),
                Throttle::Allow
            ));
            assert!(st.throttle.len() <= THROTTLE_MAP_CAP);
        }
        assert_eq!(st.throttle.len(), THROTTLE_MAP_CAP);
    }

    #[test]
    fn throttle_evicts_expired_entries() {
        let gate = Gate::new();
        let mut st = gate.lock();
        let t0 = Instant::now();
        for i in 0..1000u128 {
            throttle_allow(&mut st, v6(i), t0);
        }
        assert_eq!(st.throttle.len(), 1000);
        // Past the window and the sweep interval: next insert sweeps.
        let later = t0 + THROTTLE_WINDOW + THROTTLE_SWEEP_INTERVAL + Duration::from_secs(1);
        throttle_allow(&mut st, v6(5_000_000), later);
        assert_eq!(st.throttle.len(), 1);
    }

    #[test]
    fn throttle_still_rate_limits_a_single_ip() {
        let gate = Gate::new();
        let mut st = gate.lock();
        let now = Instant::now();
        for _ in 0..MAX_CONNECTS_PER_MIN {
            assert!(matches!(
                throttle_allow(&mut st, v6(1), now),
                Throttle::Allow
            ));
        }
        assert!(matches!(
            throttle_allow(&mut st, v6(1), now),
            Throttle::RejectFirst
        ));
        assert!(matches!(
            throttle_allow(&mut st, v6(1), now),
            Throttle::RejectRepeat
        ));
    }
}
