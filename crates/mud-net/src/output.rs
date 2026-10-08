//! Per-connection output capabilities and the wire encoder.
//!
//! Every text frame headed for a client passes through
//! [`OutputHandle::encode`] in the connection's writer task, the one
//! place all output funnels through. The encoder:
//!
//! * downgrades 256-colour / truecolour SGR sequences to what the
//!   client can show (or drops colour entirely),
//! * transliterates non-ASCII text to ASCII for clients that are not
//!   known to speak UTF-8 (see [`TRANSLIT`]),
//! * turns bare `\n` into `\r\n` (telnet's NVT newline).
//!
//! Frames that carry telnet framing (any `0xFF` byte) or are not valid
//! UTF-8 are binary and pass through untouched.
//!
//! The capability state is a small set of atomics shared between the
//! connection's reader (which learns capabilities from TTYPE / MTTS /
//! CHARSET / NEW-ENVIRON), the writer (which applies them) and the game
//! server (which layers the player's own `color` / `charset` settings
//! on top as overrides). Until the client says otherwise a connection
//! is treated as 16-colour ASCII.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// How much colour a client can display. Ordered: a greater value is a
/// superset of a lesser one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No colour at all: SGR sequences are dropped.
    None,
    /// The 16 standard / bright ANSI colours.
    Ansi16,
    /// The xterm 256-colour palette.
    Ansi256,
    /// 24-bit colour.
    TrueColor,
}

impl ColorDepth {
    fn to_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Ansi16),
            2 => Some(Self::Ansi256),
            3 => Some(Self::TrueColor),
            _ => Option::None,
        }
    }
}

/// Character set the client can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// 7-bit ASCII only; everything else is transliterated.
    Ascii,
    /// UTF-8 passes through.
    Utf8,
}

/// MTTS (Mud Terminal Type Standard) capability bits, as the decimal
/// bitmap value of each flag.
pub mod mtts {
    /// Client supports ANSI colour codes.
    pub const ANSI: u32 = 1;
    /// Client supports VT100 control sequences.
    pub const VT100: u32 = 2;
    /// Client displays UTF-8.
    pub const UTF8: u32 = 4;
    /// Client supports xterm 256 colours.
    pub const COLOR_256: u32 = 8;
    /// Client supports 24-bit colour.
    pub const TRUECOLOR: u32 = 256;
}

/// Colour depth and UTF-8 support a client declares in an MTTS bitmap.
/// A bitmap with none of the colour bits set means no colour.
#[must_use]
pub fn caps_from_mtts(bits: u32) -> (ColorDepth, bool) {
    let depth = if bits & mtts::TRUECOLOR != 0 {
        ColorDepth::TrueColor
    } else if bits & mtts::COLOR_256 != 0 {
        ColorDepth::Ansi256
    } else if bits & (mtts::ANSI | mtts::VT100) != 0 {
        ColorDepth::Ansi16
    } else {
        ColorDepth::None
    };
    (depth, bits & mtts::UTF8 != 0)
}

/// Terminal-name prefixes of the ANSI family (16 colours at least).
const ANSI_FAMILY: &[&str] = &[
    "xterm",
    "ansi",
    "vt100",
    "vt102",
    "vt220",
    "linux",
    "screen",
    "tmux",
    "rxvt",
    "cygwin",
    "putty",
    "konsole",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
];

/// Colour depth a terminal-type string implies (`xterm-256color`,
/// `ansi`, `dumb`, ...). `None` for names that say nothing about colour
/// (client product names such as `Mudlet`).
#[must_use]
pub fn depth_from_term(name: &str) -> Option<ColorDepth> {
    let n = name.trim().to_ascii_lowercase();
    if n.is_empty() {
        return None;
    }
    if n.contains("truecolor") || n.contains("24bit") || n.contains("direct") {
        return Some(ColorDepth::TrueColor);
    }
    if n.contains("256") {
        return Some(ColorDepth::Ansi256);
    }
    if n == "dumb" || n == "vt52" {
        return Some(ColorDepth::None);
    }
    ANSI_FAMILY
        .iter()
        .any(|p| n.starts_with(p))
        .then_some(ColorDepth::Ansi16)
}

/// True when a locale / charset string (`en_US.UTF-8`, `utf8`) names UTF-8.
#[must_use]
pub fn is_utf8_name(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("utf-8") || l.contains("utf8")
}

const AUTO: u8 = u8::MAX;

#[derive(Debug)]
struct OutputState {
    color: AtomicU8,
    utf8: AtomicBool,
    color_override: AtomicU8,
    /// 0 = auto, 1 = ASCII, 2 = UTF-8.
    charset_override: AtomicU8,
    /// An MTTS bitmap arrived; it outranks TERM-name guesses.
    mtts_seen: AtomicBool,
    /// The client accepted `IAC DO EOR`; prompt-end markers may be sent.
    eor: AtomicBool,
}

/// Cheaply cloneable handle on one connection's output capabilities.
#[derive(Debug, Clone)]
pub struct OutputHandle(Arc<OutputState>);

impl Default for OutputHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputHandle {
    /// Safe defaults: 16-colour ASCII, no overrides.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(OutputState {
            color: AtomicU8::new(ColorDepth::Ansi16.to_u8()),
            utf8: AtomicBool::new(false),
            color_override: AtomicU8::new(AUTO),
            charset_override: AtomicU8::new(0),
            mtts_seen: AtomicBool::new(false),
            eor: AtomicBool::new(false),
        }))
    }

    /// Colour depth the client negotiated (ignoring player overrides).
    #[must_use]
    pub fn negotiated_color(&self) -> ColorDepth {
        ColorDepth::from_u8(self.0.color.load(Ordering::Relaxed)).unwrap_or(ColorDepth::Ansi16)
    }

    /// Charset the client negotiated (ignoring player overrides).
    #[must_use]
    pub fn negotiated_charset(&self) -> Charset {
        if self.0.utf8.load(Ordering::Relaxed) {
            Charset::Utf8
        } else {
            Charset::Ascii
        }
    }

    /// Effective colour depth: the player's override, else negotiated.
    #[must_use]
    pub fn color(&self) -> ColorDepth {
        ColorDepth::from_u8(self.0.color_override.load(Ordering::Relaxed))
            .unwrap_or_else(|| self.negotiated_color())
    }

    /// Effective charset: the player's override, else negotiated.
    #[must_use]
    pub fn charset(&self) -> Charset {
        match self.0.charset_override.load(Ordering::Relaxed) {
            1 => Charset::Ascii,
            2 => Charset::Utf8,
            _ => self.negotiated_charset(),
        }
    }

    /// Player colour override; `None` follows the client.
    pub fn set_color_override(&self, depth: Option<ColorDepth>) {
        self.0
            .color_override
            .store(depth.map_or(AUTO, ColorDepth::to_u8), Ordering::Relaxed);
    }

    /// Player charset override; `None` follows the client.
    pub fn set_charset_override(&self, charset: Option<Charset>) {
        let v = match charset {
            None => 0,
            Some(Charset::Ascii) => 1,
            Some(Charset::Utf8) => 2,
        };
        self.0.charset_override.store(v, Ordering::Relaxed);
    }

    /// Apply an MTTS bitmap. Authoritative for colour depth.
    pub fn apply_mtts(&self, bits: u32) {
        let (depth, utf8) = caps_from_mtts(bits);
        self.0.mtts_seen.store(true, Ordering::Relaxed);
        self.0.color.store(depth.to_u8(), Ordering::Relaxed);
        if utf8 {
            self.mark_utf8();
        }
    }

    /// Apply a terminal-type string as a colour hint. Ignored once an
    /// MTTS bitmap has been seen.
    pub fn apply_term_hint(&self, name: &str) {
        if self.0.mtts_seen.load(Ordering::Relaxed) {
            return;
        }
        if let Some(depth) = depth_from_term(name) {
            self.0.color.store(depth.to_u8(), Ordering::Relaxed);
        }
    }

    /// The client is known to render UTF-8 (MTTS bit, accepted CHARSET,
    /// or a UTF-8 locale).
    pub fn mark_utf8(&self) {
        self.0.utf8.store(true, Ordering::Relaxed);
    }

    /// Record whether the client accepted the END-OF-RECORD option.
    pub fn set_eor(&self, on: bool) {
        self.0.eor.store(on, Ordering::Relaxed);
    }

    /// The client accepted END-OF-RECORD (`IAC DO EOR`).
    #[must_use]
    pub fn eor(&self) -> bool {
        self.0.eor.load(Ordering::Relaxed)
    }

    /// Encode one outbound frame for this client; see the module docs.
    ///
    /// A frame that is exactly `IAC EOR` (the prompt-end marker the game
    /// pushes after every prompt, login prompts included) is dropped
    /// unless the client negotiated EOR, so raw TCP clients never see
    /// the two stray bytes.
    #[must_use]
    pub fn encode(&self, frame: Vec<u8>) -> Vec<u8> {
        if frame == [0xFF, 0xEF] && !self.eor() {
            return Vec::new();
        }
        encode_frame(frame, self.color(), self.charset())
    }
}

/// Encode one frame for a client with the given capabilities.
#[must_use]
pub fn encode_frame(frame: Vec<u8>, depth: ColorDepth, charset: Charset) -> Vec<u8> {
    // Telnet framing (IAC = 0xFF never occurs in UTF-8) and anything
    // that isn't text goes out exactly as built.
    if frame.contains(&0xFF) {
        return frame;
    }
    let Ok(text) = std::str::from_utf8(&frame) else {
        return frame;
    };
    let needs_work = text.contains('\n')
        || (depth != ColorDepth::TrueColor && text.contains('\x1b'))
        || (charset == Charset::Ascii && !text.is_ascii());
    if !needs_work {
        return frame;
    }
    match encode_text(text, depth, charset) {
        Cow::Borrowed(_) => frame,
        Cow::Owned(s) => s.into_bytes(),
    }
}

/// [`encode_frame`] for a `&str`.
#[must_use]
pub fn encode_text(text: &str, depth: ColorDepth, charset: Charset) -> Cow<'_, str> {
    let mut out = String::with_capacity(text.len() + 8);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut prev = '\0';
    while i < chars.len() {
        let c = chars[i];
        if c == '\x1b' && chars.get(i + 1) == Some(&'[') {
            // CSI: params 0x30-0x3F, intermediates 0x20-0x2F, final 0x40-0x7E.
            let mut j = i + 2;
            while j < chars.len() && matches!(chars[j], '\u{20}'..='\u{3f}') {
                j += 1;
            }
            if j < chars.len() && matches!(chars[j], '\u{40}'..='\u{7e}') {
                if chars[j] == 'm' {
                    let params: String = chars[i + 2..j].iter().collect();
                    out.push_str(&rewrite_sgr(&params, depth));
                } else {
                    out.extend(&chars[i..=j]);
                }
                prev = chars[j];
                i = j + 1;
                continue;
            }
        }
        match c {
            '\n' if prev != '\r' => out.push_str("\r\n"),
            c if c.is_ascii() => out.push(c),
            c if charset == Charset::Utf8 => out.push(c),
            c => out.push_str(ascii_for(c)),
        }
        prev = c;
        i += 1;
    }
    if out == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(out)
    }
}

// ---------------------------------------------------------------------
// SGR downgrade
// ---------------------------------------------------------------------

/// xterm's standard palette for indices 0-15.
const BASE16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// RGB of an xterm-256 palette index.
#[must_use]
pub fn rgb_of_256(idx: u8) -> (u8, u8, u8) {
    match idx {
        0..=15 => BASE16[usize::from(idx)],
        16..=231 => {
            let n = usize::from(idx - 16);
            (
                CUBE_LEVELS[n / 36],
                CUBE_LEVELS[(n / 6) % 6],
                CUBE_LEVELS[n % 6],
            )
        }
        _ => {
            let g = 8 + (idx - 232) * 10;
            (g, g, g)
        }
    }
}

/// Nearest of the 16 ANSI colours (index 0-15) to an RGB value.
#[must_use]
pub fn nearest_16(rgb: (u8, u8, u8)) -> u8 {
    (0u8..16)
        .min_by_key(|&i| dist2(rgb, BASE16[usize::from(i)]))
        .unwrap_or(7)
}

/// Nearest xterm-256 index (cube or greyscale ramp) to an RGB value.
#[must_use]
pub fn nearest_256(rgb: (u8, u8, u8)) -> u8 {
    let level = |v: u8| -> usize {
        (0..6)
            .min_by_key(|&k| u32::from(CUBE_LEVELS[k].abs_diff(v)))
            .unwrap_or(0)
    };
    let (r, g, b) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube_idx = u8::try_from(16 + 36 * r + 6 * g + b).unwrap_or(16);
    let cube_rgb = (CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]);
    let avg = (u32::from(rgb.0) + u32::from(rgb.1) + u32::from(rgb.2)) / 3;
    let grey_n = u8::try_from(avg.saturating_sub(8).div_ceil(10).min(23)).unwrap_or(23);
    let grey_idx = 232 + grey_n;
    if dist2(rgb, rgb_of_256(grey_idx)) < dist2(rgb, cube_rgb) {
        grey_idx
    } else {
        cube_idx
    }
}

/// SGR code for a 16-colour index as foreground / background.
fn sgr_16(idx: u8, background: bool) -> String {
    let base = match (idx < 8, background) {
        (true, false) => 30,
        (false, false) => 90,
        (true, true) => 40,
        (false, true) => 100,
    };
    (base + (idx % 8)).to_string()
}

/// Rewrite one SGR parameter string (the part between `ESC[` and `m`)
/// for the given colour depth, returning the complete escape sequence
/// (empty when colour is off).
fn rewrite_sgr(params: &str, depth: ColorDepth) -> String {
    if depth == ColorDepth::None {
        return String::new();
    }
    if depth == ColorDepth::TrueColor {
        return format!("\x1b[{params}m");
    }
    let toks: Vec<&str> = params.split(';').collect();
    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut k = 0;
    while k < toks.len() {
        let t = toks[k];
        let background = t == "48";
        if t == "38" || background {
            let mode = toks.get(k + 1).copied();
            let num = |o: usize| toks.get(k + o).and_then(|s| s.parse::<u8>().ok());
            match mode {
                Some("5") if num(2).is_some() => {
                    let idx = num(2).unwrap_or(0);
                    out.push(downgraded(rgb_of_256(idx), Some(idx), background, depth));
                    k += 3;
                    continue;
                }
                Some("2") if num(2).is_some() && num(3).is_some() && num(4).is_some() => {
                    let rgb = (
                        num(2).unwrap_or(0),
                        num(3).unwrap_or(0),
                        num(4).unwrap_or(0),
                    );
                    out.push(downgraded(rgb, None, background, depth));
                    k += 5;
                    continue;
                }
                _ => {}
            }
        }
        out.push(t.to_string());
        k += 1;
    }
    format!("\x1b[{}m", out.join(";"))
}

/// Replacement parameter(s) for one extended colour at `depth` (16 or 256).
fn downgraded(
    rgb: (u8, u8, u8),
    idx256: Option<u8>,
    background: bool,
    depth: ColorDepth,
) -> String {
    if depth == ColorDepth::Ansi256 {
        let idx = idx256.unwrap_or_else(|| nearest_256(rgb));
        return format!("{};5;{idx}", if background { 48 } else { 38 });
    }
    sgr_16(nearest_16(rgb), background)
}

// ---------------------------------------------------------------------
// Transliteration
// ---------------------------------------------------------------------

/// The one non-ASCII -> ASCII mapping table: each entry is a set of
/// characters and their ASCII replacement. Characters in the box
/// drawing and block element ranges not listed here are handled by
/// [`ascii_for`]'s range rules; anything unknown becomes `?`.
pub const TRANSLIT: &[(&str, &str)] = &[
    // Dashes, quotes, punctuation.
    ("\u{2014}\u{2015}", "--"),
    ("\u{2013}\u{2212}\u{2010}\u{2011}\u{2012}", "-"),
    ("\u{2018}\u{2019}\u{201a}\u{2032}\u{00b4}", "'"),
    ("\u{201c}\u{201d}\u{201e}\u{2033}", "\""),
    ("\u{2026}", "..."),
    ("\u{00ab}", "<<"),
    ("\u{00bb}", ">>"),
    ("\u{2022}\u{25cf}\u{25aa}\u{25a0}", "*"),
    ("\u{25e6}\u{25cb}", "o"),
    ("\u{00b7}\u{2219}\u{22c5}", "."),
    ("\u{00a1}", "!"),
    ("\u{00bf}", "?"),
    ("\u{00a0}\u{2002}\u{2003}\u{2009}\u{202f}", " "),
    (
        "\u{200b}\u{200c}\u{200d}\u{2060}\u{feff}\u{fe0f}\u{fe0e}",
        "",
    ),
    // Arrows and symbols.
    ("\u{2192}\u{27a1}", "->"),
    ("\u{2190}", "<-"),
    ("\u{25b6}\u{25ba}\u{203a}", ">"),
    ("\u{25c0}\u{25c4}\u{2039}", "<"),
    ("\u{2194}", "<->"),
    ("\u{21d2}", "=>"),
    ("\u{21d0}", "<="),
    ("\u{2191}", "^"),
    ("\u{2193}", "v"),
    ("\u{2605}\u{2606}\u{2b50}\u{2736}\u{2726}", "*"),
    ("\u{2713}\u{2714}", "v"),
    ("\u{2717}\u{2718}", "x"),
    ("\u{00d7}", "x"),
    ("\u{00f7}", "/"),
    ("\u{00b1}", "+/-"),
    ("\u{2264}", "<="),
    ("\u{2265}", ">="),
    ("\u{2248}", "~"),
    ("\u{2260}", "!="),
    ("\u{2261}", "="),
    ("\u{221e}", "inf"),
    ("\u{00a7}", "S"),
    ("\u{00b0}", "deg"),
    ("\u{00b2}", "2"),
    ("\u{00b3}", "3"),
    ("\u{00bd}", "1/2"),
    ("\u{00bc}", "1/4"),
    ("\u{00be}", "3/4"),
    ("\u{00a9}", "(c)"),
    ("\u{00ae}", "(R)"),
    ("\u{2122}", "(TM)"),
    ("\u{20ac}", "EUR"),
    ("\u{00a3}", "GBP"),
    ("\u{00a2}", "c"),
    ("\u{00a5}", "JPY"),
    // Latin letters with diacritics.
    (
        "\u{00c0}\u{00c1}\u{00c2}\u{00c3}\u{00c4}\u{00c5}\u{0100}",
        "A",
    ),
    (
        "\u{00e0}\u{00e1}\u{00e2}\u{00e3}\u{00e4}\u{00e5}\u{0101}",
        "a",
    ),
    ("\u{00c6}", "AE"),
    ("\u{00e6}", "ae"),
    ("\u{00c7}", "C"),
    ("\u{00e7}", "c"),
    ("\u{00c8}\u{00c9}\u{00ca}\u{00cb}\u{0112}", "E"),
    ("\u{00e8}\u{00e9}\u{00ea}\u{00eb}\u{0113}", "e"),
    ("\u{00cc}\u{00cd}\u{00ce}\u{00cf}", "I"),
    ("\u{00ec}\u{00ed}\u{00ee}\u{00ef}", "i"),
    ("\u{00d1}", "N"),
    ("\u{00f1}", "n"),
    ("\u{00d2}\u{00d3}\u{00d4}\u{00d5}\u{00d6}\u{00d8}", "O"),
    ("\u{00f2}\u{00f3}\u{00f4}\u{00f5}\u{00f6}\u{00f8}", "o"),
    ("\u{0152}", "OE"),
    ("\u{0153}", "oe"),
    ("\u{00d9}\u{00da}\u{00db}\u{00dc}", "U"),
    ("\u{00f9}\u{00fa}\u{00fb}\u{00fc}", "u"),
    ("\u{00dd}\u{0178}", "Y"),
    ("\u{00fd}\u{00ff}", "y"),
    ("\u{00df}", "ss"),
    // Box drawing (single characters; the rest of U+2500..=U+257F
    // falls back to '+' in `ascii_for`).
    (
        "\u{2500}\u{2501}\u{2504}\u{2505}\u{2508}\u{2509}\u{254c}\u{254d}\u{2574}\u{2576}\u{2578}\u{257a}",
        "-",
    ),
    ("\u{2550}", "="),
    (
        "\u{2502}\u{2503}\u{2506}\u{2507}\u{250a}\u{250b}\u{254e}\u{254f}\u{2551}\u{2575}\u{2577}\u{2579}\u{257b}",
        "|",
    ),
    ("\u{2571}", "/"),
    ("\u{2572}", "\\"),
    ("\u{2573}", "X"),
    // Block elements.
    (
        "\u{2588}\u{2593}\u{2589}\u{258a}\u{258b}\u{258c}\u{258d}\u{258e}\u{258f}\u{2590}",
        "#",
    ),
    ("\u{2592}", ":"),
    ("\u{2591}", "."),
    ("\u{2580}", "\""),
    ("\u{2584}", "_"),
];

/// ASCII replacement for a non-ASCII character.
#[must_use]
pub fn ascii_for(c: char) -> &'static str {
    if c.is_ascii() {
        // Callers pass non-ASCII only; keep this total anyway.
        return "?";
    }
    for (set, repl) in TRANSLIT {
        if set.contains(c) {
            return repl;
        }
    }
    match c {
        // Remaining box-drawing: corners, tees, crosses, rounded corners.
        '\u{2500}'..='\u{257f}' => "+",
        // Remaining block elements / shades.
        '\u{2580}'..='\u{259f}' => "#",
        _ => "?",
    }
}

/// Transliterate a whole string to ASCII.
#[must_use]
pub fn to_ascii(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            out.push_str(ascii_for(c));
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str, depth: ColorDepth, cs: Charset) -> String {
        String::from_utf8(encode_frame(s.as_bytes().to_vec(), depth, cs)).unwrap()
    }

    #[test]
    fn mtts_bitmap_maps_to_depth_and_utf8() {
        assert_eq!(caps_from_mtts(0), (ColorDepth::None, false));
        assert_eq!(caps_from_mtts(1), (ColorDepth::Ansi16, false));
        assert_eq!(caps_from_mtts(1 | 4), (ColorDepth::Ansi16, true));
        assert_eq!(caps_from_mtts(1 | 4 | 8), (ColorDepth::Ansi256, true));
        assert_eq!(caps_from_mtts(1 | 8 | 256), (ColorDepth::TrueColor, false));
        // Mudlet's usual advertisement: ANSI+VT100+UTF-8+256+screen
        // reader off+truecolor+MNES = 2349 -> truecolor + UTF-8.
        assert_eq!(caps_from_mtts(2349), (ColorDepth::TrueColor, true));
        // UTF-8 alone declares no colour.
        assert_eq!(caps_from_mtts(4), (ColorDepth::None, true));
    }

    #[test]
    fn term_names_map_to_depth() {
        assert_eq!(depth_from_term("xterm-256color"), Some(ColorDepth::Ansi256));
        assert_eq!(depth_from_term("XTERM-256COLOR"), Some(ColorDepth::Ansi256));
        assert_eq!(depth_from_term("xterm-direct"), Some(ColorDepth::TrueColor));
        assert_eq!(depth_from_term("xterm"), Some(ColorDepth::Ansi16));
        assert_eq!(depth_from_term("ANSI"), Some(ColorDepth::Ansi16));
        assert_eq!(depth_from_term("dumb"), Some(ColorDepth::None));
        assert_eq!(depth_from_term("Mudlet"), None);
        assert_eq!(depth_from_term(""), None);
    }

    #[test]
    fn handle_defaults_and_negotiation() {
        let h = OutputHandle::new();
        assert_eq!(h.color(), ColorDepth::Ansi16);
        assert_eq!(h.charset(), Charset::Ascii);
        h.apply_term_hint("xterm-256color");
        assert_eq!(h.color(), ColorDepth::Ansi256);
        // MTTS outranks the TERM guess, and later TERM hints are ignored.
        h.apply_mtts(1 | 4);
        assert_eq!(h.color(), ColorDepth::Ansi16);
        assert_eq!(h.charset(), Charset::Utf8);
        h.apply_term_hint("xterm-256color");
        assert_eq!(h.color(), ColorDepth::Ansi16);
    }

    #[test]
    fn overrides_win_and_clear() {
        let h = OutputHandle::new();
        h.apply_mtts(1 | 4 | 8);
        h.set_color_override(Some(ColorDepth::None));
        h.set_charset_override(Some(Charset::Ascii));
        assert_eq!(h.color(), ColorDepth::None);
        assert_eq!(h.charset(), Charset::Ascii);
        assert_eq!(h.negotiated_color(), ColorDepth::Ansi256);
        h.set_color_override(None);
        h.set_charset_override(None);
        assert_eq!(h.color(), ColorDepth::Ansi256);
        assert_eq!(h.charset(), Charset::Utf8);
    }

    #[test]
    fn downgrade_256_to_16() {
        let s = "\x1b[38;5;196mred\x1b[0m";
        assert_eq!(
            enc(s, ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[91mred\x1b[0m"
        );
        // Orange-ish indices land on the nearest basic colour.
        assert_eq!(
            enc("\x1b[38;5;208m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[33m"
        );
        assert_eq!(
            enc("\x1b[38;5;220m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[93m"
        );
        // Greys.
        assert_eq!(
            enc("\x1b[38;5;238m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[90m"
        );
        // Background and attributes in the same sequence survive.
        assert_eq!(
            enc("\x1b[1;48;5;21;4m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[1;44;4m"
        );
        // Indices already in the 16-colour range map to themselves.
        assert_eq!(
            enc("\x1b[38;5;9m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[91m"
        );
        assert_eq!(
            enc("\x1b[38;5;2m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[32m"
        );
    }

    #[test]
    fn truecolor_downgrades_to_256_and_16() {
        let s = "\x1b[38;2;255;0;0m";
        assert_eq!(enc(s, ColorDepth::Ansi256, Charset::Utf8), "\x1b[38;5;196m");
        assert_eq!(enc(s, ColorDepth::Ansi16, Charset::Utf8), "\x1b[91m");
        assert_eq!(
            enc("\x1b[48;2;0;0;0m", ColorDepth::Ansi16, Charset::Utf8),
            "\x1b[40m"
        );
        assert_eq!(enc(s, ColorDepth::TrueColor, Charset::Utf8), s);
    }

    #[test]
    fn basic_codes_pass_through_at_16_and_256() {
        let s = "\x1b[1;33mhi\x1b[0m";
        assert_eq!(enc(s, ColorDepth::Ansi16, Charset::Utf8), s);
        assert_eq!(enc(s, ColorDepth::Ansi256, Charset::Utf8), s);
        // A 256-colour client keeps 256-colour codes.
        let c = "\x1b[38;5;196m";
        assert_eq!(enc(c, ColorDepth::Ansi256, Charset::Utf8), c);
    }

    #[test]
    fn color_off_strips_every_sgr_but_keeps_other_csi() {
        assert_eq!(
            enc(
                "\x1b[1;31mred\x1b[0m plain",
                ColorDepth::None,
                Charset::Utf8
            ),
            "red plain"
        );
        assert_eq!(
            enc("\x1b[38;5;196mx\x1b[2J", ColorDepth::None, Charset::Utf8),
            "x\x1b[2J"
        );
    }

    #[test]
    fn transliterates_when_ascii() {
        assert_eq!(
            enc("a \u{2014} b", ColorDepth::Ansi16, Charset::Ascii),
            "a -- b"
        );
        assert_eq!(enc("[\u{2605}]", ColorDepth::Ansi16, Charset::Ascii), "[*]");
        assert_eq!(
            enc("n \u{2192} e", ColorDepth::Ansi16, Charset::Ascii),
            "n -> e"
        );
        assert_eq!(
            enc(
                "\u{201c}hi\u{201d} \u{2018}x\u{2019}",
                ColorDepth::Ansi16,
                Charset::Ascii
            ),
            "\"hi\" 'x'"
        );
        assert_eq!(
            enc("wait\u{2026}", ColorDepth::Ansi16, Charset::Ascii),
            "wait..."
        );
        assert_eq!(
            enc("caf\u{e9} na\u{ef}ve", ColorDepth::Ansi16, Charset::Ascii),
            "cafe naive"
        );
        assert_eq!(
            enc(
                "\u{250c}\u{2500}\u{2510}\r\n\u{2502} \u{2502}\r\n\u{2514}\u{2500}\u{2518}",
                ColorDepth::Ansi16,
                Charset::Ascii
            ),
            "+-+\r\n| |\r\n+-+"
        );
        assert_eq!(
            enc(
                "\u{2554}\u{2550}\u{2557}\u{2551}\u{255a}\u{255d}",
                ColorDepth::Ansi16,
                Charset::Ascii
            ),
            "+=+|++"
        );
        assert_eq!(
            enc("\u{2588}\u{2591}", ColorDepth::Ansi16, Charset::Ascii),
            "#."
        );
        // Unknown characters (emoji) never reach the wire as raw UTF-8.
        let out = enc("hi \u{1f600}", ColorDepth::Ansi16, Charset::Ascii);
        assert_eq!(out, "hi ?");
        assert!(out.is_ascii());
    }

    #[test]
    fn utf8_clients_keep_unicode() {
        let s = "a \u{2014} \u{2605} \u{2588}";
        assert_eq!(enc(s, ColorDepth::Ansi16, Charset::Utf8), s);
    }

    #[test]
    fn bare_newlines_become_crlf() {
        assert_eq!(
            enc("a\nb\r\nc\n", ColorDepth::TrueColor, Charset::Utf8),
            "a\r\nb\r\nc\r\n"
        );
    }

    #[test]
    fn binary_frames_are_untouched() {
        let gmcp = vec![0xFF, 0xFA, 201, b'a', b'\n', 0xE2, 0x80, 0x94, 0xFF, 0xF0];
        assert_eq!(
            encode_frame(gmcp.clone(), ColorDepth::None, Charset::Ascii),
            gmcp
        );
        let not_utf8 = vec![0x80, 0x81, b'\n'];
        assert_eq!(
            encode_frame(not_utf8.clone(), ColorDepth::None, Charset::Ascii),
            not_utf8
        );
    }

    #[test]
    fn every_table_entry_is_ascii() {
        for (set, repl) in TRANSLIT {
            assert!(repl.is_ascii(), "{repl:?}");
            assert!(set.chars().all(|c| !c.is_ascii()), "{set:?}");
        }
    }

    #[test]
    fn rgb_helpers_round_trip_cube() {
        for idx in 16u8..=231 {
            assert_eq!(nearest_256(rgb_of_256(idx)), idx, "idx {idx}");
        }
    }

    #[test]
    fn eor_marker_frame_is_dropped_until_the_client_negotiates_eor() {
        let h = OutputHandle::new();
        assert!(h.encode(vec![0xFF, 0xEF]).is_empty());
        h.set_eor(true);
        assert_eq!(h.encode(vec![0xFF, 0xEF]), vec![0xFF, 0xEF]);
        h.set_eor(false);
        assert!(h.encode(vec![0xFF, 0xEF]).is_empty());
        // Other telnet frames are never touched.
        assert_eq!(h.encode(vec![0xFF, 0xF9]), vec![0xFF, 0xF9]);
    }
}
