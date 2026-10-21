//! Performance tracing for local builds only (feature `perf`, never in a release), kept in a
//! ring buffer read by the interface's diagnostics panel.

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::Serialize;

pub const ENABLED: bool = cfg!(feature = "perf");

pub const SLOW_SQL: Duration = Duration::from_millis(30);
pub const SLOW_WAIT: Duration = Duration::from_millis(20);
pub const SLOW_HOLD: Duration = Duration::from_millis(50);
pub const SLOW_IMAP: Duration = Duration::from_millis(250);
const KEPT: usize = 1000;

#[derive(Debug, Clone, Serialize)]
pub struct PerfEvent {
    /// Unix ms.
    pub at: i64,
    /// `sql`, `wait` (for a connection), `hold` (a connection kept long), `imap` (a folder
    /// sync or a sync's setup, with its steps).
    pub kind: &'static str,
    pub what: String,
    pub ms: f64,
}

static EVENTS: LazyLock<Mutex<VecDeque<PerfEvent>>> = LazyLock::new(|| Mutex::new(VecDeque::with_capacity(KEPT)));

pub fn record(kind: &'static str, what: impl Into<String>, took: Duration) {
    if !ENABLED {
        return;
    }
    let event = PerfEvent { at: chrono::Utc::now().timestamp_millis(), kind, what: what.into(), ms: took.as_secs_f64() * 1000.0 };
    tracing::debug!(target: "perf", "{kind} {:.0} ms: {}", event.ms, event.what);
    let mut events = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    if events.len() == KEPT {
        events.pop_front();
    }
    events.push_back(event);
}

/// Unix ms; oldest first.
pub fn since(since: i64) -> Vec<PerfEvent> {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|e| e.at > since).cloned().collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub window_secs: i64,
    pub kinds: Vec<KindSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindSummary {
    pub kind: &'static str,
    pub count: usize,
    pub total_ms: f64,
    /// Slowest first.
    pub worst: Vec<PerfEvent>,
}

/// IMAP records are all kept, whatever the window: the sync at launch is the one to look at.
pub fn summary(window_ms: i64, worst: usize) -> Summary {
    let from = chrono::Utc::now().timestamp_millis() - window_ms;
    let events = since(0);
    let kinds = ["sql", "wait", "hold", "imap"]
        .into_iter()
        .map(|kind| {
            let imap = kind == "imap";
            let mut of: Vec<&PerfEvent> = events.iter().filter(|e| e.kind == kind && (imap || e.at > from)).collect();
            let total_ms = of.iter().map(|e| e.ms).sum();
            let count = of.len();
            of.sort_by(|a, b| b.ms.total_cmp(&a.ms));
            KindSummary { kind, count, total_ms, worst: of.into_iter().take(if imap { 40 } else { worst }).cloned().collect() }
        })
        .collect();
    Summary { window_secs: window_ms / 1000, kinds }
}

/// Steps of one piece of work, recorded as one `imap` event when the whole took long. Recorded
/// on drop, so an error or an early return still shows how far it went.
pub struct Steps {
    what: String,
    start: std::time::Instant,
    last: std::time::Instant,
    steps: Vec<(&'static str, Duration)>,
}

impl Steps {
    pub fn new(what: impl FnOnce() -> String) -> Self {
        let now = std::time::Instant::now();
        Steps { what: if ENABLED { what() } else { String::new() }, start: now, last: now, steps: Vec::new() }
    }

    pub fn mark(&mut self, step: &'static str) {
        if !ENABLED {
            return;
        }
        let now = std::time::Instant::now();
        self.steps.push((step, now - self.last));
        self.last = now;
    }
}

impl Drop for Steps {
    fn drop(&mut self) {
        let took = self.start.elapsed();
        if !ENABLED || took < SLOW_IMAP {
            return;
        }
        let steps: Vec<String> = self.steps.iter().filter(|(_, d)| d.as_millis() >= 10).map(|(s, d)| format!("{s} {}", d.as_millis())).collect();
        record("imap", format!("{} [{}]", self.what, steps.join(", ")), took);
    }
}

#[cfg(feature = "perf")]
pub use held::{Held, profile};

#[cfg(feature = "perf")]
mod held {
    use std::ops::{Deref, DerefMut};
    use std::panic::Location;
    use std::sync::MutexGuard;
    use std::time::{Duration, Instant};

    use rusqlite::Connection;

    pub struct Held<'a> {
        guard: MutexGuard<'a, Connection>,
        since: Instant,
        at: &'static Location<'static>,
        which: &'static str,
    }

    impl<'a> Held<'a> {
        pub fn new(guard: MutexGuard<'a, Connection>, waited: Duration, at: &'static Location<'static>, which: &'static str) -> Self {
            if waited > super::SLOW_WAIT {
                super::record("wait", format!("{which} for {}:{}", at.file(), at.line()), waited);
            }
            Held { guard, since: Instant::now(), at, which }
        }
    }

    impl Deref for Held<'_> {
        type Target = Connection;
        fn deref(&self) -> &Connection {
            &self.guard
        }
    }

    impl DerefMut for Held<'_> {
        fn deref_mut(&mut self) -> &mut Connection {
            &mut self.guard
        }
    }

    impl Drop for Held<'_> {
        fn drop(&mut self) {
            let held = self.since.elapsed();
            if held > super::SLOW_HOLD {
                super::record("hold", format!("{} by {}:{}", self.which, self.at.file(), self.at.line()), held);
            }
        }
    }

    pub fn profile(conn: &mut Connection) {
        use rusqlite::trace::{TraceEvent, TraceEventCodes};
        conn.trace_v2(
            TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(|event: TraceEvent<'_>| {
                if let TraceEvent::Profile(stmt, took) = event
                    && took > super::SLOW_SQL
                {
                    let sql: String = stmt.sql().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(400).collect();
                    super::record("sql", sql, took);
                }
            }),
        );
    }
}
