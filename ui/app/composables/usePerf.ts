import { invoke, timeCommands } from '~/utils/ipc'

// Developer tools, off unless turned on in Settings > About; the engine records its slow
// operations all the same, from launch. Commands are timed from the UI so the figure is the delay
// a user feels, waiting included.

export interface CommandTiming {
  at: number
  command: string
  ms: number
  ok: boolean
}

export interface PerfRecord {
  at: number
  kind: 'sql' | 'wait' | 'hold' | 'imap'
  what: string
  ms: number
}

/** Every second: event-loop lag (a timer's drift) and JS heap (Chromium-based webviews only). */
export interface UiSample {
  at: number
  lag: number
  heap: number | null
}

const KEPT = 500
const SAMPLES_KEPT = 300
const enabled = ref(false)
const open = ref(false)
const commands = shallowRef<CommandTiming[]>([])
const engine = shallowRef<PerfRecord[]>([])
const freezes = shallowRef<{ at: number, ms: number }[]>([])
const samples = shallowRef<UiSample[]>([])
let engineSince = 0

function push<T>(list: Ref<T[]>, item: T, kept = KEPT) {
  const next = list.value.length >= kept ? list.value.slice(1 - kept) : list.value.slice()
  next.push(item)
  list.value = next
}

async function pollEngine() {
  const events = await invoke<PerfRecord[]>('perf_events', { since: engineSince }).catch(() => [])
  if (!events.length) return
  const next = engine.value.concat(events)
  engine.value = next.length > KEPT * 2 ? next.slice(-KEPT * 2) : next
  engineSince = events.at(-1)!.at
}

function clear() {
  commands.value = []
  engine.value = []
  freezes.value = []
  samples.value = []
}

/** The panel's own polling stays out of the measures. */
const QUIET = new Set(['perf_events', 'log_ui', 'diagnostics', 'read_log'])
let watching = false

/** Observers set once, the first time the tools are turned on; they record only while on. */
function observe() {
  if (watching) return
  watching = true
  try {
    new PerformanceObserver((list) => {
      if (!enabled.value) return
      for (const entry of list.getEntries()) push(freezes, { at: Date.now(), ms: entry.duration })
    }).observe({ type: 'longtask', buffered: true })
  } catch {}
  let expected = performance.now() + 1000
  setInterval(() => {
    const now = performance.now()
    const lag = Math.max(0, now - expected)
    expected = now + 1000
    // Over 10 s the page was not running (the machine asleep, a hidden window's throttled
    // timers): not a freeze of the interface, so not a sample.
    if (lag > 10_000 || !enabled.value) return
    const memory = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory
    push(samples, { at: Date.now(), lag, heap: memory ? memory.usedJSHeapSize : null }, SAMPLES_KEPT)
  }, 1000)
  window.addEventListener('keydown', (e) => {
    if (enabled.value && e.ctrlKey && e.shiftKey && (e.key as string | undefined)?.toLowerCase() === 'p') {
      e.preventDefault()
      open.value = !open.value
    }
  })
}

/** Applies the engine's state (`persist`: false) or turns the tools on or off for good. Never in
 *  the way of the app: a failure is only logged. */
async function setEnabled(on: boolean, persist = true) {
  try {
    if (persist) await api.setSetting('developer_tools', on ? '1' : '0')
    enabled.value = on
    if (on) {
      timeCommands((command, ms, ok) => {
        if (QUIET.has(command)) return
        push(commands, { at: Date.now(), command, ms, ok })
        if (ms > 500) logUi('warn', `perf: ${command} took ${Math.round(ms)} ms`)
      })
      observe()
    } else {
      timeCommands(null)
      open.value = false
      clear()
    }
  } catch (e) {
    console.warn('perf tools unavailable', e)
  }
}

export function usePerf() {
  return { enabled, open, commands, engine, freezes, samples, push, clear, pollEngine, setEnabled, SAMPLES_KEPT }
}

// Mirrors crates/core/src/engine/diagnostics.rs.

export interface ConnectionDiagnostics {
  purpose: 'sync' | 'interactive' | 'queue' | 'background'
  open: boolean | null
  busy: boolean
  idle_secs: number | null
}

export interface ListenerStatus {
  state: 'connecting' | 'listening' | 'syncing' | 'waiting' | 'unsupported'
  since: number
  notify: boolean | null
  retry_at: number | null
  last_error: string | null
  last_error_at: number | null
  connections: number
}

export interface LastSync {
  started: number
  ms: number
  scope: 'all' | 'inbox' | 'folders'
  error: string | null
  new_messages: number
}

export interface AccountDiagnostics {
  id: number
  email: string
  label: string | null
  connections: ConnectionDiagnostics[]
  listening_task: boolean
  listener: ListenerStatus | null
  sync: {
    running_since: number | null
    running_scope: LastSync['scope'] | null
    running: number
    last: LastSync | null
    count: number
    failures: number
    last_success: number | null
  }
  queue: { pending: number, failed: number, oldest_pending: number | null, replaying: boolean, last_error: string | null }
  auth_refused: string | null
  busy: ('history' | 'backup' | 'calendar')[]
  calendar: { state: string, message: string | null, last_sync: number | null, ignored: boolean } | null
}

export interface PerfKindSummary {
  kind: PerfRecord['kind']
  count: number
  total_ms: number
  worst: PerfRecord[]
}

export interface Diagnostics {
  at: number
  app_version: string
  os: string
  arch: string
  perf_build: boolean
  uptime_secs: number
  resumed_secs_ago: number | null
  events_queued: number
  event_receivers: number
  accounts: AccountDiagnostics[]
  runtime: { workers: number, alive_tasks: number, global_queue_depth: number, worker_busy: number[], busy_span_ms: number | null } | null
  store: {
    path: string | null
    file_bytes: number
    wal_bytes: number
    page_size: number | null
    page_count: number | null
    freelist_count: number | null
    cache_size: number | null
    connections: { role: 'writer' | 'reader', busy: boolean, cache_used_bytes: number | null, cache_hits: number | null, cache_misses: number | null }[]
  }
  memory: { resident_bytes: number, private_bytes: number | null } | null
  perf: { window_secs: number, kinds: PerfKindSummary[] } | null
}

export interface LogLine {
  ts: number
  level: 'error' | 'warn' | 'info' | 'debug' | 'trace'
  target: string
  message: string
  account: number | null
}
