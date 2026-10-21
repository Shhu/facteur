import twColors from 'tailwindcss/colors'
import type { Account, CalendarEvent, CalendarInfo, CalendarStatus } from '~/utils/types'

// The engine emits `calendar_changed` after a calendar sync (plugins/engine.client.ts bumps
// `calendarTick`); the shared `tick` also reloads, spaced out.

let watching = false
let calendarsRequest = 0

const palette = twColors as unknown as Record<string, Record<number, string>>

export function useCalendar() {
  const calendars = useState<CalendarInfo[]>('calendars', () => [])
  const statuses = useState<CalendarStatus[]>('calendar-status', () => [])
  /** Until the first answer, nothing is said about missing calendars. */
  const loaded = useState('calendars-loaded', () => false)
  const calendarTick = useState('calendar-tick', () => 0)
  const syncingCalendars = useState<(number | null)[]>('calendar-syncing', () => [])
  const { accounts, accountById, tick, loadAccounts } = useMail()
  const showError = useErrorToast()
  const toast = useToast()

  async function load() {
    const mine = ++calendarsRequest
    try {
      const [list, states] = await Promise.all([api.calendars(), api.calendarStatus()])
      if (mine !== calendarsRequest) return
      calendars.value = list
      statuses.value = states
      loaded.value = true
    } catch {
      // The engine may not know calendars yet: nothing shown, the next change retries.
      if (mine === calendarsRequest) loaded.value = true
    }
  }

  // One shared watcher, whichever screen asked first.
  if (!watching && import.meta.client) {
    watching = true
    const scope = effectScope(true)
    scope.run(() => {
      const reloader = useReloader(load)
      reloader.now()
      watch(calendarTick, () => reloader.now())
      watch(tick, reloader.schedule)
      watch(() => accounts.value.map(a => a.id).join(), () => reloader.now())
    })
  }

  const calendarById = (id: number) => calendars.value.find(c => c.id === id) ?? null
  const statusOf = (accountId: number) => statuses.value.find(s => s.account_id === accountId) ?? null
  const isIgnored = (accountId: number) => statusOf(accountId)?.ignored === true
  const used = computed(() => statuses.value.filter(s => !s.ignored))
  const calendarsOf = (accountId: number) => calendars.value.filter(c => c.account_id === accountId)
  const hasCalendars = computed(() => calendars.value.length > 0)
  /** The screen is offered even when every account still needs something (reconnect, an address)
   *  before showing a calendar. */
  const calendarsAvailable = computed(() => calendars.value.length > 0 || used.value.length > 0)

  function calendarColor(calendarId: number, accountId?: number): string {
    const c = calendarById(calendarId)
    if (c?.color && /^#[0-9a-f]{6}$/i.test(c.color)) return c.color
    return accountCssColor(c?.account_id ?? accountId ?? 0)
  }
  function accountCssColor(accountId: number): string {
    const a = accountById(accountId)
    const name = a ? accountColorName(a, accounts.value) : 'orange'
    return palette[name]?.[500] ?? '#f97316'
  }
  const eventColor = (e: CalendarEvent) => calendarColor(e.calendar_id, e.account_id)

  async function setEnabled(c: CalendarInfo, enabled: boolean) {
    // Shown at once; the engine's answer (or its error) settles it.
    calendars.value = calendars.value.map(x => (x.id === c.id ? { ...x, enabled } : x))
    try {
      await api.setCalendarEnabled(c.id, enabled)
    } catch (e) {
      showError(e)
    }
    calendarTick.value++
  }

  /** True when the engine took the change. */
  async function setIgnored(accountId: number, ignored: boolean): Promise<boolean> {
    // Shown at once; the engine's answer (or its error) settles it.
    const before = { statuses: statuses.value, calendars: calendars.value }
    statuses.value = statuses.value.map(s => (s.account_id === accountId ? { ...s, ignored } : s))
    if (ignored) calendars.value = calendars.value.filter(c => c.account_id !== accountId)
    const optimistic = { statuses: statuses.value, calendars: calendars.value }
    let ok = true
    try {
      await api.setCalendarIgnored(accountId, ignored)
    } catch (e) {
      showError(e)
      ok = false
      // Put the previous state back, unless a reload already replaced it meanwhile.
      if (statuses.value === optimistic.statuses) statuses.value = before.statuses
      if (calendars.value === optimistic.calendars) calendars.value = before.calendars
    }
    calendarTick.value++
    return ok
  }

  async function sync(accountId: number | null = null) {
    if (syncingCalendars.value.includes(accountId)) return
    syncingCalendars.value = [...syncingCalendars.value, accountId]
    try {
      await api.syncCalendars(accountId)
    } catch (e) {
      showError(e, tr('tools.calendar.syncFailed'))
    } finally {
      syncingCalendars.value = syncingCalendars.value.filter(id => id !== accountId)
      calendarTick.value++
    }
  }
  const isSyncing = (accountId: number | null = null) =>
    syncingCalendars.value.includes(accountId) || (accountId !== null && syncingCalendars.value.includes(null))

  /** Runs the OAuth flow again so that Google grants the calendar scope. */
  const reconnecting = useState<number | null>('calendar-reconnecting', () => null)
  async function reconnect(account: Account) {
    if (account.auth.kind !== 'oauth2' || reconnecting.value !== null) return
    reconnecting.value = account.id
    try {
      // From the calendar or the contacts: asks for what they need.
      const identity = await api.oauthLogin(account.auth.provider, account.email, true)
      await api.reconnect(account.id, identity.ticket)
      await loadAccounts()
      toast.add({ title: tr('settings.accounts.reconnected'), description: account.email, icon: 'i-lucide-check' })
      reconnecting.value = null
      await sync(account.id)
    } catch (e) {
      showError(e, tr('settings.accounts.reconnectFailed'))
    } finally {
      reconnecting.value = null
    }
  }

  async function setCaldavUrl(accountId: number, url: string | null) {
    try {
      // The engine syncs that account right after (`calendar_changed` follows).
      await api.setCaldavUrl(accountId, url?.trim() || null)
      calendarTick.value++
      return true
    } catch (e) {
      showError(e)
      return false
    }
  }

  /** The oldest across accounts; accounts with nothing to sync (unavailable, not configured) do
   *  not count. */
  const lastSync = (accountId: number | null = null) => {
    const times = used.value
      .filter(s => (accountId === null || s.account_id === accountId) && s.state !== 'unavailable' && s.state !== 'not_configured')
      .map(s => s.last_sync)
    return times.length && !times.includes(null) ? Math.min(...(times as number[])) : null
  }

  /** `to` exclusive; outside it the calendars say nothing. `null`: no account synced yet. */
  const syncedSpan = computed(() => {
    const spans = used.value.filter(s => s.synced_from !== null && s.synced_to !== null && s.synced_from !== undefined && s.synced_to !== undefined)
    if (!spans.length) return null
    return { from: Math.max(...spans.map(s => s.synced_from!)), to: Math.min(...spans.map(s => s.synced_to!)) }
  })

  return {
    calendars, statuses, loaded, calendarTick, hasCalendars, calendarsAvailable, reconnecting,
    calendarById, statusOf, isIgnored, calendarsOf, calendarColor, accountCssColor, eventColor,
    setEnabled, setIgnored, sync, isSyncing, reconnect, setCaldavUrl, lastSync, syncedSpan, reload: load
  }
}

/** Reloaded when calendars change, on the shared tick (spaced out) and when the range moves. */
export function useCalendarEvents(range: () => { from: number, to: number } | null, interval = 2000) {
  const { calendarTick } = useCalendar()
  const { tick } = useMail()
  const events = ref<CalendarEvent[] | null>(null)
  const loadedRange = ref<string | null>(null)
  let request = 0

  async function load() {
    const r = range()
    const mine = ++request
    if (!r) {
      events.value = []
      return
    }
    try {
      const list = await api.calendarEvents(r.from, r.to)
      if (mine !== request) return
      events.value = list
      loadedRange.value = `${r.from}-${r.to}`
    } catch {
      if (mine === request) events.value ??= []
    }
  }
  const reloader = useReloader(load, interval)
  watch(() => JSON.stringify(range()), () => reloader.now(), { immediate: true })
  watch(calendarTick, () => reloader.now())
  watch(tick, reloader.schedule)

  return { events, loadedRange, reload: reloader.now }
}
