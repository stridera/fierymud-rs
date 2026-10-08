//! Prompt template rendering.
//!
//! The `%` codes are the legacy `FieryMUD` prompt language (see
//! `prompt_str` in legacy `comm.cpp`); characters imported from the
//! live game carry prompts written in it, so the semantics here match
//! legacy exactly, including its quirks:
//!
//! * `%` + an unknown code prints nothing (the pair is swallowed), and a
//!   trailing lone `%` prints nothing;
//! * `%p`, `%c` and `%d` take a second letter (percentage / coin /
//!   cooldown selector); an unknown second letter prints nothing;
//! * the `<wizi N>` / `<AFK>` flag prefix is put on its own line ahead
//!   of the prompt unless the template contains `%x`, which places the
//!   flags inline instead. A template with no `%` at all is printed
//!   verbatim with no flag prefix.
//!
//! Codes (legacy): `h H v V` hit/move current/max; `i I` hiddenness;
//! `a A` alignment; `n` name; `k` class; `N` real name (differs from
//! `n` only while switched); `d<x>` cooldown bar; `e` / `E` experience
//! bar / message; `l L` active effects; `p<h|H|v|V>` percent of
//! hit/move; `c<p|g|s|c|P|G|S|C>` coin counts (lower = held, upper =
//! bank); `w W` all coins held / banked; `o O` opponent
//! (name+condition / name); `t T` opponent's target; `g G` group
//! leader; `r` rage; `x X` flags; `z Z` zone; `#` level; `_` newline;
//! `-` space; `%` percent sign.
//!
//! Additional `fierymud-rs` codes on letters legacy leaves unused:
//! `B` / `M` 10-cell HP / stamina bar, `K` 10-cell opponent HP bar,
//! `R` room name, `s` season, `y` in-game hour, `Y` day/night.

use bevy_ecs::prelude::{Entity, World};
use mud_world::{
    AbilityCatalog, AppliedTo, BankWealth, ClassCatalog, CombatStats, Cooldowns, EffectInstance,
    Fighting, Health, Located, MudClock, Named, PlayerFlags, Profile, Stamina, Stealth,
    SwitchedInto, Wealth, WizInvis,
};

use crate::commands::{
    effect_duration_color, group_root, level_progress, render_vital_bar, vital_color_tag,
};

/// Opponent / tank / group-leader snapshot used by `%o %O %t %T %g %G`.
#[derive(Clone, Debug)]
pub(crate) struct Combatant {
    pub name: String,
    pub hp: i32,
    pub max_hp: i32,
}

/// Coins split into denominations (platinum = 1000 cp, gold = 100,
/// silver = 10, copper = 1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coins {
    pub platinum: i64,
    pub gold: i64,
    pub silver: i64,
    pub copper: i64,
}

impl Coins {
    #[must_use]
    pub(crate) fn from_copper(total: i64) -> Self {
        let mut rest = total.max(0);
        let platinum = rest / 1000;
        rest %= 1000;
        let gold = rest / 100;
        rest %= 100;
        Self {
            platinum,
            gold,
            silver: rest / 10,
            copper: rest % 10,
        }
    }
}

/// Experience position for `%e` / `%E`, from legacy `exp_message` /
/// `exp_bar` inputs: `total` XP spans the current level, `current` is how
/// far into it the character is.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExpState {
    pub level: i32,
    pub total: i64,
    pub current: i64,
    pub starstar: bool,
}

/// One effect for `%l`.
#[derive(Clone, Debug)]
pub(crate) struct EffectEntry {
    pub name: String,
    /// Open colour tag (only when the viewer has detect magic).
    pub tag: Option<&'static str>,
}

/// Everything the prompt template can read, gathered up front so
/// [`render_prompt`] stays a pure function.
#[derive(Default, Clone)]
pub(crate) struct PromptCtx {
    pub hp: Option<Health>,
    pub stamina: Option<Stamina>,
    pub name: Option<String>,
    pub real_name: Option<String>,
    pub class_name: Option<String>,
    pub level: i32,
    pub alignment: i32,
    pub hiddenness: i64,
    pub rage: i32,
    pub zone_name: Option<String>,
    pub room: Option<String>,
    pub held: Coins,
    pub bank: Coins,
    pub victim: Option<Combatant>,
    pub tank: Option<Combatant>,
    pub group_master: Option<Combatant>,
    pub exp: ExpState,
    pub effects: Vec<EffectEntry>,
    /// `(legacy letter, remaining ms, total ms)` for cooldowns in
    /// progress; letters absent here render an idle bar.
    pub cooldowns: Vec<(char, i64, i64)>,
    pub wizinvis: i32,
    pub roomvis: bool,
    pub afk: bool,
    /// In-game hour 0..=23 for `%y`.
    pub hour: Option<i32>,
    pub season: Option<String>,
    /// "day" / "night" for `%Y`.
    pub day_night: Option<&'static str>,
}

/// Repair `%%X` patterns where X is a recognized prompt variable.
///
/// Background: an early version of the schema set
/// `Characters.prompt @default("<%%h/%%Hhp %%v/%%Vmv>")` thinking
/// Prisma would unescape `%%` → `%`. Prisma stores the literal,
/// so every newly-created character (including all seeded test
/// users) ended up with a prompt template that — after the
/// `%%` → literal-`%` rule in `render_prompt` — displays
/// literal `%h` / `%H` instead of HP values.
///
/// Login calls this on the loaded template before constructing the
/// `Prompt` component; the next save persists the cleaned form so
/// the broken row repairs itself across one disconnect cycle.
///
/// Conservative scope: only collapses `%%X` where X is one of the vital
/// letters the broken default used.
#[must_use]
pub(crate) fn sanitize_prompt_template(template: &str) -> String {
    const KNOWN: &[char] = &['h', 'H', 'v', 'V', 'B', 'M'];
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%'
            && chars.get(i + 1) == Some(&'%')
            && chars.get(i + 2).is_some_and(|c| KNOWN.contains(c))
        {
            // Saw `%%X` where X is a known variable — collapse to `%X`.
            out.push('%');
            out.push(chars[i + 2]);
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Legacy `status_string(cur, max, STATUS_ALIAS)`: a coloured
/// condition word for a health ratio.
#[must_use]
pub(crate) fn status_alias(cur: i32, max: i32) -> String {
    let percent: i64 = if max > 0 {
        i64::from(cur).saturating_mul(100) / i64::from(max)
    } else {
        -1
    };
    let (tag, cond) = if percent >= 100 {
        ("<green>", "excellent")
    } else if percent >= 88 {
        ("<yellow>", "scratches")
    } else if percent >= 75 {
        ("<b:yellow>", "small wounds")
    } else if percent >= 50 {
        ("<b:magenta>", "few wounds")
    } else if percent >= 30 {
        ("<magenta>", "nasty wounds")
    } else if percent >= 15 {
        ("<b:red>", "pretty hurt")
    } else if percent >= 0 {
        ("<red>", "awful condition")
    } else {
        ("<red>", "bleeding awfully")
    };
    format!("{tag}{cond}</>")
}

/// Legacy `exp_message`.
#[must_use]
pub(crate) fn exp_message(e: ExpState) -> String {
    const MESSAGES: [&str; 11] = [
        "<blue>You still have a very long way to go to your next level.</>",
        "<blue>You have gained some progress towards your next level.</>",
        "<cyan>You are about one-quarter of the way to your next level.</>",
        "<blue>You are about a third of the way to your next level.</>",
        "<blue>You are almost half-way to your next level.</>",
        "<cyan>You are just past the half-way point to your next level.</>",
        "<blue>You are well on your way to your next level.</>",
        "<blue>You are about three-quarters of the way to your next level.</>",
        "<cyan>You are almost ready to attain your next level.</>",
        "<blue>You should level anytime now!</>",
        "<blue>You are SO close to the next level.</>",
    ];
    if e.level >= mud_db::enums::MIN_STAFF_LEVEL {
        return "Experience has no meaning for you.".to_string();
    }
    if e.total < 1 {
        return "<green>You're fairly weak.</>".to_string();
    }
    let percent = (100 * e.current) / e.total;
    if e.starstar {
        "<yellow>You are as powerful as a mortal can be!</>".to_string()
    } else if e.level == mud_db::enums::MAX_MORTAL_LEVEL && e.current > e.total {
        "<blue>You are working towards getting your stars!</>".to_string()
    } else if e.total - e.current == 1 {
        "<blue>You are ready for the next level!</>".to_string()
    } else if percent < 4 {
        "<blue>You have just begun the journey to your next level.</>".to_string()
    } else if (0..=100).contains(&percent) {
        MESSAGES[usize::try_from(percent / 10).unwrap_or(0)].to_string()
    } else {
        "<red>You are somewhere along the way to your next level.</>".to_string()
    }
}

/// Legacy `exp_bar(ch, length, gradations, sub_gradations)`, colour
/// as XML-Lite tags (stripped for colour-off clients like everything
/// else sent through the renderer).
#[must_use]
pub(crate) fn exp_bar(e: ExpState, length: i64, gradations: i64, sub_gradations: i64) -> String {
    let length = length.clamp(1, 80);
    let immortal = e.level >= mud_db::enums::MIN_STAFF_LEVEL;
    let max_mort = e.level == mud_db::enums::MAX_MORTAL_LEVEL;
    let (mut distance, sub_distance, total, current);
    if immortal {
        distance = 0;
        sub_distance = 0;
        total = 0;
        current = 0;
    } else {
        total = e.total;
        if total < 1 {
            return "<green>-?-</>".to_string();
        }
        current = e.current.max(0);
        let grad_count = (length * current) / total;
        let length_per_grad = (length / gradations).max(1);
        distance = (grad_count / length_per_grad) * length_per_grad;
        let exp_per_grad = (total / gradations).max(1);
        let towards_next = current % exp_per_grad;
        let sub_grad_count = (length * towards_next) / exp_per_grad;
        let length_per_sub = (length / sub_gradations).max(1);
        sub_distance = (sub_grad_count / length_per_sub) * length_per_sub + 1;
    }
    if total - current == 1 {
        distance = length - 1;
    }
    if immortal {
        distance = 0;
    }
    distance = distance.clamp(0, length);

    let mut bar = String::new();
    if immortal {
        bar.push_str("<b:red>");
    } else if total - current > 1 {
        bar.push_str("<b:blue>");
    } else if e.starstar {
        bar.push_str("<b:yellow>");
    } else if max_mort {
        bar.push_str("<b:green>");
    } else {
        bar.push_str("<b:cyan>");
    }
    for i in 0..length {
        if immortal || (max_mort && current > total) {
            bar.push('-');
        } else if i == distance {
            bar.push_str("<cyan>*");
            if sub_distance - 1 <= distance {
                bar.push_str("</>");
            }
            bar.push_str("<blue>");
        } else if i < sub_distance {
            bar.push('=');
            if i == sub_distance - 1 {
                bar.push_str("<blue>");
            }
        } else {
            bar.push('-');
        }
    }
    bar.push_str("</>");
    bar
}

/// Legacy `cooldown_bar(ch, cooldown, length, gradations)` for a
/// cooldown with `current` of `total` remaining (same unit).
#[must_use]
pub(crate) fn cooldown_bar(current: i64, total: i64, length: i64, gradations: i64) -> String {
    let length = length.clamp(1, 80);
    let mut distance = 0;
    let mut percent = 0.0_f64;
    if total != 0 {
        let grad_count = (length * current) / total;
        let length_per_grad = (length / gradations).max(1);
        distance = (grad_count / length_per_grad) * length_per_grad;
        #[allow(clippy::cast_precision_loss)]
        {
            percent = current as f64 / total as f64;
        }
    }
    distance = distance.clamp(0, length);
    let mut bar = String::new();
    if current == 0 || total == 0 {
        bar.push_str("<cyan>");
    } else if percent < 0.33 {
        bar.push_str("<green>");
    } else if percent < 0.66 {
        bar.push_str("<yellow>");
    } else {
        bar.push_str("<red>");
    }
    for i in 0..length {
        if total == 0 || current == 0 || i > distance {
            bar.push('-');
        } else if i == distance {
            bar.push_str("<white>*<blue>");
        } else {
            bar.push('=');
        }
    }
    bar.push_str("</>");
    bar
}

/// Open tag + value + close for a vital reading, colour-graded by ratio
/// (red < 25%, yellow < 50%).
fn graded(value: i32, max: i32) -> String {
    match vital_color_tag(value, max) {
        Some(open) => format!("{open}{value}</>"),
        None => value.to_string(),
    }
}

fn percent_of(cur: i32, max: i32) -> i32 {
    let pct = i64::from(cur).saturating_mul(100) / i64::from(max.max(1));
    i32::try_from(pct).unwrap_or(0)
}

fn coin_digit(c: Coins, bank: Coins, code: char) -> Option<i64> {
    Some(match code {
        'p' => c.platinum,
        'g' => c.gold,
        's' => c.silver,
        'c' => c.copper,
        'P' => bank.platinum,
        'G' => bank.gold,
        'S' => bank.silver,
        'C' => bank.copper,
        _ => return None,
    })
}

fn coins_line(c: Coins) -> String {
    format!(
        "</>{}<cyan>p</> {}<yellow>g</> {}s {}<yellow>c</>",
        c.platinum, c.gold, c.silver, c.copper
    )
}

fn victim_line(c: &Combatant, with_status: bool, tail_reset: bool) -> String {
    if with_status {
        let reset = if tail_reset { "</>" } else { "" };
        format!("{} </>({}{reset})", c.name, status_alias(c.hp, c.max_hp))
    } else {
        c.name.clone()
    }
}

/// Parser state for [`render_prompt`]: which selector letter, if any, the
/// next character supplies.
enum Expect {
    Nothing,
    Control,
    Percent,
    Coin,
    Cooldown,
}

/// Render a prompt template per the module docs. Colour tags are left in
/// the output for the caller's renderer; `%_` and the flag-prefix break
/// are emitted as `\r\n`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub(crate) fn render_prompt(template: &str, ctx: &PromptCtx) -> String {
    // Legacy: no `%` anywhere means no parsing and no flag prefix.
    if !template.contains('%') {
        let mut out = template.to_string();
        if !out.ends_with(' ') {
            out.push(' ');
        }
        return out;
    }

    // Flag prefix: each flag ends with a space; `%x` reuses it minus the
    // trailing space.
    let mut flags: Vec<String> = Vec::new();
    if ctx.wizinvis > 0 {
        flags.push(format!("<wizi {:03}>", ctx.wizinvis));
    }
    if ctx.roomvis && ctx.wizinvis > 0 {
        flags.push("<rmvis>".to_string());
    }
    if ctx.afk {
        flags.push("<AFK>".to_string());
    }
    let flag_text = flags.join(" ");

    let mut out = String::with_capacity(template.len() + 32);
    let mut found_x = false;
    let mut expecting = Expect::Nothing;

    for c in template.chars() {
        match expecting {
            Expect::Nothing => {
                if c == '%' {
                    expecting = Expect::Control;
                } else {
                    out.push(c);
                }
            }
            Expect::Control => {
                expecting = Expect::Nothing;
                match c {
                    'h' => {
                        out.push_str(&ctx.hp.map_or_else(|| "?".into(), |h| graded(h.hp, h.max)));
                    }
                    'H' => out.push_str(&ctx.hp.map_or_else(|| "?".into(), |h| h.max.to_string())),
                    'v' => out.push_str(
                        &ctx.stamina
                            .map_or_else(|| "?".into(), |s| graded(s.current, s.max)),
                    ),
                    'V' => out.push_str(
                        &ctx.stamina
                            .map_or_else(|| "?".into(), |s| s.max.to_string()),
                    ),
                    'B' => match ctx.hp {
                        Some(h) => out.push_str(&render_vital_bar(h.hp, h.max)),
                        None => out.push_str("[??????????]"),
                    },
                    'M' => match ctx.stamina {
                        Some(s) => out.push_str(&render_vital_bar(s.current, s.max)),
                        None => out.push_str("[??????????]"),
                    },
                    'i' | 'I' => out.push_str(&ctx.hiddenness.to_string()),
                    'a' | 'A' => out.push_str(&ctx.alignment.to_string()),
                    'n' => out.push_str(ctx.name.as_deref().unwrap_or("?")),
                    'k' => out.push_str(ctx.class_name.as_deref().unwrap_or("")),
                    'N' => out.push_str(ctx.real_name.as_deref().unwrap_or("?")),
                    'd' => expecting = Expect::Cooldown,
                    'e' => out.push_str(&exp_bar(ctx.exp, 20, 20, 20)),
                    'E' => out.push_str(&exp_message(ctx.exp)),
                    'l' | 'L' => {
                        let n = ctx.effects.len();
                        for (i, eff) in ctx.effects.iter().enumerate() {
                            if let Some(tag) = eff.tag {
                                out.push_str(tag);
                            }
                            out.push_str(&eff.name);
                            if eff.tag.is_some() {
                                out.push_str("</>");
                            }
                            if i + 1 < n {
                                out.push_str(", ");
                            }
                        }
                    }
                    'p' | 'P' => expecting = Expect::Percent,
                    'c' | 'C' => expecting = Expect::Coin,
                    'w' => out.push_str(&coins_line(ctx.held)),
                    'W' => out.push_str(&coins_line(ctx.bank)),
                    'o' => {
                        if let Some(v) = &ctx.victim {
                            out.push_str(&victim_line(v, true, false));
                        }
                    }
                    'O' => {
                        if let Some(v) = &ctx.victim {
                            out.push_str(&victim_line(v, false, false));
                        }
                    }
                    't' => {
                        if let Some(t) = &ctx.tank {
                            out.push_str(&victim_line(t, true, true));
                        }
                    }
                    'T' => {
                        if let Some(t) = &ctx.tank {
                            out.push_str(&victim_line(t, false, false));
                        }
                    }
                    'g' => {
                        if let Some(g) = &ctx.group_master {
                            out.push_str(&victim_line(g, true, true));
                        }
                    }
                    'G' => {
                        if let Some(g) = &ctx.group_master {
                            out.push_str(&victim_line(g, false, false));
                        }
                    }
                    'r' => out.push_str(&ctx.rage.to_string()),
                    'x' | 'X' => {
                        found_x = true;
                        out.push_str(&flag_text);
                    }
                    'z' | 'Z' => out.push_str(ctx.zone_name.as_deref().unwrap_or("")),
                    '#' => out.push_str(&ctx.level.to_string()),
                    '_' => out.push_str("\r\n"),
                    '-' => out.push(' '),
                    '%' => out.push('%'),
                    // fierymud-rs extras (letters unused by legacy).
                    'K' => match &ctx.victim {
                        Some(v) => out.push_str(&render_vital_bar(v.hp, v.max_hp)),
                        None => out.push_str("[----------]"),
                    },
                    'R' => out.push_str(ctx.room.as_deref().unwrap_or("?")),
                    's' => out.push_str(ctx.season.as_deref().unwrap_or("?")),
                    'y' => match ctx.hour {
                        Some(h) => out.push_str(&format!("{h:02}")),
                        None => out.push('?'),
                    },
                    'Y' => out.push_str(ctx.day_night.unwrap_or("?")),
                    // Legacy: unknown code prints nothing.
                    _ => {}
                }
            }
            Expect::Percent => {
                expecting = Expect::Nothing;
                let pct = match c {
                    'h' | 'H' => ctx.hp.map(|h| percent_of(h.hp, h.max)),
                    'v' | 'V' => ctx.stamina.map(|s| percent_of(s.current, s.max)),
                    _ => continue,
                };
                match pct {
                    Some(p) => out.push_str(&format!("{p}%")),
                    None => out.push('?'),
                }
            }
            Expect::Coin => {
                expecting = Expect::Nothing;
                if let Some(n) = coin_digit(ctx.held, ctx.bank, c) {
                    out.push_str(&n.to_string());
                }
            }
            Expect::Cooldown => {
                expecting = Expect::Nothing;
                if !COOLDOWN_SELECTORS.contains(c) {
                    continue;
                }
                let (cur, total) = ctx
                    .cooldowns
                    .iter()
                    .find(|(l, _, _)| *l == c)
                    .map_or((0, 0), |(_, cur, total)| (*cur, *total));
                out.push_str(&cooldown_bar(cur, total, 20, 20));
            }
        }
    }

    // Flags not claimed by `%x` lead the prompt on their own line.
    if !found_x && !flags.is_empty() {
        out = format!("{flag_text}\r\n{out}");
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// Every selector letter legacy accepts after `%d`.
const COOLDOWN_SELECTORS: &str = "abcdDefghijklmnopqrstuvwxy1234567";

/// Legacy cooldown letters (`%d<letter>`) mapped to the abilities whose
/// cooldown each one tracks. Letters whose legacy innate / skill has no
/// counterpart here are absent and render an idle bar.
const COOLDOWN_LETTERS: &[(char, &[&str])] = &[
    ('a', &["innate ascension"]),
    (
        'b',
        &[
            "breathe acid",
            "breathe fire",
            "breathe frost",
            "breathe gas",
            "breathe lightning",
        ],
    ),
    ('c', &["innate brilliance"]),
    ('g', &["darkness"]),
    ('h', &["disarm"]),
    ('i', &["first aid"]),
    ('j', &["instant kill"]),
    ('k', &["invisibility"]),
    ('l', &["lay hands"]),
    ('m', &["feather fall"]),
    ('n', &["shapechange"]),
    ('o', &["summon mount"]),
    ('r', &["throatcut"]),
    ('t', &["blinding beauty"]),
    ('u', &["illumination"]),
    ('w', &["statue"]),
    ('x', &["barkskin"]),
];

/// Active, non-permanent effects on `target`: one entry per distinct
/// name, colour-graded by time left when the target has detect magic
/// (legacy `%l` behaviour).
fn active_effects(world: &mut World, target: Entity) -> Vec<EffectEntry> {
    let mut found: Vec<(String, i32, bool)> = Vec::new();
    {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        let catalog = world.get_resource::<AbilityCatalog>();
        for (inst, applied) in q.iter(world) {
            if applied.0 != target || inst.remaining_secs < 0 {
                continue;
            }
            let label = inst
                .ability_id
                .and_then(|id| catalog.and_then(|c| c.by_name.values().find(|d| d.id == id)))
                .map_or_else(|| inst.name.replace('_', " "), |d| d.plain_name.clone());
            let is_detect = inst.name.eq_ignore_ascii_case("detect_magic")
                || label.eq_ignore_ascii_case("detect magic");
            found.push((label, inst.remaining_secs, is_detect));
        }
    }
    let has_detect_magic = found.iter().any(|(_, _, d)| *d);
    let mut seen: Vec<String> = Vec::new();
    found
        .into_iter()
        .filter_map(|(name, remaining, _)| {
            if seen.contains(&name) {
                return None;
            }
            seen.push(name.clone());
            let tag = if has_detect_magic {
                effect_duration_color(u64::try_from(remaining).unwrap_or(0))
            } else {
                None
            };
            Some(EffectEntry { name, tag })
        })
        .collect()
}

/// `(legacy letter, remaining ms, total ms)` for every tracked cooldown
/// currently running on `target`.
fn active_cooldowns(world: &World, target: Entity) -> Vec<(char, i64, i64)> {
    let mut out: Vec<(char, i64, i64)> = Vec::new();
    let (Some(cd), Some(catalog)) = (
        world.get::<Cooldowns>(target),
        world.get_resource::<AbilityCatalog>(),
    ) else {
        return out;
    };
    let now = std::time::Instant::now();
    for (letter, names) in COOLDOWN_LETTERS {
        let mut best: Option<(i64, i64)> = None;
        for def in catalog.by_name.values() {
            if !names.contains(&def.plain_name.as_str()) {
                continue;
            }
            let Some(ready_at) = cd.ready_at.get(&def.id) else {
                continue;
            };
            if *ready_at <= now {
                continue;
            }
            let remaining =
                i64::try_from(ready_at.saturating_duration_since(now).as_millis()).unwrap_or(0);
            let total = i64::from(def.cooldown_ms).max(remaining);
            if best.is_none_or(|(r, _)| remaining > r) {
                best = Some((remaining, total));
            }
        }
        if let Some((remaining, total)) = best {
            out.push((*letter, remaining, total));
        }
    }
    out
}

/// XP position inside the current level for `%e` / `%E`.
fn exp_state(world: &World, p: &Profile) -> ExpState {
    let (total, current) = match level_progress(world, p) {
        Some(lp) => (
            lp.next_level_xp - lp.level_floor_xp,
            i64::from(p.experience) - lp.level_floor_xp,
        ),
        None => (0, 0),
    };
    ExpState {
        level: p.level,
        total,
        current,
        starstar: mud_world::is_starstar(world, p),
    }
}

/// Gather everything [`render_prompt`] reads for `target`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub(crate) fn build_prompt_ctx(world: &mut World, target: Entity) -> PromptCtx {
    let effects = active_effects(world, target);
    let combatant = |e: Entity| -> Option<Combatant> {
        world.get_entity(e).ok()?;
        let name = world.get::<Named>(e)?.name.clone();
        let h = world.get::<Health>(e).copied()?;
        Some(Combatant {
            name,
            hp: h.hp,
            max_hp: h.max,
        })
    };

    let real_name = world.get::<Named>(target).map(|n| n.name.clone());
    // While switched into a mob the prompt describes the puppet; `%N`
    // keeps naming the real character.
    let acting = world.get::<SwitchedInto>(target).map_or(target, |s| s.0);
    let name = world.get::<Named>(acting).map(|n| n.name.clone());

    let located = world.get::<Located>(target).map(|l| l.0);
    let room = located
        .and_then(|r| world.get::<Named>(r))
        .map(|n| n.name.clone());
    // `%z`: a mortal inside a god zone gets no zone name.
    let zone_name = located
        .filter(|r| crate::room_access::room_visible_to(world, target, *r))
        .and_then(|r| world.get::<Located>(r))
        .and_then(|z| world.get::<Named>(z.0))
        .map(|n| n.name.clone());

    let profile = world.get::<Profile>(target);
    let class_name = profile.and_then(|p| {
        p.class_id.and_then(|id| {
            world
                .get_resource::<ClassCatalog>()
                .and_then(|c| c.by_id.get(&id))
                .map(|d| d.name.clone())
        })
    });
    let level = profile.map_or(0, |p| p.level);
    let exp = profile.map_or_else(ExpState::default, |p| exp_state(world, p));

    let victim_entity = world
        .get::<Fighting>(target)
        .map(|f| f.0)
        .filter(|e| world.get_entity(*e).is_ok());
    let tank_entity = victim_entity
        .and_then(|v| world.get::<Fighting>(v))
        .map(|f| f.0)
        .filter(|e| world.get_entity(*e).is_ok());
    let group_master = {
        let leader = group_root(world, target);
        (leader != target).then(|| combatant(leader)).flatten()
    };

    let cooldowns = active_cooldowns(world, target);

    let clock = world.get_resource::<MudClock>();
    let hour = clock.map(|c| c.hour);
    PromptCtx {
        hp: world.get::<Health>(acting).copied(),
        stamina: world.get::<Stamina>(acting).copied(),
        name,
        real_name,
        class_name,
        level,
        alignment: world.get::<CombatStats>(acting).map_or(0, |c| c.alignment),
        hiddenness: i64::from(world.get::<Stealth>(acting).is_some()),
        rage: 0,
        zone_name,
        room,
        held: Coins::from_copper(world.get::<Wealth>(target).map_or(0, |w| w.0)),
        bank: Coins::from_copper(world.get::<BankWealth>(target).map_or(0, |w| w.0)),
        victim: victim_entity.and_then(combatant),
        tank: tank_entity.and_then(combatant),
        group_master,
        exp,
        effects,
        cooldowns,
        wizinvis: world.get::<WizInvis>(target).map_or(0, |w| w.0),
        roomvis: false,
        afk: world
            .get::<PlayerFlags>(target)
            .is_some_and(|f| f.has(mud_db::enums::PlayerFlag::Afk)),
        hour,
        season: clock.map(|c| c.season().label().to_string()),
        day_night: hour.map(|h| {
            if matches!(h, 0..=4 | 22..=23) {
                "night"
            } else {
                "day"
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{ColorMode, render_color_tags};

    fn plain(s: &str) -> String {
        render_color_tags(s, ColorMode::Strip)
    }

    fn orc() -> Combatant {
        Combatant {
            name: "an orc".into(),
            hp: 75,
            max_hp: 100,
        }
    }

    fn ctx() -> PromptCtx {
        PromptCtx {
            hp: Some(Health { hp: 80, max: 100 }),
            stamina: Some(Stamina {
                current: 40,
                max: 50,
            }),
            name: Some("Strider".into()),
            real_name: Some("Strider".into()),
            class_name: Some("Cleric".into()),
            level: 42,
            alignment: -350,
            hiddenness: 7,
            rage: 120,
            zone_name: Some("Mielikki".into()),
            room: Some("The Void".into()),
            held: Coins {
                platinum: 1,
                gold: 2,
                silver: 3,
                copper: 4,
            },
            bank: Coins {
                platinum: 9,
                gold: 8,
                silver: 7,
                copper: 6,
            },
            victim: Some(orc()),
            tank: Some(Combatant {
                name: "Thrum".into(),
                hp: 10,
                max_hp: 100,
            }),
            group_master: Some(Combatant {
                name: "Chinok".into(),
                hp: 100,
                max_hp: 100,
            }),
            exp: ExpState {
                level: 42,
                total: 1000,
                current: 500,
                starstar: false,
            },
            effects: vec![
                EffectEntry {
                    name: "armor".into(),
                    tag: None,
                },
                EffectEntry {
                    name: "bless".into(),
                    tag: None,
                },
            ],
            cooldowns: vec![('h', 5000, 10000)],
            wizinvis: 0,
            roomvis: false,
            afk: false,
            hour: Some(7),
            season: Some("Winter".into()),
            day_night: Some("day"),
        }
    }

    fn rp(t: &str) -> String {
        plain(&render_prompt(t, &ctx()))
    }

    #[test]
    fn sanitize_collapses_double_percent_for_vitals() {
        assert_eq!(
            sanitize_prompt_template("<%%h/%%Hhp %%v/%%Vmv>"),
            "<%h/%Hhp %v/%Vmv>"
        );
        assert_eq!(sanitize_prompt_template("100%%"), "100%%");
        assert_eq!(sanitize_prompt_template("<%h/%H>"), "<%h/%H>");
    }

    #[test]
    fn vitals_and_hidden_flags() {
        assert_eq!(rp("<%h/%H>"), "<80/100> ");
        assert_eq!(rp("<%v/%V mv>"), "<40/50 mv> ");
        assert_eq!(rp("<%h/%H %v/%V> "), "<80/100 40/50> ");
        assert_eq!(rp("[%i%I]"), "[77] ");
        assert_eq!(rp("<%aA>"), "<-350A> ");
        assert_eq!(rp("<%ih>"), "<7h> ");
        assert_eq!(rp("[%r]"), "[120] ");
    }

    #[test]
    fn issue_13_prompt_has_no_literal_codes() {
        // The prompt from the bug report, minus the effect list.
        let out = rp("<%h(%H) %v(%V)> <%aA> <%ih>%_<02>:<%o>");
        assert!(!out.contains('%'), "{out:?}");
        assert!(out.contains("<-350A>") && out.contains("<7h>"));
        assert!(out.contains("\r\n<02>:<an orc (small wounds)>"), "{out:?}");
    }

    #[test]
    fn issue_13_out_of_combat_tank_target_line_keeps_brackets() {
        // `<%t> : <%o>` with no tank and no victim must render `<> : <>`
        // through the real colour pass, not `:` (empty `<>` was dropped).
        let mut c = ctx();
        c.victim = None;
        c.tank = None;
        let out = plain(&render_prompt("<%t> : <%o>", &c));
        assert_eq!(out, "<> : <> ");
    }

    #[test]
    fn identity_codes() {
        assert_eq!(rp("[%n]"), "[Strider] ");
        assert_eq!(rp("[%N]"), "[Strider] ");
        assert_eq!(rp("[%k]"), "[Cleric] ");
        assert_eq!(rp("[%#]"), "[42] ");
        assert_eq!(rp("[%z]"), "[Mielikki] ");
        assert_eq!(rp("[%Z]"), "[Mielikki] ");
        let mut c = ctx();
        c.name = Some("a goblin".into());
        assert_eq!(plain(&render_prompt("%n/%N", &c)), "a goblin/Strider ");
    }

    #[test]
    fn percent_codes() {
        assert_eq!(rp("%ph %pH %pv %pV"), "80% 80% 80% 80% ");
        // Unknown second letter prints nothing and is consumed.
        assert_eq!(rp("[%pz]"), "[] ");
        let mut c = ctx();
        c.hp = Some(Health { hp: 0, max: 0 });
        assert_eq!(plain(&render_prompt("%ph", &c)), "0% ");
    }

    #[test]
    fn coin_codes() {
        assert_eq!(rp("%cp.%cg.%cs.%cc"), "1.2.3.4 ");
        assert_eq!(rp("%cP.%cG.%cS.%cC"), "9.8.7.6 ");
        assert_eq!(rp("%Cp|%CG"), "1|8 ");
        assert_eq!(rp("[%cx]"), "[] ");
        assert_eq!(rp("%w"), "1p 2g 3s 4c ");
        assert_eq!(rp("%W"), "9p 8g 7s 6c ");
    }

    #[test]
    fn coins_split_by_denomination() {
        assert_eq!(
            Coins::from_copper(1234),
            Coins {
                platinum: 1,
                gold: 2,
                silver: 3,
                copper: 4
            }
        );
        assert_eq!(Coins::from_copper(-5), Coins::default());
    }

    #[test]
    fn combat_codes() {
        assert_eq!(rp("%o"), "an orc (small wounds) ");
        assert_eq!(rp("%O"), "an orc ");
        assert_eq!(rp("%t"), "Thrum (awful condition) ");
        assert_eq!(rp("%T"), "Thrum ");
        assert_eq!(rp("%g"), "Chinok (excellent) ");
        assert_eq!(rp("%G"), "Chinok ");
        // Out of combat / ungrouped: nothing.
        let mut c = ctx();
        c.victim = None;
        c.tank = None;
        c.group_master = None;
        assert_eq!(plain(&render_prompt("[%o%O%t%T%g%G]", &c)), "[] ");
    }

    #[test]
    fn status_alias_bands_match_legacy() {
        let word = |cur, max| plain(&status_alias(cur, max));
        assert_eq!(word(100, 100), "excellent");
        assert_eq!(word(88, 100), "scratches");
        assert_eq!(word(75, 100), "small wounds");
        assert_eq!(word(50, 100), "few wounds");
        assert_eq!(word(30, 100), "nasty wounds");
        assert_eq!(word(15, 100), "pretty hurt");
        assert_eq!(word(0, 100), "awful condition");
        assert_eq!(word(-5, 100), "bleeding awfully");
        assert_eq!(word(5, 0), "bleeding awfully");
    }

    #[test]
    fn effect_list() {
        assert_eq!(rp("<%l>"), "<armor, bless> ");
        assert_eq!(rp("<%L>"), "<armor, bless> ");
        let mut c = ctx();
        c.effects[0].tag = Some("<red>");
        assert_eq!(
            render_prompt("%l", &c),
            "<red>armor</>, bless ",
            "detect magic colours near-expiry effects"
        );
    }

    #[test]
    fn experience_codes() {
        // Halfway through the level.
        assert_eq!(
            rp("%E"),
            "You are just past the half-way point to your next level. "
        );
        let bar = rp("%e");
        assert_eq!(bar.trim_end().chars().count(), 20, "{bar:?}");
        assert!(bar.contains('*') && bar.contains('='), "{bar:?}");
        let mut c = ctx();
        c.level = 105;
        c.exp.level = 105;
        assert_eq!(
            plain(&render_prompt("%E", &c)),
            "Experience has no meaning for you. "
        );
        assert_eq!(
            plain(&render_prompt("%e", &c)),
            format!("{} ", "-".repeat(20))
        );
        c.exp = ExpState {
            level: 99,
            total: 1000,
            current: 1000,
            starstar: true,
        };
        assert_eq!(
            plain(&render_prompt("%E", &c)),
            "You are as powerful as a mortal can be! "
        );
        c.exp = ExpState::default();
        assert_eq!(plain(&render_prompt("%E", &c)), "You're fairly weak. ");
        assert_eq!(plain(&render_prompt("%e", &c)), "-?- ");
    }

    #[test]
    fn exp_bar_marker_tracks_progress() {
        let at = |current| {
            let b = plain(&exp_bar(
                ExpState {
                    level: 10,
                    total: 1000,
                    current,
                    starstar: false,
                },
                20,
                20,
                20,
            ));
            b.find('*').unwrap()
        };
        assert_eq!(at(0), 0);
        assert!(at(500) > at(100));
        assert_eq!(at(999), 19, "ready to level puts the marker at the end");
    }

    #[test]
    fn cooldown_codes() {
        // 5s of 10s: half bar, marker at cell 10.
        let bar = rp("%dh");
        assert_eq!(bar.trim_end().chars().count(), 20, "{bar:?}");
        assert_eq!(bar.find('*'), Some(10), "{bar:?}");
        assert!(bar.starts_with("=========="), "{bar:?}");
        // Idle bar: all dashes.
        assert_eq!(rp("%da"), format!("{} ", "-".repeat(20)));
        // Digit and capital-D selectors are valid; junk consumes the
        // selector and prints nothing.
        assert_eq!(rp("%d1"), format!("{} ", "-".repeat(20)));
        assert_eq!(rp("%dD"), format!("{} ", "-".repeat(20)));
        assert_eq!(rp("[%dz]"), "[] ");
        // Colour grades with how much is left.
        assert!(render_prompt("%dh", &ctx()).starts_with("<yellow>"));
    }

    #[test]
    fn layout_codes() {
        assert_eq!(rp("a%_b"), "a\r\nb ");
        assert_eq!(rp("a%-b"), "a b ");
        assert_eq!(rp("100%%"), "100% ");
    }

    #[test]
    fn flags_prefix_and_percent_x() {
        let mut c = ctx();
        c.wizinvis = 100;
        c.afk = true;
        // No %x: flags lead on their own line.
        assert_eq!(
            plain(&render_prompt("<%h>", &c)),
            "<wizi 100> <AFK>\r\n<80> "
        );
        // %x: flags inline, no extra line.
        assert_eq!(
            plain(&render_prompt("%x <%h>", &c)),
            "<wizi 100> <AFK> <80> "
        );
        assert_eq!(plain(&render_prompt("%X", &c)), "<wizi 100> <AFK> ");
        // No flags set: %x prints nothing.
        assert_eq!(rp("[%x]"), "[] ");
        // A template with no `%` is printed as-is, without the prefix.
        assert_eq!(plain(&render_prompt("> ", &c)), "> ");
        c.roomvis = true;
        assert!(plain(&render_prompt("%x", &c)).starts_with("<wizi 100> <rmvis> <AFK>"));
    }

    #[test]
    fn unknown_codes_print_nothing() {
        assert_eq!(rp("[%Q]"), "[] ");
        assert_eq!(rp("[%j]"), "[] ");
        // Trailing lone percent is dropped, like legacy.
        assert_eq!(rp("[%h]%"), "[80] ");
    }

    #[test]
    fn missing_state_renders_placeholders() {
        let c = PromptCtx::default();
        assert_eq!(plain(&render_prompt("<%h/%H %v/%V>", &c)), "<?/? ?/?> ");
        assert_eq!(plain(&render_prompt("", &c)), " ");
    }

    #[test]
    fn rs_extension_codes() {
        assert_eq!(rp("%R"), "The Void ");
        assert_eq!(rp("%s %y %Y"), "Winter 07 day ");
        assert_eq!(rp("%B").trim_end().chars().count(), 12);
        assert_eq!(rp("%M").trim_end().chars().count(), 12);
        assert_eq!(rp("%K").trim_end().chars().count(), 12);
        let mut c = ctx();
        c.victim = None;
        assert_eq!(plain(&render_prompt("%K", &c)), "[----------] ");
    }

    #[test]
    fn low_hp_is_colour_graded() {
        let mut c = ctx();
        c.hp = Some(Health { hp: 10, max: 100 });
        assert_eq!(render_prompt("%h", &c), "<red>10</> ");
    }
}
