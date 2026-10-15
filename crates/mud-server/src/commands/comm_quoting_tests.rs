//! Issue #157: every communication line quotes the spoken text with single
//! quotes, as legacy did (`$n says, '...'`), in the text a player reads and
//! in the GMCP `Comm.Channel.Text` body. Test-only.

use bevy_ecs::prelude::*;
use mud_world::GroupMember;

use super::dispatch;
use super::gmcp_tests::{Fx, drain_bytes, fixture, frames, of, player};
use super::test_support::Rx;

/// A player and the receiver of everything sent to them.
type Seat = (Entity, Rx);

/// Alice and Bob in one room, a third player (Cara) in the other room.
fn trio() -> (Fx, Seat, Seat, Seat) {
    let mut fx = fixture();
    let (a, b) = (fx.a, fx.b);
    let alice = player(&mut fx.world, a, "Alice");
    let bob = player(&mut fx.world, a, "Bob");
    let cara = player(&mut fx.world, b, "Cara");
    (fx, alice, bob, cara)
}

/// What the player read (GMCP framing stripped) and the GMCP chat bodies.
fn read(rx: &mut Rx) -> (String, Vec<String>) {
    let bytes = drain_bytes(rx);
    let gmcp: Vec<String> = of(&frames(&bytes), "Comm.Channel.Text")
        .iter()
        .map(|j| {
            serde_json::from_str::<serde_json::Value>(j).expect("json")["text"]
                .as_str()
                .expect("text")
                .to_string()
        })
        .collect();
    let mut text = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&[255, 250]) {
            while i + 1 < bytes.len() && !(bytes[i] == 255 && bytes[i + 1] == 240) {
                i += 1;
            }
            i += 2;
        } else if bytes[i] == 0x1b {
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            i += 1;
        } else {
            text.push(bytes[i] as char);
            i += 1;
        }
    }
    (text, gmcp)
}

fn assert_single_quoted(what: &str, text: &str, gmcp: &[String], quoted: &str) {
    let expected = format!("'{quoted}'");
    assert!(text.contains(&expected), "{what} text: {text:?}");
    assert!(
        !text.contains('"'),
        "{what} text has a double quote: {text:?}"
    );
    for g in gmcp {
        assert!(!g.contains('"'), "{what} GMCP has a double quote: {g:?}");
    }
}

#[test]
fn say_uses_single_quotes_for_speaker_and_listener() {
    let (mut fx, (alice, mut arx), (_bob, mut brx), _cara) = trio();
    dispatch(&mut fx.world, alice, "say hello there");
    let (mine, mine_gmcp) = read(&mut arx);
    assert!(mine.contains("You say, 'hello there'"), "{mine:?}");
    assert_single_quoted("say (speaker)", &mine, &mine_gmcp, "hello there");
    let (theirs, their_gmcp) = read(&mut brx);
    assert!(theirs.contains("Alice says, 'hello there'"), "{theirs:?}");
    assert_single_quoted("say (listener)", &theirs, &their_gmcp, "hello there");
    assert!(
        their_gmcp.iter().any(|g| g == "Alice says, 'hello there'"),
        "{their_gmcp:?}"
    );
}

#[test]
fn tell_and_reply_use_single_quotes() {
    let (mut fx, (alice, mut arx), (bob, mut brx), _cara) = trio();
    dispatch(&mut fx.world, alice, "tell bob meet me");
    let (mine, mine_gmcp) = read(&mut arx);
    assert!(mine.contains("You tell Bob, 'meet me'"), "{mine:?}");
    assert_single_quoted("tell (sender)", &mine, &mine_gmcp, "meet me");
    let (theirs, their_gmcp) = read(&mut brx);
    assert!(theirs.contains("Alice tells you, 'meet me'"), "{theirs:?}");
    assert_single_quoted("tell (target)", &theirs, &their_gmcp, "meet me");

    dispatch(&mut fx.world, bob, "reply on my way");
    let (reply, reply_gmcp) = read(&mut brx);
    assert!(reply.contains("You tell Alice, 'on my way'"), "{reply:?}");
    assert_single_quoted("reply (sender)", &reply, &reply_gmcp, "on my way");
    let (got, got_gmcp) = read(&mut arx);
    assert!(got.contains("Bob tells you, 'on my way'"), "{got:?}");
    assert_single_quoted("reply (target)", &got, &got_gmcp, "on my way");
}

#[test]
fn whisper_and_ask_use_single_quotes() {
    let (mut fx, (alice, mut arx), (_bob, mut brx), _cara) = trio();
    dispatch(&mut fx.world, alice, "whisper bob psst");
    let (mine, mine_gmcp) = read(&mut arx);
    assert!(mine.contains("You whisper to Bob, 'psst'"), "{mine:?}");
    assert_single_quoted("whisper (sender)", &mine, &mine_gmcp, "psst");
    let (theirs, their_gmcp) = read(&mut brx);
    assert!(
        theirs.contains("Alice whispers to you, 'psst'"),
        "{theirs:?}"
    );
    assert_single_quoted("whisper (target)", &theirs, &their_gmcp, "psst");

    dispatch(&mut fx.world, alice, "ask bob the key");
    let (mine, mine_gmcp) = read(&mut arx);
    assert!(mine.contains("You ask Bob about 'the key'"), "{mine:?}");
    assert_single_quoted("ask (sender)", &mine, &mine_gmcp, "the key");
    let (_theirs, their_gmcp) = read(&mut brx);
    assert!(
        their_gmcp.iter().all(|g| !g.contains('"')) && !their_gmcp.is_empty(),
        "{their_gmcp:?}"
    );
}

#[test]
fn ask_shows_the_topic_to_the_target_and_hides_it_from_bystanders() {
    let (mut fx, (alice, mut arx), (_bob, mut brx), _cara) = trio();
    let a = fx.a;
    let (_dave, mut drx) = player(&mut fx.world, a, "Dave");
    dispatch(&mut fx.world, alice, "ask bob the key");
    let (mine, _) = read(&mut arx);
    assert!(mine.contains("You ask Bob about 'the key'"), "{mine:?}");
    let (theirs, _) = read(&mut brx);
    assert!(
        theirs.contains("Alice asks you about 'the key'"),
        "{theirs:?}"
    );
    assert!(!theirs.contains("something"), "{theirs:?}");
    let (bystander, _) = read(&mut drx);
    assert!(
        bystander.contains("Alice asks Bob about something."),
        "{bystander:?}"
    );
    assert!(!bystander.contains("the key"), "{bystander:?}");
}

#[test]
fn global_channels_use_single_quotes() {
    let (mut fx, (alice, mut arx), _bob, (_cara, mut crx)) = trio();
    for (command, mine_verb, theirs_verb) in [
        ("gossip news", "You gossip", "Alice gossips"),
        ("shout loud", "You shout", "Alice shouts"),
        ("qsay clue", "You qsay", "Alice qsays"),
    ] {
        let said = command.split_once(' ').unwrap().1;
        dispatch(&mut fx.world, alice, command);
        let (mine, mine_gmcp) = read(&mut arx);
        assert!(
            mine.contains(&format!("{mine_verb}, '{said}'")),
            "{command}: {mine:?}"
        );
        assert_single_quoted(command, &mine, &mine_gmcp, said);
        let (theirs, their_gmcp) = read(&mut crx);
        assert!(
            theirs.contains(&format!("{theirs_verb}, '{said}'")),
            "{command}: {theirs:?}"
        );
        assert_single_quoted(command, &theirs, &their_gmcp, said);
    }
}

#[test]
fn group_say_uses_single_quotes() {
    let (mut fx, (alice, mut arx), (bob, mut brx), _cara) = trio();
    fx.world.entity_mut(bob).insert(GroupMember(alice));
    dispatch(&mut fx.world, alice, "gsay form up");
    let (mine, mine_gmcp) = read(&mut arx);
    assert!(mine.contains("You group-say, 'form up'"), "{mine:?}");
    assert_single_quoted("gsay (speaker)", &mine, &mine_gmcp, "form up");
    let (theirs, their_gmcp) = read(&mut brx);
    assert!(
        theirs.contains("(Alice group-says) 'form up'"),
        "{theirs:?}"
    );
    assert_single_quoted("gsay (member)", &theirs, &their_gmcp, "form up");
}

#[test]
fn channel_history_quotes_with_single_quotes_and_keeps_inner_quotes() {
    let (mut fx, (alice, mut arx), ..) = trio();
    dispatch(&mut fx.world, alice, "gossip it is \"fine\"");
    drain_bytes(&mut arx);
    dispatch(&mut fx.world, alice, "lastgossips");
    let (out, _) = read(&mut arx);
    assert!(out.contains("Alice: 'it is \"fine\"'"), "{out:?}");
}
