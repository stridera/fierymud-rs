//! Combat verbs (35 commands). Bodies stay in commands.rs
//! (promoted to pub(crate)); only the Command records + help
//! text live here.

use mud_db::enums::UserRole;

use crate::commands::{
    Category, Command, Help, cmd_assist, cmd_attack, cmd_backstab, cmd_bandage, cmd_bash,
    cmd_berserk, cmd_breathe, cmd_buck, cmd_conceal, cmd_consider, cmd_corner, cmd_disarm,
    cmd_disengage, cmd_doorbash, cmd_drag, cmd_firstaid, cmd_flee, cmd_gouge, cmd_guard,
    cmd_hitall, cmd_kick, cmd_layhands, cmd_lure, cmd_rend, cmd_rescue, cmd_retreat,
    cmd_roar, cmd_roundhouse, cmd_sneak, cmd_springleap, cmd_stomp, cmd_sweep, cmd_tame,
    cmd_throatcut, cmd_tripup,
};

inventory::submit! {
    Command {
    names: &["attack", "kill", "k", "hit", "murder"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "attack <target>",
        summary: "Engage a target in melee combat.",
        long: "Match is by case-insensitive substring on visible names. \
               Targets with combat stats will fight back. Combat \
               resolves once per second on the world tick.",
    },
    run: cmd_attack,
    }
}

inventory::submit! {
    Command {
    names: &["consider", "con"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "consider <target>",
        summary: "Size up a potential opponent.",
        long: "Compares the target's max HP and damage roll to yours \
               and reports a rough difficulty band. Doesn't engage \
               the target — just a flavor read.",
    },
    run: cmd_consider,
    }
}

inventory::submit! {
    Command {
    names: &["flee"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "flee",
        summary: "Run away from combat through a random open exit.",
        long: "Picks an open exit at random and moves you through it. \
               You stop fighting; attackers stop on the next combat \
               tick (they auto-disengage when their target leaves the \
               room).",
    },
    run: cmd_flee,
    }
}

inventory::submit! {
    Command {
    names: &["kick"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "kick",
        summary: "Make an immediate kick attack on your current target.",
        long: "Extra attack outside the normal combat-tick rhythm. \
               Damage = dmg_roll + 4. You must already be fighting \
               someone.",
    },
    run: cmd_kick,
    }
}

inventory::submit! {
    Command {
    names: &["berserk"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "berserk",
        summary: "Self-buff: rage state for 60s.",
        long: "Costs 8 stamina, spawns a `berserk` EffectInstance \
               on yourself for 60s. Refused if already berserk. \
               Combat damage scaling is a follow-up — for now \
               this is the visible buff state.",
    },
    run: cmd_berserk,
    }
}

inventory::submit! {
    Command {
    names: &["tripup", "trip"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "tripup [<target>]",
        summary: "Trip target into Resting posture (lighter than stomp).",
        long: "Costs 5 stamina, deals 1/4 your dmg_roll, sets the \
               target to Resting. Like stomp but cheaper and \
               leaves them slightly less prone.",
    },
    run: cmd_tripup,
    }
}

inventory::submit! {
    Command {
    names: &["sweep"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "sweep",
        summary: "Sweeping kick — knock every standing mob in room prone.",
        long: "Costs 12 stamina. Deals 1/4 dmg_roll to every \
               Standing Mob in the room and sets each to Sitting. \
               Players never targeted.",
    },
    run: cmd_sweep,
    }
}

inventory::submit! {
    Command {
    names: &["roundhouse"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "roundhouse",
        summary: "Powerful kick — 1.5x dmg_roll on your current target.",
        long: "Costs 7 stamina. Heavier kick than the basic `kick` \
               skill (which adds +4); pure dmg_roll multiplier. \
               Requires you to be fighting someone.",
    },
    run: cmd_roundhouse,
    }
}

inventory::submit! {
    Command {
    names: &["stomp"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "stomp [<target>]",
        summary: "Knock the target prone (Sitting posture).",
        long: "Costs 6 stamina, deals half your dmg_roll, sets the \
               target's posture to Sitting. Default target is your \
               current Fighting target. Refused on already-prone \
               targets.",
    },
    run: cmd_stomp,
    }
}

inventory::submit! {
    Command {
    names: &["roar", "howl"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "roar",
        summary: "Intimidate every mob in the room with a fear effect.",
        long: "Costs 8 stamina. Spawns a `fear` EffectInstance on \
               each mob currently in your room (skipping any \
               already feared) for 20s. Doesn't damage anyone, \
               doesn't engage. Players are not targeted.",
    },
    run: cmd_roar,
    }
}

inventory::submit! {
    Command {
    names: &["rend"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "rend [<target>]",
        summary: "Tearing attack — damage plus bleed effect.",
        long: "Costs 7 stamina, deals dmg_roll damage, applies a \
               `bleed` EffectInstance for 30s. Default target is \
               the current Fighting target. Refused if the target \
               is already bleeding.",
    },
    run: cmd_rend,
    }
}

inventory::submit! {
    Command {
    names: &["gouge"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "gouge [<target>]",
        summary: "Eye gouge — damage plus a temporary blind effect.",
        long: "Costs 7 stamina, deals dmg_roll damage, applies a \
               `blind` EffectInstance for 30s. Default target is \
               your current Fighting target. Refused if the target \
               is already blinded.",
    },
    run: cmd_gouge,
    }
}

inventory::submit! {
    Command {
    names: &["springleap"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "springleap <target>",
        summary: "Out-of-combat leaping kick — 1.5x damage opener.",
        long: "Deals 1.5x your dmg_roll on the opening swing and \
               engages the target. Refused if you're already \
               fighting or if the target is already in combat.",
    },
    run: cmd_springleap,
    }
}

inventory::submit! {
    Command {
    names: &["throatcut"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "throatcut <target>",
        summary: "Out-of-combat assassination — 2.5x damage opener.",
        long: "Like backstab but heavier: 2.5x your dmg_roll on \
               the opening swing. Costs 8 stamina. Same engagement \
               rules — refused if you or target are already in \
               combat.",
    },
    run: cmd_throatcut,
    }
}

inventory::submit! {
    Command {
    names: &["backstab", "bs"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "backstab <target>",
        summary: "Surprise opener for double damage; out-of-combat only.",
        long: "Deals 2x your dmg_roll on the opening swing and \
               engages the target. Refused if you're already \
               fighting (the target sees you coming) or if your \
               target is already in combat with someone else.",
    },
    run: cmd_backstab,
    }
}

inventory::submit! {
    Command {
    names: &["hitall", "tantrum"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "hitall",
        summary: "One swing at every hostile mob in your room.",
        long: "Costs 10 stamina. Damages each Mob in the room \
               for half your dmg_roll. Mobs with no Health (test \
               dummy) are skipped. The first surviving mob \
               becomes your Fighting target if you weren't \
               already fighting. Players are never targeted.",
    },
    run: cmd_hitall,
    }
}

inventory::submit! {
    Command {
    names: &["disarm"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "disarm [<target>]",
        summary: "Knock your opponent's weapon to the ground.",
        long: "Removes the target's wielded item; the weapon drops \
               to the floor where any combatant can pick it up. \
               Default target is your current Fighting target. \
               Costs 5 stamina. Refused if the target isn't \
               wielding anything.",
    },
    run: cmd_disarm,
    }
}

inventory::submit! {
    Command {
    names: &["rescue"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "rescue <player>",
        summary: "Take an enemy's aggression onto yourself.",
        long: "Find <player> in your room. Their attacker now \
               targets you instead and you target them. The ally \
               is freed from combat. Costs 6 stamina. Refused if \
               you're already fighting and refused if your ally \
               isn't being attacked.",
    },
    run: cmd_rescue,
    }
}

inventory::submit! {
    Command {
    names: &["guard"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "guard <player|off>",
        summary: "Stand bodyguard — intercept incoming swings on a target.",
        long: "Sets a `Guarding` link from you onto the named \
               player; while you're in the same room, attackers \
               targeting them swing at you instead. `guard off` \
               clears the link. `guard` with no arg reports \
               the current target.",
    },
    run: cmd_guard,
    }
}

inventory::submit! {
    Command {
    names: &["assist"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "assist <player>",
        summary: "Engage your ally's current target.",
        long: "Looks up <player> in your current room, finds whom \
               they're fighting, and engages that target — same \
               stamina cost and rules as `attack`. Refused if \
               they're not fighting, if their target is gone, or \
               if you're already fighting someone else.",
    },
    run: cmd_assist,
    }
}

inventory::submit! {
    Command {
    names: &["layhands", "lay"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "layhands [<target>]",
        summary: "Holy heal — bigger than bandage, works in combat.",
        long: "Heals 30 HP at a cost of 12 stamina. Works while \
               fighting (unlike `bandage`). Refused on full-HP \
               targets. Default target is yourself.",
    },
    run: cmd_layhands,
    }
}

inventory::submit! {
    Command {
    names: &["retreat"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "retreat <direction>",
        summary: "Flee combat in a specific direction.",
        long: "Like `flee` but you choose where to go. Refused if \
               the direction has no exit, the door's closed, or \
               the target room is dangling.",
    },
    run: cmd_retreat,
    }
}


inventory::submit! {
    Command {
    names: &["tame"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "tame <target>",
        summary: "Befriend an animal mob into following you.",
        long: "Drains 4 stamina and dispatches the TAME skill at \
               the named target. The schema's `charmed` status \
               effect spawns on the mob; the runtime also installs \
               `Follower(you)` so existing pet-handling treats it \
               as your follower. Mob charm persists until dismiss \
               or the mob dies — animal-control checks against \
               the will save aren't modeled yet, so v1 always \
               succeeds at the schema-formula amount.",
    },
    run: cmd_tame,
    }
}

inventory::submit! {
    Command {
    names: &["drag"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "drag",
        summary: "Self-apply the DRAG speed penalty.",
        long: "Drains 3 stamina and dispatches the DRAG skill via \
               the data path. The schema's `drag` effect doubles \
               movement stamina cost (speedPenalty 0.5). Legacy \
               `drag <body>` for hauling corpses isn't modeled — \
               we have no corpse mechanic — so v1 is a self-cast \
               that exercises the speed-penalty runtime.",
    },
    run: cmd_drag,
    }
}

inventory::submit! {
    Command {
    names: &["buck"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "buck <target>",
        summary: "Throw a rider — dismount + knockdown.",
        long: "Drains 5 stamina and dispatches the BUCK skill at \
               the named target. The schema's data path runs \
               `dismount` (forced=true) → clears Mounted/RiddenBy, \
               then `knockdown` (duration=1) → drops the target's \
               posture. v1 dispatches as a player skill so \
               characters with BUCK trained (Sorcerer/Druid/etc.) \
               can fire it; mob-AI usage waits for an autonomous \
               ability scheduler.",
    },
    run: cmd_buck,
    }
}

inventory::submit! {
    Command {
    names: &["breathe"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "breathe [<target>]",
        summary: "Dragonborn breath weapon — race-typed.",
        long: "Dispatches one of BREATHE_FIRE / BREATHE_FROST / \
               BREATHE_ACID / BREATHE_GAS / BREATHE_LIGHTNING \
               based on your race (only the DRAGONBORN_* races \
               carry one). Refuses for races with no breath \
               weapon. Drains 6 stamina; the actual damage / \
               target gating runs through the data path.",
    },
    run: cmd_breathe,
    }
}

inventory::submit! {
    Command {
    names: &["lure"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "lure <target>",
        summary: "Bait a mob into engaging you with a stinging hit.",
        long: "Drains 4 stamina and dispatches the LURE skill at \
               the named target. Effect is a level-scaling \
               physical-damage application; combat starts via the \
               normal damage→engage path. Same arg-resolution as \
               `backstab`.",
    },
    run: cmd_lure,
    }
}

inventory::submit! {
    Command {
    names: &["corner"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "corner <target>",
        summary: "Pin a mob with a hard hit to keep them in melee.",
        long: "Drains 4 stamina and dispatches the CORNER skill at \
               the named target. Effect is a level-scaling \
               physical-damage application like LURE; \
               pin-in-place mechanics aren't modeled in the schema, \
               so v1 is the damage hit and the engage.",
    },
    run: cmd_corner,
    }
}

inventory::submit! {
    Command {
    names: &["sneak"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "sneak",
        summary: "Move silently — stealth that survives footsteps.",
        long: "Drains 3 stamina and dispatches the SNEAK skill \
               via the data path. Spawns a `sneak` status effect \
               and installs the Stealth marker (same gate as \
               `hide`). Movement-stealth-break logic isn't wired \
               yet, so sneak is functionally identical to hide \
               until that lands.",
    },
    run: cmd_sneak,
    }
}

inventory::submit! {
    Command {
    names: &["conceal"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "conceal",
        summary: "Magical concealment — improved hiding.",
        long: "Drains 4 stamina and dispatches the CONCEAL skill \
               via the data path. Spawns a `hidden` status effect \
               and installs the Stealth marker. Difference vs. \
               `hide` is in the schema (different proficiency \
               curve, longer duration), not in the runtime path.",
    },
    run: cmd_conceal,
    }
}

inventory::submit! {
    Command {
    names: &["firstaid"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "firstaid [<target>]",
        summary: "Quick self/ally heal — wisdom-scaling.",
        long: "Drains 4 stamina and dispatches the FIRST_AID \
               skill via the data path. Heal amount comes from \
               the schema formula `skill / 4` scaled by wisdom. \
               Defaults to self when no target given. The shim \
               gates `Fighting` since first aid isn't an in-combat \
               action.",
    },
    run: cmd_firstaid,
    }
}

inventory::submit! {
    Command {
    names: &["bandage"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "bandage [<target>]",
        summary: "Apply first aid for a small heal (out of combat).",
        long: "Heals 10 HP at a cost of 4 stamina. With no arg or \
               `me`/`self`, bandages yourself. Otherwise tries to \
               find the target in your room. Refused while fighting \
               and refused on full-HP targets.",
    },
    run: cmd_bandage,
    }
}







inventory::submit! {
    Command {
    names: &["disengage"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "disengage",
        summary: "Stop fighting your current target.",
        long: "Removes your Fighting state — you stop swinging. \
               Opponents may keep attacking until they auto-disengage \
               or you leave the room.",
    },
    run: cmd_disengage,
    }
}

inventory::submit! {
    Command {
    names: &["doorbash"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "doorbash <direction>",
        summary: "Force-open a closed or locked door.",
        long: "Costs 10 stamina. Flips closed/locked exits to \
               Open on both sides — useful when you don't have \
               the key. Refused on already-open exits and when \
               no exit exists in the named direction.",
    },
    run: cmd_doorbash,
    }
}

inventory::submit! {
    Command {
    names: &["bash", "bodyslam", "maul"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "bash <target>",
        summary: "Slam a target, knocking them off their feet.",
        long: "Deals dmg_roll+3 damage and forces the target into a \
               sitting posture. Targets without combat stats simply \
               take the damage.",
    },
    run: cmd_bash,
    }
}











//  `gsay` / `gtell` / `gecho` / `gt` migrated to commands/room_chat.rs.
