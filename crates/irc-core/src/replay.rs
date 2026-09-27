//! Tells replayed history apart from live messages, so history does not
//! raise desktop notifications again.

use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use irc::proto::{Command as IrcCommand, Message as IrcMessage, Prefix};

/// A server-time tag this far in the past marks history. Generous so a
/// server clock running slightly behind ours does not hide live messages.
const REPLAY_AGE: Duration = Duration::from_secs(5 * 60);
/// A misbehaving server could open batches without closing them.
const MAX_OPEN_BATCHES: usize = 64;
/// IRCv3 batch types that carry history rather than live traffic.
const HISTORY_BATCH_TYPES: [&str; 2] = ["CHATHISTORY", "ZNC.IN/PLAYBACK"];

/// Remembers open IRCv3 history batches (`chathistory`, `znc.in/playback`).
#[derive(Default)]
pub(crate) struct ReplayTracker {
    history_batches: HashSet<String>,
}

impl ReplayTracker {
    /// Follows `BATCH +ref type` / `BATCH -ref`. A batch nested in a history
    /// batch is history too.
    pub(crate) fn observe(&mut self, message: &IrcMessage) {
        let IrcCommand::BATCH(reference, kind, _) = &message.command else {
            return;
        };
        if let Some(reference) = reference.strip_prefix('-') {
            self.history_batches.remove(reference);
        } else if let Some(reference) = reference.strip_prefix('+') {
            let history = kind
                .as_ref()
                .is_some_and(|kind| HISTORY_BATCH_TYPES.contains(&kind.to_str()))
                || self.in_history_batch(message);
            if history && self.history_batches.len() < MAX_OPEN_BATCHES {
                self.history_batches.insert(reference.to_owned());
            }
        }
    }

    /// Whether a PRIVMSG or NOTICE is not a live message from a user: it is
    /// in a history batch, carries an old server-time, or was sent by the
    /// server or bouncer itself without a user mask. Tiarra's Log::Recent,
    /// for example, replays channel logs as NOTICEs from `:tiarra`.
    pub(crate) fn replayed(&self, message: &IrcMessage) -> bool {
        self.replayed_at(message, SystemTime::now())
    }

    fn replayed_at(&self, message: &IrcMessage, now: SystemTime) -> bool {
        let from_user = matches!(
            &message.prefix,
            Some(Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()
        );
        !from_user
            || self.in_history_batch(message)
            || tag(message, "time")
                .and_then(parse_server_time)
                .is_some_and(|sent| now.duration_since(sent).is_ok_and(|age| age >= REPLAY_AGE))
    }

    fn in_history_batch(&self, message: &IrcMessage) -> bool {
        tag(message, "batch").is_some_and(|reference| self.history_batches.contains(reference))
    }
}

fn tag<'a>(message: &'a IrcMessage, name: &str) -> Option<&'a str> {
    message
        .tags
        .as_ref()?
        .iter()
        .find(|tag| tag.0 == name)?
        .1
        .as_deref()
}

/// Parses the IRCv3 server-time format `YYYY-MM-DDThh:mm:ss[.sss]Z`.
fn parse_server_time(value: &str) -> Option<SystemTime> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let time = time.split_once('.').map_or(time, |(whole, _)| whole);
    let mut time = time.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).ok()?))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard
/// Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> IrcMessage {
        line.parse().unwrap()
    }

    #[test]
    fn server_time_parses_to_unix_seconds() {
        let at = |value| {
            parse_server_time(value).map(|time| time.duration_since(UNIX_EPOCH).unwrap().as_secs())
        };
        assert_eq!(at("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(at("2026-09-27T12:34:56.789Z"), Some(1_790_512_496));
        assert_eq!(at("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(at("2026-09-27T12:34:56"), None);
        assert_eq!(at("2026-13-01T00:00:00Z"), None);
        assert_eq!(at("garbage"), None);
    }

    #[test]
    fn server_and_bouncer_lines_without_a_user_mask_are_replayed() {
        let tracker = ReplayTracker::default();
        assert!(tracker.replayed(&parse(":tiarra NOTICE #chan :12:34 <alice> me: hi")));
        assert!(tracker.replayed(&parse(":irc.example NOTICE #chan :maintenance")));
        assert!(!tracker.replayed(&parse(":alice!u@h PRIVMSG #chan :me: hi")));
        assert!(!tracker.replayed(&parse(":alice!u@h NOTICE #chan :me: hi")));
    }

    #[test]
    fn history_batches_and_old_server_times_are_replayed() {
        let mut tracker = ReplayTracker::default();
        tracker.observe(&parse(":srv BATCH +h1 chathistory #chan"));
        tracker.observe(&parse(":srv BATCH +n1 netsplit a.example b.example"));
        tracker.observe(&parse("@batch=h1 :srv BATCH +h2 draft/multiline #chan"));
        tracker.observe(&parse(":znc BATCH +p1 znc.in/playback #chan"));
        let tagged = |tags: &str| parse(&format!("@{tags} :alice!u@h PRIVMSG #chan :me: hi"));
        assert!(tracker.replayed(&tagged("batch=h1")));
        assert!(tracker.replayed(&tagged("batch=h2")));
        assert!(tracker.replayed(&tagged("batch=p1")));
        assert!(!tracker.replayed(&tagged("batch=n1")));
        tracker.observe(&parse(":srv BATCH -h1"));
        assert!(!tracker.replayed(&tagged("batch=h1")));

        let now = UNIX_EPOCH + Duration::from_secs(1_790_512_496);
        let old = tagged("time=2026-09-27T12:29:56.000Z");
        let recent = tagged("time=2026-09-27T12:34:50.000Z");
        let ahead = tagged("time=2026-09-27T12:40:00.000Z");
        assert!(tracker.replayed_at(&old, now));
        assert!(!tracker.replayed_at(&recent, now));
        assert!(!tracker.replayed_at(&ahead, now));
    }
}
