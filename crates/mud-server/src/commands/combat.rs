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
    RESCUE_COST, ROAR_COST, ROUNDHOUSE_COST, SPRINGLEAP_COST, STOMP_COST, SWEEP_COST, TAUNT_COST,
    THROATCUT_COST, TRIPUP_COST, aggro_alignment, apply_damage_from, auto_assist_followers_of,
    broadcast_room_except_players_rendered, broadcast_room_except_rendered, check_stamina,
    cmd_look, consider_verdict_color, direction_name, drain_stamina, engage_skill_shim,
    find_actor_in_room, flip_door_both_sides, hit_chance_color, invoke_ability, invoke_ability_aoe,
    mob_helpers_engage, name_of, name_or, opposite, parse_direction, remove_effect_named,
    require_alert_posture, send_rendered, send_to, skill_stamina_cost, try_insert, try_remove,
};

inventory::submit! {
    Command {
    names: &["attack", "kill", "hit", "murder"],
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
        long: "Sizes up the target — compares their HP, accuracy, \
               evasion, and attack power against yours and reports \
               a rough difficulty band. Doesn't engage the target; \
               purely informational.",
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
               Rolls weapon damage scaled by attack_power, then \
               adds +4. You must already be fighting someone.",
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
        long: "Costs 5 stamina, deals 1/4 your damage, sets the \
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
        long: "Costs 12 stamina. Deals 1/4 damage to every \
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
        summary: "Powerful kick — 1.5x damage on your current target.",
        long: "Costs 7 stamina. Heavier kick than the basic `kick` \
               skill (which adds +4); pure damage multiplier. \
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
        long: "Costs 6 stamina, deals half your damage, sets the \
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
        summary: "Terrify everyone in the room into running.",
        long: "Costs 8 stamina. Also deals the Roar skill's damage \
               to each enemy in the room. Each enemy that fails \
               its saving throw panics: it flees, trips over its \
               own feet, or (if asleep) may be jolted awake. Aware \
               and no-summon mobs, and mobs immune to fear, ignore \
               it. Group members are not affected.",
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
        long: "Costs 7 stamina, deals weapon damage, applies a \
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
        long: "Costs 7 stamina, deals weapon damage, applies a \
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
        long: "Deals 1.5x your damage on the opening swing and \
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
        long: "Like backstab but heavier: 2.5x your damage on \
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
        long: "Deals 2x your damage on the opening swing and \
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
               for half your damage. Mobs with no Health (test \
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
    names: &["assist", "ass"],
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
        long: "Deals weapon damage + 3 and forces the target into a \
               sitting posture. Targets without combat stats simply \
               take the damage.",
    },
    run: cmd_bash,
    }
}

inventory::submit! {
    Command {
    names: &["taunt", "provoke"],
    min_role: UserRole::Player,
    required_perm: None,
    category: Category::Combat,
    help: Help {
        usage: "taunt <target>",
        summary: "Pull a mob's aggro onto yourself (tank tool).",
        long: "Forces the target to focus its attacks on you, \
               regardless of who else it was engaging. Also pushes \
               you to the front of the target's grudge list so it \
               re-engages you first when its current target falls. \
               Costs stamina but no damage — paired with a healer, \
               this lets a tank hold the front line while the dps \
               works behind them.",
    },
    run: cmd_taunt,
    }
}

//  `gsay` / `gtell` / `gecho` / `gt` migrated to commands/room_chat.rs.

// ---- handler bodies ----

#[allow(clippy::too_many_lines)]
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
    // Wall-bash gate. If a WALL_OF_STONE / WALL_OF_ICE blocks this
    // direction, the swing slams into the wall instead of the door
    // beyond. Deduct STR-scaled damage from the per-direction HP
    // pool; on hp ≤ 0 despawn the backing EffectInstance and drop
    // the entry so movement clears. Wall HP defaults 100 (ice) /
    // 200 (stone) on the cast site so an unmodified wall survives
    // ~20-40 bashes.
    let wall_entry = world
        .get::<mud_world::RoomBlockedExits>(room)
        .and_then(|b| b.by_direction.get(&dir).cloned());
    if let Some(wall) = wall_entry {
        drain_stamina(world, player, cost);
        let str_bonus = world
            .get::<mud_world::CoreStats>(player)
            .map_or(0, |s| mud_world::CoreStats::bonus(s.strength));
        let damage = (5 + str_bonus).max(1);
        let new_hp = wall.hp.saturating_sub(damage);
        let player_name = name_of(world, player);
        let dir_label = direction_name(dir);
        let kind = wall.kind_label.clone();
        if new_hp <= 0 {
            // Crumble — despawn the backing instance, drop the
            // map entry, broadcast the crash. The teardown path
            // in effects.rs is keyed off the EffectInstance's
            // expiry; we beat it to the punch here.
            if let Ok(em) = world.get_entity_mut(wall.backed_by) {
                em.despawn();
            }
            if let Some(mut b) = world.get_mut::<mud_world::RoomBlockedExits>(room) {
                b.by_direction.remove(&dir);
            }
            send_to(
                world,
                player,
                format!(
                    "You smash through the {kind} {dir_label} with a final shattering blow!\r\n"
                ),
            );
            broadcast_room_except_players_rendered(
                world,
                room,
                &[player],
                &format!(
                    "{player_name} shatters the {kind} {dir_label} into a thousand pieces!\r\n"
                ),
            );
        } else {
            if let Some(mut b) = world.get_mut::<mud_world::RoomBlockedExits>(room)
                && let Some(entry) = b.by_direction.get_mut(&dir)
            {
                entry.hp = new_hp;
            }
            send_to(
                world,
                player,
                format!(
                    "You hammer the {kind} {dir_label} — it cracks but holds (~{new_hp} HP).\r\n"
                ),
            );
            broadcast_room_except_players_rendered(
                world,
                room,
                &[player],
                &format!(
                    "{player_name} slams into the {kind} {dir_label} with a thunderous crash!\r\n"
                ),
            );
        }
        return;
    }
    let (cur_state, cur_hp, is_bashable) = world
        .get::<Exits>(room)
        .and_then(|e| {
            e.0.get(&dir)
                .map(|ed| (Some(ed.state), ed.hit_points, ed.is_bashable))
        })
        .unwrap_or((None, None, false));
    let Some(state) = cur_state else {
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
        return;
    };
    if state == ExitState::Open {
        send_to(
            world,
            player,
            format!("It's already open {}.\r\n", direction_name(dir)),
        );
        return;
    }
    if !is_bashable {
        send_to(
            world,
            player,
            format!(
                "The way {} is sealed by something stronger than your shoulder.\r\n",
                direction_name(dir)
            ),
        );
        return;
    }
    drain_stamina(world, player, cost);
    // Roll the hit. STR bonus drives the damage taken; default ~5
    // per swing means an unmodified door (50 HP) survives ~10
    // bashes. CoreStats::bonus on the 0-100 scale gives ±10.
    let str_bonus = world
        .get::<mud_world::CoreStats>(player)
        .map_or(0, |s| mud_world::CoreStats::bonus(s.strength));
    let damage = (5 + str_bonus).max(1);
    let new_hp = cur_hp.unwrap_or(50).saturating_sub(damage);
    let player_name = name_of(world, player);
    if new_hp <= 0 {
        // Splintered — fully open on this side AND mirror on the
        // far side so the player can walk through.
        flip_door_both_sides(world, room, dir, ExitState::Open);
        // Reset HP for future re-locks (some triggers re-close).
        if let Some(mut exits) = world.get_mut::<Exits>(room)
            && let Some(ed) = exits.0.get_mut(&dir)
        {
            ed.hit_points = Some(50);
        }
        send_to(
            world,
            player,
            format!(
                "You bash open the way {} with a splintering crash!\r\n",
                direction_name(dir),
            ),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!(
                "{player_name} bashes the door {} wide open!\r\n",
                direction_name(dir)
            ),
        );
    } else {
        if let Some(mut exits) = world.get_mut::<Exits>(room)
            && let Some(ed) = exits.0.get_mut(&dir)
        {
            ed.hit_points = Some(new_hp);
        }
        send_to(
            world,
            player,
            format!(
                "You shoulder-charge the door {} — it groans but holds.\r\n",
                direction_name(dir),
            ),
        );
        broadcast_room_except_players_rendered(
            world,
            room,
            &[player],
            &format!(
                "{player_name} slams against the door {} with a thunderous crash!\r\n",
                direction_name(dir)
            ),
        );
    }
}
/// Re-engage lag: one combat round (`REENGAGE_LAG_TICKS`) after any
/// engage, `disengage` or failed switch. Inside the window a new
/// `kill` still sets `Fighting` (the normal combat tick swings at its
/// usual cadence) but gets no instant swing and no second ATTACK
/// trigger, so `disengage` + `kill` cannot be looped for free rounds.
/// `last_opponent` makes `disengage` + `kill <other>` count as a
/// switch attempt. Cleared when the target dies (`disengage_attackers_of`).
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct ReengageLag {
    until: u64,
    last_opponent: Option<Entity>,
}

/// One combat round, in 10Hz ticks (matches `combat::COMBAT_PERIOD_TICKS`).
const REENGAGE_LAG_TICKS: u64 = 40;

fn now_tick(world: &World) -> u64 {
    world.get_resource::<crate::TickCount>().map_or(0, |t| t.0)
}

/// The actor's lag if the window is still open.
fn active_reengage_lag(world: &World, actor: Entity) -> Option<ReengageLag> {
    let now = now_tick(world);
    world
        .get::<ReengageLag>(actor)
        .copied()
        .filter(|l| l.until > now)
}

/// Open (or restart) the re-engage window, remembering `opponent`.
fn stamp_reengage(world: &mut World, actor: Entity, opponent: Option<Entity>) {
    let lag = ReengageLag {
        until: now_tick(world) + REENGAGE_LAG_TICKS,
        last_opponent: opponent,
    };
    try_insert(world, actor, lag);
}

/// A mob's Switch percent at `level` once it has the skill. Legacy
/// `roll_mob_skill` (chars.cpp:247) gives an NPC `random(50,100)` plus
/// `random(5,15)` per level above the first, capped at 1000, and
/// `GET_SKILL` divides by 10. Rust mobs carry no stored skill rows, so
/// this is that roll's mean, `75 + 10 * (level - 1)` tenths, as a
/// percent.
fn mob_switch_percent(level: i32) -> i32 {
    let tenths = 75 + 10 * (level.max(1) - 1);
    (tenths / 10).clamp(0, 100)
}

/// A mob's Switch percent, 0 when it lacks the skill. Legacy
/// `init_char_skills` (skills.cpp:255-275) only calls `roll_mob_skill`
/// for skills the mob's class learns at or below its level
/// (`skill_assign` rows, class.cpp); everything else is zeroed, and
/// `switch_ok` refuses at skill 0. Here that is the mob prototype's
/// `class_id` against `ClassSkillsData` for the `switch` ability.
fn mob_switch_skill(world: &World, mob: Entity) -> i32 {
    let Some(class_id) = world
        .get::<mud_world::WorldKey>(mob)
        .and_then(|wk| {
            world
                .get_resource::<mud_world::MobPrototypes>()?
                .by_key
                .get(&(wk.zone, wk.id))
        })
        .and_then(|p| p.class_id)
    else {
        return 0;
    };
    let Some(ability_id) = world
        .get_resource::<mud_world::AbilityCatalog>()
        .and_then(|c| c.by_name.get("switch"))
        .map(|def| def.id)
    else {
        return 0;
    };
    let level = mud_world::effective_level(world, mob);
    let learned = world
        .get_resource::<mud_world::ClassSkillsData>()
        .and_then(|d| d.min_level_for(class_id, ability_id))
        .is_some_and(|min| min <= level);
    if learned {
        mob_switch_percent(level)
    } else {
        0
    }
}

/// Legacy `switch_ok`: moving to a new opponent mid-fight needs the
/// `Switch` skill (mobs: `mob_switch_skill`, class-gated). No
/// skill refuses outright; a failed roll (`roll`
/// is 1..=101 against the skill percent) drops the current fight
/// without starting a new one; success drops it so the caller can
/// engage the new target. Returns true when the caller may proceed.
fn try_switch_opponent(world: &mut World, player: Entity, old: Entity, roll: i32) -> bool {
    let skill = if world.get::<Mob>(player).is_some() {
        mob_switch_skill(world, player)
    } else {
        world
            .get_resource::<mud_world::AbilityCatalog>()
            .and_then(|c| c.by_name.get("switch"))
            .and_then(|def| {
                world
                    .get::<mud_world::KnownAbilities>(player)?
                    .entries
                    .iter()
                    .find(|(id, _, known)| *id == def.id && *known)
                    .map(|(_, prof, _)| (prof / 10).clamp(0, 100))
            })
            .unwrap_or(0)
    };
    if skill <= 0 {
        let old_name = name_of(world, old);
        send_to(
            world,
            player,
            format!("You are already busy fighting with {old_name}.\r\n"),
        );
        return false;
    }
    let player_name = name_of(world, player);
    let room = world.get::<Located>(player).map(|l| l.0);
    try_remove::<Fighting>(world, player);
    if roll > skill {
        stamp_reengage(world, player, Some(old));
        send_to(
            world,
            player,
            "You try to switch opponents and become confused.\r\n",
        );
        if let Some(room) = room {
            broadcast_room_except_rendered(
                world,
                room,
                &[player],
                &format!("{player_name} tries to switch opponents, but becomes confused!\r\n"),
            );
        }
        return false;
    }
    send_to(world, player, "You switch opponents!\r\n");
    if let Some(room) = room {
        broadcast_room_except_rendered(
            world,
            room,
            &[player],
            &format!("{player_name} switches opponents!\r\n"),
        );
    }
    true
}
pub(crate) fn cmd_attack(world: &mut World, player: Entity, target_name: &str) {
    attack_with_switch_roll(world, player, target_name, rand::random_range(1..=101));
}

/// `cmd_attack` with the Switch d101 roll injected (tests pin it).
#[allow(clippy::too_many_lines)]
fn attack_with_switch_roll(world: &mut World, player: Entity, target_name: &str, switch_roll: i32) {
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

    // Filter out corpses up front. A corpse keeps the dead mob's
    // keywords ("the corpse of a frost stallion" still matches
    // "stallion"), and was tied with the live stallion via insertion
    // order — `kill stallion` could land on the corpse and surface
    // "You attack the corpse" while the live mob sits unaffected.
    // CombatStats keeps the filter targeted to attackable actors
    // (mobs/players with the combat package) rather than e.g. items
    // that happen to share a keyword.
    let target = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &Named), (With<mud_world::CombatStats>, Without<mud_world::Corpse>)>();
        q.iter(world)
            .find(|(e, l, n)| {
                *e != player
                    && l.0 == located.0
                    && n.name.to_ascii_lowercase().contains(&target_lower)
                    && crate::commands::can_see_player(world, player, *e)
            })
            .map(|(e, _, _)| e)
    };

    let Some(target) = target else {
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_name}' here.\r\n"),
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
            crate::commands::cap_sentence_start(&format!(
                "{target_name_owned} radiates a calm that turns your blow aside.\r\n"
            )),
        );
        return;
    }

    if !super::attack_ok::attack_ok(world, player, target, true) {
        return;
    }

    // Already-fighting gate (legacy `do_hit`): re-issuing the command
    // on the current opponent is a no-op, and picking a different one
    // is a `switch` skill check (mobs included, by level). Without this, every `kill` re-ran the
    // engage path below -- a free first swing, a fresh ATTACK trigger
    // and a stamina drain each time -- and swapped targets with no
    // skill at all.
    let current = world
        .get::<Fighting>(player)
        .map(|f| f.0)
        .filter(|&c| world.get::<Located>(c).is_some_and(|l| l.0 == located.0));
    if current == Some(target) {
        send_to(world, player, "You're doing the best you can!\r\n");
        return;
    }
    // `disengage` / a failed switch leave no `Fighting` behind, so the
    // lag window remembers who we were fighting: moving to someone else
    // inside it is still a switch attempt.
    let lag = active_reengage_lag(world, player);
    let switching_from = current.or_else(|| {
        lag.and_then(|l| l.last_opponent).filter(|&o| {
            o != target
                && world.get::<Located>(o).is_some_and(|l| l.0 == located.0)
                && world.get::<Health>(o).is_some_and(|h| h.hp > 0)
        })
    });
    if let Some(old) = switching_from
        && !try_switch_opponent(world, player, old, switch_roll)
    {
        return;
    }
    // Only a fresh engage (no open lag window) gets the instant swing
    // and the ATTACK trigger.
    let fresh = lag.is_none();
    stamp_reengage(world, player, Some(target));
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

    // Attacking breaks the attacker's invisibility (legacy
    // `aggro_lose_spells` -> `appear`) before the attack lines go out,
    // so they name the attacker.
    crate::commands::break_invisibility(world, player);
    send_to(world, player, format!("You attack {actual_name}!\r\n"));
    send_rendered(world, target, &format!("{player_name} attacks you!\r\n"));
    crate::commands::broadcast_room_anonymised(
        world,
        located.0,
        &[player, target],
        &[(player, &player_name), (target, &actual_name)],
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
    if fresh {
        crate::triggers::fire_event_with_actor(
            world,
            target,
            player,
            mud_world::TriggerEvent::Attack,
        );
    }

    // G3.1: fire the player's first swing right here so they don't
    // sit through "You attack X!" with no follow-up until the next
    // combat tick (up to ~4s). Subsequent swings come from the
    // regular `combat_tick` cadence. ATTACK trigger above may have
    // killed / moved the target — verify the engagement still holds.
    if fresh
        && world.get_entity(target).is_ok()
        && world.get::<Fighting>(player).is_some_and(|f| f.0 == target)
    {
        crate::combat::engage_swing_now(world, player, target);
    }
}
#[allow(clippy::too_many_lines)]
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
        send_rendered(
            world,
            player,
            &format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    let target_name = name_of(world, target);

    let self_max_hp = world.get::<Health>(player).map_or(1, |h| h.max).max(1);
    let self_stats = world.get::<CombatStats>(player).copied();
    // attack_power feeds the consider verdict in place of the
    // legacy damage. Same intent: "how hard does this side hit?"
    let self_dmg = self_stats.map_or(0, |c| c.attack_power);
    let self_accuracy = self_stats.map_or(0, |c| c.accuracy);
    let self_evasion = self_stats.map_or(0, |c| c.evasion);
    let target_max_hp = world.get::<Health>(target).map_or(0, |h| h.max);
    let target_stats = world.get::<CombatStats>(target).copied();
    let target_dmg = target_stats.map_or(0, |c| c.attack_power);
    let target_accuracy = target_stats.map_or(0, |c| c.accuracy);
    let target_evasion = target_stats.map_or(0, |c| c.evasion);

    if target_max_hp == 0 {
        send_rendered(
            world,
            player,
            &crate::commands::cap_sentence_start(&format!(
                "{target_name} doesn't look like a fighter at all.\r\n"
            )),
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
        "<b:cyan>{}</> {verdict_open}{verdict}</>\r\n",
        crate::commands::cap_sentence_start(&target_name),
    );
    // Hit chances + raw HP are *god-only*. Mortal players see a
    // verbal impression of the target's wound state instead of an
    // exact integer. Staff need the numbers for tuning passes.
    let staff = crate::commands::is_staff(world, player);
    let your_chance = crate::combat::hit_chance_pct(self_accuracy, target_evasion);
    let their_chance = crate::combat::hit_chance_pct(target_accuracy, self_evasion);
    let target_hp = world.get::<Health>(target).map_or(0, |h| h.hp).max(0);
    let target_pct = if target_max_hp > 0 {
        (target_hp * 100) / target_max_hp
    } else {
        0
    };
    if staff {
        let your_pct_text = hit_chance_color(your_chance)
            .map_or(format!("{your_chance}%"), |open| {
                format!("{open}{your_chance}%</>")
            });
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
        out.push_str(&format!(
            "Their condition: <b:yellow>{target_hp}/{target_max_hp} HP</> ({target_pct}%).\r\n",
        ));
    } else {
        // Mortal: verbal impression only. Mirrors the classic MUD
        // `consider` flavor — give a feel for how worn-down the
        // target looks, never the exact ratio. Uses a pronoun so
        // the line doesn't repeat the target name a second time.
        let impression = match target_pct {
            100 => "<green>looks untouched</>",
            90..=99 => "<green>has only a few scratches</>",
            70..=89 => "<yellow>is wounded</>",
            40..=69 => "<yellow>is bleeding heavily</>",
            10..=39 => "<red>looks badly hurt</>",
            _ => "<b:red>is on the verge of death</>",
        };
        out.push_str(&format!("It {impression}.\r\n"));
    }
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
            out.push_str("<b:red>It remembers you, and its hand goes to its weapon.</>\r\n");
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
            send_to(
                world,
                player,
                format!("{} whom?\r\n", crate::commands::capitalize(skill_name)),
            );
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
    if !super::attack_ok::attack_ok(world, player, target, true) {
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
        let (dead, _msg) = apply_damage_from(world, target, dam, player);
        if dead && let Some(loc) = world.get::<Located>(target).copied() {
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
        send_to(world, player, "Grow some longer fingernails first.\r\n");
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
        send_to(
            world,
            player,
            "Stealing from yourself is rather stupid.\r\n",
        );
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
    // Legacy: player-stealing is only allowed during PK.
    if world.get::<mud_world::Player>(target).is_some()
        && !super::attack_ok::attack_ok(world, player, target, true)
    {
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
        // Getting caught breaks invisibility (legacy `appear()`) before
        // the victim is told who it was.
        crate::commands::break_invisibility(world, player);
        send_to(world, player, "Oops...\r\n");
        let thief_seen = crate::commands::cap_sentence_start(&crate::commands::seen_name(
            world,
            target,
            player,
            &player_name,
        ));
        send_rendered(
            world,
            target,
            &format!("<b:yellow>{thief_seen} tried to steal something from you!</>\r\n"),
        );
        crate::commands::broadcast_room_anonymised(
            world,
            room,
            &[player, target],
            &[(player, &player_name), (target, &target_name)],
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
            send_to(
                world,
                player,
                format!(
                    "{} has no coin worth lifting.\r\n",
                    crate::commands::cap_sentence_start(&target_name)
                ),
            );
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
        let mut q = world.query_filtered::<(
            Entity,
            &mud_world::Located,
            &Named,
            Option<&mud_world::Keywords>,
            Option<&mud_world::EquippedSlot>,
        ), With<Item>>();
        q.iter(world)
            .find(|(_, l, n, kw, eq)| {
                l.0 == target && eq.is_none() && crate::commands::matches(&needle, n, *kw)
            })
            .map(|(e, _, n, _, _)| (e, n.name.clone()))
    };
    let Some((item, item_name)) = item_opt else {
        send_rendered(
            world,
            player,
            &format!(
                "{} hasn't got '{what}' on them.\r\n",
                crate::commands::cap_sentence_start(&target_name)
            ),
        );
        return;
    };
    if world.get::<mud_world::Located>(item).is_some() {
        world.entity_mut(item).insert(mud_world::Located(player));
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
    let mut candidates = candidates;
    crate::room_access::retain_admitted(world, &same_room, &mut candidates);
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
        // Each retreating member drops their own Fighting; `relocate`
        // also stops everyone fighting them.
        try_remove::<Fighting>(world, *m);
        crate::combat::relocate(world, *m, target);
        crate::combat::carry_mount(world, *m, target);
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
        send_to(
            world,
            *m,
            format!("You retreat with the group {dir_name}!\r\n"),
        );
        cmd_look(world, *m, "");
    }
}

/// The `flee` command. Legacy `do_flee` refusals apply
/// ([`crate::fear::can_flee_now`]).
pub(crate) fn cmd_flee(world: &mut World, player: Entity, _args: &str) {
    if crate::fear::can_flee_now(world, player) {
        flee_through_exit(world, player);
    }
}

/// Bolt through a random open exit, ending the fight if it succeeds.
/// Callers have already passed [`crate::fear::can_flee_now`].
pub(crate) fn flee_through_exit(world: &mut World, player: Entity) {
    // Panic exit cancels any in-progress cast — no concentration to
    // be had while bolting for the door.
    crate::casting::cancel_own_cast(world, player);
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
    let mut candidates = candidates;
    crate::room_access::retain_admitted(world, &[player], &mut candidates);

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

    // Moving ends the fight both ways (`relocate`, legacy `char_from_room`).
    try_remove::<Fighting>(world, player);

    // Move + announce arrival + auto-look.
    crate::combat::relocate(world, player, target);
    crate::combat::carry_mount(world, player, target);
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
        send_to(
            world,
            player,
            format!("{target_name} is already on the ground.\r\n",),
        );
        return;
    }
    let Some(target_room) = world.get::<Located>(target).copied().map(|l| l.0) else {
        send_to(world, player, "Target is in limbo.\r\n");
        return;
    };

    // Skill base damage scales with the attacker's attack_power.
    // Pre-pivot this read `damage / 2`; in the new model
    // attack_power is a +%, so we recover an effective damage
    // as `attack_power / 5` (the inverse of the migration's
    // `damage_roll * 5 = attack_power` mapping).
    let dmg = world
        .get::<CombatStats>(player)
        .map_or(1, |c| ((c.attack_power / 5) / 2).max(1));
    drain_stamina(world, player, cost);

    let player_name = name_of(world, player);
    let target_name = name_or(world, target, "(unknown)");
    let (dead, _) = apply_damage_from(world, target, dmg, player);

    if !dead && let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(Posture(PostureKind::Sitting));
    }

    send_to(
        world,
        player,
        format!("You stomp on {target_name} for {dmg} damage; they go down!\r\n"),
    );
    if !dead {
        send_rendered(
            world,
            target,
            &format!("{player_name} stomps you to the ground!\r\n"),
        );
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
/// Area attacks hit every mob in the room; drop the attacker's own group
/// (own pets, followers and group members' pets, as legacy `area_attack_target`
/// skips `is_grouped` and `master` links) and other players' pets the PK rule
/// forbids `player` to attack (silently, as legacy `mass_attack_ok`).
fn skip_forbidden_pets(world: &mut World, player: Entity, targets: Vec<Entity>) -> Vec<Entity> {
    let my_root = super::group_root(world, player);
    targets
        .into_iter()
        .filter(|t| {
            super::group_root(world, *t) != my_root
                && (super::attack_ok::pet_owner(world, *t).is_none()
                    || super::attack_ok::attack_ok(world, player, *t, false))
        })
        .collect()
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
        let mut q = world
            .query_filtered::<(Entity, &Located, Option<&Posture>, Option<&Health>), With<Mob>>();
        q.iter(world)
            .filter(|(_, l, p, h)| {
                l.0 == room
                    && h.is_some()
                    && matches!(p.map(|p| p.0), None | Some(PostureKind::Standing))
            })
            .map(|(e, _, _, _)| e)
            .collect()
    };
    let targets = skip_forbidden_pets(world, player, targets);
    if targets.is_empty() {
        send_to(world, player, "Nothing here to sweep.\r\n");
        return;
    }
    drain_stamina(world, player, cost);
    let player_name = name_of(world, player);
    let count = targets.len();
    for t in targets {
        let target_name = name_or(world, t, "(unknown)");
        let (dead, _) = apply_damage_from(world, t, dmg, player);
        if dead {
            crate::combat::handle_death(world, t, &target_name, room);
        } else if let Ok(mut e) = world.get_entity_mut(t) {
            e.insert(Posture(PostureKind::Sitting));
        }
    }
    send_to(
        world,
        player,
        format!("You sweep your leg in a wide arc — {count} go down!\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        room,
        &[player],
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
    // the rest using `aoe_repeat = true`.
    let landed = invoke_ability_aoe(
        world,
        player,
        mud_db::abilities::AbilityKind::Skill,
        "use",
        "roar",
        AoeScope::RoomEnemies,
        "There's nothing here to roar at.\r\n",
    );
    if !landed {
        return;
    }
    // Legacy `do_roar`: the roar itself leaves nothing behind; each
    // victim that fails its saves panics (flees, trips or wakes).
    let Some(room) = world.get::<Located>(player).map(|l| l.0) else {
        return;
    };
    let victims: Vec<Entity> =
        super::aoe_targets_in_room(world, player, room, AoeScope::RoomEnemies)
            .into_iter()
            .map(|(e, _)| e)
            .filter(|t| super::attack_ok::attack_ok(world, player, *t, false))
            .collect();
    for victim in victims {
        crate::fear::roar_target(world, player, victim, crate::fear::RoarRolls::random());
    }
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
        send_to(
            world,
            player,
            "You can't springleap while already fighting.\r\n",
        );
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
    if !super::attack_ok::attack_ok(world, player, target, true) {
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
    if !super::attack_ok::attack_ok(world, player, target, true) {
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
    if !super::attack_ok::attack_ok(world, player, target, true) {
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
    let mob_targets = skip_forbidden_pets(world, player, mob_targets);
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
        let (dead, _msg) = apply_damage_from(world, *target, dmg, player);
        hits.push((target_name.clone(), dead));
        if dead {
            crate::combat::handle_death(world, *target, &target_name, room);
        } else if first_alive.is_none() {
            first_alive = Some(*target);
        }
    }

    // Engage the first survivor if we weren't already fighting.
    if !already_fighting && let Some(first) = first_alive {
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
        &format!("{player_name} swings wildly at everyone here.\r\n",),
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
    if !super::attack_ok::attack_ok(world, player, target, true) {
        return;
    }

    // Find the target's wielded item.
    let weapon: Option<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Located, &EquippedSlot), With<Item>>();
        q.iter(world)
            .find(|(_, l, eq)| l.0 == target && eq.0 == Slot::Wield)
            .map(|(e, _, _)| e)
    };
    let Some(weapon) = weapon else {
        let target_name = name_or(world, target, "(unknown)");
        send_to(
            world,
            player,
            format!(
                "{} isn't wielding anything.\r\n",
                crate::commands::cap_sentence_start(&target_name)
            ),
        );
        return;
    };
    let Some(target_room) = world.get::<Located>(target).copied().map(|l| l.0) else {
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
    send_to(
        world,
        player,
        format!("You disarm {target_name}; {weapon_name} clatters to the ground.\r\n"),
    );
    if target != player {
        send_rendered(
            world,
            target,
            &format!("{player_name} disarms you! {weapon_name} clatters to the ground.\r\n"),
        );
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
        let was_guarding = world.get::<mud_world::Guarding>(player).map(|g| g.0);
        try_remove::<mud_world::Guarding>(world, player);
        if let Some(target) = was_guarding {
            send_to(world, player, "You stop guarding.\r\n");
            let target_name = name_of(world, target);
            let player_name = name_of(world, player);
            send_rendered(
                world,
                target,
                &format!("{player_name} stops guarding you.\r\n"),
            );
            if let Some(located) = world.get::<Located>(player).copied() {
                crate::commands::broadcast_room_visual(
                    world,
                    located.0,
                    player,
                    &[player, target],
                    &crate::commands::cap_sentence_start(&format!(
                        "{player_name} steps back from {target_name}'s side.\r\n"
                    )),
                );
            }
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
    world.entity_mut(player).insert(mud_world::Guarding(target));
    let n = name_of(world, target);
    let player_name = name_of(world, player);
    send_to(world, player, format!("You begin guarding {n}.\r\n"));
    send_rendered(
        world,
        target,
        &format!("{player_name} stands ready to defend you.\r\n"),
    );
    // Broadcast to the rest of the room so allies see the formation
    // forming. A "X moves to Y's side" line is more atmospheric than
    // the silent state-change and matches legacy guard UX.
    crate::commands::broadcast_room_visual(
        world,
        located.0,
        player,
        &[player, target],
        &crate::commands::cap_sentence_start(&format!(
            "{player_name} moves to {n}'s side, ready to defend them.\r\n"
        )),
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
        send_to(
            world,
            player,
            format!("{ally_name} isn't fighting anyone.\r\n"),
        );
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
        send_to(
            world,
            player,
            format!("No exit {}.\r\n", direction_name(dir)),
        );
        return;
    };
    if ed.state != ExitState::Open {
        send_to(
            world,
            player,
            format!("The exit {} is closed.\r\n", direction_name(dir)),
        );
        return;
    }
    let Some(target) = ed.to else {
        send_to(world, player, "That exit goes nowhere.\r\n");
        return;
    };
    if crate::room_access::refuse_entry(world, player, target) {
        return;
    }

    let dir_name = direction_name(dir);
    let mover_name = name_of(world, player);

    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[player],
        &format!("{mover_name} retreats {dir_name}!\r\n"),
    );
    try_remove::<Fighting>(world, player);
    crate::combat::relocate(world, player, target);
    crate::combat::carry_mount(world, player, target);
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
    let target =
        if arg.is_empty() || arg.eq_ignore_ascii_case("me") || arg.eq_ignore_ascii_case("self") {
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
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };

    if !super::attack_ok::attack_ok(world, player, target, true) {
        return;
    }

    // Engage if not already.
    let already_fighting = world.get::<Fighting>(player).is_some();
    if !already_fighting && let Ok(mut e) = world.get_entity_mut(player) {
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
    let damage = world
        .get::<CombatStats>(player)
        .map_or(1, |cs| cs.attack_power / 5);
    let damage = (damage + 3).max(1);
    drain_stamina(world, player, cost);

    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    let (dead, threshold_msg) = apply_damage_from(world, target, damage, player);

    // Knockdown — set target to Sitting.
    if !dead && let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(Posture(PostureKind::Sitting));
    }
    // Concentration break — a bash that knocks a caster on their
    // back shatters any spell they were winding up. This is the
    // tactical point of BASH against a mage (the ability carries
    // an `interrupt` effect intent the inline bash path otherwise
    // never realized). `interrupt_cast` no-ops cleanly when the
    // target wasn't casting, so the call is unconditional.
    if !dead {
        crate::casting::interrupt_cast(world, target, "the bash knocks you flat");
    }

    send_rendered(
        world,
        player,
        &format!("You bash {target_name} for {damage} damage, knocking them down!\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{player_name} bashes you for {damage} damage, knocking you down!\r\n"),
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
/// `taunt <target>`: redirect the target mob's aggro onto the
/// caster. Sets target.Fighting = caster (overriding whoever
/// they were on), moves caster to the front of the `HateList` so
/// the re-engage pre-pass picks them. The legacy semantic is
/// "tank pulls heat off the squishies" — making this a real
/// skill closes Q6 from combat-rebalance.md.
///
/// Players can be tauntd by other players in PK contexts (mirrors
/// bash). Non-mob, non-player targets refuse. Peaceful mob gate
/// applies (a peaceful shopkeeper won't attack you back even if
/// you taunt them — the gate stays consistent with `cmd_attack`).
pub(crate) fn cmd_taunt(world: &mut World, player: Entity, target_word: &str) {
    if !require_alert_posture(world, player, "taunt") {
        return;
    }
    let cost = skill_stamina_cost(world, "taunt", TAUNT_COST);
    if !check_stamina(world, player, cost, "taunt") {
        return;
    }
    let target_word = target_word.trim();
    if target_word.is_empty() {
        send_to(world, player, "Taunt whom?\r\n");
        return;
    }
    let Some(located) = world.get::<Located>(player).copied() else {
        return;
    };
    let Some(target) = find_actor_in_room(world, target_word, located.0, player) else {
        send_to(
            world,
            player,
            format!("You don't see '{target_word}' here.\r\n"),
        );
        return;
    };
    if target == player {
        send_to(world, player, "Taunting yourself accomplishes little.\r\n");
        return;
    }
    if world.get::<CombatStats>(target).is_none() {
        send_to(
            world,
            player,
            "Nothing about that target responds to provocation.\r\n",
        );
        return;
    }
    if world.get::<mud_world::PeacefulRoom>(located.0).is_some() {
        send_to(
            world,
            player,
            "A peaceful aura keeps violence — and your taunt — at bay here.\r\n",
        );
        return;
    }
    if world
        .get::<mud_world::MobBehaviors>(target)
        .is_some_and(|b| b.has(mud_db::enums::MobBehavior::Peaceful))
    {
        let n = name_of(world, target);
        send_to(
            world,
            player,
            format!("{n} simply ignores your provocation.\r\n"),
        );
        return;
    }
    if !super::attack_ok::attack_ok(world, player, target, true) {
        return;
    }
    drain_stamina(world, player, cost);

    let target_name = name_of(world, target);
    let player_name = name_of(world, player);

    // Engage the caster on the target — and force the target's
    // Fighting onto the caster regardless of who they were
    // previously engaged with. That's the whole point of taunt.
    if let Ok(mut e) = world.get_entity_mut(player) {
        e.insert(Fighting(target));
    }
    if let Ok(mut e) = world.get_entity_mut(target) {
        e.insert(Fighting(player));
    }
    // Push caster to the FRONT of the HateList so the combat
    // tick's re-engage pre-pass picks them when the current
    // Fighting clears (e.g. caster dies mid-combat). HateList::push
    // appends to the tail; we want the opposite for taunt, so
    // walk through and rotate.
    let already = world.get::<crate::combat::HateList>(target).is_some();
    if already {
        if let Some(mut h) = world.get_mut::<crate::combat::HateList>(target) {
            h.0.retain(|e| *e != player);
            h.0.push(player);
        }
    } else {
        let mut list = crate::combat::HateList::default();
        list.0.push(player);
        try_insert(world, target, list);
    }

    send_to(
        world,
        player,
        format!("You taunt {target_name} into focusing on you!\r\n"),
    );
    send_rendered(
        world,
        target,
        &format!("{player_name} taunts you into focusing on them!\r\n"),
    );
    broadcast_room_except_rendered(
        world,
        located.0,
        &[player, target],
        &format!("{player_name} taunts {target_name} into focusing the attack.\r\n"),
    );
}

pub(crate) fn cmd_disengage(world: &mut World, player: Entity, args: &str) {
    // Legacy: while casting, `disengage` is just another way to abort.
    if world.get::<mud_world::Casting>(player).is_some() {
        crate::commands::spells::cmd_abort(world, player, args);
        return;
    }
    if world.get::<Fighting>(player).is_none() {
        send_to(world, player, "You aren't fighting anyone.\r\n");
        return;
    }
    let old_target = world.get::<Fighting>(player).map(|f| f.0);
    try_remove::<Fighting>(world, player);
    stamp_reengage(world, player, old_target);
    send_to(world, player, "You stop fighting.\r\n");
}

#[cfg(test)]
mod attack_while_fighting_tests {
    use super::*;
    use crate::commands::test_support::{ability_def, drain, player_in};
    use mud_db::abilities::AbilityKind;
    use mud_world::{AbilityCatalog, KnownAbilities, PlayerFlags};

    const SWITCH: i32 = 7;

    /// Player "Tester" fighting mob "ogre" with a second mob "rat"
    /// in the room. Both mobs fight back.
    fn setup() -> (
        World,
        Entity,
        Entity,
        Entity,
        crate::commands::test_support::Rx,
    ) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (p, rx) = player_in(&mut world, room);
        world
            .entity_mut(p)
            .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
        let mk = |world: &mut World, name: &str| {
            world
                .spawn((
                    Mob,
                    Named { name: name.into() },
                    Located(room),
                    CombatStats::default(),
                    Health { hp: 100, max: 100 },
                ))
                .id()
        };
        let ogre = mk(&mut world, "ogre");
        let rat = mk(&mut world, "rat");
        world.entity_mut(p).insert(Fighting(ogre));
        world.entity_mut(ogre).insert(Fighting(p));
        (world, p, ogre, rat, rx)
    }

    fn hp(world: &World, e: Entity) -> i32 {
        world.get::<Health>(e).unwrap().hp
    }

    fn with_switch(world: &mut World, p: Entity, proficiency: i32) {
        let mut catalog = AbilityCatalog::default();
        catalog.by_name.insert(
            "switch".to_string(),
            ability_def(SWITCH, "Switch", AbilityKind::Skill),
        );
        world.insert_resource(catalog);
        world.entity_mut(p).insert(KnownAbilities {
            entries: vec![(SWITCH, proficiency, true)],
        });
    }

    #[test]
    fn kill_same_target_is_a_no_op() {
        let (mut world, p, ogre, _rat, mut rx) = setup();
        for _ in 0..3 {
            cmd_attack(&mut world, p, "ogre");
        }
        let out = drain(&mut rx);
        assert_eq!(
            out.matches("You're doing the best you can!").count(),
            3,
            "{out}"
        );
        assert!(!out.contains("You attack"), "{out}");
        assert_eq!(hp(&world, ogre), 100, "no free swing");
        assert_eq!(hp(&world, p), 100);
        assert_eq!(world.get::<Fighting>(p).map(|f| f.0), Some(ogre));
        assert_eq!(world.get::<Fighting>(ogre).map(|f| f.0), Some(p));
    }

    #[test]
    fn kill_other_without_switch_is_refused() {
        let (mut world, p, ogre, rat, mut rx) = setup();
        cmd_attack(&mut world, p, "rat");
        let out = drain(&mut rx);
        assert!(out.contains("You are already busy fighting with"), "{out}");
        assert_eq!(world.get::<Fighting>(p).map(|f| f.0), Some(ogre));
        assert!(world.get::<Fighting>(rat).is_none());
        assert_eq!(hp(&world, rat), 100);
    }

    #[test]
    fn failed_switch_roll_drops_the_fight_without_engaging() {
        let (mut world, p, ogre, _rat, mut rx) = setup();
        with_switch(&mut world, p, 500); // 50%
        assert!(!try_switch_opponent(&mut world, p, ogre, 51));
        assert!(drain(&mut rx).contains("become confused"));
        assert!(world.get::<Fighting>(p).is_none());
    }

    #[test]
    fn successful_switch_roll_clears_the_old_target() {
        let (mut world, p, ogre, _rat, mut rx) = setup();
        with_switch(&mut world, p, 500);
        assert!(try_switch_opponent(&mut world, p, ogre, 50));
        assert!(drain(&mut rx).contains("You switch opponents!"));
        assert!(world.get::<Fighting>(p).is_none());
    }

    #[test]
    fn kill_other_still_obeys_the_pk_rule() {
        let (mut world, p, ogre, _rat, mut rx) = setup();
        let room = world.get::<Located>(p).unwrap().0;
        let (victim, _vrx) = player_in(&mut world, room);
        world.entity_mut(victim).insert((
            Named {
                name: "Victim".into(),
            },
            CombatStats::default(),
            PlayerFlags(vec![]),
        ));
        world.entity_mut(p).insert(PlayerFlags(vec![]));
        cmd_attack(&mut world, p, "victim");
        let out = drain(&mut rx);
        assert!(out.contains("You must turn on PK first"), "{out}");
        assert!(!out.contains("busy fighting"), "{out}");
        assert_eq!(world.get::<Fighting>(p).map(|f| f.0), Some(ogre));
    }

    #[test]
    fn mob_switch_percent_follows_the_legacy_mob_roll_mean() {
        // 75 + 10 per level above the first, in tenths of a percent.
        assert_eq!(mob_switch_percent(1), 7);
        assert_eq!(mob_switch_percent(10), 16);
        assert_eq!(mob_switch_percent(50), 56);
        assert_eq!(mob_switch_percent(94), 100);
        assert_eq!(mob_switch_percent(200), 100);
        assert_eq!(mob_switch_percent(0), 7, "level floors at 1");
    }

    const MOB_CLASS: i32 = 3;

    /// Give the ogre (setup mob) a prototype of `level` and `class_id`,
    /// plus a `ClassSkills` row for `switch` on `MOB_CLASS` (when
    /// `switch_min_level` is `Some`), so `mob_switch_skill` finds them.
    fn with_mob_class(
        world: &mut World,
        mob: Entity,
        level: i32,
        class_id: Option<i32>,
        switch_min_level: Option<i32>,
    ) {
        use crate::commands::test_support::mob_proto;
        let mut protos = mud_world::MobPrototypes::default();
        let mut proto = mob_proto(9, 9, mud_db::enums::MobProfession::Trainer);
        proto.level = level;
        proto.class_id = class_id;
        protos.by_key.insert((9, 9), proto);
        world.insert_resource(protos);
        world
            .entity_mut(mob)
            .insert(mud_world::WorldKey { zone: 9, id: 9 });
        let mut catalog = AbilityCatalog::default();
        catalog.by_name.insert(
            "switch".to_string(),
            ability_def(SWITCH, "Switch", AbilityKind::Skill),
        );
        world.insert_resource(catalog);
        let mut skills = mud_world::ClassSkillsData::default();
        if let Some(min) = switch_min_level {
            skills.min_level.insert((MOB_CLASS, SWITCH), min);
        }
        world.insert_resource(skills);
    }

    /// A mob whose class learns Switch at level 1.
    fn with_mob_level(world: &mut World, mob: Entity, level: i32) {
        with_mob_class(world, mob, level, Some(MOB_CLASS), Some(1));
    }

    #[test]
    fn mob_switch_skill_needs_a_class_that_learns_it() {
        let (mut world, _p, ogre, _rat, _rx) = setup();
        with_mob_class(&mut world, ogre, 60, Some(MOB_CLASS), Some(1));
        assert_eq!(mob_switch_skill(&world, ogre), 66);
        // Classless mob.
        with_mob_class(&mut world, ogre, 60, None, Some(1));
        assert_eq!(mob_switch_skill(&world, ogre), 0);
        // Class without a Switch row.
        with_mob_class(&mut world, ogre, 60, Some(MOB_CLASS), None);
        assert_eq!(mob_switch_skill(&world, ogre), 0);
        // Class learns it later than the mob's level.
        with_mob_class(&mut world, ogre, 9, Some(MOB_CLASS), Some(10));
        assert_eq!(mob_switch_skill(&world, ogre), 0);
        with_mob_class(&mut world, ogre, 10, Some(MOB_CLASS), Some(10));
        assert_eq!(mob_switch_skill(&world, ogre), 16);
    }

    #[test]
    fn mob_without_the_class_skill_never_switches() {
        let (mut world, p, ogre, _rat, _rx) = setup();
        with_mob_class(&mut world, ogre, 60, Some(MOB_CLASS), None);
        attack_with_switch_roll(&mut world, ogre, "rat", 1);
        assert_eq!(
            world.get::<Fighting>(ogre).map(|f| f.0),
            Some(p),
            "refused outright: still fighting the player"
        );
    }

    #[test]
    fn low_level_mob_usually_fails_to_switch() {
        let (mut world, p, ogre, rat, _rx) = setup();
        with_mob_level(&mut world, ogre, 1); // 7%
        attack_with_switch_roll(&mut world, ogre, "rat", 50);
        assert!(
            world.get::<Fighting>(ogre).is_none(),
            "a failed switch drops the fight"
        );
        assert!(world.get::<Fighting>(rat).is_none());
        assert_eq!(world.get::<Fighting>(p).map(|f| f.0), Some(ogre));
    }

    #[test]
    fn low_level_mob_switches_on_a_lucky_roll() {
        let (mut world, _p, ogre, rat, _rx) = setup();
        with_mob_level(&mut world, ogre, 1); // 7%
        attack_with_switch_roll(&mut world, ogre, "rat", 7);
        assert_eq!(world.get::<Fighting>(ogre).map(|f| f.0), Some(rat));
    }

    #[test]
    fn high_level_mob_switches_on_the_same_roll() {
        let (mut world, _p, ogre, rat, _rx) = setup();
        with_mob_level(&mut world, ogre, 60); // 66%
        attack_with_switch_roll(&mut world, ogre, "rat", 50);
        assert_eq!(world.get::<Fighting>(ogre).map(|f| f.0), Some(rat));
    }

    #[test]
    fn high_level_mob_still_fails_above_its_percent() {
        let (mut world, _p, ogre, rat, _rx) = setup();
        with_mob_level(&mut world, ogre, 60); // 66%
        attack_with_switch_roll(&mut world, ogre, "rat", 67);
        assert!(world.get::<Fighting>(ogre).is_none());
        assert!(world.get::<Fighting>(rat).is_none());
    }

    use mud_world::{AttachedTriggers, TriggerAttach, TriggerCatalog, TriggerDef, TriggerEvent};

    /// Player "Tester" (not fighting) with sleeping, auto-hit mobs
    /// "ogre" (carries an ATTACK trigger) and "rat"; `TickCount` at 0.
    fn fresh() -> (
        World,
        Entity,
        Entity,
        Entity,
        crate::commands::test_support::Rx,
    ) {
        let mut world = World::new();
        world.insert_resource(crate::TickCount(0));
        let mut catalog = TriggerCatalog::default();
        catalog.by_key.insert(
            (99, 1),
            TriggerDef {
                zone_id: 99,
                id: 1,
                name: "t".to_string(),
                attach_type: TriggerAttach::Mob,
                commands: "return".to_string(),
                flags: vec![TriggerEvent::Attack],
                arg_list: vec![],
                num_args: 0,
            },
        );
        world.insert_resource(catalog);
        world.insert_resource(mud_script::LuaHost::new());
        let room = world.spawn_empty().id();
        let (p, rx) = player_in(&mut world, room);
        world
            .entity_mut(p)
            .insert((CombatStats::default(), Health { hp: 100, max: 100 }));
        let mk = |world: &mut World, name: &str| {
            world
                .spawn((
                    Mob,
                    Named { name: name.into() },
                    Located(room),
                    CombatStats::default(),
                    Health { hp: 100, max: 100 },
                    Posture(PostureKind::Sleeping),
                ))
                .id()
        };
        let ogre = mk(&mut world, "ogre");
        let rat = mk(&mut world, "rat");
        world
            .entity_mut(ogre)
            .insert(AttachedTriggers(vec![(99, 1)]));
        (world, p, ogre, rat, rx)
    }

    fn attack_triggers(world: &World) -> u64 {
        world
            .get_resource::<crate::triggers::TriggerStats>()
            .and_then(|s| s.by_event.get("Attack"))
            .map_or(0, |c| c.fired)
    }

    fn advance(world: &mut World, ticks: u64) {
        world.resource_mut::<crate::TickCount>().0 += ticks;
    }

    #[test]
    fn disengage_then_kill_gets_no_free_round() {
        let (mut world, p, ogre, _rat, _rx) = fresh();
        cmd_attack(&mut world, p, "ogre");
        assert_eq!(hp(&world, ogre), 99, "fresh engage swings once");
        assert_eq!(attack_triggers(&world), 1);
        for _ in 0..3 {
            cmd_disengage(&mut world, p, "");
            cmd_attack(&mut world, p, "ogre");
            assert_eq!(world.get::<Fighting>(p).map(|f| f.0), Some(ogre));
        }
        assert_eq!(hp(&world, ogre), 99, "no instant swing inside the window");
        assert_eq!(attack_triggers(&world), 1, "ATTACK fires once per window");
    }

    #[test]
    fn a_fresh_engage_after_the_window_swings_again() {
        let (mut world, p, ogre, _rat, _rx) = fresh();
        cmd_attack(&mut world, p, "ogre");
        cmd_disengage(&mut world, p, "");
        advance(&mut world, REENGAGE_LAG_TICKS);
        // Damage woke it; put it back to sleep so the swing auto-hits.
        world
            .entity_mut(ogre)
            .insert(Posture(PostureKind::Sleeping));
        cmd_attack(&mut world, p, "ogre");
        assert_eq!(hp(&world, ogre), 98);
        assert_eq!(attack_triggers(&world), 2);
    }

    #[test]
    fn disengage_then_kill_other_still_needs_the_switch_skill() {
        let (mut world, p, ogre, rat, mut rx) = fresh();
        cmd_attack(&mut world, p, "ogre");
        cmd_disengage(&mut world, p, "");
        drain(&mut rx);
        cmd_attack(&mut world, p, "rat");
        let out = drain(&mut rx);
        assert!(out.contains("You are already busy fighting with"), "{out}");
        assert!(world.get::<Fighting>(p).is_none());
        assert_eq!(hp(&world, rat), 100);
        let _ = ogre;
    }

    #[test]
    fn failed_switch_roll_then_kill_gets_no_free_round() {
        let (mut world, p, ogre, rat, mut rx) = fresh();
        with_switch(&mut world, p, 500);
        cmd_attack(&mut world, p, "ogre");
        advance(&mut world, REENGAGE_LAG_TICKS);
        attack_with_switch_roll(&mut world, p, "rat", 101);
        assert!(drain(&mut rx).contains("become confused"));
        assert!(world.get::<Fighting>(p).is_none());
        // The very next kill, same or other, is lagged.
        attack_with_switch_roll(&mut world, p, "ogre", 1);
        assert_eq!(hp(&world, ogre), 99, "no swing after the failed switch");
        attack_with_switch_roll(&mut world, p, "rat", 101);
        assert_eq!(hp(&world, rat), 100);
    }

    #[test]
    fn successful_switch_swings_once_through_cmd_attack() {
        let (mut world, p, ogre, rat, mut rx) = fresh();
        with_switch(&mut world, p, 500);
        cmd_attack(&mut world, p, "ogre");
        advance(&mut world, REENGAGE_LAG_TICKS);
        attack_with_switch_roll(&mut world, p, "rat", 50);
        assert!(drain(&mut rx).contains("You switch opponents!"));
        assert_eq!(world.get::<Fighting>(p).map(|f| f.0), Some(rat));
        assert_eq!(hp(&world, rat), 99, "one swing at the new target");
        assert_eq!(hp(&world, ogre), 99);
        // Switching straight back inside the new window: no free swing.
        attack_with_switch_roll(&mut world, p, "ogre", 50);
        assert_eq!(hp(&world, ogre), 99);
    }

    #[test]
    fn pet_target_alternation_gets_no_free_swings() {
        let (mut world, _p, ogre, rat, _rx) = fresh();
        let room = world.get::<Located>(ogre).unwrap().0;
        let pet = world
            .spawn((
                Mob,
                Named { name: "pet".into() },
                Located(room),
                CombatStats::default(),
                Health { hp: 100, max: 100 },
            ))
            .id();
        // A class that learns Switch, so the mob has the skill at all.
        with_mob_class(&mut world, pet, 1, Some(MOB_CLASS), Some(1));
        cmd_attack(&mut world, pet, "ogre");
        assert_eq!(hp(&world, ogre), 99);
        for _ in 0..3 {
            // Roll 1 always passes the mob's Switch check.
            attack_with_switch_roll(&mut world, pet, "rat", 1);
            assert_eq!(world.get::<Fighting>(pet).map(|f| f.0), Some(rat));
            attack_with_switch_roll(&mut world, pet, "ogre", 1);
            assert_eq!(world.get::<Fighting>(pet).map(|f| f.0), Some(ogre));
        }
        assert_eq!(hp(&world, ogre), 99);
        assert_eq!(hp(&world, rat), 100);
    }

    #[test]
    fn a_kill_clears_the_lag_for_the_next_target() {
        let (mut world, p, ogre, rat, _rx) = fresh();
        cmd_attack(&mut world, p, "ogre");
        crate::commands::disengage_attackers_of(&mut world, ogre);
        cmd_attack(&mut world, p, "rat");
        assert_eq!(hp(&world, rat), 99, "next target swings right away");
    }
}
