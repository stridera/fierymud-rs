//! Output layout: width-aware word wrapping and column grids for
//! XML-Lite markup (`<red>text</>`).
//!
//! The server owns line wrapping. Clients that do not wrap (raw
//! `telnet`, some web terminals) otherwise break indented text mid
//! column, and clients that do wrap lose hanging indents. Everything
//! here is markup-aware: colour tags and ANSI escapes have zero width,
//! and a break never lands inside a tag or inside a word.
//!
//! Pure string functions; the only world-facing helper is
//! [`wrap_width`], which resolves the effective width for a player.
//! Callers opt in to wrapping (see `commands::send_prose`), which is
//! how pre-formatted tables and ASCII art are kept out of it.

use bevy_ecs::prelude::{Entity, World};
use mud_world::{ClientWidth, PREF_COLUMNS_KEY, ScriptVars};

use crate::commands::visible_width;

/// Width assumed until the client reports one via NAWS.
pub(crate) const DEFAULT_COLS: usize = 80;
/// Lower / upper bounds for any width we are willing to wrap at.
pub(crate) const MIN_COLS: usize = 40;
pub(crate) const MAX_COLS: usize = 250;
/// First-line indent for room descriptions (legacy convention).
pub(crate) const ROOM_DESC_INDENT: usize = 3;

/// Effective wrap width for `player`: their persisted `columns`
/// preference when set, else the NAWS-reported client width, else
/// [`DEFAULT_COLS`]. Always within `MIN_COLS..=MAX_COLS`.
pub(crate) fn wrap_width(world: &World, player: Entity) -> usize {
    if let Some(n) = pref_columns(world, player) {
        return n.clamp(MIN_COLS, MAX_COLS);
    }
    let reported = world
        .get::<ClientWidth>(player)
        .map_or(0, |w| usize::from(w.0));
    if reported == 0 {
        DEFAULT_COLS
    } else {
        reported.clamp(MIN_COLS, MAX_COLS)
    }
}

/// The player's explicit `columns` preference, if any.
pub(crate) fn pref_columns(world: &World, player: Entity) -> Option<usize> {
    world
        .get::<ScriptVars>(player)?
        .0
        .get(PREF_COLUMNS_KEY)?
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
}

/// Remove ANSI CSI sequences (`ESC [ ... final`) so text that was
/// already rendered still measures correctly.
fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\x1b') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Visible width of `s`: colour tags and ANSI escapes count zero.
pub(crate) fn text_width(s: &str) -> usize {
    visible_width(&strip_ansi(s))
}

/// Split into `(whitespace_before, word)` pairs. Markup tags never
/// contain whitespace, so a whitespace split cannot cut one in half.
fn words_with_gaps(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let word_start = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        let (gap, tail) = rest.split_at(word_start);
        if tail.is_empty() {
            break;
        }
        let word_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (word, after) = tail.split_at(word_end);
        out.push((gap, word));
        rest = after;
    }
    out
}

/// Greedy word-wrap of a single paragraph (no line breaks inside).
/// The first line starts with `first_indent` spaces and later lines
/// with `rest_indent`. Gaps between words are kept as written when the
/// words share a line (so the legacy double space after a full stop
/// survives) and dropped at a break. A word wider than the line is
/// never split; it gets a line to itself and overflows.
pub(crate) fn wrap_paragraph(
    text: &str,
    width: usize,
    first_indent: usize,
    rest_indent: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = " ".repeat(first_indent);
    let mut cur_w = first_indent;
    let mut have_word = false;
    for (gap, word) in words_with_gaps(text) {
        let ww = text_width(word);
        // Whitespace that is not plain spaces (tabs) collapses to one.
        let gap_w = if gap.chars().all(|c| c == ' ') {
            gap.len()
        } else {
            1
        };
        if !have_word {
            cur.push_str(word);
            cur_w += ww;
            have_word = true;
        } else if cur_w + gap_w + ww <= width {
            if gap.chars().all(|c| c == ' ') {
                cur.push_str(gap);
            } else {
                cur.push(' ');
            }
            cur.push_str(word);
            cur_w += gap_w + ww;
        } else {
            lines.push(std::mem::replace(&mut cur, " ".repeat(rest_indent)));
            cur.push_str(word);
            cur_w = rest_indent + ww;
        }
    }
    if have_word {
        lines.push(cur);
    }
    lines
}

/// True for lines that are visibly laid out by hand: runs of three or
/// more spaces inside the text (table columns), box-drawing characters,
/// or rule lines made of `=-_*#+|`. Such lines are left untouched by
/// [`wrap_lines`] so tables and ASCII art survive narrow clients.
fn is_preformatted(line: &str) -> bool {
    let plain = strip_ansi(line);
    let body = plain.trim_start();
    if body.contains("   ") {
        return true;
    }
    if body.chars().any(|c| ('\u{2500}'..='\u{259f}').contains(&c)) {
        return true;
    }
    let mut run = 0usize;
    for c in body.chars() {
        if matches!(c, '=' | '-' | '_' | '*' | '#' | '+' | '|' | '~') {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Wrap prose that already has line structure (help articles,
/// ability cards, board posts). Each line that fits is left exactly
/// as written; a line that is too wide is re-wrapped with a hanging
/// indent equal to its own leading spaces, which is what keeps
/// indented lists readable (issue #2). Blank lines and the original
/// `\r\n` / `\n` terminators are preserved; lines that look
/// pre-formatted (see [`is_preformatted`]) are never touched.
pub(crate) fn wrap_lines(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for segment in text.split_inclusive('\n') {
        let (content, terminator) = if let Some(c) = segment.strip_suffix("\r\n") {
            (c, "\r\n")
        } else if let Some(c) = segment.strip_suffix('\n') {
            (c, "\n")
        } else {
            (segment, "")
        };
        if text_width(content) <= width || is_preformatted(content) {
            out.push_str(content);
        } else {
            let indent = content.chars().take_while(|c| *c == ' ').count();
            let indent = indent.min(width / 2);
            let wrapped = wrap_paragraph(content.trim_start(), width, indent, indent);
            let nl = if terminator.is_empty() {
                "\r\n"
            } else {
                terminator
            };
            out.push_str(&wrapped.join(nl));
        }
        out.push_str(terminator);
    }
    out
}

/// Reflow a room description into legacy layout: paragraphs with a
/// 3-space first-line indent, wrapped to `width`, one blank line
/// between paragraphs. Lines are joined with `\r\n`; there is no
/// trailing terminator.
///
/// Input rules (determined from legacy, where `&_` forced a newline
/// and imported text reaches us with that as a bare `\n`):
/// * a blank line, or a line that starts with whitespace, begins a new
///   paragraph (imported `&_` breaks look like `"...room.\n In the..."`);
/// * any other single line break is hard-wrap residue and becomes a
///   space (no space after a trailing hyphen, so `north-\nwest`
///   rejoins as `north-west`).
pub(crate) fn format_room_description(desc: &str, width: usize) -> String {
    let normalized = desc.replace("\r\n", "\n").replace('\r', "\n");
    let mut paragraphs: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in normalized.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !cur.is_empty() {
                paragraphs.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let starts_indented = line.starts_with(char::is_whitespace);
        if cur.is_empty() {
            cur.push_str(trimmed);
        } else if starts_indented {
            paragraphs.push(std::mem::replace(&mut cur, trimmed.to_string()));
        } else {
            let joins_hyphen = cur
                .strip_suffix('-')
                .is_some_and(|head| head.ends_with(|c: char| c.is_alphabetic()));
            if !joins_hyphen {
                cur.push(' ');
            }
            cur.push_str(trimmed);
        }
    }
    if !cur.is_empty() {
        paragraphs.push(cur);
    }
    let rendered: Vec<String> = paragraphs
        .iter()
        .map(|p| wrap_paragraph(p, width, ROOM_DESC_INDENT, 0).join("\r\n"))
        .collect();
    rendered.join("\r\n\r\n")
}

/// Lay `entries` out in left-aligned columns of `col_width` visible
/// columns, as many per row as fit in `width` after `indent`. Cells
/// are space-padded from their visible width (never tabs); the last
/// cell of a row is not padded. Returns one string per row, without
/// terminators. Always at least one column.
pub(crate) fn format_grid(
    entries: &[String],
    col_width: usize,
    indent: usize,
    width: usize,
) -> Vec<String> {
    let col_width = col_width.max(1);
    let per_row = (width.saturating_sub(indent) / col_width).max(1);
    entries
        .chunks(per_row)
        .map(|row| {
            let mut line = " ".repeat(indent);
            for (i, cell) in row.iter().enumerate() {
                line.push_str(cell);
                if i + 1 < row.len() {
                    let pad = col_width.saturating_sub(text_width(cell));
                    line.push_str(&" ".repeat(pad));
                }
            }
            line
        })
        .collect()
}

/// Column width for a grid shared by several lists: widest visible
/// entry plus `gutter`, at least `min`. Computing it once across every
/// list keeps columns aligned from one block to the next.
pub(crate) fn grid_col_width<'a>(
    lists: impl IntoIterator<Item = &'a [String]>,
    gutter: usize,
    min: usize,
) -> usize {
    lists
        .into_iter()
        .flat_map(|l| l.iter())
        .map(|e| text_width(e))
        .max()
        .map_or(min, |w| (w + gutter).max(min))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths(lines: &[String]) -> Vec<usize> {
        lines.iter().map(|l| text_width(l)).collect()
    }

    #[test]
    fn wrap_paragraph_respects_width() {
        let text = "the quick brown fox jumps over the lazy dog and keeps running far away";
        for width in [20, 33, 40, 80] {
            let lines = wrap_paragraph(text, width, 0, 0);
            assert!(widths(&lines).iter().all(|w| *w <= width), "{lines:?}");
            assert_eq!(lines.join(" "), text);
        }
    }

    #[test]
    fn wrap_paragraph_ignores_colour_tags_when_measuring() {
        // 40 visible chars; with tags the raw string is much longer.
        let text = "<red>aaaa</> <b:cyan>bbbb</> <green>cccc</> <yellow>dddd</> <red>eeee</> ffff";
        let lines = wrap_paragraph(text, 24, 0, 0);
        assert!(widths(&lines).iter().all(|w| *w <= 24), "{lines:?}");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].ends_with("</>"), "{lines:?}");
    }

    #[test]
    fn wrap_paragraph_never_splits_a_tag() {
        let text = "alpha <b:yellow>bravo</> charlie <red>delta</> echo foxtrot golf";
        for width in 5..40 {
            for line in wrap_paragraph(text, width, 0, 0) {
                // Every tag opened on a line is closed on that line.
                assert_eq!(
                    line.matches('<').count(),
                    line.matches('>').count(),
                    "width {width}: {line}"
                );
            }
        }
    }

    #[test]
    fn wrap_paragraph_keeps_long_word_whole() {
        let url = "https://example.invalid/a/very/long/path/that/cannot/fit";
        let text = format!("see {url} now");
        let lines = wrap_paragraph(&text, 20, 0, 0);
        assert_eq!(lines, vec!["see", url, "now"]);
    }

    #[test]
    fn wrap_paragraph_applies_indents_and_keeps_double_space() {
        let lines = wrap_paragraph("one.  two three four five six", 14, 3, 1);
        assert_eq!(lines[0], "   one.  two");
        assert!(lines[1..].iter().all(|l| l.starts_with(' ')));
        assert!(widths(&lines).iter().all(|w| *w <= 14), "{lines:?}");
    }

    #[test]
    fn wrap_lines_leaves_fitting_lines_and_paragraph_breaks_alone() {
        let text = "Short line.\r\n\r\nAnother short line.\r\n";
        assert_eq!(wrap_lines(text, 40), text);
    }

    #[test]
    fn wrap_lines_preserves_hanging_indent() {
        let text = "  Available to: Bard (C1), Conjurer (C1), Illusionist (C1), Ranger (C1)\r\n";
        let out = wrap_lines(text, 40);
        let lines: Vec<&str> = out.trim_end().split("\r\n").collect();
        assert!(lines.len() >= 2, "{out:?}");
        assert!(lines.iter().all(|l| l.starts_with("  ")), "{out:?}");
        assert!(lines.iter().all(|l| text_width(l) <= 40), "{out:?}");
        assert!(out.ends_with("\r\n"));
    }

    #[test]
    fn wrap_lines_skips_preformatted_lines() {
        let table = "Usage         : cast 'magic missile' [<victim>] and some more text here\r\n";
        assert_eq!(wrap_lines(table, 30), table);
        let rule = "==========================================================\r\n";
        assert_eq!(wrap_lines(rule, 30), rule);
        let boxed = "┌────────────────────────────────────────────────────┐\r\n";
        assert_eq!(wrap_lines(boxed, 30), boxed);
    }

    #[test]
    fn wrap_lines_measures_prerendered_ansi() {
        let line = "\x1b[36mshort\x1b[0m words\r\n";
        assert_eq!(wrap_lines(line, 12), line);
    }

    #[test]
    fn room_description_indents_and_wraps() {
        let out = format_room_description(
            "A narrow trail winds north through the pines and over a low ridge.",
            30,
        );
        let lines: Vec<&str> = out.split("\r\n").collect();
        assert!(lines[0].starts_with("   A narrow"), "{out:?}");
        assert!(lines[1..].iter().all(|l| !l.starts_with(' ')), "{out:?}");
        assert!(lines.iter().all(|l| text_width(l) <= 30), "{out:?}");
    }

    #[test]
    fn room_description_imported_amp_underscore_paragraphs() {
        // Shape of room 12/29 as imported: `&_` became "\n" plus a space.
        let imported = "Part of the bark of this giant oak.\n In the little kitchen area is a fireplace.\n The sewing room is neat.";
        let out = format_room_description(imported, 80);
        assert_eq!(
            out,
            "   Part of the bark of this giant oak.\r\n\r\n   In the little kitchen area is a fireplace.\r\n\r\n   The sewing room is neat."
        );
    }

    #[test]
    fn room_description_reflows_hard_wrapped_fixture() {
        // Raw legacy file text: 3-space paragraph starts, hard wraps
        // at ~40 columns, blank line between two paragraphs.
        let fixture = "   Part of the bark of this giant oak\nhas been fashioned into a door,\nleading into a large hollow.\n\n   In the little kitchen area is a large\nstone fireplace, a great cooking fire\nblazing north-\nwest.";
        let out = format_room_description(fixture, 60);
        let paragraphs: Vec<&str> = out.split("\r\n\r\n").collect();
        assert_eq!(paragraphs.len(), 2, "{out:?}");
        assert!(
            paragraphs[0].starts_with("   Part of the bark of this giant oak has been"),
            "{out:?}"
        );
        assert!(paragraphs[1].contains("north-west"), "{out:?}");
        for l in out.split("\r\n") {
            assert!(text_width(l) <= 60, "{l:?}");
        }
        // No hard wraps survive: each paragraph is reflowed to the new width.
        assert!(!paragraphs[0].contains("oak\r\n"), "{out:?}");
    }

    #[test]
    fn room_description_preserves_blank_line_paragraphs_and_strips_old_indent() {
        let out = format_room_description("   One.\r\n\r\n\r\n   Two.\r\n", 80);
        assert_eq!(out, "   One.\r\n\r\n   Two.");
    }

    #[test]
    fn room_description_colour_tags_do_not_count() {
        let out = format_room_description("<b:red>aaaa</> <b:red>bbbb</> <b:red>cccc</>", 16);
        // Visible: "   aaaa bbbb cccc" = 17 > 16 → wraps after bbbb.
        let lines: Vec<&str> = out.split("\r\n").collect();
        assert_eq!(lines.len(), 2, "{out:?}");
    }

    #[test]
    fn grid_pads_by_visible_width_with_spaces_only() {
        let entries = vec![
            "<red>Fire</> <red>(fire)</>".to_string(),
            "Ice <cyan>(water)</>".to_string(),
            "Sleep <magenta>(enchantment)</>".to_string(),
            "Haste".to_string(),
        ];
        let col = grid_col_width([entries.as_slice()], 2, 10);
        let rows = format_grid(&entries, col, 2, 80);
        assert!(rows.iter().all(|r| !r.contains('\t')));
        // Row 1 holds 3 cells; the 2nd cell starts at the same visible
        // column regardless of the colour tags in the first.
        let first = &rows[0];
        let second_start = 2 + col;
        let plain: String = {
            let mut s = String::new();
            let mut in_tag = false;
            for c in first.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' if in_tag => in_tag = false,
                    _ if !in_tag => s.push(c),
                    _ => {}
                }
            }
            s
        };
        assert_eq!(&plain[second_start..second_start + 3], "Ice", "{plain:?}");
        // Last cell is not padded.
        assert!(!rows[1].ends_with(' '));
    }

    #[test]
    fn grid_column_count_follows_width() {
        let entries: Vec<String> = (0..9).map(|i| format!("spell{i}")).collect();
        let wide = format_grid(&entries, 20, 2, 80);
        assert_eq!(wide.len(), 3); // 3 per row
        let narrow = format_grid(&entries, 20, 2, 45);
        assert_eq!(narrow.len(), 5); // 2 per row
        let tiny = format_grid(&entries, 20, 2, 10);
        assert_eq!(tiny.len(), 9); // always at least one column
    }

    #[test]
    fn shared_col_width_aligns_across_blocks() {
        let a = vec!["Short".to_string()];
        let b = vec!["A much longer entry here".to_string()];
        let w = grid_col_width([a.as_slice(), b.as_slice()], 2, 10);
        assert_eq!(w, "A much longer entry here".len() + 2);
    }
}
