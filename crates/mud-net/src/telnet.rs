//! Telnet IAC protocol — option negotiation and subnegotiation
//! framing. The MUD wire is bytes (not strings) because it carries
//! both UTF-8 text and the binary IAC frames that drive
//! GMCP / MSSP / MCCP2 / NAWS / TTYPE / CHARSET / EOR / MXP. This
//! module owns the parser state machine and the encoders for
//! every option we care about. The per-connection read task
//! (`handle_connection` in `lib.rs`) feeds bytes in via
//! [`Parser::feed`] and reacts to the [`Event`] stream.
//!
//! Reference: RFC 854 (telnet), RFC 855 (option negotiation),
//! the Mudlet wiki "Standards" section, and the IRE GMCP spec.

/// IAC framing bytes — RFC 854, table 2.
pub const IAC: u8 = 0xFF; // 255
pub const SE: u8 = 0xF0; // 240, end of subnegotiation
pub const NOP: u8 = 0xF1; // 241
pub const GA: u8 = 0xF9; // 249, "go ahead" (suppressed by Suppress-Go-Ahead)
pub const SB: u8 = 0xFA; // 250, start of subnegotiation
pub const WILL: u8 = 0xFB; // 251
pub const WONT: u8 = 0xFC; // 252
pub const DO: u8 = 0xFD; // 253
pub const DONT: u8 = 0xFE; // 254
pub const EOR: u8 = 0xEF; // 239, "end of record" — used as prompt boundary

/// Standard telnet options we negotiate. Decimal codes match the
/// IANA registry / Mudlet documentation.
pub mod opt {
    /// Echo (RFC 857). Used so the server can echo for the client
    /// during password entry.
    pub const ECHO: u8 = 1;
    /// Suppress Go-Ahead (RFC 858). Standard for line-mode telnet.
    pub const SGA: u8 = 3;
    /// Terminal type (RFC 1091); reused for MTTS capability bitmap.
    pub const TTYPE: u8 = 24;
    /// End of Record (RFC 885). Used as a prompt-boundary marker.
    pub const EOR: u8 = 25;
    /// NAWS — Negotiate About Window Size (RFC 1073). Client pushes
    /// `cols rows` (each 16-bit big-endian) inside an SB.
    pub const NAWS: u8 = 31;
    /// New-Environ (RFC 1572). Client volunteers env vars
    /// (LANG, TIMEZONE, IPADDRESS, ...).
    pub const NEW_ENVIRON: u8 = 39;
    /// CHARSET (RFC 2066). We advertise WILL, then on DO push
    /// `REQUEST UTF-8` — Mudlet/MUSHclient/BlightMud all accept.
    pub const CHARSET: u8 = 42;
    /// MSSP (MUD Server Status Protocol). Servers WILL, scrapers DO.
    pub const MSSP: u8 = 70;
    /// MCCP2 — MUD Client Compression Protocol v2. Server WILL,
    /// client DO, then `SB MCCP2 IAC SE` switches the wire to
    /// zlib-deflated.
    pub const MCCP2: u8 = 86;
    /// MXP (MUD eXtension Protocol). Inline `<send>`-style tags
    /// inside the text stream. Optional, low priority.
    pub const MXP: u8 = 91;
    /// GMCP (Generic MUD Communication Protocol, IRE / Mudlet).
    /// JSON payloads inside SB frames keyed by package name.
    pub const GMCP: u8 = 201;
}

/// CHARSET subnegotiation byte codes per RFC 2066. We only emit
/// `REQUEST` and parse `ACCEPTED` / `REJECTED`.
pub mod charset {
    pub const REQUEST: u8 = 1;
    pub const ACCEPTED: u8 = 2;
    pub const REJECTED: u8 = 3;
}

/// TTYPE subnegotiation byte codes per RFC 1091. `IS` carries the
/// client's response, `SEND` is the server's poll. Cycling SEND
/// queries multiple times yields client name → terminal name →
/// MTTS bitmap, matching the Mudlet/TinTin++ convention.
pub mod ttype {
    pub const IS: u8 = 0;
    pub const SEND: u8 = 1;
}

/// Events the parser surfaces to the connection task. Plain text
/// bytes (the player's input) come back through `feed`'s return
/// value, not as events — this keeps the hot path zero-allocation
/// for ordinary chat lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Client said `IAC <command> <option>` — DO / DONT / WILL /
    /// WONT. The peer-conn task decides whether the option is
    /// supported and replies with the matching command.
    Negotiate { command: u8, option: u8 },
    /// Client sent `IAC SB <option> <payload> IAC SE`. `payload`
    /// has already been de-escaped (any `IAC IAC` collapsed to a
    /// single 0xFF). The receiver dispatches by option: NAWS for
    /// window size, TTYPE for terminal type, GMCP for JSON
    /// packages, CHARSET for UTF-8 acks, etc.
    Subneg { option: u8, payload: Vec<u8> },
    /// Client sent `IAC GA` (Go Ahead) — extremely rare in
    /// modern MUDs since SGA is universally negotiated, but
    /// surfaced for completeness.
    GoAhead,
    /// Client sent `IAC EOR` — only meaningful after we've
    /// negotiated WILL EOR with them. Rare on the inbound side
    /// but parsed for symmetry.
    EndOfRecord,
}

/// Stateful IAC parser. One instance per connection — the state
/// persists across read calls because subnegotiation frames can
/// span multiple network reads. Designed for incremental feed:
/// give it whatever bytes you have, get back the data bytes plus
/// any complete events.
#[derive(Debug)]
pub struct Parser {
    state: State,
    /// Subneg payload accumulator. Cleared on each `IAC SB ...`
    /// and emitted on the matching `IAC SE`. Capped to 64 KiB so
    /// a misbehaving / malicious peer can't grow the buffer
    /// unboundedly.
    sb_buf: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    AfterIac,
    AfterCommand,
    InSubneg,
    SubnegAfterIac,
}

/// Hard cap on a single subnegotiation payload. Mudlet GMCP
/// payloads are typically a few hundred bytes; even a fat
/// `Char.Items.List` is well under 16 KiB. 64 KiB is generous.
const MAX_SUBNEG_LEN: usize = 64 * 1024;

impl Default for Parser {
    fn default() -> Self {
        Self {
            state: State::Normal,
            sb_buf: Vec::new(),
        }
    }
}

impl Parser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one chunk of input. Returns the data bytes (player
    /// text — caller decodes UTF-8) and any complete telnet events
    /// surfaced during this chunk. Events are guaranteed to be in
    /// arrival order so the receiver can act on them sequentially.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Vec<Event>) {
        let mut data = Vec::with_capacity(input.len());
        let mut events = Vec::new();
        for &b in input {
            self.feed_byte(b, &mut data, &mut events);
        }
        (data, events)
    }

    fn feed_byte(&mut self, b: u8, data: &mut Vec<u8>, events: &mut Vec<Event>) {
        match self.state {
            State::Normal => {
                if b == IAC {
                    self.state = State::AfterIac;
                } else {
                    data.push(b);
                }
            }
            State::AfterIac => match b {
                IAC => {
                    // Escaped 0xFF. Producer code in player text
                    // doesn't generate IAC bytes, so this is rare;
                    // emit a literal 0xFF for completeness.
                    data.push(IAC);
                    self.state = State::Normal;
                }
                WILL | WONT | DO | DONT => {
                    // Two-byte command; remember which one then
                    // grab the option byte next.
                    self.state = State::AfterCommand;
                    // Stash the command byte in sb_buf[0] —
                    // safe because Normal/AfterIac never read
                    // sb_buf and we clear it on entry to InSubneg.
                    self.sb_buf.clear();
                    self.sb_buf.push(b);
                }
                SB => {
                    self.sb_buf.clear();
                    self.state = State::InSubneg;
                }
                GA => {
                    events.push(Event::GoAhead);
                    self.state = State::Normal;
                }
                EOR => {
                    events.push(Event::EndOfRecord);
                    self.state = State::Normal;
                }
                _ => {
                    // NOP and other miscellaneous IAC commands
                    // we don't model; ignore and resume.
                    self.state = State::Normal;
                }
            },
            State::AfterCommand => {
                let command = self.sb_buf[0];
                events.push(Event::Negotiate { command, option: b });
                self.sb_buf.clear();
                self.state = State::Normal;
            }
            State::InSubneg => {
                if b == IAC {
                    self.state = State::SubnegAfterIac;
                } else if self.sb_buf.len() < MAX_SUBNEG_LEN {
                    self.sb_buf.push(b);
                }
                // else: drop bytes past the cap; the SE will end
                // the frame eventually and we'll emit a truncated
                // payload (better than unbounded growth).
            }
            State::SubnegAfterIac => match b {
                SE => {
                    if !self.sb_buf.is_empty() {
                        let option = self.sb_buf[0];
                        let payload = self.sb_buf[1..].to_vec();
                        events.push(Event::Subneg { option, payload });
                    }
                    self.sb_buf.clear();
                    self.state = State::Normal;
                }
                IAC => {
                    // Escaped 0xFF inside payload.
                    if self.sb_buf.len() < MAX_SUBNEG_LEN {
                        self.sb_buf.push(IAC);
                    }
                    self.state = State::InSubneg;
                }
                _ => {
                    // Unexpected byte after IAC in SB — RFC says
                    // this is malformed but we just bail out of
                    // the subnegotiation rather than panic.
                    self.sb_buf.clear();
                    self.state = State::Normal;
                }
            },
        }
    }
}

// -----------------------------------------------------------------
// Encoders — small builders for the frames the server sends. All
// return `Vec<u8>` so the caller can `try_send` them through the
// outbound channel without further wrapping. Each escapes 0xFF in
// the payload with a doubled IAC per RFC 855.
// -----------------------------------------------------------------

/// `IAC <command> <option>` — 3-byte negotiation reply / advert.
#[must_use]
pub fn negotiate(command: u8, option: u8) -> Vec<u8> {
    vec![IAC, command, option]
}

/// `IAC WILL <option>`.
#[must_use]
pub fn will(option: u8) -> Vec<u8> {
    negotiate(WILL, option)
}

/// `IAC WONT <option>`.
#[must_use]
pub fn wont(option: u8) -> Vec<u8> {
    negotiate(WONT, option)
}

/// `IAC DO <option>`.
#[must_use]
pub fn do_(option: u8) -> Vec<u8> {
    negotiate(DO, option)
}

/// `IAC DONT <option>`.
#[must_use]
pub fn dont(option: u8) -> Vec<u8> {
    negotiate(DONT, option)
}

/// `IAC EOR` — sent after a prompt to mark end-of-record so MUD
/// clients can split prompt from preceding output. Only valid
/// after the client has agreed with `IAC DO 25` (EOR option).
#[must_use]
pub fn iac_eor() -> Vec<u8> {
    vec![IAC, EOR]
}

/// `IAC GA` — legacy go-ahead marker. Clients that haven't
/// negotiated SGA still expect it; modern clients ignore it.
#[must_use]
pub fn iac_ga() -> Vec<u8> {
    vec![IAC, GA]
}

/// `IAC SB <option> <payload (with IAC-escaping)> IAC SE`. The
/// generic subnegotiation builder; option-specific builders below
/// wrap this for clarity at call sites.
#[must_use]
pub fn subneg(option: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(IAC);
    out.push(SB);
    out.push(option);
    for &b in payload {
        if b == IAC {
            out.push(IAC);
            out.push(IAC);
        } else {
            out.push(b);
        }
    }
    out.push(IAC);
    out.push(SE);
    out
}

/// `IAC SB CHARSET REQUEST <separator> UTF-8 IAC SE`. The
/// separator byte is the first byte of the request body per
/// RFC 2066; we use a space, which every implementation accepts.
#[must_use]
pub fn charset_request_utf8() -> Vec<u8> {
    let mut payload = Vec::with_capacity(8);
    payload.push(charset::REQUEST);
    payload.push(b' ');
    payload.extend_from_slice(b"UTF-8");
    subneg(opt::CHARSET, &payload)
}

/// `IAC SB TTYPE SEND IAC SE` — poll the client for its terminal
/// type. Mudlet and other MTTS-aware clients cycle responses on
/// repeated SENDs (client name → terminal name → "MTTS <bits>"),
/// so this builder is meant to be called multiple times.
#[must_use]
pub fn ttype_send() -> Vec<u8> {
    subneg(opt::TTYPE, &[ttype::SEND])
}

/// `IAC SB MCCP2 IAC SE` — start-of-compression marker per the
/// MCCP2 spec. The client begins zlib-inflating subsequent
/// bytes the moment it sees this frame; the server begins
/// zlib-deflating the moment it sends it. Whatever's in the
/// outbound queue *before* this frame must not be zlib-encoded.
#[must_use]
pub fn mccp2_start() -> Vec<u8> {
    subneg(opt::MCCP2, &[])
}

/// Decode an MTTS bitmap response per the standard. Clients
/// reply to the third TTYPE SEND with `"MTTS <decimal-bitmap>"`.
/// Bits we care about (per Mudlet's TermType doc):
///   * 0 = ANSI 16-color
///   * 1 = VT100 / xterm capabilities
///   * 2 = UTF-8
///   * 3 = 256-color
///   * 4 = mouse tracking
///   * 5 = OSC color palette
///   * 6 = screen reader
///   * 7 = proxy reported
///   * 8 = truecolor (24-bit)
///   * 9 = MNES (MUD New-Environ Standard)
/// Returns the parsed bitmap, or `None` if the payload doesn't
/// match the `MTTS <num>` shape.
#[must_use]
pub fn parse_mtts(payload: &str) -> Option<u32> {
    let rest = payload.strip_prefix("MTTS ")?;
    rest.parse::<u32>().ok()
}

/// Decode an NAWS subnegotiation payload per RFC 1073. The
/// payload is exactly 4 bytes: `cols_hi cols_lo rows_hi rows_lo`,
/// each pair big-endian. Returns `None` for malformed lengths.
/// Note: per the RFC, `IAC IAC` was already de-escaped by the
/// parser, so the payload here is the raw 4 bytes even when one
/// of them is 0xFF.
#[must_use]
pub fn parse_naws(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() != 4 {
        return None;
    }
    let cols = (u16::from(payload[0]) << 8) | u16::from(payload[1]);
    let rows = (u16::from(payload[2]) << 8) | u16::from(payload[3]);
    Some((cols, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_text() {
        let mut p = Parser::new();
        let (data, events) = p.feed(b"hello world");
        assert_eq!(data, b"hello world");
        assert!(events.is_empty());
    }

    #[test]
    fn parses_iac_do_gmcp() {
        let mut p = Parser::new();
        let input = [b'h', b'i', IAC, DO, opt::GMCP];
        let (data, events) = p.feed(&input);
        assert_eq!(data, b"hi");
        assert_eq!(events, vec![Event::Negotiate { command: DO, option: opt::GMCP }]);
    }

    #[test]
    fn parses_iac_will_naws() {
        let mut p = Parser::new();
        let (_, events) = p.feed(&[IAC, WILL, opt::NAWS]);
        assert_eq!(events, vec![Event::Negotiate { command: WILL, option: opt::NAWS }]);
    }

    #[test]
    fn parses_naws_subneg_payload() {
        let mut p = Parser::new();
        // SB NAWS 0 80 0 24 IAC SE — 80 cols × 24 rows.
        let input = [IAC, SB, opt::NAWS, 0, 80, 0, 24, IAC, SE];
        let (data, events) = p.feed(&input);
        assert!(data.is_empty());
        assert_eq!(
            events,
            vec![Event::Subneg {
                option: opt::NAWS,
                payload: vec![0, 80, 0, 24],
            }]
        );
        assert_eq!(parse_naws(&[0, 80, 0, 24]), Some((80, 24)));
    }

    #[test]
    fn parses_subneg_with_escaped_iac() {
        // SB GMCP "Char.Vitals" with a 0xFF byte in payload.
        let mut p = Parser::new();
        let mut input = vec![IAC, SB, opt::GMCP];
        input.extend_from_slice(b"Char.Vitals ");
        input.extend_from_slice(b"{\"hp\":");
        input.push(IAC); // escaped 0xFF
        input.push(IAC);
        input.extend_from_slice(b"100}");
        input.push(IAC);
        input.push(SE);

        let (data, events) = p.feed(&input);
        assert!(data.is_empty());
        assert_eq!(events.len(), 1);
        if let Event::Subneg { option, payload } = &events[0] {
            assert_eq!(*option, opt::GMCP);
            // Single 0xFF in the de-escaped payload.
            assert!(payload.contains(&IAC));
            assert!(payload.starts_with(b"Char.Vitals "));
        } else {
            panic!("expected Subneg, got {:?}", events[0]);
        }
    }

    #[test]
    fn state_persists_across_chunks() {
        let mut p = Parser::new();
        let (d1, e1) = p.feed(&[b'a', IAC, SB, opt::GMCP, b'p']);
        assert_eq!(d1, b"a");
        assert!(e1.is_empty());
        let (d2, e2) = p.feed(&[b'q', IAC, SE, b'b']);
        assert_eq!(d2, b"b");
        assert_eq!(
            e2,
            vec![Event::Subneg {
                option: opt::GMCP,
                payload: b"pq".to_vec(),
            }]
        );
    }

    #[test]
    fn parses_iac_eor_event() {
        let mut p = Parser::new();
        let (_, events) = p.feed(&[IAC, EOR]);
        assert_eq!(events, vec![Event::EndOfRecord]);
    }

    #[test]
    fn mtts_decodes_known_bitmap() {
        // 285 = ANSI(1) + VT100(2) + UTF-8(4) + 256-color(8) + truecolor(256)
        // = 1 + 4 + 8 + 16 + 256 = 285. (Actual Mudlet 4 default.)
        assert_eq!(parse_mtts("MTTS 285"), Some(285));
        assert_eq!(parse_mtts("MTTS 0"), Some(0));
        assert_eq!(parse_mtts("garbage"), None);
        assert_eq!(parse_mtts("MTTS notanum"), None);
    }

    #[test]
    fn naws_rejects_wrong_length() {
        assert_eq!(parse_naws(&[0, 80, 0]), None);
        assert_eq!(parse_naws(&[]), None);
    }

    #[test]
    fn negotiation_encoders_match_iac_format() {
        assert_eq!(will(opt::GMCP), vec![IAC, WILL, opt::GMCP]);
        assert_eq!(do_(opt::NAWS), vec![IAC, DO, opt::NAWS]);
        assert_eq!(dont(opt::ECHO), vec![IAC, DONT, opt::ECHO]);
        assert_eq!(wont(opt::CHARSET), vec![IAC, WONT, opt::CHARSET]);
    }

    #[test]
    fn subneg_escapes_iac_in_payload() {
        let frame = subneg(opt::GMCP, &[b'a', IAC, b'b']);
        // a is bytes[3] (after IAC SB OPT), then IAC IAC, then b, then IAC SE
        assert_eq!(
            frame,
            vec![IAC, SB, opt::GMCP, b'a', IAC, IAC, b'b', IAC, SE]
        );
    }
}
