use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub type ConnId = u64;
/// Outbound message to a connected client. Bytes (not String) so the
/// channel can carry telnet IAC framing for GMCP / MSSP / option
/// negotiation alongside ordinary UTF-8 text.
pub type Outbound = mpsc::UnboundedSender<Vec<u8>>;
pub type InboundTx = mpsc::UnboundedSender<Inbound>;
pub type InboundRx = mpsc::UnboundedReceiver<Inbound>;

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
pub async fn serve(bind_addr: &str, inbound: InboundTx) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    info!(addr = %listener.local_addr()?, "telnet listener accepting connections");

    let mut next_id: ConnId = 1;
    loop {
        let (stream, peer) = listener.accept().await?;
        let conn_id = next_id;
        next_id += 1;
        tokio::spawn(handle_connection(conn_id, peer, stream, inbound.clone()));
    }
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
        let (stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let inbound = inbound.clone();
        let conn_id = next_id;
        next_id += 1;
        tokio::spawn(async move {
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

async fn handle_connection<S>(
    conn_id: ConnId,
    peer: SocketAddr,
    stream: S,
    inbound: InboundTx,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    if inbound
        .send(Inbound {
            conn: conn_id,
            kind: InboundKind::Connected {
                peer,
                outbound: out_tx,
            },
        })
        .is_err()
    {
        return;
    }

    let writer = tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if write_half.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });

    let mut reader = BufReader::new(read_half);
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => break,
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']).to_string();
                if inbound
                    .send(Inbound {
                        conn: conn_id,
                        kind: InboundKind::Line(line),
                    })
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

    let _ = inbound.send(Inbound {
        conn: conn_id,
        kind: InboundKind::Disconnected,
    });

    writer.abort();
}

