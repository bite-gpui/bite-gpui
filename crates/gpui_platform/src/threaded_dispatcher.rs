use std::time::Instant;

use parking_lot::{Condvar, Mutex};

use crate::RunnableVariant;

/// The minimum number of worker threads a threaded dispatcher spawns.
pub const MIN_THREADS: usize = 2;

/// Tracks how many background and timer runnables are queued or running so
/// `ThreadedDispatcher::run_until_idle` knows when to stop waiting.
#[derive(Default)]
#[allow(missing_docs)]
pub struct IdleTracker {
    pub inflight: Mutex<usize>,
    pub condvar: Condvar,
}

#[allow(missing_docs)]
impl IdleTracker {
    pub fn increment(&self) {
        *self.inflight.lock() += 1;
    }

    pub fn decrement(&self) {
        let mut inflight = self.inflight.lock();
        *inflight -= 1;
        if *inflight == 0 {
            self.condvar.notify_all();
        }
    }

    /// Returns a guard that decrements the in-flight count when dropped, so
    /// the count stays correct even if the runnable being executed panics.
    pub fn decrement_on_drop(&self) -> impl Drop + '_ {
        gpui_util::defer(|| self.decrement())
    }

    /// Notifies waiters while holding the in-flight lock. `run_until_idle`
    /// re-checks its wake conditions under this lock before waiting, so the
    /// notification can't slip between its check and its wait and be lost.
    pub fn notify_under_lock(&self) {
        let _inflight = self.inflight.lock();
        self.condvar.notify_all();
    }
}

#[allow(missing_docs)]
pub struct TimerQueue {
    pub state: Mutex<TimerQueueState>,
    pub condvar: Condvar,
}

#[allow(missing_docs)]
pub struct TimerQueueState {
    pub heap: std::collections::BinaryHeap<TimerEntry>,
    pub next_seq: u64,
}

#[allow(missing_docs)]
pub struct TimerEntry {
    pub due: Instant,
    pub seq: u64,
    pub runnable: RunnableVariant,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.due == other.due && self.seq == other.seq
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reversed so that the entry with the earliest due time (breaking ties
        // by insertion order) is at the top of the max-heap.
        other
            .due
            .cmp(&self.due)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
