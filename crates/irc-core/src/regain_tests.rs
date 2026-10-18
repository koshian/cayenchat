// SPDX-License-Identifier: MPL-2.0
use super::*;

fn users(list: &[&str]) -> Vec<String> {
    list.iter().map(|user| (*user).to_owned()).collect()
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
    let mut regain = OpRegain::new(true);
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
    // Operator status was seen, so a later loss of it gets another try.
    regain.roster("#a", &users(&["Me_"]), "Me_", false, judged);
    assert_eq!(regain.next_deadline(), Some(judged + STABLE));
}

#[test]
fn one_attempt_per_connection_until_operator_or_a_manual_rejoin() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    regain.parted("#a");
    regain.joined("#a");
    // Joined again without operator status.
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start + STABLE);
    assert_eq!(regain.next_deadline(), None);
    regain.roster("#a", &users(&["Me_", "x"]), "Me_", false, start + STABLE);
    regain.roster("#a", &users(&["Me_"]), "Me_", false, start + STABLE);
    assert_eq!(regain.next_deadline(), None);
    // Someone gives us operator status, then takes it back.
    regain.roster("#a", &users(&["@Me_", "x"]), "Me_", false, start + STABLE);
    regain.roster("#a", &users(&["Me_"]), "Me_", false, start + STABLE);
    assert_eq!(regain.next_deadline(), Some(start + STABLE * 2));
    assert!(!regain.poll(start + STABLE * 2, true, flags).is_empty());

    // The user leaves and joins again by hand.
    let mut regain = OpRegain::new(true);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    regain.parted("#a");
    regain.joined("#a");
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start + STABLE);
    regain.leaving("#a");
    regain.joined("#a");
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start + STABLE);
    assert_eq!(regain.next_deadline(), Some(start + STABLE * 2));
}

#[test]
fn others_present_or_an_op_cancel() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true);
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
fn unsynced_or_ineligible_never_act() {
    let start = Instant::now();
    // The partial roster after our own JOIN does not count.
    let mut early = OpRegain::new(true);
    early.joined("#a");
    early.roster("#a", &users(&["Me_"]), "Me_", false, start);
    assert_eq!(early.next_deadline(), None);
    // Disabled.
    let mut off = OpRegain::new(false);
    arrive(&mut off, &["Me_"], start);
    assert_eq!(off.next_deadline(), None);
    // `!` and `+` channels are not touched.
    let mut safe = OpRegain::new(true);
    for name in ["!ABCDEsafe", "+modeless"] {
        safe.joined(name);
        safe.roster(name, &users(&["Me_"]), "Me_", true, start);
    }
    assert_eq!(safe.next_deadline(), None);
}

#[test]
fn not_ready_or_unknown_modes_wait_and_a_manual_part_cancels() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true);
    arrive(&mut regain, &["Me_"], start);
    assert!(regain.poll(start + STABLE, false, flags).is_empty());
    assert!(regain.poll(start + STABLE * 2, true, |_| None).is_empty());
    assert!(!regain.poll(start + STABLE * 3, true, flags).is_empty());
    let mut parted = OpRegain::new(true);
    arrive(&mut parted, &["Me_"], start);
    parted.parted("#a");
    assert_eq!(parted.next_deadline(), None);
    // Sending the PART is enough: no echo is waited for.
    let mut leaving = OpRegain::new(true);
    arrive(&mut leaving, &["Me_"], start);
    leaving.leaving("#A");
    assert_eq!(leaving.next_deadline(), None);
    assert!(leaving.poll(start + STABLE * 3, true, flags).is_empty());
}

#[test]
fn failures_end_the_attempt_without_a_retry() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    regain.parted("#a");
    assert_eq!(
        regain.join_failed("#a"),
        [Step::Status("#a".into(), OpRegainStatus::RejoinFailed)]
    );
    assert_eq!(regain.next_deadline(), None);

    // Joined again, but not made operator.
    let mut regain = OpRegain::new(true);
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
    let mut regain = OpRegain::new(true);
    arrive(&mut regain, &["Me_"], start);
    regain.poll(start + STABLE, true, flags);
    let late = start + STABLE + ATTEMPT_TIMEOUT;
    assert_eq!(
        regain.poll(late, true, flags),
        [Step::Status("#a".into(), OpRegainStatus::NotRegained)]
    );
}

#[test]
fn unknown_modes_hold_the_attempt_until_they_are_known() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true);
    arrive(&mut regain, &["Me_"], start);
    // No time limit: still waiting long after, and acting once known.
    for round in 1..=4 {
        assert!(
            regain
                .poll(start + STABLE * round, true, |_| None)
                .is_empty()
        );
        assert!(regain.next_deadline().is_some());
    }
    assert!(!regain.poll(start + STABLE * 5, true, flags).is_empty());
}

/// Operator again after a rejoin, with the restoring commands sent.
fn restoring(topic: &str, start: Instant) -> OpRegain {
    let mut regain = OpRegain::new(true);
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
fn losing_operator_while_restoring_is_retried_without_another_roster_event() {
    let start = Instant::now();
    let judged = start + STABLE + RESTORE_WAIT;
    let mut regain = restoring("hello", start);
    // Deopped, alone, during the check.
    assert!(
        regain
            .roster("#a", &users(&["Me_"]), "Me_", false, start + STABLE * 2)
            .is_empty()
    );
    regain.poll(judged, true, |_| Some("ntkl".into()));
    assert_eq!(regain.next_deadline(), Some(judged + STABLE));
    let steps = regain.poll(judged + STABLE, true, flags);
    assert!(steps.contains(&Step::Rejoin("#a".into())));
}

#[test]
fn a_topic_that_cannot_be_put_back_keeps_the_channel_as_it_is() {
    let start = Instant::now();
    let mut regain = OpRegain::new(true);
    regain.joined("#a");
    regain.topic("#a", &"あ".repeat(MAX_TOPIC / 3 + 1));
    regain.roster("#a", &users(&["Me_"]), "Me_", true, start);
    assert_eq!(
        regain.poll(start + STABLE, true, flags),
        [Step::Status("#a".into(), OpRegainStatus::TopicTooLong)]
    );
    assert_eq!(regain.next_deadline(), None);
    // The limit itself still works.
    let mut regain = OpRegain::new(true);
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
    let mut regain = OpRegain::new(true);
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
