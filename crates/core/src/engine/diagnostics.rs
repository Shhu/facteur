//! Read-only diagnostics for the diagnostics panel. Never blocks (locks are only tried): the
//! panel polls every 2 seconds and must still answer when something is stuck.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{Engine, Purpose};
use crate::error::Result;
use crate::model::CalendarState;
use crate::store::StoreDiagnostics;
use crate::sync::Scope;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(super) struct Tracker {
    started: Instant,
    listeners: Mutex<HashMap<i64, ListenerStatus>>,
    syncs: Mutex<HashMap<i64, SyncRecord>>,
    next_run: std::sync::atomic::AtomicU64,
    /// Workers' busy time at the last call, to measure the load since.
    workers: Mutex<Option<(Instant, Vec<Duration>)>>,
}

impl Tracker {
    pub(super) fn new() -> Self {
        Tracker {
            started: Instant::now(),
            listeners: Mutex::default(),
            syncs: Mutex::default(),
            next_run: std::sync::atomic::AtomicU64::new(1),
            workers: Mutex::default(),
        }
    }

    pub(super) fn listener(&self, account_id: i64, update: impl FnOnce(&mut ListenerStatus)) {
        let mut listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
        update(listeners.entry(account_id).or_insert_with(ListenerStatus::new));
    }

    pub(super) fn listening(&self, account_id: i64) -> bool {
        self.listeners.lock().unwrap_or_else(|e| e.into_inner()).get(&account_id).is_some_and(|l| matches!(l.state, "listening" | "syncing"))
    }

    pub(super) fn listener_gone(&self, account_id: i64) {
        self.listeners.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
    }

    /// A sync runs on some account.
    pub(super) fn any_sync(&self) -> bool {
        self.syncs.lock().unwrap_or_else(|e| e.into_inner()).values().any(|s| !s.running.is_empty())
    }

    /// The returned guard records the sync as interrupted if dropped unfinished.
    pub(super) fn sync_started(&self, account_id: i64, scope: &Scope) -> SyncRun<'_> {
        let id = self.next_run.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scope = match scope {
            Scope::All => "all",
            Scope::Inbox => "inbox",
            Scope::System => "system",
            Scope::Folders(_) => "folders",
        };
        let started_at = now_ms();
        let mut syncs = self.syncs.lock().unwrap_or_else(|e| e.into_inner());
        syncs.entry(account_id).or_default().running.push((id, started_at, scope));
        SyncRun { tracker: self, account_id, id, started: Instant::now(), started_at, scope, done: false }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ListenerStatus {
    /// `connecting`, `listening`, `syncing` (what it heard), `waiting` (before connecting
    /// again, until `retry_at`), `unsupported` (no IDLE: the periodic sync only).
    pub state: &'static str,
    /// Unix ms.
    pub since: i64,
    /// NOTIFY accepted: every folder is listened to, not only the inbox.
    pub notify: Option<bool>,
    pub retry_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
    pub connections: u32,
}

impl ListenerStatus {
    fn new() -> Self {
        ListenerStatus { state: "connecting", since: now_ms(), notify: None, retry_at: None, last_error: None, last_error_at: None, connections: 0 }
    }

    pub(super) fn set(&mut self, state: &'static str) {
        if self.state != state {
            self.state = state;
            self.since = now_ms();
        }
        if state != "waiting" {
            self.retry_at = None;
        }
    }

    pub(super) fn connecting(&mut self) {
        self.set("connecting");
        self.connections += 1;
        self.notify = None;
    }

    pub(super) fn failed(&mut self, error: String) {
        self.last_error = Some(error);
        self.last_error_at = Some(now_ms());
    }

    pub(super) fn waiting(&mut self, pause: Duration) {
        self.set("waiting");
        self.retry_at = Some(now_ms() + pause.as_millis() as i64);
    }
}

#[derive(Default)]
struct SyncRecord {
    /// (run id, start in Unix ms, scope).
    running: Vec<(u64, i64, &'static str)>,
    last: Option<LastSync>,
    count: u64,
    failures: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastSync {
    pub started: i64,
    pub ms: u64,
    pub scope: &'static str,
    pub error: Option<String>,
    pub new_messages: usize,
}

pub(super) struct SyncRun<'a> {
    tracker: &'a Tracker,
    account_id: i64,
    id: u64,
    started: Instant,
    started_at: i64,
    scope: &'static str,
    done: bool,
}

impl SyncRun<'_> {
    pub(super) fn finish(mut self, error: Option<&str>, new_messages: usize) {
        self.end(error.map(str::to_string), new_messages);
    }

    fn end(&mut self, error: Option<String>, new_messages: usize) {
        self.done = true;
        let mut syncs = self.tracker.syncs.lock().unwrap_or_else(|e| e.into_inner());
        let record = syncs.entry(self.account_id).or_default();
        record.running.retain(|r| r.0 != self.id);
        record.count += 1;
        if error.is_some() {
            record.failures += 1;
        }
        let ms = self.started.elapsed().as_millis() as u64;
        record.last = Some(LastSync { started: self.started_at, ms, scope: self.scope, error, new_messages });
    }
}

impl Drop for SyncRun<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.end(Some("interrupted".into()), 0);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostics {
    /// Unix ms.
    pub at: i64,
    /// Filled by the application; the engine does not know it.
    pub app_version: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub perf_build: bool,
    pub uptime_secs: u64,
    /// Seconds since the machine last woke up or came back online, if it did.
    pub resumed_secs_ago: Option<u64>,
    /// Events waiting for the slowest receiver (the interface's relay).
    pub events_queued: usize,
    pub event_receivers: usize,
    pub accounts: Vec<AccountDiagnostics>,
    pub runtime: Option<RuntimeDiagnostics>,
    pub store: StoreDiagnostics,
    pub memory: Option<ProcessMemory>,
    pub perf: Option<crate::perf::Summary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountDiagnostics {
    pub id: i64,
    pub email: String,
    pub label: Option<String>,
    pub connections: Vec<ConnectionDiagnostics>,
    /// The listening task runs; it may be between connections (see `listener`).
    pub listening_task: bool,
    pub listener: Option<ListenerStatus>,
    pub sync: SyncDiagnostics,
    pub queue: QueueDiagnostics,
    /// Nothing connects until the user acts.
    pub auth_refused: Option<String>,
    /// Long work under way: `history`, `backup`, `calendar`.
    pub busy: Vec<&'static str>,
    pub calendar: Option<CalendarDiagnostics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionDiagnostics {
    pub purpose: &'static str,
    /// Unknown (`None`) while the connection is in use.
    pub open: Option<bool>,
    pub busy: bool,
    pub idle_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncDiagnostics {
    /// Unix ms.
    pub running_since: Option<i64>,
    pub running_scope: Option<&'static str>,
    pub running: usize,
    pub last: Option<LastSync>,
    pub count: u64,
    pub failures: u64,
    /// Unix s.
    pub last_success: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueDiagnostics {
    pub pending: i64,
    pub failed: i64,
    /// Unix s.
    pub oldest_pending: Option<i64>,
    pub replaying: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalendarDiagnostics {
    pub state: CalendarState,
    pub message: Option<String>,
    pub last_sync: Option<i64>,
    /// The user does not use this account's calendar: never synced, its state means nothing.
    pub ignored: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnostics {
    pub workers: usize,
    pub alive_tasks: usize,
    /// Tasks waiting for a free worker: above a few, the workers are saturated.
    pub global_queue_depth: usize,
    /// Busy share of each worker since the previous call (0 to 1).
    pub worker_busy: Vec<f64>,
    /// None on the first call, which has no span to measure.
    pub busy_span_ms: Option<u64>,
}

/// The webview runs in processes of its own on Windows: this is the engine's process only.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessMemory {
    pub resident_bytes: u64,
    /// Private bytes (Windows), or the data segment (Linux, Android).
    pub private_bytes: Option<u64>,
}

impl Purpose {
    const ALL: [Purpose; 4] = [Purpose::Sync, Purpose::Interactive, Purpose::Queue, Purpose::Background];

    fn name(self) -> &'static str {
        match self {
            Purpose::Sync => "sync",
            Purpose::Interactive => "interactive",
            Purpose::Queue => "queue",
            Purpose::Background => "background",
        }
    }
}

impl Engine {
    pub fn diagnostics(&self) -> Result<Diagnostics> {
        let accounts = self.accounts()?;
        let queue = self.store.ops_health()?;
        let mut calendars: HashMap<i64, CalendarDiagnostics> = self
            .calendar_status()
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.account_id, CalendarDiagnostics { state: c.state, message: c.message, last_sync: c.last_sync, ignored: c.ignored }))
            .collect();
        let slots: HashMap<(i64, Purpose), super::Slot> = self.slots.lock().unwrap().clone();
        let flush_locks = self.flush_locks.lock().unwrap().clone();
        let listeners = self.diag.listeners.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let idling = self.idling.lock().unwrap().clone();
        let auth = self.auth_refused.lock().unwrap().clone();
        let errors = self.queue_errors.lock().unwrap().clone();
        let archiving = self.archiving.lock().unwrap().clone();
        let history = self.fetching_history.lock().unwrap().clone();
        let calendar_syncing: Vec<i64> = self.calendar_syncing.lock().unwrap().keys().copied().collect();

        let accounts = accounts
            .into_iter()
            .map(|a| {
                let connections = Purpose::ALL
                    .iter()
                    .map(|&purpose| {
                        let (open, busy, idle_secs) = match slots.get(&(a.id, purpose)) {
                            None => (Some(false), false, None),
                            Some(slot) => match slot.try_lock() {
                                Ok(conn) => (Some(conn.is_some()), false, conn.as_ref().map(|c| c.used.elapsed().as_secs())),
                                Err(_) => (None, true, None),
                            },
                        };
                        ConnectionDiagnostics { purpose: purpose.name(), open, busy, idle_secs }
                    })
                    .collect();
                let sync = {
                    let syncs = self.diag.syncs.lock().unwrap_or_else(|e| e.into_inner());
                    let record = syncs.get(&a.id);
                    let oldest = record.and_then(|r| r.running.iter().min_by_key(|r| r.1));
                    SyncDiagnostics {
                        running_since: oldest.map(|r| r.1),
                        running_scope: oldest.map(|r| r.2),
                        running: record.map_or(0, |r| r.running.len()),
                        last: record.and_then(|r| r.last.clone()),
                        count: record.map_or(0, |r| r.count),
                        failures: record.map_or(0, |r| r.failures),
                        last_success: a.last_sync,
                    }
                };
                let (pending, failed, oldest_pending) = queue.get(&a.id).copied().unwrap_or_default();
                let mut busy = Vec::new();
                if history.contains(&a.id) {
                    busy.push("history");
                }
                if archiving.contains(&a.id) {
                    busy.push("backup");
                }
                if calendar_syncing.contains(&a.id) {
                    busy.push("calendar");
                }
                AccountDiagnostics {
                    id: a.id,
                    email: a.email,
                    label: a.label,
                    connections,
                    listening_task: idling.contains(&a.id),
                    listener: listeners.get(&a.id).cloned(),
                    sync,
                    queue: QueueDiagnostics {
                        pending,
                        failed,
                        oldest_pending,
                        replaying: flush_locks.get(&a.id).is_some_and(|l| l.try_lock().is_err()),
                        last_error: errors.get(&a.id).cloned(),
                    },
                    auth_refused: auth.get(&a.id).cloned(),
                    busy,
                    calendar: calendars.remove(&a.id),
                }
            })
            .collect();

        Ok(Diagnostics {
            at: now_ms(),
            app_version: String::new(),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            perf_build: crate::perf::enabled(),
            uptime_secs: self.diag.started.elapsed().as_secs(),
            resumed_secs_ago: self.last_resume.lock().unwrap().map(|t| t.elapsed().as_secs()),
            events_queued: self.events.len(),
            event_receivers: self.events.receiver_count(),
            accounts,
            runtime: self.runtime_diagnostics(),
            store: self.store.diagnostics(),
            memory: process_memory(),
            perf: crate::perf::enabled().then(|| crate::perf::summary(5 * 60 * 1000, 10)),
        })
    }

    fn runtime_diagnostics(&self) -> Option<RuntimeDiagnostics> {
        let handle = self.runtime.get().cloned().or_else(|| tokio::runtime::Handle::try_current().ok())?;
        let m = handle.metrics();
        let workers = m.num_workers();
        let (worker_busy, busy_span_ms) = self.worker_load(&m);
        Some(RuntimeDiagnostics { workers, alive_tasks: m.num_alive_tasks(), global_queue_depth: m.global_queue_depth(), worker_busy, busy_span_ms })
    }

    #[cfg(target_has_atomic = "64")]
    fn worker_load(&self, m: &tokio::runtime::RuntimeMetrics) -> (Vec<f64>, Option<u64>) {
        let now = Instant::now();
        let busy: Vec<Duration> = (0..m.num_workers()).map(|w| m.worker_total_busy_duration(w)).collect();
        let mut last = self.diag.workers.lock().unwrap_or_else(|e| e.into_inner());
        let load = match last.as_ref() {
            Some((at, before)) if before.len() == busy.len() && now > *at => {
                let span = now - *at;
                let shares = busy.iter().zip(before).map(|(b, a)| (b.saturating_sub(*a).as_secs_f64() / span.as_secs_f64()).min(1.0)).collect();
                (shares, Some(span.as_millis() as u64))
            }
            _ => (Vec::new(), None),
        };
        *last = Some((now, busy));
        load
    }

    #[cfg(not(target_has_atomic = "64"))]
    fn worker_load(&self, _: &tokio::runtime::RuntimeMetrics) -> (Vec<f64>, Option<u64>) {
        (Vec::new(), None)
    }
}

#[cfg(windows)]
fn process_memory() -> Option<ProcessMemory> {
    use std::ffi::c_void;
    /// PROCESS_MEMORY_COUNTERS_EX (psapi.h).
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut counters = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Default::default() };
    // SAFETY: plain Win32 calls; the pseudo-handle of the current process needs no closing, and
    // the structure is the size given.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then(|| ProcessMemory { resident_bytes: counters.working_set_size as u64, private_bytes: Some(counters.private_usage as u64) })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_memory() -> Option<ProcessMemory> {
    // Pages: total, resident, shared, text, lib, data, dirty. 4 KiB pages on the platforms built.
    const PAGE: u64 = 4096;
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let fields: Vec<u64> = statm.split_whitespace().filter_map(|f| f.parse().ok()).collect();
    Some(ProcessMemory { resident_bytes: fields.get(1)? * PAGE, private_bytes: fields.get(5).map(|d| d * PAGE) })
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
fn process_memory() -> Option<ProcessMemory> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_dropped_unfinished_is_recorded_as_interrupted() {
        let tracker = Tracker::new();
        let run = tracker.sync_started(1, &Scope::Inbox);
        assert_eq!(tracker.syncs.lock().unwrap()[&1].running.len(), 1);
        drop(run);
        let syncs = tracker.syncs.lock().unwrap();
        let record = &syncs[&1];
        assert!(record.running.is_empty());
        assert_eq!((record.count, record.failures), (1, 1));
        assert_eq!(record.last.as_ref().unwrap().error.as_deref(), Some("interrupted"));
        assert_eq!(record.last.as_ref().unwrap().scope, "inbox");
    }

    #[test]
    fn a_finished_sync_keeps_its_outcome() {
        let tracker = Tracker::new();
        let first = tracker.sync_started(1, &Scope::All);
        let second = tracker.sync_started(1, &Scope::Inbox);
        first.finish(None, 3);
        let syncs = tracker.syncs.lock().unwrap();
        assert_eq!(syncs[&1].running.len(), 1);
        assert_eq!(syncs[&1].last.as_ref().unwrap().new_messages, 3);
        drop(syncs);
        second.finish(Some("offline"), 0);
        assert_eq!(tracker.syncs.lock().unwrap()[&1].failures, 1);
    }

    #[test]
    fn the_listener_status_follows_its_states() {
        let tracker = Tracker::new();
        tracker.listener(2, |l| l.connecting());
        tracker.listener(2, |l| {
            l.failed("timeout".into());
            l.waiting(Duration::from_secs(60));
        });
        let status = tracker.listeners.lock().unwrap()[&2].clone();
        assert_eq!(status.state, "waiting");
        assert!(status.retry_at.is_some());
        assert_eq!(status.connections, 1);
        tracker.listener(2, |l| l.set("listening"));
        assert_eq!(tracker.listeners.lock().unwrap()[&2].retry_at, None);
        tracker.listener_gone(2);
        assert!(tracker.listeners.lock().unwrap().is_empty());
    }

    #[test]
    fn the_engine_reports_its_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let d = engine.diagnostics().unwrap();
        assert!(d.accounts.is_empty());
        assert!(d.store.page_count.unwrap_or(0) > 0);
        assert!(d.store.file_bytes > 0);
    }
}
