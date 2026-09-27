//! The application-wide record of previews: what is ready, loading, queued
//! or failed. It decides what to load next but runs nothing itself; the UI
//! runs the jobs it hands out on background threads and reports back.
//!
//! Everything is bounded: jobs in flight, queued requests, records (ready,
//! failed, queued and loading together), the bytes of ready images, and the
//! rows waiting on each record. Requests come from rendering, so only rows
//! on screen (plus the list's small overdraw) ask for images. The queue is
//! served newest first and drops its oldest request when full, which favors
//! what is visible now after fast scrolling.
//!
//! Turning previews off cancels running jobs, forgets every record, hands
//! all ready images back for release and bumps a generation, so a job that
//! finishes afterwards is ignored instead of repopulating the cache. The
//! in-flight count still includes such jobs until they return, so the
//! concurrency bound holds across off/on switches.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use crate::{CancelFlag, LoadError, MediaRef};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheLimits {
    /// Jobs fetching or decoding at the same time, application-wide.
    pub max_in_flight: usize,
    /// Requests waiting for a job.
    pub max_queued: usize,
    /// Records of every state together, including failures.
    pub max_records: usize,
    /// Bytes charged for ready images (see [`PreviewCache::finish`]).
    pub budget_bytes: usize,
    /// Rows remembered per record, to update their layout when it finishes.
    pub max_waiters: usize,
    /// A transient failure may be retried once this long after it happened.
    pub retry_after: Duration,
    /// Loads per record, including the first.
    pub max_attempts: u8,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_in_flight: 2,
            max_queued: 16,
            max_records: 256,
            budget_bytes: 32 * 1024 * 1024,
            max_waiters: 4,
            retry_after: Duration::from_secs(300),
            max_attempts: 2,
        }
    }
}

/// What a row should show for its link.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup<'a, T> {
    Ready(&'a T),
    /// Queued or loading.
    Pending,
    /// No preview: failed, or previews are off. Show only the text link.
    None,
}

/// Work handed out by [`PreviewCache::next_job`].
#[derive(Debug)]
pub struct Job {
    pub source: MediaRef,
    pub cancel: CancelFlag,
    generation: u64,
}

/// The outcome of [`PreviewCache::finish`].
#[derive(Debug, PartialEq, Eq)]
pub struct Finished<R> {
    /// Whether the record changed (false for stale or unknown jobs).
    pub applied: bool,
    /// Rows that showed the record while it was pending.
    pub waiters: Vec<R>,
}

#[derive(Debug)]
enum State<T> {
    Queued,
    Loading(CancelFlag),
    Ready { value: T, bytes: usize },
    Failed { at: Instant, transient: bool },
}

#[derive(Debug)]
struct Record<T, R> {
    state: State<T>,
    last_used: u64,
    attempts: u8,
    waiters: Vec<R>,
}

/// Counters for tests and measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub jobs_started: u64,
    pub ready: u64,
    pub failed: u64,
    pub stale: u64,
    pub evicted: u64,
    pub dropped_from_queue: u64,
}

#[derive(Debug)]
pub struct PreviewCache<T, R> {
    limits: CacheLimits,
    enabled: bool,
    generation: u64,
    records: HashMap<MediaRef, Record<T, R>>,
    /// Queued requests, oldest first.
    queue: VecDeque<MediaRef>,
    in_flight: usize,
    ready_bytes: usize,
    clock: u64,
    /// Evicted or discarded ready images the UI must release.
    released: Vec<T>,
    stats: Stats,
}

impl<T, R: PartialEq> PreviewCache<T, R> {
    /// A cache that starts disabled.
    pub fn new(limits: CacheLimits) -> Self {
        Self {
            limits,
            enabled: false,
            generation: 0,
            records: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: 0,
            ready_bytes: 0,
            clock: 0,
            released: Vec::new(),
            stats: Stats::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            return;
        }
        self.generation += 1;
        self.queue = VecDeque::new();
        for (_, record) in std::mem::take(&mut self.records) {
            match record.state {
                State::Loading(cancel) => cancel.cancel(),
                State::Ready { value, .. } => self.released.push(value),
                State::Queued | State::Failed { .. } => {}
            }
        }
        self.ready_bytes = 0;
    }

    /// Looks up `source` for a row being drawn and queues it if needed.
    /// `waiter` identifies the row; it is told when a pending load finishes.
    pub fn request(&mut self, source: &MediaRef, waiter: R, now: Instant) -> Lookup<'_, T> {
        if !self.enabled {
            return Lookup::None;
        }
        self.clock += 1;
        let clock = self.clock;
        let (limits, queue) = (self.limits, &mut self.queue);
        let mut queued = false;
        let pending = match self.records.get_mut(source) {
            Some(record) => {
                record.last_used = clock;
                match &record.state {
                    State::Ready { .. } => false,
                    State::Queued | State::Loading(_) => true,
                    State::Failed { at, transient } => {
                        let retry = *transient
                            && record.attempts < limits.max_attempts
                            && now.saturating_duration_since(*at) >= limits.retry_after;
                        if retry {
                            record.state = State::Queued;
                            queued = true;
                        }
                        retry
                    }
                }
                .then(|| {
                    if let Some(position) = queue.iter().position(|queued| queued == source) {
                        // Seen again: serve it before older requests.
                        queue.remove(position);
                        queued = true;
                    }
                    add_waiter(&mut record.waiters, waiter, limits.max_waiters);
                })
                .is_some()
            }
            None => {
                self.records.insert(
                    source.clone(),
                    Record {
                        state: State::Queued,
                        last_used: clock,
                        attempts: 0,
                        waiters: vec![waiter],
                    },
                );
                queued = true;
                true
            }
        };
        if queued {
            self.queue.push_back(source.clone());
            if self.queue.len() > self.limits.max_queued
                && let Some(oldest) = self.queue.pop_front()
            {
                self.records.remove(&oldest);
                self.stats.dropped_from_queue += 1;
            }
            self.evict(Some(source));
        }
        match self.records.get(source) {
            Some(Record {
                state: State::Ready { value, .. },
                ..
            }) => Lookup::Ready(value),
            Some(_) if pending => Lookup::Pending,
            _ => Lookup::None,
        }
    }

    /// The next load to run, if a slot is free. Newest requests go first.
    pub fn next_job(&mut self) -> Option<Job> {
        if !self.enabled || self.in_flight >= self.limits.max_in_flight {
            return None;
        }
        let source = self.queue.pop_back()?;
        let record = self.records.get_mut(&source)?;
        let cancel = CancelFlag::default();
        record.state = State::Loading(cancel.clone());
        record.attempts = record.attempts.saturating_add(1);
        self.in_flight += 1;
        self.stats.jobs_started += 1;
        Some(Job {
            source,
            cancel,
            generation: self.generation,
        })
    }

    /// Records a finished job. `bytes` is what a ready image is charged
    /// against the budget; the UI charges its CPU copy and its GPU texture.
    /// Results of jobs started before previews were turned off are dropped.
    pub fn finish(
        &mut self,
        job: Job,
        result: Result<(T, usize), LoadError>,
        now: Instant,
    ) -> Finished<R> {
        self.in_flight = self.in_flight.saturating_sub(1);
        let current = self.enabled && job.generation == self.generation;
        let record = self
            .records
            .get_mut(&job.source)
            .filter(|record| current && matches!(record.state, State::Loading(_)));
        let Some(record) = record else {
            self.stats.stale += 1;
            return Finished {
                applied: false,
                waiters: Vec::new(),
            };
        };
        let waiters = std::mem::take(&mut record.waiters);
        match result {
            Ok((value, bytes)) => {
                record.state = State::Ready { value, bytes };
                self.ready_bytes += bytes;
                self.stats.ready += 1;
                self.evict(Some(&job.source));
            }
            Err(error) => {
                record.state = State::Failed {
                    at: now,
                    transient: error.is_transient(),
                };
                self.stats.failed += 1;
            }
        }
        Finished {
            applied: true,
            waiters,
        }
    }

    /// Forgets rows that no longer exist (removed servers, dropped
    /// conversations). Queued requests left without rows are dropped.
    pub fn retain_waiters(&mut self, mut keep: impl FnMut(&R) -> bool) {
        let mut orphaned = Vec::new();
        for (source, record) in &mut self.records {
            record.waiters.retain(&mut keep);
            if record.waiters.is_empty() && matches!(record.state, State::Queued) {
                orphaned.push(source.clone());
            }
        }
        for source in orphaned {
            self.records.remove(&source);
            self.queue.retain(|queued| *queued != source);
        }
    }

    /// Ready images removed from the cache since the last call. The UI
    /// frees their GPU textures.
    pub fn take_released(&mut self) -> Vec<T> {
        std::mem::take(&mut self.released)
    }

    pub fn ready_bytes(&self) -> usize {
        self.ready_bytes
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn records(&self) -> usize {
        self.records.len()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Drops the least recently used ready or failed records until the
    /// budget and the record limit hold. `keep` is never dropped.
    fn evict(&mut self, keep: Option<&MediaRef>) {
        while self.ready_bytes > self.limits.budget_bytes
            || self.records.len() > self.limits.max_records
        {
            let victim = self
                .records
                .iter()
                .filter(|(source, record)| {
                    Some(*source) != keep
                        && match record.state {
                            State::Ready { .. } => true,
                            // Failures only count against the record limit.
                            State::Failed { .. } => self.records.len() > self.limits.max_records,
                            State::Queued | State::Loading(_) => false,
                        }
                })
                .min_by_key(|(_, record)| record.last_used)
                .map(|(source, _)| source.clone());
            let Some(victim) = victim else { break };
            if let Some(Record {
                state: State::Ready { value, bytes },
                ..
            }) = self.records.remove(&victim)
            {
                self.ready_bytes -= bytes;
                self.released.push(value);
            }
            self.stats.evicted += 1;
        }
    }
}

fn add_waiter<R: PartialEq>(waiters: &mut Vec<R>, waiter: R, max: usize) {
    if waiters.contains(&waiter) || max == 0 {
        return;
    }
    if waiters.len() >= max {
        waiters.remove(0);
    }
    waiters.push(waiter);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(n: usize) -> MediaRef {
        MediaRef::Link(url::Url::parse(&format!("https://example.com/{n}.png")).unwrap())
    }

    fn cache(limits: CacheLimits) -> PreviewCache<String, u32> {
        let mut cache = PreviewCache::new(limits);
        cache.set_enabled(true);
        cache
    }

    #[test]
    fn disabled_cache_queues_and_loads_nothing() {
        let mut cache: PreviewCache<String, u32> = PreviewCache::new(CacheLimits::default());
        let now = Instant::now();
        assert_eq!(cache.request(&link(1), 1, now), Lookup::None);
        assert!(cache.next_job().is_none());
        assert_eq!((cache.records(), cache.queued()), (0, 0));
    }

    #[test]
    fn repeated_links_share_one_record_and_one_load() {
        let mut cache = cache(CacheLimits::default());
        let now = Instant::now();
        for row in 0..10 {
            assert_eq!(cache.request(&link(1), row, now), Lookup::Pending);
        }
        let job = cache.next_job().unwrap();
        assert!(cache.next_job().is_none(), "one load for one link");
        // Requests while loading do not start another.
        assert_eq!(cache.request(&link(1), 11, now), Lookup::Pending);
        assert!(cache.next_job().is_none());
        let finished = cache.finish(job, Ok(("image".into(), 100)), now);
        assert!(finished.applied);
        // Waiters are bounded: the newest four rows.
        assert_eq!(finished.waiters, [7, 8, 9, 11]);
        assert_eq!(
            cache.request(&link(1), 12, now),
            Lookup::Ready(&"image".to_string())
        );
        assert!(cache.next_job().is_none());
        assert_eq!(cache.stats().jobs_started, 1);
    }

    #[test]
    fn concurrency_and_queue_are_bounded_newest_first() {
        let mut cache = cache(CacheLimits::default());
        let now = Instant::now();
        for n in 0..100 {
            cache.request(&link(n), n as u32, now);
        }
        assert_eq!(cache.queued(), 16);
        assert_eq!(cache.records(), 16, "dropped requests leave no record");
        assert_eq!(cache.stats().dropped_from_queue, 84);
        let first = cache.next_job().unwrap();
        let second = cache.next_job().unwrap();
        assert!(cache.next_job().is_none());
        assert_eq!((first.source, second.source), (link(99), link(98)));
        assert_eq!(cache.in_flight(), 2);
    }

    #[test]
    fn ready_images_are_evicted_by_bytes_least_recently_used_first() {
        let limits = CacheLimits {
            budget_bytes: 250,
            ..CacheLimits::default()
        };
        let mut cache = cache(limits);
        let now = Instant::now();
        for n in 0..3 {
            cache.request(&link(n), 0, now);
            let job = cache.next_job().unwrap();
            cache.finish(job, Ok((format!("image {n}"), 100)), now);
            // Keep the first one in use.
            cache.request(&link(0), 0, now);
        }
        assert_eq!(cache.ready_bytes(), 200);
        assert_eq!(cache.take_released(), ["image 1"]);
        assert!(matches!(cache.request(&link(0), 0, now), Lookup::Ready(_)));
        assert_eq!(
            cache.request(&link(1), 0, now),
            Lookup::Pending,
            "loads again when shown"
        );
    }

    #[test]
    fn failures_are_remembered_and_retried_only_when_transient_and_late() {
        let mut cache = cache(CacheLimits::default());
        let start = Instant::now();
        cache.request(&link(1), 0, start);
        let job = cache.next_job().unwrap();
        cache.finish(job, Err(LoadError::Network), start);
        cache.request(&link(2), 0, start);
        let job = cache.next_job().unwrap();
        cache.finish(job, Err(LoadError::NotImage), start);
        // Redrawing does not retry.
        for _ in 0..100 {
            assert_eq!(cache.request(&link(1), 0, start), Lookup::None);
            assert_eq!(cache.request(&link(2), 0, start), Lookup::None);
        }
        assert!(cache.next_job().is_none());
        let later = start + Duration::from_secs(301);
        assert_eq!(cache.request(&link(1), 0, later), Lookup::Pending);
        assert_eq!(cache.request(&link(2), 0, later), Lookup::None, "permanent");
        let job = cache.next_job().unwrap();
        cache.finish(job, Err(LoadError::Status(503)), later);
        let much_later = later + Duration::from_secs(3600);
        assert_eq!(
            cache.request(&link(1), 0, much_later),
            Lookup::None,
            "attempts are bounded"
        );
        assert!(cache.next_job().is_none());
        assert_eq!(cache.stats().jobs_started, 3);
    }

    #[test]
    fn records_including_failures_are_bounded() {
        let limits = CacheLimits {
            max_records: 8,
            ..CacheLimits::default()
        };
        let mut cache = cache(limits);
        let now = Instant::now();
        for n in 0..50 {
            cache.request(&link(n), 0, now);
            let job = cache.next_job().unwrap();
            cache.finish(job, Err(LoadError::NotImage), now);
        }
        assert_eq!(cache.records(), 8);
    }

    #[test]
    fn disabling_cancels_loads_releases_images_and_ignores_late_results() {
        let mut cache = cache(CacheLimits::default());
        let now = Instant::now();
        cache.request(&link(1), 0, now);
        let ready = cache.next_job().unwrap();
        cache.finish(ready, Ok(("ready".into(), 10)), now);
        cache.request(&link(2), 0, now);
        cache.request(&link(3), 0, now);
        let loading = cache.next_job().unwrap();
        let cancel = loading.cancel.clone();

        cache.set_enabled(false);
        assert!(cancel.is_cancelled());
        assert_eq!(cache.take_released(), ["ready"]);
        assert_eq!(
            (cache.records(), cache.queued(), cache.ready_bytes()),
            (0, 0, 0)
        );
        assert_eq!(cache.request(&link(2), 0, now), Lookup::None);

        // Turned on again before the old job returned: the job still counts
        // against concurrency and its result is ignored.
        cache.set_enabled(true);
        cache.request(&link(2), 0, now);
        cache.request(&link(4), 0, now);
        let fresh = cache.next_job().unwrap();
        assert!(
            cache.next_job().is_none(),
            "the stale job still holds a slot"
        );
        let finished = cache.finish(loading, Ok(("late".into(), 10)), now);
        assert!(!finished.applied);
        assert_eq!(cache.ready_bytes(), 0);
        assert_eq!(cache.stats().stale, 1);
        assert_eq!(fresh.source, link(4));
        assert!(cache.next_job().is_some(), "slot freed");
    }

    #[test]
    fn removed_rows_drop_their_queued_requests() {
        let mut cache = cache(CacheLimits::default());
        let now = Instant::now();
        cache.request(&link(1), 1, now);
        cache.request(&link(2), 2, now);
        cache.request(&link(3), 1, now);
        cache.request(&link(3), 3, now);
        cache.retain_waiters(|row| *row != 1);
        assert_eq!(cache.queued(), 2);
        assert_eq!(cache.next_job().unwrap().source, link(3));
        assert_eq!(cache.next_job().unwrap().source, link(2));
    }
}
