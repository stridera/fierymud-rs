//! Tick timing: makes game-loop lag visible (GitHub issue #29).
//!
//! The loop is single-threaded, so one slow system or one awaited DB call
//! freezes every player at once, and nothing used to record it. This module
//! keeps
//!
//! * a per-tick duration ring covering the last minute, surfaced as
//!   p50 / p95 / max in `GET /api/admin/world/status`, and
//! * a "slowest phase" tracker (`lap`) so a slow tick can name the system
//!   or loop phase that ate the time, logged as a WARN rate-limited to one
//!   line per 10 s (with a count of the slow turns it suppressed).
//!
//! All time-dependent logic takes `now` / durations as arguments so the
//! rate limiter and percentiles are unit-testable without sleeping.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;

/// A loop turn at or above this is "slow" and eligible for a WARN.
pub(crate) const SLOW_TURN: Duration = Duration::from_millis(100);
/// Minimum spacing between slow-turn WARN lines.
const WARN_INTERVAL: Duration = Duration::from_secs(10);
/// Window the percentiles cover.
const WINDOW: Duration = Duration::from_secs(60);

/// Details for one rate-limited slow-turn WARN.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SlowReport {
    pub(crate) phase: &'static str,
    pub(crate) duration: Duration,
    /// Slow turns since the previous WARN that were not logged.
    pub(crate) suppressed: u32,
}

/// p50 / p95 / max (milliseconds) of tick durations over the last minute.
#[derive(Debug, PartialEq)]
pub(crate) struct TickSummary {
    pub(crate) samples: usize,
    pub(crate) p50_ms: f64,
    pub(crate) p95_ms: f64,
    pub(crate) max_ms: f64,
}

#[derive(Resource)]
pub(crate) struct TickStats {
    samples: VecDeque<(Instant, Duration)>,
    lap_start: Instant,
    slowest: (&'static str, Duration),
    last_warn: Option<Instant>,
    suppressed: u32,
}

impl Default for TickStats {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            lap_start: Instant::now(),
            slowest: ("", Duration::ZERO),
            last_warn: None,
            suppressed: 0,
        }
    }
}

impl TickStats {
    /// Start timing a new turn: resets the lap clock and slowest phase.
    pub(crate) fn begin_turn(&mut self, now: Instant) {
        self.lap_start = now;
        self.slowest = ("", Duration::ZERO);
    }

    /// Attribute the time since the previous lap to `phase`.
    pub(crate) fn lap(&mut self, phase: &'static str, now: Instant) {
        let d = now.saturating_duration_since(self.lap_start);
        if d > self.slowest.1 {
            self.slowest = (phase, d);
        }
        self.lap_start = now;
    }

    /// Record a finished tick of `total` duration. Returns a report when it
    /// was slow and the rate limiter allows a WARN now.
    pub(crate) fn finish_tick(&mut self, now: Instant, total: Duration) -> Option<SlowReport> {
        self.samples.push_back((now, total));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) > WINDOW)
        {
            self.samples.pop_front();
        }
        let phase = if self.slowest.0.is_empty() {
            "tick"
        } else {
            self.slowest.0
        };
        self.report_slow(now, phase, total)
    }

    /// Rate-limited slow-turn check, also used for non-tick loop turns
    /// (inbound commands, auth completions). `None` when the turn is fast
    /// or a WARN was already emitted within the last 10 s (the miss is
    /// counted and reported on the next allowed WARN).
    pub(crate) fn report_slow(
        &mut self,
        now: Instant,
        phase: &'static str,
        duration: Duration,
    ) -> Option<SlowReport> {
        if duration < SLOW_TURN {
            return None;
        }
        if self
            .last_warn
            .is_some_and(|t| now.saturating_duration_since(t) < WARN_INTERVAL)
        {
            self.suppressed = self.suppressed.saturating_add(1);
            return None;
        }
        self.last_warn = Some(now);
        Some(SlowReport {
            phase,
            duration,
            suppressed: std::mem::take(&mut self.suppressed),
        })
    }

    /// Percentiles over the samples within the last minute of `now`.
    pub(crate) fn summary(&self, now: Instant) -> TickSummary {
        let mut ms: Vec<f64> = self
            .samples
            .iter()
            .filter(|(t, _)| now.saturating_duration_since(*t) <= WINDOW)
            .map(|(_, d)| d.as_secs_f64() * 1000.0)
            .collect();
        ms.sort_by(f64::total_cmp);
        let pick = |q: f64| -> f64 {
            if ms.is_empty() {
                return 0.0;
            }
            // Nearest-rank percentile.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let idx = ((q * ms.len() as f64).ceil() as usize).clamp(1, ms.len()) - 1;
            ms[idx]
        };
        TickSummary {
            samples: ms.len(),
            p50_ms: pick(0.50),
            p95_ms: pick(0.95),
            max_ms: ms.last().copied().unwrap_or(0.0),
        }
    }
}

/// Chain-marker system: credits the time since the previous marker to
/// `phase`. Placed right after a system in the (fully chained) schedule,
/// it times exactly that system.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn lap_after(phase: &'static str) -> impl FnMut(ResMut<TickStats>) {
    move |mut stats: ResMut<TickStats>| stats.lap(phase, Instant::now())
}

/// Lap marker for code outside the schedule (the tick body in `main`).
pub(crate) fn lap(world: &mut World, phase: &'static str) {
    world.resource_mut::<TickStats>().lap(phase, Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn fast_ticks_never_warn() {
        let mut s = TickStats::default();
        let t0 = Instant::now();
        assert!(s.finish_tick(t0, ms(99)).is_none());
        assert_eq!(s.suppressed, 0);
    }

    #[test]
    fn slow_warn_is_rate_limited_to_one_per_ten_seconds_with_count() {
        let mut s = TickStats::default();
        let t0 = Instant::now();
        // First slow tick warns immediately and names the slowest phase.
        s.begin_turn(t0);
        s.lap("combat_tick", t0 + ms(150));
        s.lap("regen_tick", t0 + ms(160));
        let r = s.finish_tick(t0 + ms(160), ms(160)).expect("first warns");
        assert_eq!(r.phase, "combat_tick");
        assert_eq!(r.duration, ms(160));
        assert_eq!(r.suppressed, 0);
        // Slow ticks inside the next 10 s are counted, not logged.
        for i in 1..=3u64 {
            assert!(
                s.finish_tick(t0 + Duration::from_secs(i), ms(200))
                    .is_none()
            );
        }
        // Fast ticks neither warn nor count.
        assert!(s.finish_tick(t0 + Duration::from_secs(5), ms(5)).is_none());
        // After the window the next slow tick warns and reports the count.
        let r = s
            .finish_tick(t0 + Duration::from_secs(11), ms(300))
            .expect("window elapsed");
        assert_eq!(r.suppressed, 3);
        // And the counter was reset.
        assert!(
            s.finish_tick(t0 + Duration::from_secs(12), ms(300))
                .is_none()
        );
        let r = s
            .finish_tick(t0 + Duration::from_secs(22), ms(300))
            .expect("second window");
        assert_eq!(r.suppressed, 1);
    }

    #[test]
    fn non_tick_turns_share_the_limiter() {
        let mut s = TickStats::default();
        let t0 = Instant::now();
        assert!(s.report_slow(t0, "inbound:line", ms(500)).is_some());
        assert!(
            s.report_slow(t0 + Duration::from_secs(1), "inbound:line", ms(500))
                .is_none()
        );
    }

    #[test]
    fn summary_covers_only_the_last_minute() {
        let mut s = TickStats::default();
        let t0 = Instant::now();
        // An old outlier that must age out of the window.
        s.finish_tick(t0, ms(900));
        for i in 0..100u64 {
            s.finish_tick(t0 + Duration::from_secs(61) + ms(i), ms(i + 1));
        }
        let sum = s.summary(t0 + Duration::from_secs(62));
        assert_eq!(sum.samples, 100);
        assert!((sum.p50_ms - 50.0).abs() < 1e-6, "p50 {}", sum.p50_ms);
        assert!((sum.p95_ms - 95.0).abs() < 1e-6, "p95 {}", sum.p95_ms);
        assert!((sum.max_ms - 100.0).abs() < 1e-6, "max {}", sum.max_ms);
        assert_eq!(TickStats::default().summary(t0).samples, 0);
    }

    /// The chain marker credits the gap since the previous marker to the
    /// system it follows, so a slow system is named as the slowest phase.
    #[test]
    fn lap_markers_name_the_slow_system() {
        fn quick() {}
        fn slow() {
            std::thread::sleep(Duration::from_millis(30));
        }
        let mut world = World::new();
        world.insert_resource(TickStats::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                (quick, lap_after("quick")).chain(),
                (slow, lap_after("slow")).chain(),
                (quick, lap_after("quick2")).chain(),
            )
                .chain(),
        );
        world.resource_mut::<TickStats>().begin_turn(Instant::now());
        schedule.run(&mut world);
        assert_eq!(world.resource::<TickStats>().slowest.0, "slow");
        assert!(world.resource::<TickStats>().slowest.1 >= ms(30));
    }
}
