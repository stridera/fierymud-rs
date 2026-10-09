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
