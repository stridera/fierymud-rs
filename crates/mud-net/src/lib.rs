use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

pub mod telnet;
pub use telnet::{
    charset_request_utf8, do_, dont, iac_eor, iac_ga, mccp2_start, negotiate, opt, parse_mtts,
    parse_naws, subneg, ttype_send, will, wont, Event as TelnetEvent, Parser as TelnetParser,
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

/// Live count of accepted-but-not-yet-disconnected connections,
/// summed across both the plain-telnet and TLS listeners. Compared
/// against the per-`serve()` cap on every accept; the connection-
/// handler decrements on exit via the `ConnGuard` RAII handle so the
/// count reflects sockets that have actually closed.
static ACTIVE_CONNECTIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// RAII guard that increments `ACTIVE_CONNECTIONS` on construction
/// and decrements on Drop. Owned by the spawned per-connection task
/// so the count reflects the connection's actual lifetime — Drop
/// fires whether the task ends gracefully, errors out (TLS
/// handshake failure, IAC parse panic), or is cancelled. Without
/// this, a failure path would leak a permanent +1 against the cap.
struct ConnGuard;

impl ConnGuard {
    fn new() -> Self {
        ACTIVE_CONNECTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        ACTIVE_CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct Inbound {
    pub conn: ConnId,
    pub kind: InboundKind,
}

#[derive(Debug)]
pub enum InboundKind {
    Connected {
        peer: SocketAddr,
        outbound: Outbound,
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

/// Bind a plain-TCP listener and forward every accepted connection's
/// lines into `inbound`. Returns only on listener error; runs forever
/// otherwise.
///
/// `max_connections` caps the *total* accepted-and-still-open count
/// across this listener and the TLS sibling — they share the same
/// `ACTIVE_CONNECTIONS` counter, so a flood that fills the plain-TCP
/// channel can't leave the TLS listener wide open. A zero or
/// negative-sized cap (sentinel `usize::MAX`) means "no limit," for
/// dev / unrestricted operator override.
pub async fn serve(
    bind_addr: &str,
    inbound: InboundTx,
    max_connections: usize,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    info!(
        addr = %listener.local_addr()?,
        max_connections,
        "telnet listener accepting connections"
    );

    let mut next_id: ConnId = 1;
    loop {
        let (stream, peer) = listener.accept().await?;
        if banned(peer.ip()) {
            warn!(%peer, "banlist: refusing connection");
            drop(stream);
            continue;
        }
        match throttle_allow(peer.ip()) {
            Throttle::Allow => {}
            Throttle::RejectFirst => {
                warn!(%peer, "throttle: rejecting connection — over rate limit");
                drop(stream);
                continue;
            }
            Throttle::RejectRepeat => {
                debug!(%peer, "throttle: rejecting connection (continuing flood)");
                drop(stream);
                continue;
            }
        }
        if ACTIVE_CONNECTIONS.load(std::sync::atomic::Ordering::SeqCst) >= max_connections {
            warn!(%peer, max_connections, "max_connections reached; refusing");
            drop(stream);
            continue;
        }
        let conn_id = next_id;
        next_id += 1;
        let inbound = inbound.clone();
        tokio::spawn(async move {
            // ConnGuard lifetime spans the whole task, so a panic /
            // early-return inside `handle_connection` still
            // decrements the counter on Drop.
            let _guard = ConnGuard::new();
            handle_connection(conn_id, peer, stream, inbound).await;
        });
    }
}

/// Per-IP connection-rate gate. Refuses an accept when the source
/// IP has connected more than `MAX_CONNECTS_PER_MIN` times in the
/// last minute. Shared between the plain-TCP and TLS listeners
/// since they both share the same threat model.
static THROTTLER: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, ThrottleEntry>>,
> = std::sync::OnceLock::new();

/// Per-IP throttle state: connection timestamps in the rolling
/// window plus a `warned` latch so a sustained flood logs a single
/// WARN on the transition into throttled state rather than one per
/// rejected connection. The latch clears once the peer falls back
/// under the limit (window slides / flood stops).
#[derive(Default)]
struct ThrottleEntry {
    times: std::collections::VecDeque<std::time::Instant>,
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

const MAX_CONNECTS_PER_MIN: usize = 10;

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
static BANLIST: std::sync::OnceLock<std::collections::HashSet<std::net::IpAddr>> =
    std::sync::OnceLock::new();

fn banlist() -> &'static std::collections::HashSet<std::net::IpAddr> {
    BANLIST.get_or_init(|| {
        std::env::var("MUD_BANLIST")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .filter_map(|s| s.trim().parse::<std::net::IpAddr>().ok())
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn banned(ip: std::net::IpAddr) -> bool {
    banlist().contains(&ip)
}

/// Returns true when the IP is allowed to connect. Records the
/// attempt; expired entries are pruned in the same call so the
/// map doesn't grow unboundedly.
fn throttle_allow(ip: std::net::IpAddr) -> Throttle {
    let map_lock = THROTTLER.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = map_lock.lock().expect("throttler poisoned");
    let now = std::time::Instant::now();
    let window = std::time::Duration::from_secs(60);
    let entry = map.entry(ip).or_default();
    while entry.times.front().is_some_and(|t| now.duration_since(*t) > window) {
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
    max_connections: usize,
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

    // High bit set so TLS conn ids never collide with plain ones.
    let mut next_id: ConnId = 1u64 << 40;
    loop {
        // Same throttler guards both listeners — limit IS shared
        // (a flood that exhausts plain TCP shouldn't fall through
        // to TLS, and vice versa).
        let (stream, peer) = listener.accept().await?;
        if banned(peer.ip()) {
            warn!(%peer, "banlist: refusing TLS connection");
            drop(stream);
            continue;
        }
        match throttle_allow(peer.ip()) {
            Throttle::Allow => {}
            Throttle::RejectFirst => {
                warn!(%peer, "throttle: rejecting TLS connection — over rate limit");
                drop(stream);
                continue;
            }
            Throttle::RejectRepeat => {
                debug!(%peer, "throttle: rejecting TLS connection (continuing flood)");
                drop(stream);
                continue;
            }
        }
        if ACTIVE_CONNECTIONS.load(std::sync::atomic::Ordering::SeqCst) >= max_connections {
            warn!(%peer, max_connections, "max_connections reached; refusing TLS");
            drop(stream);
            continue;
        }
        let acceptor = acceptor.clone();
        let inbound = inbound.clone();
        let conn_id = next_id;
        next_id += 1;
        tokio::spawn(async move {
            // Guard before TLS handshake so a handshake failure also
            // decrements; the count tracks accept-side commitment,
            // not just successfully-handshaked connections.
            let _guard = ConnGuard::new();
            match acceptor.accept(stream).await {
                Ok(tls) => handle_connection(conn_id, peer, tls, inbound).await,
                // A handshake failure from a random peer is expected
                // background noise on a public TLS port — port
                // scanners and non-TLS clients probe 4443 constantly
                // and rustls rejects them with "corrupt message".
                // That's not operator-actionable, so log at debug
                // rather than spamming the WARN-level operational log.
                Err(e) => debug!(conn_id, peer = %peer, error = %e, "TLS accept failed"),
            }
        });
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
    /// MSSP variable-name marker per the spec (1 = MSSP_VAR).
    const MSSP_VAR: u8 = 0x01;
    /// MSSP value marker per the spec (2 = MSSP_VAL).
    const MSSP_VAL: u8 = 0x02;
    let mut payload =
        Vec::with_capacity(2 + vars.iter().map(|(k, v)| k.len() + v.len() + 2).sum::<usize>());
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
/// compression). Reset on disconnect — every connect re-negotiates
/// from scratch since clients may differ between sessions.
#[derive(Debug, Default, Clone, Copy)]
struct CapsLocal {
    gmcp: bool,
    eor: bool,
    mxp: bool,
    naws: bool,
    ttype: bool,
    charset_utf8: bool,
    /// Number of `IAC SB TTYPE SEND` polls we've sent. Mudlet's
    /// MTTS cycle yields name → terminal → bitmap on the first
    /// three; further polls return the same bitmap. We poll up
    /// to three times then stop.
    ttype_polls: u8,
}

async fn handle_connection<S>(
    conn_id: ConnId,
    peer: SocketAddr,
    stream: S,
    inbound: InboundTx,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_CAP);

    // Push the full negotiation advertisement on connect. Each
    // `will`/`do_` is 3 bytes; the whole burst is well under a
    // single TCP segment so clients see it as one round-trip and
    // reply in order. Options we send WILL for: GMCP (we'll push
    // JSON state), MSSP (we'll respond with server metadata),
    // MCCP2 (the client opts in to compression by replying DO),
    // EOR (prompt boundary marker), CHARSET (for UTF-8 confirmation),
    // MXP (clickable links — optional). Options we send DO for:
    // NAWS (window size), TTYPE (terminal type / MTTS), NEW-ENVIRON
    // (env vars). Suppress-Go-Ahead is mutually negotiated — both
    // WILL and DO so each side knows the other won't send GA.
    let _ = out_tx.try_send(will(opt::SGA));
    let _ = out_tx.try_send(do_(opt::SGA));
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

    if inbound
        .send(Inbound {
            conn: conn_id,
            kind: InboundKind::Connected {
                peer,
                outbound: out_tx.clone(),
            },
        })
        .await
        .is_err()
    {
        return;
    }

    let writer = tokio::spawn(async move {
        // MCCP2 — server-to-client zlib compression. Stays at
        // `None` until the read task observes `IAC DO 86` and
        // pushes the start-of-compression marker (`IAC SB 86 IAC SE`)
        // through this channel. The marker frame itself is sent
        // *uncompressed*; the next byte after the marker begins the
        // zlib stream. We detect the marker by exact-match on the
        // outgoing Vec — the protocol guarantees it arrives as a
        // standalone 5-byte frame (built via `mccp2_start()`),
        // never split or concatenated with other content.
        //
        // The compressor is a `flate2::Compress` rather than a
        // `ZlibEncoder<Vec<u8>>` because the streaming pattern
        // requires per-frame `Sync` flushes against a long-lived
        // zlib context. `compress_vec` with `FlushCompress::Sync`
        // emits a flush marker after each chunk so the client can
        // decompress incrementally without buffering whole
        // messages.
        //
        // The channel itself is bounded (`OUTBOUND_QUEUE_CAP`),
        // so there's no per-connection memory exhaustion risk.
        // Senders use `try_send` and drop on Full; this task just
        // drains as fast as the socket accepts.
        let mut compressor: Option<flate2::Compress> = None;
        while let Some(bytes) = out_rx.recv().await {
            // Wrap in compression if active. Otherwise pass
            // through. The `bytes` Vec is cheaply moved either
            // way — we don't clone unless compression is on.
            let payload: Vec<u8> = if let Some(z) = compressor.as_mut() {
                let mut out = Vec::with_capacity(bytes.len() + 16);
                if z
                    .compress_vec(&bytes, &mut out, flate2::FlushCompress::Sync)
                    .is_err()
                {
                    // Compression error is fatal — the stream is
                    // now out of sync with the client's decoder.
                    // Drop the connection rather than corrupt the
                    // wire.
                    break;
                }
                out
            } else {
                bytes.clone()
            };
            if write_half.write_all(&payload).await.is_err() {
                break;
            }
            // Detect the MCCP2 start marker AFTER writing — the
            // marker itself must reach the client uncompressed,
            // and only subsequent frames are deflated.
            if compressor.is_none() && bytes_are_mccp2_marker(&bytes) {
                compressor = Some(flate2::Compress::new(
                    flate2::Compression::default(),
                    true, // zlib header
                ));
            }
        }
    });

    let mut parser = TelnetParser::new();
    let mut caps = CapsLocal::default();
    let mut line_buf: Vec<u8> = Vec::with_capacity(256);
    let mut chunk = [0u8; READ_CHUNK];

    loop {
        let n = match read_half.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // A client dropping its socket abruptly (closed
                // laptop, network blip, killed client) surfaces as
                // ConnectionReset / BrokenPipe / ConnectionAborted /
                // UnexpectedEof — normal disconnect noise, not
                // operator-actionable. Log those at debug; keep WARN
                // for genuinely unexpected I/O errors that might
                // signal a real problem.
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
                break;
            }
        };
        let (data, events) = parser.feed(&chunk[..n]);

        // Handle telnet events first so any negotiation reply lands
        // before the line we forward upstream.
        for event in events {
            if !handle_telnet_event(conn_id, &out_tx, &inbound, &mut caps, event).await {
                // Connection-fatal event (rare — we don't currently
                // emit any). Bail out of the read loop.
                break;
            }
        }

        // Append data bytes to line buffer; flush on each newline.
        // CRLF / CR / LF are all treated as line terminators per
        // RFC 854 §3.3.5 — strip a trailing CR before forwarding.
        for b in data {
            if b == b'\n' {
                strip_trailing_cr(&mut line_buf);
                let line = String::from_utf8_lossy(&line_buf).to_string();
                line_buf.clear();
                // A blank line still gets forwarded — players use
                // `<enter>` to dismiss prompts; the dispatcher
                // treats it as a no-op and refreshes the prompt.
                if inbound
                    .send(Inbound {
                        conn: conn_id,
                        kind: InboundKind::Line(line),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            } else if line_buf.len() < MAX_LINE_LEN {
                line_buf.push(b);
            } else {
                warn!(
                    conn_id,
                    cap = MAX_LINE_LEN,
                    "line exceeded max length; dropping connection"
                );
                let _ = inbound
                    .send(Inbound {
                        conn: conn_id,
                        kind: InboundKind::Disconnected,
                    })
                    .await;
                writer.abort();
                return;
            }
        }
    }

    let _ = inbound
        .send(Inbound {
            conn: conn_id,
            kind: InboundKind::Disconnected,
        })
        .await;

    writer.abort();
}

/// True if `bytes` is exactly the MCCP2 start-of-compression
/// marker — `IAC SB 86 IAC SE`, 5 bytes — produced by
/// [`mccp2_start`]. Used by the writer task to detect when to
/// flip into compressed mode AFTER passing the marker through
/// uncompressed. Exact-match is safe because we only build this
/// frame in one place; nothing else queues this byte sequence.
fn bytes_are_mccp2_marker(bytes: &[u8]) -> bool {
    bytes == [
        telnet::IAC,
        telnet::SB,
        telnet::opt::MCCP2,
        telnet::IAC,
        telnet::SE,
    ]
}

/// Trim a single trailing `\r` from the line buffer, in place.
/// Callers invoke this exactly when they're about to emit a line
/// that ended with `\n`; CR-LF clients send `\r\n` and we strip
/// the CR here so the dispatcher sees clean text. LF-only clients
/// just no-op through this path.
fn strip_trailing_cr(buf: &mut Vec<u8>) {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
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
    inbound: &InboundTx,
    caps: &mut CapsLocal,
    event: TelnetEvent,
) -> bool {
    match event {
        TelnetEvent::Negotiate { command, option } => {
            handle_negotiate(conn_id, out_tx, inbound, caps, command, option).await;
        }
        TelnetEvent::Subneg { option, payload } => {
            handle_subneg(conn_id, out_tx, inbound, caps, option, &payload).await;
        }
        TelnetEvent::GoAhead | TelnetEvent::EndOfRecord => {
            // Inbound GA / EOR is unusual — modern MUD clients
            // don't send these to the server. Log and ignore.
            debug!(conn_id, ?event, "inbound IAC marker (ignored)");
        }
    }
    true
}

async fn handle_negotiate(
    conn_id: ConnId,
    out_tx: &Outbound,
    inbound: &InboundTx,
    caps: &mut CapsLocal,
    command: u8,
    option: u8,
) {
    use telnet::{DO, DONT, WILL, WONT};
    match (command, option) {
        // Client confirms our WILLs (it agrees we may speak this).
        (DO, opt::GMCP) => {
            caps.gmcp = true;
            forward_capability(inbound, conn_id, "gmcp", true).await;
        }
        (DO, opt::EOR) => {
            caps.eor = true;
            forward_capability(inbound, conn_id, "eor", true).await;
        }
        (DO, opt::MXP) => {
            caps.mxp = true;
            forward_capability(inbound, conn_id, "mxp", true).await;
        }
        (DO, opt::CHARSET) => {
            // Client agreed we may negotiate charset; send the
            // request now. ACCEPTED comes back as a SB CHARSET
            // ACCEPTED frame (handled in handle_subneg).
            let _ = out_tx.try_send(charset_request_utf8());
        }
        (DO, opt::MSSP) => {
            // MSSP payload was already sent unconditionally on
            // connect; nothing more to do.
        }
        (DO, opt::SGA) => {
            // Standard line-mode negotiation; nothing to track.
        }
        (DO, opt::MCCP2) => {
            // Client confirmed MCCP2. Push the start-of-compression
            // marker through the outbound channel — the writer
            // task detects it (last frame to be sent uncompressed)
            // and flips into zlib-deflate mode for everything that
            // follows. Subsequent frames go on the wire compressed
            // automatically; nothing else needs to know.
            let _ = out_tx.try_send(mccp2_start());
            debug!(conn_id, "MCCP2 enabled (marker queued)");
        }
        // Client refuses our WILLs.
        (DONT, _) => {
            // Quietly accept. The tracking flag stays false.
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
        (WILL, opt::NEW_ENVIRON) => {
            // We agree, but we don't currently query env vars.
            // Could send `IAC SB NEW-ENVIRON SEND VAR LANG VAR
            // CHARSET IAC SE` here for future use.
        }
        (WILL, opt::SGA) => {
            // Client also agrees to SGA — line mode confirmed.
        }
        // Client refuses our DOs.
        (WONT, _) => {
            // Client doesn't speak the option. No-op; tracking
            // flag stays false. We don't bother replying (the
            // protocol says we could send DONT but most clients
            // won't care and Mudlet's negotiation history is
            // already settled at this point).
        }
        _ => {
            debug!(conn_id, command, option, "unhandled IAC negotiate");
        }
    }
}

async fn handle_subneg(
    conn_id: ConnId,
    out_tx: &Outbound,
    inbound: &InboundTx,
    caps: &mut CapsLocal,
    option: u8,
    payload: &[u8],
) {
    match option {
        opt::NAWS => {
            if let Some((cols, rows)) = parse_naws(payload) {
                let _ = inbound
                    .send(Inbound {
                        conn: conn_id,
                        kind: InboundKind::WindowSize { cols, rows },
                    })
                    .await;
            }
        }
        opt::TTYPE => {
            // Payload format: `IS <name>` — first byte 0x00, rest
            // is the value. We forward the value upstream and
            // poll for the next response in the MTTS cycle.
            if payload.first() == Some(&telnet::ttype::IS) {
                let value = String::from_utf8_lossy(&payload[1..]).into_owned();
                let _ = inbound
                    .send(Inbound {
                        conn: conn_id,
                        kind: InboundKind::Terminal {
                            index: caps.ttype_polls,
                            value,
                        },
                    })
                    .await;
                // Cycle up to 3 polls (name → term → MTTS bitmap).
                if caps.ttype_polls < 3 {
                    caps.ttype_polls += 1;
                    let _ = out_tx.try_send(ttype_send());
                }
            }
        }
        opt::CHARSET => {
            // First byte: ACCEPTED (2) / REJECTED (3).
            match payload.first() {
                Some(&telnet::charset::ACCEPTED) => {
                    caps.charset_utf8 = true;
                    forward_capability(inbound, conn_id, "utf8", true).await;
                }
                Some(&telnet::charset::REJECTED) => {
                    forward_capability(inbound, conn_id, "utf8", false).await;
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
            let _ = inbound
                .send(Inbound {
                    conn: conn_id,
                    kind: InboundKind::Gmcp {
                        package,
                        payload: body,
                    },
                })
                .await;
        }
        _ => {
            debug!(conn_id, option, len = payload.len(), "unhandled subneg");
        }
    }
}

async fn forward_capability(inbound: &InboundTx, conn_id: ConnId, name: &'static str, on: bool) {
    let _ = inbound
        .send(Inbound {
            conn: conn_id,
            kind: InboundKind::Capability { name, on },
        })
        .await;
}

#[cfg(test)]
mod iac_tests {
    use super::*;

    #[test]
    fn strip_trailing_cr_handles_crlf_and_lf() {
        let mut b = b"hello\r".to_vec();
        strip_trailing_cr(&mut b);
        assert_eq!(b, b"hello");

        let mut b2 = b"hello".to_vec();
        strip_trailing_cr(&mut b2);
        assert_eq!(b2, b"hello"); // no CR — no change
    }

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
        let fake = [
            telnet::IAC,
            telnet::SB,
            opt::GMCP,
            telnet::IAC,
            telnet::SE,
        ];
        assert!(!bytes_are_mccp2_marker(&fake));
    }
}
