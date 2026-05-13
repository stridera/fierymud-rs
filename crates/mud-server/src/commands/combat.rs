//! Combat verbs (35 commands). Both Command records and handler
//! bodies live here.

use bevy_ecs::prelude::*;
use mud_db::enums::{ExitState, UserRole};
use mud_world::{
    CombatStats, EquippedSlot, Exits, Fighting, Health, Item, Located, Mob, Named, Posture,
    PostureKind, Profile, Slot,
};

use crate::commands::{
    ATTACK_COST, AoeScope, BACKSTAB_COST, BANDAGE_COST, BASH_COST, BERSERK_COST, Category, Command,
    DISARM_COST, DOORBASH_COST, GOUGE_COST, HITALL_COST, Help, KICK_COST, LAYHANDS_COST, REND_COST,
    RESCUE_COST, ROAR_COST, ROUNDHOUSE_COST, SPRINGLEAP_COST, STOMP_COST, SWEEP_COST, THROATCUT_COST,
    TRIPUP_COST, aggro_alignment, apply_damage, auto_assist_followers_of, skill_stamina_cost,
    broadcast_room_except_players_rendered, broadcast_room_except_rendered, check_stamina,
    cmd_look, consider_verdict_color, direction_name, drain_stamina, engage_skill_shim,
    find_actor_in_room, flip_door_both_sides, hit_chance_color, invoke_ability, invoke_ability_aoe,
    mob_helpers_engage, name_of, name_or, opposite, parse_direction,
    remove_effect_named, require_alert_posture, send_rendered, send_to, try_insert, try_remove,
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
    names: &["claw"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "claw <target>",
        summary: "Slash with bestial claws (Druid / Shaman).",
        long: "Class-gated to Druid or Shaman. Counts as a violent \
               opening — engages the target if you're not already \
               fighting them. Random damage scaled by your level.",
    },
    run: cmd_claw,
    }
}

inventory::submit! {
    Command {
    names: &["peck"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "peck <target>",
        summary: "Drive your beak into a target (Avariel only).",
        long: "Race-gated to Avariel. Piercing strike — engages \
               combat if not already fighting the target. Damage \
               scales with level.",
    },
    run: cmd_peck,
    }
}

inventory::submit! {
    Command {
    names: &["electrify"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "electrify <target>",
        summary: "Channel lightning into a target (mage classes).",
        long: "Class-gated to Sorcerer / Necromancer / Conjurer / \
               Diabolist. Electric strike — engages combat. Damage \
               scales with level.",
    },
    run: cmd_electrify,
    }
}

inventory::submit! {
    Command {
    names: &["steal"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "steal <item|coins> <target>",
        summary: "Pickpocket from a target.",
        long: "Class-gated to Thief or Assassin. Refused while \
               fighting, against yourself, against a Shopkeeper, \
               and against staff. On failure the target notices \
               and re-aggros on you. Pass `coins` / `gold` to \
               grab a chunk of their coin instead of an item.",
    },
    run: cmd_steal,
    }
}

inventory::submit! {
    Command {
    names: &["gretreat"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Group,
    help: Help {
        usage: "gretreat",
        summary: "Coordinated group retreat — every group member in your room flees together.",
        long: "Picks one open exit at random; every group member in \
               your current room moves through it and drops their \
               Fighting state. Refused if you're not grouped or if \
               the room has no open exits.",
    },
    run: cmd_gretreat,
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
    names: &["rescue", "res"],
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


// ---- handler bodies ----

pub(crate) fn cmd_doorbash(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "doorbash") {
        return;
    }
    let cost = skill_stamina_cost(world, "doorbash", DOORBASH_COST);
    if !check_stamina(world, player, cost, "doorbash") {
        return;
    }
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Doorbash which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let cur_state = world
        .get::<Exits>(room)
        .and_then(|e| e.0.get(&dir).map(|ed| ed.state));
    let Some(state) = cur_state else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    if state == ExitState::Open {
        send_to(world, player, format!("It's already open {}.\r\n", direction_name(dir)));
        return;
    }
    drain_stamina(world, player, cost);
    flip_door_both_sides(world, room, dir, ExitState::Open);

    let player_name = name_of(world, player);
    send_to(world, player, format!(
        "You bash open the way {}!\r\n",
        direction_name(dir),
    ));
    broadcast_room_except_players_rendered(
        world,
        room,
        &[player],
        &format!("{player_name} bashes the door {} wide open!\r\n", direction_name(dir)),
    );
}
pub(crate) fn cmd_attack(world: &mut World, player: Entity, target_name: &str) {
    if !require_alert_posture(world, player, "attack") {
        return;
    }
    let cost = skill_stamina_cost(world, "attack", ATTACK_COST);
    if !check_stamina(world, player, cost, "attack") {
        return;
    }
    let target_name = target_name.trim();
    if target_name.is_empty() {
        send_to(world, player, "Attack what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target_lower = target_name.to_ascii_lowercase();

    let target = {
        let mut q = world.query::<(Entity, &Located, &Named)>();
        q.iter(world)
            .find(|(e, l, n)| {
                *e != player
                    && l.0 == located.0
                    && n.name.to_ascii_lowercase().contains(&target_lower)
            })
            .map(|(e, _, _)| e)
    };

    let Some(target) = target else {
        send_rendered(world, player, &format!("You don't see '{target_name}' here.\r\n"),
        );
        return;
    };

    // PeacefulRoom gate — `Room.is_peaceful` marks sanctuaries /
    // shop interiors / quest hubs where combat is refused outright.
    // Catches both PvP and PvE engage attempts before any state
    // mutates (no Fighting set, no stamina drained).
    if world.get::<mud_world::PeacefulRoom>(located.0).is_some() {
        send_to(
            world,
            player,
            "A peaceful aura fills this place — violence simply won't happen here.\r\n",
        );
        return;
    }

    // Peaceful mob gate — `MobBehavior::Peaceful` mobs refuse to be
    // attacked. Mirrors the legacy aura that quest-givers and
    // shopkeepers tend to have so a misclick doesn't aggro a
    // critical NPC. Doesn't apply to PvP — players never carry
    // MobBehaviors and don't get covered.
    if world
        .get::<mud_world::MobBehaviors>(target)
        .is_some_and(|b| b.has(mud_db::enums::MobBehavior::Peaceful))
    {
        let target_name_owned = name_of(world, target);
        send_to(
            world,
            player,
            format!("{target_name_owned} radiates a calm that turns your blow aside.\r\n"),
        );
        return;
    }

    let actual_name = name_of(world, target);
    let player_name = name_of(world, player);

    try_insert(world, player, Fighting(target));
    // First-attacker priority: don't steal aggro from whoever's
    // already engaged with this target. Players joining a tanked
    // fight push to the hate list (via apply_swing) but the active
    // Fighting target stays the original puller. `rescue` is the
    // explicit aggro-redirect path.
    if world.get::<Fighting>(target).is_none()
        && world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }
    drain_stamina(world, player, cost);

    send_to(world, player, format!("You attack {actual_name}!\r\n"));
    send_rendered(world, target, &format!("{player_name} attacks you!\r\n"));
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} attacks {actual_name}.\r\n"),
    );

    // Auto-assist: anyone following `target` with AUTO_ASSIST set, in
    // the same room, not already fighting — they engage `player`.
    auto_assist_followers_of(world, target, player, located.0);

    // Mob HELPER behavior: any mob in the room (other than the
    // attacker / defender) with the `Helper` flag joins in and
    // engages the attacker. Same room-mismatch auto-disengage as
    // any other combat enrollment if the attacker leaves.
    mob_helpers_engage(world, target, player, located.0);

    // Fire ATTACK trigger on the target. Bodies typically run
    // initial-aggression flavor or counter-attacks. `self` = target,
    // `actor` = attacker.
    crate::triggers::fire_event_with_actor(
        world,
        target,
        player,
        mud_world::TriggerEvent::Attack,
    );
}
pub(crate) fn cmd_consider(world: &mut World, player: Entity, target_word: &str) {
    let target_word = target_word.trim();
    if target_word.is_empty() {
        send_to(world, player, "Consider whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_rendered(world, player, &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let target_name = name_of(world, target);

    let self_max_hp = world.get::<Health>(player).map_or(1, |h| h.max).max(1);
    let self_stats = world.get::<CombatStats>(player).copied();
    // attack_power feeds the consider verdict in place of the
    // legacy dmg_roll. Same intent: "how hard does this side hit?"
    let self_dmg = self_stats.map_or(0, |c| c.attack_power);
    let self_hit_roll = self_stats.map_or(0, |c| c.accuracy);
    let target_max_hp = world.get::<Health>(target).map_or(0, |h| h.max);
    let target_stats = world.get::<CombatStats>(target).copied();
    let target_dmg = target_stats.map_or(0, |c| c.attack_power);
    let target_hit_roll = target_stats.map_or(0, |c| c.accuracy);
    let self_ac = self_stats.map_or(0, |c| c.armor_pct);
    let target_ac = target_stats.map_or(0, |c| c.armor_pct);

    if target_max_hp == 0 {
        send_rendered(world, player, &format!("{target_name} doesn't look like a fighter at all.\r\n"),
        );
        return;
    }

    // Score = max_hp scaled by damage output (1 + dmg/10). Compare ratio to
    // self. The cutoffs are chosen by feel — easy to retune later.
    let self_score = f64::from(self_max_hp) * (1.0 + f64::from(self_dmg) / 10.0);
    let target_score = f64::from(target_max_hp) * (1.0 + f64::from(target_dmg) / 10.0);
    let ratio = target_score / self_score.max(1.0);

    let verdict = if ratio < 0.30 {
        "is no match for you."
    } else if ratio < 0.70 {
        "looks like an easy fight."
    } else if ratio < 1.50 {
        "might give you a fight."
    } else if ratio < 3.00 {
        "looks tougher than you."
    } else {
        "would slaughter you. Don't try it."
    };

    // Verdict line: target name bold-cyan as the focal subject;
    // verdict colored by the same ratio cutoff used to pick the
    // text, so a "would slaughter you" reads bold-red without the
    // player having to parse the prose.
    let verdict_open = consider_verdict_color(ratio);
    let mut out = format!(
        "<b:cyan>{target_name}</> {verdict_open}{verdict}</>\r\n"
    );
    // Hit-chance hints both ways. Use the same formula combat does
    // so what `consider` predicts matches what swings actually land.
    // Each percentage is graded green→red so a player can compare
    // their swing chance vs the target's at a glance.
    let your_chance = crate::combat::hit_chance_pct(self_hit_roll, target_ac);
    let their_chance = crate::combat::hit_chance_pct(target_hit_roll, self_ac);
    let your_pct_text =
        hit_chance_color(your_chance).map_or(format!("{your_chance}%"), |open| {
            format!("{open}{your_chance}%</>")
        });
    // Their chance flips the gradient — a high percentage means
    // *they* land swings reliably (bad for the player), so wrap in
    // red bands. Reuse the helper but invert: pct >= 65 → red, low
    // pct → green from the player's perspective.
    let their_pct_text = match their_chance {
        i32::MIN..=14 => format!("<b:green>{their_chance}%</>"),
        15..=34 => format!("<green>{their_chance}%</>"),
        35..=64 => format!("{their_chance}%"),
        65..=84 => format!("<red>{their_chance}%</>"),
        _ => format!("<b:red>{their_chance}%</>"),
    };
    out.push_str(&format!(
        "Your strikes would land about {your_pct_text}; theirs about {their_pct_text}.\r\n",
    ));
    // Aggro hint: same threshold the room-entry check uses, so
    // `consider` matches the auto-engage rule. Players passing
    // through a known-hostile zone can size up the danger before
    // walking back in. Memory check first — a remembered grudge
    // is the more specific reason a particular target would
    // attack you. Both reads as a bold-red threat tag — distinct
    // from the verdict hue so the alarm doesn't blend into the
    // gradient.
    if world.get::<Mob>(target).is_some() {
        let remembers_you = world
            .get::<crate::combat::MobMemory>(target)
            .is_some_and(|m| m.0.contains(&player));
        let target_alignment = target_stats.map_or(0, |c| c.alignment);
        if remembers_you {
            out.push_str(
                "<b:red>It remembers you, and its hand goes to its weapon.</>\r\n",
            );
        } else if target_alignment <= aggro_alignment(world) {
            out.push_str(
                "<b:red>Its eyes follow you with malice — it would attack on sight.</>\r\n",
            );
        }
    }
    // PeacefulRoom hint — if this room won't let combat happen,
    // the verdict is moot. Surfaced last so the rest of the
    // analysis still renders (useful when debugging encounters).
    // Cyan because it's calming reassurance, not a threat.
    if world.get::<mud_world::PeacefulRoom>(located.0).is_some() {
        out.push_str(
            "<cyan>But a peaceful aura fills this place — violence won't happen here.</>\r\n",
        );
    }
    send_rendered(world, player, &out);
}
/// Class IDs that can `steal`. Thief = 3, Assassin = 10 in the
/// seeded Class catalog (verified against fierydev). A
/// "rogue-skill" tag on the class would be the cleaner long-term
/// shape so subclassing doesn't have to chase the list.
const STEAL_CLASS_IDS: &[i32] = &[3, 10];
/// Druid (8) / Shaman (9) for `claw`.
const CLAW_CLASS_IDS: &[i32] = &[8, 9];
/// Mage-family classes for `electrify`: Sorcerer (1), Necromancer
/// (12), Conjurer (13), Diabolist (17).
const ELECTRIFY_CLASS_IDS: &[i32] = &[1, 12, 13, 17];

/// Body shared by the simple class-skill strikes (claw / peck /
/// electrify). Verifies the class/race gate, finds a target,
/// rolls damage, applies it, engages combat. The specifics
/// (`skill_name`, `verb_self`, `verb_other`, damage band) live in
/// the per-command call site.
fn perform_class_strike(
    world: &mut World,
    player: Entity,
    args: &str,
    skill_name: &str,
    verb_self: &str,
    verb_other: &str,
) {
    let arg = args.trim();
    let target_word = if arg.is_empty() {
        // No arg → attack current combat target if any.
        let Some(f) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, format!("{} whom?\r\n", crate::commands::capitalize(skill_name)));
            return;
        };
        let Some(loc) = world.get::<Located>(player).map(|l| l.0) else {
            return;
        };
        let _ = loc;
        // Fall through with the current target's name resolved
        // through the existing find path so the rest of the body
        // is uniform.
        crate::commands::name_of(world, f.0)
    } else {
        arg.to_string()
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target = find_actor_in_room(world, &target_word, located.0, player);
    let Some(target) = target else {
        send_to(world, player, format!("No '{target_word}' here.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Ouch, that would hurt.\r\n");
        return;
    }
    let player_level = world.get::<Profile>(player).map_or(1, |p| p.level);
    // Damage band: low = level + 5, high = level + 20. A 95% skill
    // success rate at all levels keeps the kit feeling reliable;
    // refining with an actual skill stat is a follow-up.
    let dam = if rand::random_range(0..100) < 95 {
        rand::random_range(player_level + 5..=player_level + 20)
    } else {
        0
    };
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);
    if dam == 0 {
        send_rendered(
            world,
            player,
            &format!("Your {skill_name} misses {target_name}.\r\n"),
        );
        send_rendered(
            world,
            target,
            &format!("{player_name}'s {skill_name} misses you.\r\n"),
        );
    } else {
        send_rendered(
            world,
            player,
            &format!("You {verb_self} {target_name} for {dam} damage.\r\n"),
        );
        send_rendered(
            world,
            target,
            &format!("{player_name} {verb_other} you for {dam} damage!\r\n"),
        );
        let (dead, _msg) = apply_damage(world, target, dam);
        if dead
            && let Some(loc) = world.get::<Located>(target).copied()
        {
            crate::combat::handle_death(world, target, &target_name, loc.0);
            return;
        }
    }
    // Engage combat if not already.
    if world.get::<Fighting>(player).is_none() {
        try_insert(world, player, Fighting(target));
    }
    if world.get::<Fighting>(target).is_none() {
        try_insert(world, target, Fighting(player));
        if world.get::<Mob>(target).is_some() {
            crate::combat::remember_attacker(world, target, player);
        }
    }
}

pub(crate) fn cmd_claw(world: &mut World, player: Entity, args: &str) {
    let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
    if !class_id.is_some_and(|id| CLAW_CLASS_IDS.contains(&id)) {
        send_to(
            world,
            player,
            "Grow some longer fingernails first.\r\n",
        );
        return;
    }
    perform_class_strike(world, player, args, "claw", "rake", "rakes");
}

pub(crate) fn cmd_peck(world: &mut World, player: Entity, args: &str) {
    // Avariel race only. Race is stored as a lower-case string on
    // Profile; substring-match in case the race system adds
    // sub-races / morph forms later.
    let race = world.get::<Profile>(player).map(|p| p.race.clone());
    if !race.is_some_and(|r| r.to_ascii_lowercase().contains("avariel")) {
        send_to(world, player, "How do you expect to do that?\r\n");
        return;
    }
    perform_class_strike(world, player, args, "peck", "peck", "pecks");
}

pub(crate) fn cmd_electrify(world: &mut World, player: Entity, args: &str) {
    let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
    if !class_id.is_some_and(|id| ELECTRIFY_CLASS_IDS.contains(&id)) {
        send_to(
            world,
            player,
            "You haven't the arcane training for that.\r\n",
        );
        return;
    }
    perform_class_strike(world, player, args, "lightning", "electrify", "electrifies");
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_steal(world: &mut World, player: Entity, args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't steal while fighting.\r\n");
        return;
    }
    let class_id = world.get::<Profile>(player).and_then(|p| p.class_id);
    if !class_id.is_some_and(|id| STEAL_CLASS_IDS.contains(&id)) {
        send_to(world, player, "You don't know how to steal.\r\n");
        return;
    }

    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        send_to(world, player, "Usage: steal <item|coins> <target>\r\n");
        return;
    }
    let what = parts[0].trim();
    let who = parts[1..].join(" ");
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let Some(target) = find_actor_in_room(world, &who, room, player) else {
        send_to(world, player, format!("No '{who}' here.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "Stealing from yourself is rather stupid.\r\n");
        return;
    }
    // Refuse against staff, shopkeepers, and the room's
    // PeacefulRoom marker.
    let target_role = world.get::<mud_world::Account>(target).map(|a| a.role);
    if target_role.is_some_and(|r| r.at_least(mud_db::enums::UserRole::Builder)) {
        send_to(world, player, "You can't steal from staff.\r\n");
        return;
    }
    if world.get::<mud_world::Shopkeeper>(target).is_some() {
        send_to(
            world,
            player,
            "Shopkeepers keep their coin a little too well guarded.\r\n",
        );
        return;
    }
    if world.get::<mud_world::PeacefulRoom>(room).is_some() {
        send_to(
            world,
            player,
            "A peaceful aura wards off such attempts here.\r\n",
        );
        return;
    }

    // Simplified skill check: 50% base, +5% per level above 1, -25%
    // if the target's awake. Future polish: dex bonus, target
    // alertness, weight modifier on items. Floor 5%, cap 95%.
    let player_level = world.get::<Profile>(player).map_or(1, |p| p.level);
    let awake = world
        .get::<Posture>(target)
        .is_none_or(|p| !matches!(p.0, PostureKind::Sleeping));
    let chance: i32 = {
        let base = 50 + (player_level - 1) * 5;
        let after_awake = if awake { base - 25 } else { base };
        after_awake.clamp(5, 95)
    };
    let roll = rand::random_range(1..=100);
    let success = roll <= chance;
    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    if !success {
        send_to(world, player, "Oops...\r\n");
        send_rendered(
            world,
            target,
            &format!("<b:yellow>{player_name} tried to steal something from you!</>\r\n"),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[player, target],
            &format!("<b:yellow>{player_name} tries to steal from {target_name}.</>\r\n"),
        );
        // Caught — make the target aggro the thief. For mobs, push
        // onto the HateList + MobMemory so they re-engage later.
        // For PvP, just install Fighting.
        if world.get::<Mob>(target).is_some() {
            crate::combat::remember_attacker(world, target, player);
        }
        try_insert(world, target, Fighting(player));
        return;
    }

    // Success path: coin or item.
    if what.eq_ignore_ascii_case("coins") || what.eq_ignore_ascii_case("gold") {
        // Grab roughly 1/4 of the target's wealth, capped at level*100 cp.
        let pool = world.get::<mud_world::Wealth>(target).map_or(0, |w| w.0);
        let take = (pool / 4).min(i64::from(player_level) * 100).max(0);
        if take == 0 {
            send_to(world, player, format!("{target_name} has no coin worth lifting.\r\n"));
            return;
        }
        if let Some(mut w) = world.get_mut::<mud_world::Wealth>(target) {
            w.0 = w.0.saturating_sub(take);
        }
        if let Some(mut w) = world.get_mut::<mud_world::Wealth>(player) {
            w.0 = w.0.saturating_add(take);
        } else if let Ok(mut em) = world.get_entity_mut(player) {
            em.insert(mud_world::Wealth(take));
        }
        let coin = crate::commands::format_wealth(take).unwrap_or_else(|| "no coin".to_string());
        send_rendered(
            world,
            player,
            &format!("You lift {coin} from {target_name}.\r\n"),
        );
        return;
    }

    // Item path: find a carried (non-equipped) item by keyword.
    let needle = what.to_ascii_lowercase();
    let item_opt: Option<(Entity, String)> = {
        let mut q = world
            .query_filtered::<(Entity, &mud_world::Located, &Named, Option<&mud_world::Keywords>, Option<&mud_world::EquippedSlot>), With<Item>>();
        q.iter(world)
            .find(|(_, l, n, kw, eq)| {
                l.0 == target
                    && eq.is_none()
                    && crate::commands::matches(&needle, n, *kw)
            })
            .map(|(e, _, n, _, _)| (e, n.name.clone()))
    };
    let Some((item, item_name)) = item_opt else {
        send_rendered(
            world,
            player,
            &format!("{target_name} hasn't got '{what}' on them.\r\n"),
        );
        return;
    };
    if let Some(mut l) = world.get_mut::<mud_world::Located>(item) {
        l.0 = player;
    }
    send_rendered(
        world,
        player,
        &format!("You quietly pluck {item_name} from {target_name}.\r\n"),
    );
}

pub(crate) fn cmd_gretreat(world: &mut World, player: Entity, _args: &str) {
    use crate::commands::{cap_sentence_start, group_members, group_root};
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;

    // Group members in the same room. Solo callers get refused —
    // they can use plain `flee` instead.
    let root = group_root(world, player);
    let same_room: Vec<Entity> = group_members(world, root)
        .into_iter()
        .filter(|m| world.get::<Located>(*m).is_some_and(|l| l.0 == from_room))
        .collect();
    if same_room.len() <= 1 {
        send_to(
            world,
            player,
            "You're not grouped with anyone here — try `flee` solo.\r\n",
        );
        return;
    }

    let candidates: Vec<(mud_db::enums::Direction, Entity)> = world
        .get::<Exits>(from_room)
        .map(|e| {
            e.0.iter()
                .filter_map(|(dir, ed)| {
                    if ed.state == ExitState::Open {
                        ed.to.map(|t| (*dir, t))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if candidates.is_empty() {
        send_to(world, player, "There's nowhere to run!\r\n");
        return;
    }
    let pick = rand::random_range(0..candidates.len());
    let (dir, target) = candidates[pick];
    let dir_name = direction_name(dir);

    // Source-room broadcast: announce all retreating members at
    // once before the moves so onlookers see one line per fleer.
    for m in &same_room {
        let name = name_of(world, *m);
        let capped = cap_sentence_start(&name);
        broadcast_room_except_players_rendered(
            world,
            from_room,
            &same_room,
            &format!("{capped} retreats with the group {dir_name}!\r\n"),
        );
        // Each retreating member drops their own Fighting; the
        // combat tick auto-disengages attackers on next pass.
        try_remove::<Fighting>(world, *m);
        if let Some(mut l) = world.get_mut::<Located>(*m) {
            l.0 = target;
        }
    }
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    for m in &same_room {
        let name = name_of(world, *m);
        let capped = cap_sentence_start(&name);
        broadcast_room_except_players_rendered(
            world,
            target,
            &same_room,
            &format!("{capped} arrives, panting, from {arrival_dir}.\r\n"),
        );
        send_to(world, *m, format!("You retreat with the group {dir_name}!\r\n"));
        cmd_look(world, *m, "");
    }
}

pub(crate) fn cmd_flee(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;

    // Collect open exits with valid targets.
    let candidates: Vec<(mud_db::enums::Direction, Entity)> = world
        .get::<Exits>(from_room)
        .map(|e| {
            e.0.iter()
                .filter_map(|(dir, ed)| {
                    if ed.state == mud_db::enums::ExitState::Open {
                        ed.to.map(|t| (*dir, t))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    if candidates.is_empty() {
        send_to(world, player, "There's nowhere to run!\r\n");
        return;
    }

    let pick = rand::random_range(0..candidates.len());
    let (dir, target) = candidates[pick];
    let dir_name = direction_name(dir);

    let mover_name = name_of(world, player);
    let mover_capped = crate::commands::cap_sentence_start(&mover_name);

    // Notify the source room you're fleeing.
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_capped} panics and flees {dir_name}!\r\n"),
    );

    // Drop our own Fighting; combat_tick auto-disengages attackers on
    // the next 1Hz pass via the room-mismatch check.
    try_remove::<Fighting>(world, player);

    // Move + announce arrival + auto-look.
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    broadcast_room_except_players_rendered(
        world,
        target,
        &[player],
        &format!("{mover_capped} arrives, panting, from {arrival_dir}.\r\n"),
    );
    send_to(world, player, format!("You flee {dir_name}!\r\n"));
    cmd_look(world, player, "");
}
pub(crate) fn cmd_kick(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "kick") {
        return;
    }
    let Some(fighting) = world.get::<Fighting>(player).copied() else {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    };
    let target = fighting.0;
    if world.get_entity(target).is_err() {
        try_remove::<Fighting>(world, player);
        send_to(world, player, "Your target is gone.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "kick", KICK_COST);
    if !check_stamina(world, player, cost, "kick") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("kick {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_berserk(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "berserk") {
        return;
    }
    let cost = skill_stamina_cost(world, "berserk", BERSERK_COST);
    if !check_stamina(world, player, cost, "berserk") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        "berserk",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_stomp(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "stomp") {
        return;
    }
    let cost = skill_stamina_cost(world, "stomp", STOMP_COST);
    if !check_stamina(world, player, cost, "stomp") {
        return;
    }
    let arg = args.trim();
    let target = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Stomp whom? You aren't fighting.\r\n");
            return;
        };
        t
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if target == player {
        send_to(world, player, "You can't stomp yourself.\r\n");
        return;
    }
    let cur_posture = world.get::<Posture>(target).map(|p| p.0);
    if !matches!(cur_posture, Some(PostureKind::Standing)) {
        let target_name = name_or(world, target, "(unknown)");
        send_to(world, player, format!(
            "{target_name} is already on the ground.\r\n",
        ));
        return;
    }
    let Some(target_room) = world.get::<Located>(target).copied().map(|l| l.0) else {
        send_to(world, player, "Target is in limbo.\r\n");
        return;
    };

    // Skill base damage scales with the attacker's attack_power.
    // Pre-pivot this read `dmg_roll / 2`; in the new model
    // attack_power is a +%, so we recover an effective dmg_roll
    // as `attack_power / 5` (the inverse of the migration's
    // `damage_roll * 5 = attack_power` mapping).
    let dmg = world
        .get::<CombatStats>(player)
        .map_or(1, |c| ((c.attack_power / 5) / 2).max(1));
    drain_stamina(world, player, cost);

    let player_name = name_of(world, player);
    let target_name = name_or(world, target, "(unknown)");
    let (dead, _) = apply_damage(world, target, dmg);

    if !dead
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Posture(PostureKind::Sitting));
    }

    send_to(world, player, format!(
        "You stomp on {target_name} for {dmg} damage; they go down!\r\n"
    ));
    if !dead {
        send_rendered(world, target, &format!(
            "{player_name} stomps you to the ground!\r\n"
        ));
    }
    broadcast_room_except_rendered(
        world,
        target_room,
        &[player, target],
        &format!("{player_name} stomps {target_name} to the ground!\r\n"),
    );

    if dead {
        crate::combat::handle_death(world, target, &target_name, target_room);
    }
}
pub(crate) fn cmd_tripup(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "tripup") {
        return;
    }
    let arg = args.trim();
    // Empty-arg shortcut: current Fighting target. The data path
    // doesn't synthesize this; we resolve it here and pass the name
    // through.
    let dispatched = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Trip up whom? You aren't fighting.\r\n");
            return;
        };
        let target_name = name_of(world, t);
        format!("trip_up {target_name}")
    } else if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        // Targeting gate would also catch this, but refusing here
        // skips wasted stamina.
        send_to(world, player, "You can't trip yourself.\r\n");
        return;
    } else {
        format!("trip_up {arg}")
    };
    let cost = skill_stamina_cost(world, "tripup", TRIPUP_COST);
    if !check_stamina(world, player, cost, "tripup") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_sweep(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "sweep") {
        return;
    }
    let cost = skill_stamina_cost(world, "sweep", SWEEP_COST);
    if !check_stamina(world, player, cost, "sweep") {
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let room = located.0;
    let dmg = world
        .get::<CombatStats>(player)
        .map_or(1, |c| ((c.attack_power / 5) / 4).max(1));
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&Posture>, Option<&Health>), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, p, h)| {
                l.0 == room
                    && h.is_some()
                    && matches!(p.map(|p| p.0), None | Some(PostureKind::Standing))
            })
            .map(|(e, _, _, _)| e)
            .collect()
    };
    if targets.is_empty() {
        send_to(world, player, "Nothing here to sweep.\r\n");
        return;
    }
    drain_stamina(world, player, cost);
    let player_name = name_of(world, player);
    let count = targets.len();
    for t in targets {
        let target_name = name_or(world, t, "(unknown)");
        let (dead, _) = apply_damage(world, t, dmg);
        if dead {
            crate::combat::handle_death(world, t, &target_name, room);
        } else if let Ok(mut e) = world.get_entity_mut(t) {
            e.insert(Posture(PostureKind::Sitting));
        }
    }
    send_to(world, player, format!(
        "You sweep your leg in a wide arc — {count} go down!\r\n"
    ));
    broadcast_room_except_rendered(
        world, room, &[player],
        &format!("{player_name} sweeps a wide kick across the room!\r\n"),
    );
}
pub(crate) fn cmd_roundhouse(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "roundhouse") {
        return;
    }
    let Some(Fighting(target)) = world.get::<Fighting>(player).copied() else {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    };
    if world.get_entity(target).is_err() {
        try_remove::<Fighting>(world, player);
        send_to(world, player, "Your target is gone.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "roundhouse", ROUNDHOUSE_COST);
    if !check_stamina(world, player, cost, "roundhouse") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("roundhouse {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_roar(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "roar") {
        return;
    }
    let cost = skill_stamina_cost(world, "roar", ROAR_COST);
    if !check_stamina(world, player, cost, "roar") {
        return;
    }
    drain_stamina(world, player, cost);
    // RoomEnemies scope handles per-ability target expansion (every
    // mob in the room minus group members) plus per-target
    // dispatch with the first call carrying the description box and
    // the rest using `aoe_repeat = true`. Already-feared targets
    // get re-applied — `fear` effect-type stacks duration which is
    // the right behavior for a player roaring repeatedly.
    invoke_ability_aoe(
        world,
        player,
        mud_db::abilities::AbilityKind::Skill,
        "use",
        "roar",
        AoeScope::RoomEnemies,
        "There's nothing here to roar at.\r\n",
    );
}
pub(crate) fn cmd_rend(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "rend") {
        return;
    }
    let arg = args.trim();
    let target_word = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Rend whom? You aren't fighting.\r\n");
            return;
        };
        name_of(world, t)
    } else {
        arg.to_string()
    };
    let cost = skill_stamina_cost(world, "rend", REND_COST);
    if !check_stamina(world, player, cost, "rend") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        &format!("rend {target_word}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_gouge(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "gouge") {
        return;
    }
    let arg = args.trim();
    let target_word = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Gouge whom? You aren't fighting.\r\n");
            return;
        };
        name_of(world, t)
    } else {
        arg.to_string()
    };
    let cost = skill_stamina_cost(world, "gouge", GOUGE_COST);
    if !check_stamina(world, player, cost, "gouge") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        &format!("eye_gouge {target_word}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_springleap(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "springleap") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't springleap while already fighting.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Springleap whom?\r\n");
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't springleap yourself.\r\n");
        return;
    }
    // Resolve the target up front so we can read its Fighting and
    // know the entity for the post-dispatch auto-engage.
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "They're already fighting; no surprise.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "springleap", SPRINGLEAP_COST);
    if !check_stamina(world, player, cost, "springleap") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("springleap {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    // Auto-engage if the target survived. The data path doesn't model
    // engagement; springleap's gameplay contract is "open combat with
    // a leap kick".
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        // First-attacker priority: don't steal aggro (see cmd_attack
        // for full reasoning). `rescue` is the explicit redirect.
        if world.get::<Fighting>(target).is_none()
            && world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}
pub(crate) fn cmd_throatcut(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "throatcut") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "Your target is already aware of you.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Throatcut whom?\r\n");
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't throatcut yourself.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "They're too alert.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "throatcut", THROATCUT_COST);
    if !check_stamina(world, player, cost, "throatcut") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("throatcut {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        // First-attacker priority: don't steal aggro (see cmd_attack
        // for full reasoning). `rescue` is the explicit redirect.
        if world.get::<Fighting>(target).is_none()
            && world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}
pub(crate) fn cmd_backstab(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "backstab") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "Your target is already aware of you.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Backstab whom?\r\n");
        return;
    }
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't backstab yourself.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Fighting>(target).is_some() {
        send_to(world, player, "They're too alert to backstab.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "backstab", BACKSTAB_COST);
    if !check_stamina(world, player, cost, "backstab") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("backstab {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
    if world.get_entity(target).is_ok() {
        try_insert(world, player, Fighting(target));
        // First-attacker priority: don't steal aggro (see cmd_attack
        // for full reasoning). `rescue` is the explicit redirect.
        if world.get::<Fighting>(target).is_none()
            && world.get::<CombatStats>(target).is_some()
            && let Ok(mut e) = world.get_entity_mut(target)
        {
            e.insert(Fighting(player));
        }
    }
}
pub(crate) fn cmd_hitall(world: &mut World, player: Entity, _args: &str) {
    if !require_alert_posture(world, player, "hitall") {
        return;
    }
    let cost = skill_stamina_cost(world, "hitall", HITALL_COST);
    if !check_stamina(world, player, cost, "hitall") {
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;

    let dmg = world
        .get::<CombatStats>(player)
        .map_or(1, |c| ((c.attack_power / 5) / 2).max(1));
    let mob_targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, Option<&Health>), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, h)| l.0 == room && h.is_some())
            .map(|(e, _, _)| e)
            .collect()
    };
    if mob_targets.is_empty() {
        send_to(world, player, "Nothing here to swing at.\r\n");
        return;
    }
    drain_stamina(world, player, cost);

    let player_name = name_of(world, player);
    let already_fighting = world.get::<Fighting>(player).is_some();
    let mut first_alive: Option<Entity> = None;
    let mut hits: Vec<(String, bool)> = Vec::with_capacity(mob_targets.len());
    for target in &mob_targets {
        let target_name = name_or(world, *target, "(unknown)");
        let (dead, _msg) = apply_damage(world, *target, dmg);
        hits.push((target_name.clone(), dead));
        if dead {
            crate::combat::handle_death(world, *target, &target_name, room);
        } else if first_alive.is_none() {
            first_alive = Some(*target);
        }
    }

    // Engage the first survivor if we weren't already fighting.
    if !already_fighting
        && let Some(first) = first_alive
    {
        try_insert(world, player, Fighting(first));
        // First-attacker priority (see cmd_attack).
        if world.get::<Fighting>(first).is_none()
            && world.get::<CombatStats>(first).is_some()
            && let Ok(mut e) = world.get_entity_mut(first)
        {
            e.insert(Fighting(player));
        }
    }

    let total_hits = hits.len();
    let kills = hits.iter().filter(|(_, dead)| *dead).count();
    send_to(
        world,
        player,
        format!(
            "You swing wildly: {total_hits} hit(s), {kills} kill(s) for {dmg} damage each.\r\n",
        ),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
        &format!(
            "{player_name} swings wildly at everyone here.\r\n",
        ),
    );
}
pub(crate) fn cmd_disarm(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "disarm") {
        return;
    }
    let cost = skill_stamina_cost(world, "disarm", DISARM_COST);
    if !check_stamina(world, player, cost, "disarm") {
        return;
    }
    let arg = args.trim();
    let target = if arg.is_empty() {
        let Some(Fighting(t)) = world.get::<Fighting>(player).copied() else {
            send_to(world, player, "Disarm whom? You aren't fighting.\r\n");
            return;
        };
        t
    } else {
        let Some(located) = world.get::<Located>(player).copied() else {
            send_to(world, player, "You are nowhere.\r\n");
            return;
        };
        let Some(t) = find_actor_in_room(world, arg, located.0, player) else {
            send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
            return;
        };
        t
    };
    if target == player {
        send_to(world, player, "You can't disarm yourself.\r\n");
        return;
    }

    // Find the target's wielded item.
    let weapon: Option<Entity> = {
        let mut q = world
            .query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
        q.iter(world)
            .find(|(_, l, eq)| l.0 == target && eq.0 == Slot::Wield)
            .map(|(e, _, _)| e)
    };
    let Some(weapon) = weapon else {
        let target_name = name_or(world, target, "(unknown)");
        send_to(world, player, format!("{target_name} isn't wielding anything.\r\n"));
        return;
    };
    let Some(target_room) = world
        .get::<Located>(target)
        .copied()
        .map(|l| l.0)
    else {
        send_to(world, player, "Target is in limbo; can't disarm.\r\n");
        return;
    };
    drain_stamina(world, player, cost);

    // Drop weapon: remove EquippedSlot, re-Located to the room.
    if let Ok(mut e) = world.get_entity_mut(weapon) {
        e.remove::<EquippedSlot>();
        e.insert(Located(target_room));
    }
    let weapon_name = name_or(world, weapon, "<weapon>");
    let target_name = name_or(world, target, "(unknown)");
    let player_name = name_of(world, player);
    send_to(world, player, format!(
        "You disarm {target_name}; {weapon_name} clatters to the ground.\r\n"
    ));
    if target != player {
        send_rendered(world, target, &format!(
            "{player_name} disarms you! {weapon_name} clatters to the ground.\r\n"
        ));
    }
    broadcast_room_except_rendered(
        world,
        target_room,
        &[player, target],
        &format!("{player_name} disarms {target_name}; {weapon_name} drops.\r\n"),
    );
}
pub(crate) fn cmd_guard(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        if let Some(g) = world.get::<mud_world::Guarding>(player) {
            let n = name_of(world, g.0);
            send_to(world, player, format!("You are guarding {n}.\r\n"));
        } else {
            send_to(world, player, "You aren't guarding anyone.\r\n");
        }
        return;
    }
    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("none") {
        let had = world.get::<mud_world::Guarding>(player).is_some();
        try_remove::<mud_world::Guarding>(world, player);
        if had {
            send_to(world, player, "You stop guarding.\r\n");
        } else {
            send_to(world, player, "You aren't guarding anyone.\r\n");
        }
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_rendered(world, player, &format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if target == player {
        send_to(world, player, "You can't guard yourself.\r\n");
        return;
    }
    world
        .entity_mut(player)
        .insert(mud_world::Guarding(target));
    let n = name_of(world, target);
    send_to(world, player, format!("You begin guarding {n}.\r\n"));
    send_rendered(
        world,
        target,
        &format!(
            "{} stands ready to defend you.\r\n",
            name_of(world, player)
        ),
    );
}
pub(crate) fn cmd_rescue(world: &mut World, player: Entity, args: &str) {
    if !require_alert_posture(world, player, "rescue") {
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You're already fighting.\r\n");
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Rescue whom?\r\n");
        return;
    }
    // Self-target shortcut: refuse before draining stamina (the
    // redirect arm in invoke_ability also refuses, but we'd waste
    // the cost otherwise).
    if arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
        send_to(world, player, "You can't rescue yourself.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "rescue", RESCUE_COST);
    if !check_stamina(world, player, cost, "rescue") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        &format!("rescue {arg}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_assist(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Assist whom?\r\n");
        return;
    }
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You're already fighting.\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(ally) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    let Some(Fighting(ally_target)) = world.get::<Fighting>(ally).copied() else {
        let ally_name = name_or(world, ally, "(unknown)");
        send_to(world, player, format!("{ally_name} isn't fighting anyone.\r\n"));
        return;
    };
    if world.get_entity(ally_target).is_err() {
        send_to(world, player, "Their target is already gone.\r\n");
        return;
    }
    let target_name = name_or(world, ally_target, "(unknown)");
    cmd_attack(world, player, &target_name);
}
pub(crate) fn cmd_retreat(world: &mut World, player: Entity, args: &str) {
    let arg = args.trim();
    let Some(dir) = parse_direction(arg) else {
        send_to(world, player, "Retreat which way?\r\n");
        return;
    };
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let from_room = located.0;
    let Some(exits) = world.get::<Exits>(from_room).cloned() else {
        send_to(world, player, "No exits here.\r\n");
        return;
    };
    let Some(ed) = exits.0.get(&dir).cloned() else {
        send_to(world, player, format!("No exit {}.\r\n", direction_name(dir)));
        return;
    };
    if ed.state != ExitState::Open {
        send_to(world, player, format!("The exit {} is closed.\r\n", direction_name(dir)));
        return;
    }
    let Some(target) = ed.to else {
        send_to(world, player, "That exit goes nowhere.\r\n");
        return;
    };

    let dir_name = direction_name(dir);
    let mover_name = name_of(world, player);

    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} retreats {dir_name}!\r\n"),
    );
    try_remove::<Fighting>(world, player);
    if let Some(mut l) = world.get_mut::<Located>(player) {
        l.0 = target;
    }
    let arrival_dir = opposite(dir).map_or("nearby".to_string(), |d| {
        format!("the {}", direction_name(d))
    });
    broadcast_room_except_players_rendered(
        world,
        target,
        &[player],
        &format!("{mover_name} retreats here from {arrival_dir}.\r\n"),
    );
    send_to(world, player, format!("You retreat {dir_name}.\r\n"));
    cmd_look(world, player, "");
}
pub(crate) fn cmd_layhands(world: &mut World, player: Entity, args: &str) {
    let cost = skill_stamina_cost(world, "layhands", LAYHANDS_COST);
    if !check_stamina(world, player, cost, "lay hands") {
        return;
    }
    drain_stamina(world, player, cost);
    let arg = args.trim();
    let dispatched = if arg.is_empty() {
        String::from("lay_hands")
    } else {
        format!("lay_hands {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_tame(world: &mut World, player: Entity, args: &str) {
    const TAME_COST: i32 = 4;
    if !require_alert_posture(world, player, "tame") {
        return;
    }
    let arg = args.trim();
    if arg.is_empty() {
        send_to(world, player, "Tame what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let Some(target) = find_actor_in_room(world, arg, located.0, player) else {
        send_to(world, player, format!("You don't see '{arg}' here.\r\n"));
        return;
    };
    if world.get::<Mob>(target).is_none() {
        send_to(world, player, "You can only tame animals.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "tame", TAME_COST);
    if !check_stamina(world, player, cost, "tame") {
        return;
    }
    drain_stamina(world, player, cost);
    let target_name = name_of(world, target);
    invoke_ability(
        world,
        player,
        &format!("tame {target_name}"),
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_drag(world: &mut World, player: Entity, _args: &str) {
    const DRAG_COST: i32 = 3;
    if !require_alert_posture(world, player, "drag") {
        return;
    }
    let cost = skill_stamina_cost(world, "drag", DRAG_COST);
    if !check_stamina(world, player, cost, "drag") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        "drag",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_buck(world: &mut World, player: Entity, args: &str) {
    engage_skill_shim(world, player, args, "buck", 5);
}
pub(crate) fn cmd_breathe(world: &mut World, player: Entity, args: &str) {
    const BREATHE_COST: i32 = 6;
    let race = world
        .get::<Profile>(player)
        .map(|p| p.race.clone())
        .unwrap_or_default();
    let ability_name = match race.as_str() {
        "DRAGONBORN_FIRE" => "breathe_fire",
        "DRAGONBORN_FROST" => "breathe_frost",
        "DRAGONBORN_ACID" => "breathe_acid",
        "DRAGONBORN_GAS" => "breathe_gas",
        "DRAGONBORN_LIGHTNING" => "breathe_lightning",
        _ => {
            send_to(world, player, "You have no breath weapon.\r\n");
            return;
        }
    };
    if !require_alert_posture(world, player, "breathe") {
        return;
    }
    let cost = skill_stamina_cost(world, "breathe", BREATHE_COST);
    if !check_stamina(world, player, cost, "breathe") {
        return;
    }
    drain_stamina(world, player, cost);
    let arg = args.trim();
    let dispatched = if arg.is_empty() {
        ability_name.to_string()
    } else {
        format!("{ability_name} {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_lure(world: &mut World, player: Entity, args: &str) {
    engage_skill_shim(world, player, args, "lure", 4);
}
pub(crate) fn cmd_corner(world: &mut World, player: Entity, args: &str) {
    engage_skill_shim(world, player, args, "corner", 4);
}
pub(crate) fn cmd_sneak(world: &mut World, player: Entity, _args: &str) {
    const SNEAK_COST: i32 = 3;
    let cost = skill_stamina_cost(world, "sneak", SNEAK_COST);
    if !check_stamina(world, player, cost, "sneak") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        "sneak",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_conceal(world: &mut World, player: Entity, _args: &str) {
    const CONCEAL_COST: i32 = 4;
    let cost = skill_stamina_cost(world, "conceal", CONCEAL_COST);
    if !check_stamina(world, player, cost, "conceal") {
        return;
    }
    drain_stamina(world, player, cost);
    invoke_ability(
        world,
        player,
        "conceal",
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_firstaid(world: &mut World, player: Entity, args: &str) {
    const FIRSTAID_COST: i32 = 4;
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't apply first aid in combat.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "firstaid", FIRSTAID_COST);
    if !check_stamina(world, player, cost, "firstaid") {
        return;
    }
    drain_stamina(world, player, cost);
    let arg = args.trim();
    let dispatched = if arg.is_empty() {
        String::from("first_aid")
    } else {
        format!("first_aid {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_bandage(world: &mut World, player: Entity, args: &str) {
    if world.get::<Fighting>(player).is_some() {
        send_to(world, player, "You can't bandage in combat.\r\n");
        return;
    }
    let cost = skill_stamina_cost(world, "bandage", BANDAGE_COST);
    if !check_stamina(world, player, cost, "bandage") {
        return;
    }
    drain_stamina(world, player, cost);
    // Resolve target (for the bleed staunch — invoke_ability also
    // resolves it but we need access to call remove_effect_named).
    let arg = args.trim();
    let target = if arg.is_empty()
        || arg.eq_ignore_ascii_case("me")
        || arg.eq_ignore_ascii_case("self")
    {
        Some(player)
    } else if let Some(located) = world.get::<Located>(player).copied() {
        find_actor_in_room(world, arg, located.0, player)
    } else {
        None
    };
    if let Some(t) = target {
        let staunched = remove_effect_named(world, t, "bleed") > 0;
        if staunched {
            send_to(world, player, "Bleeding stops.\r\n");
            if t != player {
                send_rendered(world, t, "Your bleeding stops.\r\n");
            }
        }
    }
    let dispatched = if arg.is_empty() {
        String::from("bandage")
    } else {
        format!("bandage {arg}")
    };
    invoke_ability(
        world,
        player,
        &dispatched,
        mud_db::abilities::AbilityKind::Skill,
        "use",
    );
}
pub(crate) fn cmd_bash(world: &mut World, player: Entity, target_word: &str) {
    if !require_alert_posture(world, player, "bash") {
        return;
    }
    let cost = skill_stamina_cost(world, "bash", BASH_COST);
    if !check_stamina(world, player, cost, "bash") {
        return;
    }
    let target_word = target_word.trim();
    if target_word.is_empty() {
        send_to(world, player, "Bash what?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let target = find_actor_in_room(world, target_word, located.0, player);
    let Some(target) = target else {
        send_to(world, player, format!("You don't see '{target_word}' here.\r\n"));
        return;
    };

    // Engage if not already.
    let already_fighting = world.get::<Fighting>(player).is_some();
    if !already_fighting
        && let Ok(mut e) = world.get_entity_mut(player)
    {
        e.insert(Fighting(target));
    }
    // First-attacker priority (see cmd_attack).
    if world.get::<Fighting>(target).is_none()
        && world.get::<CombatStats>(target).is_some()
        && let Ok(mut e) = world.get_entity_mut(target)
    {
        e.insert(Fighting(player));
    }

    // Effective legacy damroll for the +3 flat formula.
    let dmg_roll = world
        .get::<CombatStats>(player)
        .map_or(1, |cs| cs.attack_power / 5);
    let damage = (dmg_roll + 3).max(1);
    drain_stamina(world, player, cost);

    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    let (dead, threshold_msg) = apply_damage(world, target, damage);

    // Knockdown — set target to Sitting.
    if !dead && let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(Posture(PostureKind::Sitting));
    }

    send_rendered(world, player, &format!("You bash {target_name} for {damage} damage, knocking them down!\r\n"),
    );
    send_rendered(world, target, &format!("{player_name} bashes you for {damage} damage, knocking you down!\r\n"),
    );
    if let Some(m) = threshold_msg {
        send_to(world, target, m);
    }
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} bashes {target_name}, knocking them down.\r\n"),
    );

    if dead {
        crate::combat::handle_death(world, target, &target_name, located.0);
    }
}
pub(crate) fn cmd_disengage(world: &mut World, player: Entity, _args: &str) {
    if world.get::<Fighting>(player).is_none() {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    }
    try_remove::<Fighting>(world, player);
    send_to(world, player, "You stop fighting.\r\n");
}
