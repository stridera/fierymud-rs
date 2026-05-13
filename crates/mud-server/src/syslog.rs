//! In-memory ring buffer of recent tracing events. The `syslog` admin
//! command reads from it; a layered tracing subscriber writes to it.
//! Capped at 512 entries so a runaway log loop can't blow memory —
//! older lines fall off the front.
//!
//! The buffer is a global `LazyLock<Mutex<...>>` because tracing layers
//! emit from arbitrary threads and we don't have access to the bevy
//! `World` at emit time. The `syslog` command snapshots into a `Vec`
//! and walks it without holding the lock.
use std::collections::VecDeque;
use std::fmt::Write;
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

const CAP: usize = 512;
/// Real-time broadcast queue. Capped so a runaway log loop can't
/// exhaust memory before the tick loop drains; overflow simply
/// drops the oldest pending notification rather than blocking the
/// tracing thread (which can be _any_ thread) on a Mutex contention
/// or back-pressure. Drain runs once per tick in `main.rs`.
const BROADCAST_CAP: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct SyslogEntry {
    pub(crate) at: SystemTime,
    pub(crate) level: Level,
    pub(crate) target: String,
    pub(crate) message: String,
}

static SYSLOG: LazyLock<Mutex<VecDeque<SyslogEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(CAP)));

/// Cross-thread queue feeding the in-game `syslog watch` push path.
/// `SyslogLayer::on_event` appends WARN+ entries here from arbitrary
/// threads; the tick loop drains via `drain_broadcast` and fans out
/// to each subscribed player. Separate from `SYSLOG` so the pull-mode
/// `syslog` command's history doesn't get truncated by drain reads.
static BROADCAST: LazyLock<Mutex<VecDeque<SyslogEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(BROADCAST_CAP)));

fn push(entry: SyslogEntry) {
    if let Ok(mut buf) = SYSLOG.lock() {
        if buf.len() >= CAP {
            buf.pop_front();
        }
        buf.push_back(entry);
    }
}

fn push_broadcast(entry: SyslogEntry) {
    if let Ok(mut buf) = BROADCAST.lock() {
        if buf.len() >= BROADCAST_CAP {
            buf.pop_front();
        }
        buf.push_back(entry);
    }
}

pub(crate) fn snapshot() -> Vec<SyslogEntry> {
    SYSLOG
        .lock()
        .map(|g| g.iter().cloned().collect())
        .unwrap_or_default()
}

/// Take everything pending on the real-time broadcast queue, leaving
/// it empty. Called once per tick from `main.rs` so subscribers see
/// every WARN+ event with at most one tick of latency.
pub(crate) fn drain_broadcast() -> Vec<SyslogEntry> {
    BROADCAST
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default()
}

pub(crate) struct SyslogLayer;

impl<S: Subscriber> Layer<S> for SyslogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let entry = SyslogEntry {
            at: SystemTime::now(),
            level: *meta.level(),
            target: meta.target().to_string(),
            message: visitor.into_message(),
        };
        // Pull-mode history — every level is captured for the
        // `syslog` command's after-the-fact tail.
        push(entry.clone());
        // Real-time push — only WARN and above. INFO is too noisy
        // (every heartbeat / autosave / connect line) to land on
        // staff prompts, and DEBUG / TRACE only fire when explicitly
        // enabled via RUST_LOG.
        if matches!(*meta.level(), Level::WARN | Level::ERROR) {
            push_broadcast(entry);
        }
    }
}

/// Renders a tracing event's fields into a single-line string. The
/// special `message` field is the lead; remaining fields are appended
/// `key=value`-style.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl MessageVisitor {
    fn into_message(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields
        } else {
            format!("{} {}", self.message, self.fields)
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value}", field.name());
        }
    }
}
