//! Group membership is consent-based (legacy `do_group`): `follow` needs no
//! consent and never makes anyone a group member; the leader's `invite` plus
//! the target's `accept` does. Group-scoped features (group spells, `gsay`,
//! XP split, `split`) read the membership, not the follow tree. Test-only.

use bevy_ecs::prelude::*;
use mud_world::{GroupMember, group_members, group_root};

use super::dispatch;
use super::gmcp_tests::{Fx, fixture, player};
use super::test_support::{Rx, drain};

struct Trio {
    fx: Fx,
    leader: Entity,
    lrx: Rx,
    friend: Entity,
    frx: Rx,
    stalker: Entity,
    srx: Rx,
}

fn trio() -> Trio {
    let mut fx = fixture();
    let a = fx.a;
    let (leader, lrx) = player(&mut fx.world, a, "Leader");
    let (friend, frx) = player(&mut fx.world, a, "Friend");
    let (stalker, srx) = player(&mut fx.world, a, "Mallory");
    Trio {
        fx,
        leader,
        lrx,
        friend,
        frx,
        stalker,
        srx,
    }
}

fn say(fx: &mut Fx, who: Entity, rx: &mut Rx, line: &str) -> String {
    let _ = drain(rx);
    dispatch(&mut fx.world, who, line);
    drain(rx)
}

fn grouped(fx: &mut Fx, leader: Entity, lrx: &mut Rx, member: Entity, mrx: &mut Rx, name: &str) {
    say(fx, leader, lrx, &format!("invite {name}"));
    say(fx, member, mrx, "accept");
}

#[test]
fn following_a_player_does_not_put_you_in_their_group() {
    let mut t = trio();
    let out = say(&mut t.fx, t.stalker, &mut t.srx, "follow leader");
    assert!(out.contains("You start following"), "{out}");
    assert!(
        t.fx.world.get::<mud_world::Follower>(t.stalker).is_some(),
        "the follow link is made"
    );
    assert!(t.fx.world.get::<GroupMember>(t.stalker).is_none());
    assert_eq!(group_root(&t.fx.world, t.stalker), t.stalker);
    assert_eq!(group_members(&mut t.fx.world, t.leader), vec![t.leader]);
    let out = say(&mut t.fx, t.leader, &mut t.lrx, "group");
    assert!(out.contains("not in a group"), "{out}");
}

#[test]
fn invite_and_accept_make_a_member_and_a_follower() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    assert_eq!(group_root(&t.fx.world, t.friend), t.leader);
    assert_eq!(group_root(&t.fx.world, t.leader), t.leader);
    let members = group_members(&mut t.fx.world, t.leader);
    assert_eq!(members.len(), 2);
    assert!(members.contains(&t.friend));
    assert!(
        t.fx.world
            .get::<mud_world::Follower>(t.friend)
            .is_some_and(|f| f.0 == t.leader),
        "a new member also falls in behind the leader"
    );
}

#[test]
fn a_follower_is_not_pulled_into_gsay_by_following() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    say(&mut t.fx, t.stalker, &mut t.srx, "follow leader");
    say(&mut t.fx, t.leader, &mut t.lrx, "gsay hello team");
    let friend_heard = drain(&mut t.frx);
    let stalker_heard = drain(&mut t.srx);
    assert!(friend_heard.contains("hello team"), "{friend_heard}");
    assert!(!stalker_heard.contains("hello team"), "{stalker_heard}");
}

#[test]
fn only_the_head_of_a_group_can_enrol_and_nobody_joins_two_groups() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    // A member cannot enrol others.
    let out = say(&mut t.fx, t.friend, &mut t.frx, "invite mallory");
    assert!(out.contains("without being head of a group"), "{out}");
    assert!(
        t.fx.world
            .get::<mud_world::GroupInvite>(t.stalker)
            .is_none()
    );
    // Someone already in a group cannot be invited into another.
    let out = say(&mut t.fx, t.stalker, &mut t.srx, "invite friend");
    assert!(out.contains("already in a group"), "{out}");
    // Nor can a group leader be.
    let out = say(&mut t.fx, t.stalker, &mut t.srx, "invite leader");
    assert!(out.contains("leading a group"), "{out}");
}

#[test]
fn unfollowing_your_leader_leaves_the_group() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    say(&mut t.fx, t.friend, &mut t.frx, "unfollow");
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_none());
    assert_eq!(group_members(&mut t.fx.world, t.leader), vec![t.leader]);
}

#[test]
fn dismiss_and_disband_drop_members_and_followers() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    say(&mut t.fx, t.stalker, &mut t.srx, "follow leader");
    say(&mut t.fx, t.leader, &mut t.lrx, "dismiss friend");
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_none());
    assert!(t.fx.world.get::<mud_world::Follower>(t.friend).is_none());
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    say(&mut t.fx, t.leader, &mut t.lrx, "disband");
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_none());
    assert!(t.fx.world.get::<mud_world::Follower>(t.friend).is_none());
    assert!(t.fx.world.get::<mud_world::Follower>(t.stalker).is_none());
}

#[test]
fn a_dead_leader_leaves_no_phantom_group() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    t.fx.world.despawn(t.leader);
    assert_eq!(group_root(&t.fx.world, t.friend), t.friend);
}

/// Leader plus two members: Friend and Mallory both accepted.
fn full_group() -> Trio {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.stalker, &mut t.srx, "mallory",
    );
    assert_eq!(group_members(&mut t.fx.world, t.leader).len(), 3);
    t
}

#[test]
fn the_leader_leaving_hands_the_group_to_the_next_member() {
    let mut t = full_group();
    let _ = (drain(&mut t.frx), drain(&mut t.srx), drain(&mut t.lrx));
    super::ungroup(&mut t.fx.world, t.leader, true, false);
    t.fx.world.despawn(t.leader);

    let (new_lead, other) = if t.fx.world.get::<GroupMember>(t.friend).is_none() {
        (t.friend, t.stalker)
    } else {
        (t.stalker, t.friend)
    };
    assert!(t.fx.world.get::<GroupMember>(new_lead).is_none());
    assert_eq!(group_root(&t.fx.world, other), new_lead);
    assert_eq!(group_members(&mut t.fx.world, new_lead).len(), 2);
    assert!(
        t.fx.world
            .get::<mud_world::Follower>(new_lead)
            .is_none_or(|f| f.0 != t.leader),
        "nobody keeps following the departed leader"
    );
    let (lead_rx, other_rx) = if new_lead == t.friend {
        (&mut t.frx, &mut t.srx)
    } else {
        (&mut t.srx, &mut t.frx)
    };
    let lead_out = drain(lead_rx);
    let other_out = drain(other_rx);
    assert!(
        lead_out.contains("You're now leading the group!"),
        "{lead_out}"
    );
    assert!(
        other_out.contains("is now leading your group!"),
        "{other_out}"
    );
}

#[test]
fn a_member_leaving_leaves_the_group_going() {
    let mut t = full_group();
    let _ = (drain(&mut t.lrx), drain(&mut t.srx));
    super::ungroup(&mut t.fx.world, t.friend, true, false);
    t.fx.world.despawn(t.friend);

    assert_eq!(group_members(&mut t.fx.world, t.leader).len(), 2);
    assert_eq!(group_root(&t.fx.world, t.stalker), t.leader);
    let lead_out = drain(&mut t.lrx);
    let other_out = drain(&mut t.srx);
    assert!(
        lead_out.contains("Friend has left your group!"),
        "{lead_out}"
    );
    assert!(
        other_out.contains("Friend has left your group!"),
        "{other_out}"
    );
}

#[test]
fn a_two_person_group_dissolves_when_either_side_leaves() {
    let mut t = trio();
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    let _ = drain(&mut t.frx);
    super::ungroup(&mut t.fx.world, t.leader, true, false);
    t.fx.world.despawn(t.leader);
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_none());
    assert_eq!(group_members(&mut t.fx.world, t.friend), vec![t.friend]);
    let out = drain(&mut t.frx);
    assert!(out.contains("Leader has disbanded the group."), "{out}");
}

#[test]
fn a_member_with_no_follow_link_can_still_leave() {
    let mut t = trio();
    // The leader is already following Friend, so accepting cannot make
    // Friend follow back (that would be a cycle).
    say(&mut t.fx, t.leader, &mut t.lrx, "follow friend");
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_some());
    assert!(t.fx.world.get::<mud_world::Follower>(t.friend).is_none());

    let out = say(&mut t.fx, t.friend, &mut t.frx, "unfollow");
    assert!(out.contains("You have left your group!"), "{out}");
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_none());

    // `follow self` is the same exit.
    grouped(
        &mut t.fx, t.leader, &mut t.lrx, t.friend, &mut t.frx, "friend",
    );
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_some());
    say(&mut t.fx, t.friend, &mut t.frx, "follow self");
    assert!(t.fx.world.get::<GroupMember>(t.friend).is_none());
}
