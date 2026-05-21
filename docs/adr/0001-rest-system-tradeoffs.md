# 0001: Rest system trade-offs

**Status:** accepted (2026-05-17)

Four design choices in the rest system diverge from MMO defaults. They are recorded here so a future reader (or LLM) doesn't "fix" them by reverting to the obvious-but-wrong shape. Full design lives in [`../design/rest-and-repose.md`](../design/rest-and-repose.md); vocabulary in [`/muditor/CONTEXT.md`](../../../muditor/CONTEXT.md).

## 1. RestSource is consumed on first XP gain, not on login

We chose first-XP-gain (rather than login) so a player who logs in to check mail, sell, or chat does not forfeit a rental they paid for. The cost is a degenerate path where a player never gains XP and never consumes — fine; that player is not progressing anyway. Alternative rejected: WoW-style consume-on-login.

## 2. Repose is sticky forever; no decay, no expiry

The feature exists to remove "penalty for leaving early." A decay mechanic would silently re-introduce that penalty. We accept the unbounded-stick consequence (a player away for a year still has whatever Repose they earned) because the cap already bounds total reward. Alternative rejected: time-based decay of unspent Repose.

## 3. Rent fee is flat per tier, not per hour or per night

A rental is a single fixed charge regardless of how long the player stays offline. Penthouse is 50gp whether the player returns in 1 hour or 1 month. This keeps the player's mental model legible ("I bought a thing") rather than confusing ("how much do I owe?"). Alternative rejected: per-night billing.

## 4. Single tier scalar; no layered Repose pool

`restTier` is a single Int 0-3, not a JSON list of tier-segments. Higher-tier rent overwrites cap and rate; the previous tier's accumulated Repose is preserved (sticky) but the previous tier itself does not persist as a separate accounting layer. Display: "You have N Repose," not "You have 500 deluxe + 1000 standard." Alternative rejected: WoW-style layered tiers where premium-tier Repose spends first at a higher multiplier.
