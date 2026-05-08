use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{info, warn};

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
    Connected { peer: SocketAddr, outbound: Outbound },
    Line(String),
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
        if !throttle_allow(peer.ip()) {
            warn!(%peer, "throttle: rejecting connection — over rate limit");
            drop(stream);
            continue;
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
    std::sync::Mutex<
        std::collections::HashMap<std::net::IpAddr, std::collections::VecDeque<std::time::Instant>>,
    >,
> = std::sync::OnceLock::new();

const MAX_CONNECTS_PER_MIN: usize = 10;

/// Hard cap on a single inbound command line. A peer streaming bytes
/// without a newline can otherwise grow `read_until`'s buffer
/// without bound. 4 KiB is well above any plausible MUD command
/// (longest legitimate inputs are emote / who-tag / mail-body lines
/// on the order of a few hundred bytes); going over indicates either
/// a buggy client or an attacker. On overflow we drop the connection.
const MAX_LINE_LEN: usize = 4096;


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
fn throttle_allow(ip: std::net::IpAddr) -> bool {
    let map_lock = THROTTLER.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = map_lock.lock().expect("throttler poisoned");
    let now = std::time::Instant::now();
    let window = std::time::Duration::from_secs(60);
    let entry = map.entry(ip).or_default();
    while entry.front().is_some_and(|t| now.duration_since(*t) > window) {
        entry.pop_front();
    }
    if entry.len() >= MAX_CONNECTS_PER_MIN {
        return false;
    }
    entry.push_back(now);
    true
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
        if !throttle_allow(peer.ip()) {
            warn!(%peer, "throttle: rejecting TLS connection — over rate limit");
            drop(stream);
            continue;
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
                Err(e) => warn!(conn_id, peer = %peer, error = %e, "TLS accept failed"),
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

// Telnet protocol bytes used for GMCP framing.
const TELNET_IAC: u8 = 0xFF;
const TELNET_WILL: u8 = 0xFB;
const TELNET_SB: u8 = 0xFA;
const TELNET_SE: u8 = 0xF0;
/// GMCP option number per the protocol (decimal 201, hex 0xC9).
const TELNET_OPT_GMCP: u8 = 0xC9;
/// MSSP option number (RFC-ish, decimal 70). Servers advertise
/// MSSP via `IAC WILL 70`; clients (MUD listing scrapers) request
/// the data via `IAC DO 70`. The reply is a single SB frame
/// holding `<MSSP_VAR> <name> <MSSP_VAL> <value>` pairs.
const TELNET_OPT_MSSP: u8 = 0x46;
/// MSSP variable-name marker per the spec (1 = `MSSP_VAR`).
const MSSP_VAR: u8 = 0x01;
/// MSSP value marker per the spec (2 = `MSSP_VAL`).
const MSSP_VAL: u8 = 0x02;

/// Build the 3-byte `IAC WILL GMCP` sequence the server sends on
/// connect to advertise GMCP support. Mainstream MUD clients
/// (`Mudlet`, `MUSHclient`, `BlightMUD`) reply `IAC DO 201` to confirm.
#[must_use]
pub fn iac_will_gmcp() -> Vec<u8> {
    vec![TELNET_IAC, TELNET_WILL, TELNET_OPT_GMCP]
}

/// Build the 3-byte `IAC WILL MSSP` advertisement. MUD list
/// scrapers (`TMS`, `MudConnect`) reply `IAC DO 70` and expect the
/// server to follow with an MSSP subnegotiation frame.
#[must_use]
pub fn iac_will_mssp() -> Vec<u8> {
    vec![TELNET_IAC, TELNET_WILL, TELNET_OPT_MSSP]
}

/// Build an MSSP subnegotiation frame: `IAC SB 70 (MSSP_VAR name
/// MSSP_VAL value)+ IAC SE`. Vars / values are framed per the spec
/// with their leading 1 / 2 bytes. Standard variable names per the
/// MSSP spec: NAME, PLAYERS, UPTIME, CODEBASE, FAMILY, CONTACT,
/// GENRE, LANGUAGE — the caller picks which to send.
#[must_use]
pub fn mssp_packet(vars: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + vars.iter().map(|(k, v)| k.len() + v.len() + 2).sum::<usize>());
    out.push(TELNET_IAC);
    out.push(TELNET_SB);
    out.push(TELNET_OPT_MSSP);
    for (name, value) in vars {
        out.push(MSSP_VAR);
        out.extend_from_slice(name.as_bytes());
        out.push(MSSP_VAL);
        // Escape any 0xFF in the value (unlikely for ASCII metadata
        // but cheap insurance).
        for b in value.as_bytes() {
            if *b == TELNET_IAC {
                out.push(TELNET_IAC);
                out.push(TELNET_IAC);
            } else {
                out.push(*b);
            }
        }
    }
    out.push(TELNET_IAC);
    out.push(TELNET_SE);
    out
}

/// Build a GMCP subnegotiation frame:
/// `IAC SB 201 <package_name> <space?> <json_payload> IAC SE`.
///
/// `package` is the dotted package name like `Char.Vitals` or
/// `Room.Info`. `payload` is a JSON literal — pass an empty string
/// for packages that don't carry data.
///
/// The frame escapes any 0xFF byte in the payload as `IAC IAC`
/// per the telnet protocol so clients see a single 0xFF in their
/// reassembled payload.
#[must_use]
pub fn gmcp_packet(package: &str, payload: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + package.len() + payload.len());
    out.push(TELNET_IAC);
    out.push(TELNET_SB);
    out.push(TELNET_OPT_GMCP);
    out.extend_from_slice(package.as_bytes());
    if !payload.is_empty() {
        out.push(b' ');
        for b in payload.as_bytes() {
            if *b == TELNET_IAC {
                out.push(TELNET_IAC);
                out.push(TELNET_IAC);
            } else {
                out.push(*b);
            }
        }
    }
    out.push(TELNET_IAC);
    out.push(TELNET_SE);
    out
}

/// State for the inbound IAC stripper. Persists across `read_until`
/// calls because telnet subnegotiation sequences may span multiple
/// reads (rare in practice but legal per RFC 854).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IacState {
    /// Pass-through. Switch to `AfterIac` on 0xFF.
    Normal,
    /// Saw an IAC byte. Next byte tells us what kind of sequence.
    AfterIac,
    /// Saw IAC + DO/DONT/WILL/WONT (0xFB–0xFE). Next byte is the
    /// option number; drop it and return to Normal.
    AfterCommand,
    /// Inside `IAC SB ... IAC SE`. Consume bytes until IAC SE.
    InSubneg,
    /// Inside SB and just saw an IAC. The next byte is either SE
    /// (0xF0, end of subneg) or another IAC (0xFF, escaped data
    /// byte). Either way return to `InSubneg` or Normal accordingly.
    SubnegAfterIac,
}

/// Filter telnet IAC sequences out of a byte buffer. Returns only
/// the data bytes (player text); IAC negotiation and subnegotiation
/// frames are silently dropped. The state machine persists across
/// calls so multi-read subnegotiations resolve correctly.
fn strip_iac(input: &[u8], state: &mut IacState) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for &b in input {
        match *state {
            IacState::Normal => {
                if b == 0xFF {
                    *state = IacState::AfterIac;
                } else {
                    out.push(b);
                }
            }
            IacState::AfterIac => {
                match b {
                    0xFF => {
                        // Escaped data byte (IAC IAC). Telnet says
                        // emit a single 0xFF, but player text won't
                        // contain such bytes legitimately and we
                        // can't UTF-8-encode them, so drop.
                        *state = IacState::Normal;
                    }
                    0xFB..=0xFE => *state = IacState::AfterCommand,
                    0xFA => *state = IacState::InSubneg,
                    _ => *state = IacState::Normal,
                }
            }
            IacState::AfterCommand => *state = IacState::Normal,
            IacState::InSubneg => {
                if b == 0xFF {
                    *state = IacState::SubnegAfterIac;
                }
                // else: subnegotiation payload byte, drop.
            }
            IacState::SubnegAfterIac => {
                if b == 0xF0 {
                    *state = IacState::Normal;
                } else {
                    *state = IacState::InSubneg;
                }
            }
        }
    }
    out
}

async fn handle_connection<S>(
    conn_id: ConnId,
    peer: SocketAddr,
    stream: S,
    inbound: InboundTx,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_CAP);

    // Advertise GMCP support immediately. Clients that speak it
    // (`Mudlet`, `MUSHclient`, `BlightMUD`) reply `IAC DO 201`; we
    // currently don't parse inbound IAC bytes, so the server
    // assumes "client said yes" if it later receives a GMCP
    // subnegotiation. Plain telnet clients ignore the WILL.
    let _ = out_tx.try_send(iac_will_gmcp());
    // Same one-shot pattern for MSSP: advertise WILL 70 then push
    // the variable list inline. MUD list scrapers parse the SB
    // frame whether or not they replied with DO; sending it
    // unconditionally costs ~80 bytes per connect and reaches
    // every scraper in one round-trip.
    let _ = out_tx.try_send(iac_will_mssp());
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
                outbound: out_tx,
            },
        })
        .await
        .is_err()
    {
        return;
    }

    let writer = tokio::spawn(async move {
        // The channel itself is now bounded (`OUTBOUND_QUEUE_CAP`),
        // so there's no per-connection memory exhaustion risk to
        // watch. Senders use `try_send` and drop on Full; this task
        // just drains as fast as the socket accepts.
        while let Some(bytes) = out_rx.recv().await {
            if write_half.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });

    // Raw-bytes reader: read_until(\n) instead of read_line because
    // clients that speak GMCP (or any telnet option) reply with IAC
    // sequences containing 0xFF — invalid UTF-8, which read_line
    // refuses. We parse out IAC framing here, then lossy-convert
    // what remains to UTF-8 for the line dispatcher.
    //
    // Bounded line length: wrap each read_until in a `take(MAX_LINE_LEN)`
    // so a peer streaming bytes without a newline can't grow the
    // server-side buffer without bound. If the take limit is hit
    // before a `\n`, the loop detects it (final byte != '\n', length ==
    // cap) and drops the connection.
    let mut reader = BufReader::new(read_half);
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut iac_state = IacState::Normal;
    loop {
        buf.clear();
        let read_res = (&mut reader)
            .take(MAX_LINE_LEN as u64)
            .read_until(b'\n', &mut buf)
            .await;
        match read_res {
            Ok(0) => break,
            Ok(_) => {
                // Take hit its limit and we never saw a newline —
                // peer is sending an oversized line. Disconnect
                // instead of silently truncating.
                if buf.len() >= MAX_LINE_LEN && !buf.ends_with(b"\n") {
                    warn!(
                        conn_id,
                        cap = MAX_LINE_LEN,
                        "line exceeded max length; dropping connection"
                    );
                    break;
                }
                let stripped = strip_iac(&buf, &mut iac_state);
                let line = String::from_utf8_lossy(&stripped)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                // A line that was nothing but IAC bytes (option
                // negotiation reply, GMCP subneg, etc.) leaves
                // `stripped` empty after filtering — skip the
                // empty-line forward instead of treating it as an
                // empty player command.
                if stripped.is_empty() {
                    continue;
                }
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
            }
            Err(e) => {
                warn!(conn_id, error = %e, "read error");
                break;
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


#[cfg(test)]
mod iac_tests {
    use super::{strip_iac, IacState};

    fn run(input: &[u8]) -> Vec<u8> {
        let mut state = IacState::Normal;
        strip_iac(input, &mut state)
    }

    #[test]
    fn passes_plain_text() {
        assert_eq!(run(b"hello\r\n"), b"hello\r\n");
    }

    #[test]
    fn strips_iac_do_gmcp() {
        // Mudlet's reply to our IAC WILL 201: IAC DO 201
        let input = [b'h', b'i', 0xFF, 0xFD, 0xC9, b'\r', b'\n'];
        assert_eq!(run(&input), b"hi\r\n");
    }

    #[test]
    fn strips_iac_subneg() {
        // IAC SB 201 some payload IAC SE
        let input: Vec<u8> =
            [&[b'a'][..], &[0xFF, 0xFA, 0xC9], b"payload", &[0xFF, 0xF0], b"b"].concat();
        assert_eq!(run(&input), b"ab");
    }

    #[test]
    fn state_persists_across_calls() {
        let mut state = IacState::Normal;
        // First chunk: opens a subneg but doesn't close it.
        assert_eq!(strip_iac(&[b'x', 0xFF, 0xFA, 0xC9, b'p'], &mut state), b"x");
        assert_eq!(state, IacState::InSubneg);
        // Second chunk: more subneg payload then IAC SE then real text.
        assert_eq!(strip_iac(&[b'q', 0xFF, 0xF0, b'y'], &mut state), b"y");
        assert_eq!(state, IacState::Normal);
    }
}
