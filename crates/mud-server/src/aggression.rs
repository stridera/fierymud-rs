//! Per-mob aggression formula evaluator (engine §B3).
//!
//! Mob protos carry an optional `aggression_formula` — a tiny boolean
//! Lua-shaped expression in the schema's `Mob.aggression_formula`
//! column. The legacy intent was for an interpreter to evaluate it
//! against a player on engage check, with mobs whose formula returns
//! true attacking on sight (alignment hatred, race hatred, etc.).
//!
//! Only 20 distinct formulas exist across 597 populated mob protos
//! (audit 2026-05-17). They share a tiny grammar:
//!
//! ```text
//! expr     := or_expr
//! or_expr  := and_expr (`or` and_expr)*
//! and_expr := atom (`and` atom)*
//! atom     := `true` | `false` | `not` atom | `(` expr `)` | cmp
//! cmp      := lhs op rhs
//! lhs      := `target.alignment` | `target.race.alignment`
//! op       := `<=` | `>=` | `<` | `>` | `==` | `!=`
//! rhs      := `ALIGN.EVIL` | `ALIGN.GOOD` | integer | `'STRING'`
//! ```
//!
//! `ALIGN.EVIL` = -350, `ALIGN.GOOD` = 350 (matches
//! `mud_db::enums::Alignment::from_score`'s thresholds — the bare
//! integers in the schema's classic CircleMUD model).
//!
//! Parsing happens once per distinct formula string and gets cached
//! in `AggressionFormulaCache`. Evaluation against a player is a
//! tiny tree walk — no allocations on the hot path.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use mud_db::enums::Alignment;

const ALIGN_EVIL_THRESHOLD: i32 = -350;
const ALIGN_GOOD_THRESHOLD: i32 = 350;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LhsKind {
    Alignment,
    RaceAlignment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Le,
    Ge,
    Lt,
    Gt,
    Eq,
    Ne,
}

#[derive(Debug, Clone)]
enum Rhs {
    Int(i32),
    Str(String),
}

#[derive(Debug, Clone)]
enum Expr {
    True,
    False,
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Cmp(LhsKind, Op, Rhs),
}

/// Read-only view of the player needed to evaluate a formula.
/// `race_alignment` is the lowercase label of the race's default
/// alignment bucket ("good" / "neutral" / "evil"), looked up via
/// `Alignment::from_score(race.default_alignment).label()`.
#[derive(Debug, Clone, Copy)]
pub struct EvalCtx {
    pub alignment: i32,
    pub race_alignment: Alignment,
}

/// Parsed-formula cache keyed by the verbatim formula string from
/// `Mob.aggression_formula`. Most entries point at the same Expr
/// node since only 20 distinct formula strings exist across the
/// 600-odd mobs that carry one — the cache amortizes the parse cost
/// to a one-time hit per distinct string at boot.
#[derive(Resource, Default)]
pub struct AggressionFormulaCache {
    by_formula: HashMap<String, Option<Expr>>,
}

impl AggressionFormulaCache {
    /// Parse-or-fetch from the cache, then evaluate against `ctx`.
    /// Returns false on parse failure so a malformed row doesn't
    /// accidentally make every mob aggro to everyone.
    pub fn eval(&mut self, formula: &str, ctx: EvalCtx) -> bool {
        let entry = self
            .by_formula
            .entry(formula.to_string())
            .or_insert_with(|| {
                let parsed = parse(formula);
                if parsed.is_none() {
                    tracing::warn!(
                        formula,
                        "aggression_formula failed to parse — mob will use the alignment-threshold fallback only"
                    );
                }
                parsed
            });
        entry.as_ref().is_some_and(|e| eval(e, ctx))
    }

    #[cfg(test)]
    pub fn parsed_count(&self) -> usize {
        self.by_formula.values().filter(|v| v.is_some()).count()
    }
}

fn parse(s: &str) -> Option<Expr> {
    let tokens = tokenize(s)?;
    let mut p = Parser { tokens, pos: 0 };
    let e = p.parse_or()?;
    if p.pos != p.tokens.len() {
        return None;
    }
    Some(e)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Not,
    True,
    False,
    Op(Op),
    AlignLhs,
    RaceAlignLhs,
    AlignConstEvil,
    AlignConstGood,
    Int(i32),
    Str(String),
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'(' {
            out.push(Tok::LParen);
            i += 1;
        } else if c == b')' {
            out.push(Tok::RParen);
            i += 1;
        } else if c == b'\'' {
            // Single-quoted string literal.
            let end = bytes[i + 1..].iter().position(|&b| b == b'\'')? + i + 1;
            let lit = std::str::from_utf8(&bytes[i + 1..end]).ok()?.to_string();
            out.push(Tok::Str(lit));
            i = end + 1;
        } else if c == b'<' || c == b'>' || c == b'=' || c == b'!' {
            // Two-char ops first.
            let next = bytes.get(i + 1).copied();
            let (op, len) = match (c, next) {
                (b'<', Some(b'=')) => (Op::Le, 2),
                (b'>', Some(b'=')) => (Op::Ge, 2),
                (b'=', Some(b'=')) => (Op::Eq, 2),
                (b'!', Some(b'=')) => (Op::Ne, 2),
                (b'<', _) => (Op::Lt, 1),
                (b'>', _) => (Op::Gt, 1),
                _ => return None,
            };
            out.push(Tok::Op(op));
            i += len;
        } else if c.is_ascii_digit() || (c == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            // Signed integer literal.
            let start = i;
            if c == b'-' {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let s = std::str::from_utf8(&bytes[start..i]).ok()?;
            out.push(Tok::Int(s.parse().ok()?));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            // Identifier — possibly dotted (`target.race.alignment`).
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            let word = std::str::from_utf8(&bytes[start..i]).ok()?;
            let tok = match word {
                "and" => Tok::And,
                "or" => Tok::Or,
                "not" => Tok::Not,
                "true" => Tok::True,
                "false" => Tok::False,
                "target.alignment" => Tok::AlignLhs,
                "target.race.alignment" => Tok::RaceAlignLhs,
                "ALIGN.EVIL" => Tok::AlignConstEvil,
                "ALIGN.GOOD" => Tok::AlignConstGood,
                _ => return None, // unknown identifier — fail closed
            };
            out.push(tok);
        } else {
            return None;
        }
    }
    Some(out)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn expect(&mut self, want: &Tok) -> Option<()> {
        if self.peek() == Some(want) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn parse_or(&mut self) -> Option<Expr> {
        let mut acc = vec![self.parse_and()?];
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            acc.push(self.parse_and()?);
        }
        Some(if acc.len() == 1 {
            acc.into_iter().next().unwrap()
        } else {
            Expr::Or(acc)
        })
    }
    fn parse_and(&mut self) -> Option<Expr> {
        let mut acc = vec![self.parse_atom()?];
        while self.peek() == Some(&Tok::And) {
            self.pos += 1;
            acc.push(self.parse_atom()?);
        }
        Some(if acc.len() == 1 {
            acc.into_iter().next().unwrap()
        } else {
            Expr::And(acc)
        })
    }
    fn parse_atom(&mut self) -> Option<Expr> {
        match self.peek()? {
            Tok::True => {
                self.pos += 1;
                Some(Expr::True)
            }
            Tok::False => {
                self.pos += 1;
                Some(Expr::False)
            }
            Tok::Not => {
                self.pos += 1;
                Some(Expr::Not(Box::new(self.parse_atom()?)))
            }
            Tok::LParen => {
                self.pos += 1;
                let e = self.parse_or()?;
                self.expect(&Tok::RParen)?;
                Some(e)
            }
            Tok::AlignLhs | Tok::RaceAlignLhs => self.parse_cmp(),
            _ => None,
        }
    }
    fn parse_cmp(&mut self) -> Option<Expr> {
        let lhs = match self.bump()? {
            Tok::AlignLhs => LhsKind::Alignment,
            Tok::RaceAlignLhs => LhsKind::RaceAlignment,
            _ => return None,
        };
        let op = match self.bump()? {
            Tok::Op(o) => o,
            _ => return None,
        };
        let rhs = match self.bump()? {
            Tok::AlignConstEvil => Rhs::Int(ALIGN_EVIL_THRESHOLD),
            Tok::AlignConstGood => Rhs::Int(ALIGN_GOOD_THRESHOLD),
            Tok::Int(n) => Rhs::Int(n),
            Tok::Str(s) => Rhs::Str(s),
            _ => return None,
        };
        Some(Expr::Cmp(lhs, op, rhs))
    }
}

fn eval(e: &Expr, ctx: EvalCtx) -> bool {
    match e {
        Expr::True => true,
        Expr::False => false,
        Expr::Not(b) => !eval(b, ctx),
        Expr::And(items) => items.iter().all(|x| eval(x, ctx)),
        Expr::Or(items) => items.iter().any(|x| eval(x, ctx)),
        Expr::Cmp(lhs, op, rhs) => match (lhs, rhs) {
            (LhsKind::Alignment, Rhs::Int(n)) => match op {
                Op::Le => ctx.alignment <= *n,
                Op::Ge => ctx.alignment >= *n,
                Op::Lt => ctx.alignment < *n,
                Op::Gt => ctx.alignment > *n,
                Op::Eq => ctx.alignment == *n,
                Op::Ne => ctx.alignment != *n,
            },
            (LhsKind::RaceAlignment, Rhs::Str(s)) => {
                let label = ctx.race_alignment.label();
                let s_norm = s.to_ascii_lowercase();
                match op {
                    Op::Eq => label.eq_ignore_ascii_case(&s_norm),
                    Op::Ne => !label.eq_ignore_ascii_case(&s_norm),
                    _ => false, // numeric compares on strings are nonsense
                }
            }
            // Mismatched lhs/rhs shapes (e.g. comparing alignment to a
            // string) eval to false — the formula was authored wrong.
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(formula: &str, alignment: i32, race: Alignment) -> bool {
        let mut cache = AggressionFormulaCache::default();
        cache.eval(
            formula,
            EvalCtx {
                alignment,
                race_alignment: race,
            },
        )
    }

    #[test]
    fn evil_target_threshold_engages_evil_aggro() {
        assert!(ev("target.alignment <= ALIGN.EVIL", -500, Alignment::Neutral));
        assert!(!ev("target.alignment <= ALIGN.EVIL", 0, Alignment::Neutral));
        assert!(!ev("target.alignment <= ALIGN.EVIL", -349, Alignment::Neutral));
    }

    #[test]
    fn neutral_band_check() {
        let f = "target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD";
        assert!(ev(f, 0, Alignment::Neutral));
        assert!(!ev(f, -500, Alignment::Neutral));
        assert!(!ev(f, 500, Alignment::Neutral));
    }

    #[test]
    fn race_alignment_evil_match() {
        let f = "target.race.alignment == 'EVIL'";
        assert!(ev(f, 0, Alignment::Evil));
        assert!(!ev(f, 0, Alignment::Good));
    }

    #[test]
    fn compound_or_disjunction() {
        let f = "(target.alignment <= ALIGN.EVIL) or (target.race.alignment == 'EVIL')";
        assert!(ev(f, -500, Alignment::Neutral));
        assert!(ev(f, 0, Alignment::Evil));
        assert!(!ev(f, 0, Alignment::Neutral));
    }

    #[test]
    fn true_literal_always_engages() {
        assert!(ev("true", 0, Alignment::Neutral));
    }

    #[test]
    fn malformed_formula_fails_closed() {
        assert!(!ev("garbage", 0, Alignment::Neutral));
    }

    #[test]
    fn cache_hit_rate_is_one_per_distinct_string() {
        let mut cache = AggressionFormulaCache::default();
        let ctx = EvalCtx {
            alignment: -500,
            race_alignment: Alignment::Neutral,
        };
        cache.eval("target.alignment <= ALIGN.EVIL", ctx);
        cache.eval("target.alignment <= ALIGN.EVIL", ctx);
        cache.eval("target.alignment >= ALIGN.GOOD", ctx);
        assert_eq!(cache.parsed_count(), 2);
    }

    #[test]
    fn all_twenty_distinct_formulas_parse() {
        let inputs = [
            "target.alignment <= ALIGN.EVIL",
            "(target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD) or (target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "(target.alignment <= ALIGN.EVIL) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD)",
            "(target.alignment <= ALIGN.EVIL) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD) or (target.race.alignment == 'EVIL')",
            "(target.alignment <= ALIGN.EVIL) or (target.alignment >= ALIGN.GOOD)",
            "(target.alignment <= ALIGN.EVIL) or (target.alignment >= ALIGN.GOOD) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD)",
            "(target.alignment <= ALIGN.EVIL) or (target.alignment >= ALIGN.GOOD) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD) or (target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "(target.alignment <= ALIGN.EVIL) or (target.alignment >= ALIGN.GOOD) or (target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "(target.alignment <= ALIGN.EVIL) or (target.race.alignment == 'EVIL')",
            "(target.alignment <= ALIGN.EVIL) or (target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "target.alignment >= ALIGN.GOOD",
            "(target.alignment >= ALIGN.GOOD) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD)",
            "(target.alignment >= ALIGN.GOOD) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD) or (target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "(target.alignment >= ALIGN.GOOD) or (target.alignment > ALIGN.EVIL and target.alignment < ALIGN.GOOD) or (target.race.alignment == 'GOOD')",
            "(target.alignment >= ALIGN.GOOD) or (target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "(target.alignment >= ALIGN.GOOD) or (target.race.alignment == 'GOOD')",
            "target.race.alignment == 'EVIL'",
            "(target.race.alignment == 'EVIL') or (target.race.alignment == 'GOOD')",
            "target.race.alignment == 'GOOD'",
            "true",
        ];
        for f in inputs {
            assert!(parse(f).is_some(), "failed to parse: {f}");
        }
    }
}
