//! How a thread of the toolkit hands its freed memory back before it waits.
//! The program's allocator belongs to its runtime, which the binding that
//! starts the toolkit installs here.

use std::sync::OnceLock;
use std::time::Duration;

static RELEASE: OnceLock<fn() -> Option<Duration>> = OnceLock::new();

/// Installs the runtime's release of a waiting thread's freed memory, which
/// [`thread_idle`] calls; the first installation stays.
pub fn set_thread_idle(release: fn() -> Option<Duration>) {
    let _ = RELEASE.set(release);
}

/// Hands the calling thread's freed memory back before it waits. Answers how
/// long the wait may last before the thread calls again, when the runtime
/// deferred the release; `None` when the wait needs no bound.
pub fn thread_idle() -> Option<Duration> {
    RELEASE.get().and_then(|release| release())
}

/// `bound` in whole milliseconds, rounded up so a wait it limits ends after
/// the deferral rather than before it.
pub(crate) fn millis(bound: Duration) -> i64 {
    i64::try_from(bound.as_micros().div_ceil(1000)).unwrap_or(i64::MAX)
}
