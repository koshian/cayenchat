// SPDX-License-Identifier: MPL-2.0
use super::*;

fn users(list: &[&str]) -> Vec<String> {
    list.iter().map(|user| (*user).to_owned()).collect()
}

fn carried(channel: &str, now: Instant) -> OpRegain {
    OpRegain::new(true, &[channel.to_owned()], now)
}

fn flags(_: &str) -> Option<String> {
    Some("ntskl".into())
}

/// Joined and synced with `roster`.
fn arrive(regain: &mut OpRegain, roster: &[&str], now: Instant) -> Vec<Step> {
    regain.joined("#a");
    regain.topic("#a", "hello");
    regain.roster("#a", &users(&["Me_"]), "Me_", false, now);
    regain.roster("#a", &users(roster), "Me_", true, now)
}

#[test]
fn alone_without_op_rejoins_once_after_the_wait() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    assert!(arrive(&mut regain, &["Me_"], start).is_empty());
    assert_eq!(regain.next_deadline(), Some(start + STABLE));
    assert!(regain.poll(start, true, flags).is_empty());
    let steps = regain.poll(start + STABLE, true, flags);
    assert_eq!(
        steps,
        [
            Step::Rejoin("#a".into()),
            Step::Status("#a".into(), OpRegainStatus::Rejoining)
        ]
    );
    // The echoes of the PART and the new JOIN, then the new channel's NAMES.
    regain.parted("#a");
    regain.joined("#a");
    let done = regain.roster("#a", &users(&["@Me_"]), "Me_", true, start + STABLE);
    assert_eq!(
        done,
        [Step::Restore {
            channel: "#a".into(),
            flags: "nts".into(),
            topic: Some("hello".into()),
        }]
    );
    // The server shows both: reported once it is confirmed.
    regain.topic("#a", "hello");
    let judged = start + STABLE + RESTORE_WAIT;
    assert_eq!(
        regain.poll(judged, true, |_| Some("nts".into())),
        [Step::Status(
            "#a".into(),
            OpRegainStatus::Regained {
                modes: true,
                topic: true
            }
        )]
    );
    // Once only: alone and opless again later changes nothing.
    regain.roster("#a", &users(&["Me_"]), "Me_", false, start + STABLE);
    assert_eq!(regain.next_deadline(), None);
}

#[test]
fn others_present_or_an_op_cancel() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    arrive(&mut regain, &["Me_", "Other"], start);
    assert!(regain.poll(start + STABLE, true, flags).is_empty());
    // The other leaves: alone, the wait starts now.
    regain.roster("#a", &users(&["Me_"]), "Me_", false, start + STABLE);
    assert_eq!(regain.next_deadline(), Some(start + STABLE * 2));
    // Someone joins during the wait.
    regain.roster("#a", &users(&["Me_", "Late"]), "Me_", false, start + STABLE);
    assert!(regain.poll(start + STABLE * 3, true, flags).is_empty());
    // We were made operator.
    regain.roster("#a", &users(&["@Me_"]), "Me_", false, start + STABLE);
    assert!(regain.poll(start + STABLE * 3, true, flags).is_empty());
}

#[test]
fn unsynced_not_carried_or_ineligible_never_act() {
    let start = Instant::now();
    // Not carried: opless and alone is nothing special.
    let mut plain = OpRegain::new(true, &[], start);
    arrive(&mut plain, &["Me_"], start);
    assert_eq!(plain.next_deadline(), None);
    // The partial roster after our own JOIN does not count.
    let mut early = carried("#a", start);
    early.joined("#a");
    early.roster("#a", &users(&["Me_"]), "Me_", false, start);
    assert_eq!(early.next_deadline(), Some(start + CARRY_WINDOW));
    // Disabled.
    let mut off = OpRegain::new(false, &["#a".into()], start);
    arrive(&mut off, &["Me_"], start);
    assert_eq!(off.next_deadline(), None);
    assert_eq!(off.changed_ops(), None);
    // `!` and `+` channels are not touched.
    let mut safe = OpRegain::new(true, &["!ABCDEsafe".into(), "+modeless".into()], start);
    safe.joined("!ABCDEsafe");
    safe.roster("!ABCDEsafe", &users(&["Me_"]), "Me_", true, start);
    assert_eq!(safe.next_deadline(), None);
}

#[test]
fn not_ready_or_unknown_modes_wait_and_a_manual_part_cancels() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    arrive(&mut regain, &["Me_"], start);
    assert!(regain.poll(start + STABLE, false, flags).is_empty());
    assert!(regain.poll(start + STABLE * 2, true, |_| None).is_empty());
    assert!(!regain.poll(start + STABLE * 3, true, flags).is_empty());
    let mut parted = carried("#a", start);
    arrive(&mut parted, &["Me_"], start);
    parted.parted("#a");
    assert_eq!(parted.next_deadline(), None);
    // Sending the PART is enough: no echo is waited for.
    let mut leaving = carried("#a", start);
    arrive(&mut leaving, &["Me_"], start);
    leaving.leaving("#A");
    assert_eq!(leaving.next_deadline(), None);
    assert!(leaving.poll(start + STABLE * 3, true, flags).is_empty());
}

#[test]
fn failures_end_the_attempt_without_a_retry() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    regain.parted("#a");
    assert_eq!(
        regain.join_failed("#a"),
        [Step::Status("#a".into(), OpRegainStatus::RejoinFailed)]
    );
    assert_eq!(regain.next_deadline(), None);

    // Joined again, but not made operator.
    let mut regain = carried("#a", start);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    regain.parted("#a");
    regain.joined("#a");
    assert_eq!(
        regain.roster("#a", &users(&["Me_"]), "Me_", true, start + STABLE),
        [Step::Status("#a".into(), OpRegainStatus::NotRegained)]
    );
    assert_eq!(regain.next_deadline(), None);

    // No answer at all.
    let mut regain = carried("#a", start);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    let late = start + STABLE + ATTEMPT_TIMEOUT;
    assert_eq!(
        regain.poll(late, true, flags),
        [Step::Status("#a".into(), OpRegainStatus::NotRegained)]
    );
}

#[test]
fn the_carried_loss_expires() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    assert_eq!(regain.next_deadline(), Some(start + CARRY_WINDOW));
    regain.poll(start + CARRY_WINDOW, true, flags);
    assert_eq!(regain.next_deadline(), None);
    assert_eq!(regain.changed_ops(), Some(Vec::new()));
}

#[test]
fn operator_channels_are_reported_when_they_change() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true, &[], start);
    assert_eq!(regain.changed_ops(), None);
    regain.joined("#a");
    regain.roster("#a", &users(&["@Me", "x"]), "me", true, start);
    assert_eq!(regain.changed_ops(), Some(vec!["#a".into()]));
    regain.roster("#a", &users(&["@Me", "x", "y"]), "me", false, start);
    assert_eq!(regain.changed_ops(), None);
    regain.roster("#a", &users(&["Me", "x"]), "me", false, start);
    assert_eq!(regain.changed_ops(), Some(Vec::new()));
}

/// Operator again after a rejoin, with the restoring commands sent.
fn restoring(topic: &str, start: Instant) -> OpRegain {
    let mut regain = carried("#a", start);
    regain.joined("#a");
    regain.topic("#a", topic);
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start);
    regain.poll(start + STABLE, true, |_| Some("ntkl".into()));
    regain.parted("#a");
    regain.joined("#a");
    let steps = regain.roster("#a", &users(&["@Me_"]), "Me_", true, start + STABLE);
    assert!(matches!(steps[..], [Step::Restore { .. }]));
    regain
}

#[test]
fn a_restore_the_server_did_not_show_is_reported_as_failed() {
    let start = Instant::now();
    let judged = start + STABLE + RESTORE_WAIT;
    // Nothing came back (both commands refused).
    let mut regain = restoring("hello", start);
    assert_eq!(regain.next_deadline(), Some(judged));
    assert!(regain.poll(judged - STABLE, true, |_| None).is_empty());
    assert_eq!(
        regain.poll(judged, true, |_| Some(String::new())),
        [Step::Status(
            "#a".into(),
            OpRegainStatus::RestoreFailed {
                modes: true,
                topic: true
            }
        )]
    );
    assert_eq!(regain.next_deadline(), None);
    // Only the topic arrived.
    let mut regain = restoring("hello", start);
    regain.topic("#a", "other");
    assert_eq!(
        regain.poll(judged, true, |_| Some("nt".into())),
        [Step::Status(
            "#a".into(),
            OpRegainStatus::RestoreFailed {
                modes: false,
                topic: true
            }
        )]
    );
}

#[test]
fn a_topic_that_cannot_be_put_back_keeps_the_channel_as_it_is() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    regain.joined("#a");
    regain.topic("#a", &"あ".repeat(MAX_TOPIC / 3 + 1));
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start);
    assert_eq!(
        regain.poll(start + STABLE, true, flags),
        [Step::Status("#a".into(), OpRegainStatus::TopicTooLong)]
    );
    assert_eq!(regain.next_deadline(), None);
    // The limit itself still works.
    let mut regain = carried("#a", start);
    regain.joined("#a");
    regain.topic("#a", &"x".repeat(MAX_TOPIC));
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start);
    assert!(matches!(
        regain.poll(start + STABLE, true, flags)[0],
        Step::Rejoin(_)
    ));
}

#[test]
fn unusual_flags_are_left_out() {
    let start = Instant::now();
    let mut regain = carried("#a", start);
    regain.joined("#a");
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start);
    regain.poll(start + STABLE, true, |_| Some("kl".into()));
    regain.parted("#a");
    regain.joined("#a");
    let done = regain.roster("#a", &users(&["@Me_"]), "Me_", true, start);
    assert_eq!(
        done,
        [Step::Status(
            "#a".into(),
            OpRegainStatus::Regained {
                modes: false,
                topic: false
            }
        )]
    );
}
