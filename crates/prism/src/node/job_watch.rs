//! Bounding long libvirt calls without a wall-clock limit.
//!
//! A snapshot of a guest with a big disk or a live migration blocks in the
//! daemon for as long as the data takes to move, so no fixed timeout fits.
//! [`watch_job`] polls the domain job while the call runs. It stops the job
//! when the task is cancelled, or when the job has reported no progress for
//! the configured stall time.

use std::future::Future;
use std::time::Duration;

use prism_libvirt::{Client, Domain, Params, TypedParam};
use tokio::time::Instant;

use crate::tasks::TaskCtx;

/// How often the domain job is polled.
const POLL: Duration = Duration::from_secs(1);
/// How long a single poll may take before it is skipped.
const POLL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the call gets to return after the job was aborted.
const ABORT_GRACE: Duration = Duration::from_secs(30);

/// Decides when a job has stalled: its progress counter has not moved for
/// the stall time. A job that reports no progress at all (no job running, or
/// one without counters) is never judged, because silence there is normal.
#[derive(Debug)]
pub(super) struct StallTracker {
    timeout: Duration,
    last: Option<u64>,
    since: Duration,
}

impl StallTracker {
    /// A zero `timeout` disables the check.
    pub(super) fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            last: None,
            since: Duration::ZERO,
        }
    }

    /// Records the progress counter seen `elapsed` after the start and says
    /// whether the job has now stalled.
    pub(super) fn observe(&mut self, elapsed: Duration, progress: Option<u64>) -> bool {
        if progress.is_none() || progress != self.last {
            self.last = progress;
            self.since = elapsed;
            return false;
        }
        !self.timeout.is_zero() && elapsed.saturating_sub(self.since) >= self.timeout
    }

    /// How long the counter has been still, as of `elapsed`.
    pub(super) fn idle(&self, elapsed: Duration) -> Duration {
        elapsed.saturating_sub(self.since)
    }
}

/// The progress counter of a job-stats reply: bytes processed so far.
pub(super) fn progress_counter(job_type: i32, stats: &[TypedParam]) -> Option<u64> {
    if job_type == 0 {
        return None;
    }
    stats
        .u64("data_processed")
        .or_else(|| stats.u64("memory_processed"))
}

/// Fraction of the job done, from its totals; `None` while the total is unknown.
pub(super) fn progress_fraction(stats: &[TypedParam]) -> Option<f64> {
    let total = stats.u64("data_total").filter(|&t| t > 0)?;
    let done = stats.u64("data_processed").unwrap_or(0);
    Some((done as f64 / total as f64).clamp(0.0, 1.0))
}

/// Why [`watch_job`] stopped a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stopped {
    Cancelled,
    /// No progress for this long.
    Stalled(Duration),
}

impl Stopped {
    pub(super) fn describe(self) -> String {
        match self {
            Self::Cancelled => "cancelled".into(),
            Self::Stalled(d) => format!(
                "no progress for {} seconds; the job was aborted",
                d.as_secs()
            ),
        }
    }
}

/// How a watched call ended.
pub(super) enum Watched<R> {
    /// The call returned by itself.
    Done(R),
    /// The job was aborted on the daemon and the call given time to return.
    Stopped(Stopped),
}

/// Runs `call` while polling the job of `dom`. `on_stats` sees every poll
/// that found a running job, for progress reporting.
pub(super) async fn watch_job<R>(
    c: &Client,
    dom: &Domain,
    ctx: &TaskCtx,
    stall: Duration,
    mut on_stats: impl FnMut(&[TypedParam]),
    call: impl Future<Output = R>,
) -> Watched<R> {
    tokio::pin!(call);
    let start = Instant::now();
    let mut tracker = StallTracker::new(stall);
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stopped = loop {
        tokio::select! {
            r = &mut call => return Watched::Done(r),
            () = ctx.cancel.cancelled() => break Stopped::Cancelled,
            _ = tick.tick() => {
                let polled = tokio::time::timeout(POLL_TIMEOUT, c.job_stats(dom, 0)).await;
                let (kind, stats) = match polled {
                    Ok(Ok(s)) => s,
                    _ => (0, Vec::new()),
                };
                if kind != 0 {
                    on_stats(&stats);
                }
                let elapsed = start.elapsed();
                if tracker.observe(elapsed, progress_counter(kind, &stats)) {
                    break Stopped::Stalled(tracker.idle(elapsed));
                }
            }
        }
    };
    if let Err(e) = c.abort_job(dom).await {
        tracing::warn!(vm = %dom.name, "could not abort the job: {e}");
    }
    // The aborted call returns an error; wait for it so its reply is not left
    // dangling, but do not hang if the daemon never answers.
    // A call that finished anyway (the abort lost the race, or came after the
    // point of no return) is reported as finished, not as stopped.
    match tokio::time::timeout(ABORT_GRACE, &mut call).await {
        Ok(r) => Watched::Done(r),
        Err(_) => Watched::Stopped(stopped),
    }
}

impl super::Node {
    /// Runs a long libvirt call as a bounded task step: progress from the
    /// domain job is mapped onto `range` of the task's bar, cancelling the
    /// task or a stall (see `job_stall_timeout_secs`) aborts the job and
    /// fails the step.
    pub(super) async fn watched<T>(
        &self,
        c: &Client,
        dom: &Domain,
        ctx: &TaskCtx,
        range: (f64, f64),
        label: &str,
        call: impl Future<Output = Result<T, prism_libvirt::Error>>,
    ) -> super::Result<T> {
        let report = |stats: &[TypedParam]| {
            if let Some(f) = progress_fraction(stats) {
                let at = range.0 + (range.1 - range.0) * f;
                ctx.progress(Some(at), format!("{label} ({:.0}%)", f * 100.0));
            }
        };
        match watch_job(c, dom, ctx, self.cfg.job_stall_timeout(), report, call).await {
            Watched::Done(r) => Ok(r?),
            Watched::Stopped(why) => Err(super::NodeError::Internal(anyhow::anyhow!(
                "{label}: {}",
                why.describe()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn moving_progress_never_stalls() {
        let mut t = StallTracker::new(secs(10));
        for i in 0..100 {
            assert!(!t.observe(secs(i * 5), Some(i)));
        }
    }

    #[test]
    fn flat_progress_stalls_after_the_timeout() {
        let mut t = StallTracker::new(secs(10));
        assert!(!t.observe(secs(0), Some(7)));
        assert!(!t.observe(secs(9), Some(7)));
        assert!(t.observe(secs(10), Some(7)));
        assert_eq!(t.idle(secs(12)), secs(12));
    }

    #[test]
    fn progress_resets_the_clock() {
        let mut t = StallTracker::new(secs(10));
        assert!(!t.observe(secs(0), Some(1)));
        assert!(!t.observe(secs(8), Some(1)));
        assert!(!t.observe(secs(9), Some(2)));
        assert!(!t.observe(secs(18), Some(2)));
        assert!(t.observe(secs(19), Some(2)));
    }

    #[test]
    fn a_job_without_counters_is_not_judged() {
        let mut t = StallTracker::new(secs(10));
        for i in 0..50 {
            assert!(!t.observe(secs(i * 10), None));
        }
        // The clock starts when counters first appear.
        assert!(!t.observe(secs(500), Some(0)));
        assert!(!t.observe(secs(505), Some(0)));
        assert!(t.observe(secs(510), Some(0)));
    }

    #[test]
    fn zero_timeout_disables_the_check() {
        let mut t = StallTracker::new(Duration::ZERO);
        assert!(!t.observe(secs(0), Some(1)));
        assert!(!t.observe(secs(100_000), Some(1)));
    }

    #[test]
    fn counters_and_fractions() {
        let stats = vec![
            TypedParam::ullong("data_processed", 25),
            TypedParam::ullong("data_total", 100),
        ];
        assert_eq!(progress_counter(1, &stats), Some(25));
        assert_eq!(progress_counter(0, &stats), None);
        assert_eq!(progress_fraction(&stats), Some(0.25));
        assert_eq!(progress_fraction(&[]), None);
    }

    #[test]
    fn stop_reasons_read_plainly() {
        assert_eq!(Stopped::Cancelled.describe(), "cancelled");
        assert!(
            Stopped::Stalled(secs(300))
                .describe()
                .contains("300 seconds")
        );
    }
}
