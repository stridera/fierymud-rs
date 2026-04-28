use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{info, warn};

pub type ConnId = u64;
pub type Outbound = mpsc::UnboundedSender<String>;
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

/// Bind a TCP listener and forward every accepted connection's lines into `inbound`.
/// Returns only on listener error; runs forever otherwise.
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

async fn handle_connection(
    conn_id: ConnId,
    peer: SocketAddr,
    stream: TcpStream,
    inbound: InboundTx,
) {
    let (read_half, mut write_half) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

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
        while let Some(text) = out_rx.recv().await {
            if write_half.write_all(text.as_bytes()).await.is_err() {
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
