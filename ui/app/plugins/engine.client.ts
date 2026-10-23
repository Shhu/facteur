import { listen } from '@tauri-apps/api/event'
import type { MessageDetail } from '~/utils/types'
import type { EngineEvent } from '~/utils/types'

export default defineNuxtPlugin(async () => {
  const { syncing, syncProgress, tick, accountById, loadMailboxes, loadAccounts } = useMail()
  const toast = useToast()
  const backup = useBackup()
  const { progress, busy } = backup
  // After a UI reload: pick up the backups still running.
  backup.syncBusy()
  const ops = useOps()
  const logos = useSenderLogos()
  const outbox = useOutbox()
  const followUps = useFollowUps()
  outbox.refresh()
  followUps.refresh()
  const { editOutgoing, composeMailto, reply } = useComposer()
  const { calendarTick } = useCalendar()
  const health = useAccountHealth()
  const connectionAdvice = useConnectionAdvice()
  const ai = useAi()
  ai.refresh()
  const openEvent = useState<{ id: number, start: number } | null>('calendar-open-event', () => null)
  health.refreshAuth()
  const refinedTrust = useRefinedTrust()

  // Only the touched accounts' folders are reloaded. Throttled: a lone change (a user action) lands after a short coalescing delay; during a burst
  // (sync emits one every few ms) flushes are spaced by FLUSH_GAP, so every view reloads at most
  // ~3 times a second and no change waits more than ~300 ms.
  const COALESCE = 40
  const FLUSH_GAP = 300
  const touched = new Set<number>()
  let flushTimer: ReturnType<typeof setTimeout> | undefined
  let lastFlush = 0
  // Folder counts aggregate whole folders: during a sync they are read at most every 1.5 s, the
  // open list and thread keep the faster pace.
  const COUNTS_GAP = 1500
  let lastCounts = 0
  let countsTimer: ReturnType<typeof setTimeout> | undefined
  const countsFor = new Set<number>()
  function loadCounts() {
    countsTimer = undefined
    lastCounts = Date.now()
    const ids = [...countsFor]
    countsFor.clear()
    if (ids.length) loadMailboxes(ids)
  }
  function flush() {
    flushTimer = undefined
    lastFlush = Date.now()
    for (const id of touched) countsFor.add(id)
    touched.clear()
    if (countsFor.size && !countsTimer) countsTimer = setTimeout(loadCounts, Math.max(0, lastCounts + COUNTS_GAP - Date.now()))
    tick.value++
  }
  function changed(accountId: number) {
    touched.add(accountId)
    if (flushTimer) return
    flushTimer = setTimeout(flush, Math.max(COALESCE, lastFlush + FLUSH_GAP - Date.now()))
  }

  // A dropped SyncFinished would leave a spinner and a progress bar on until the account's next
  // sync: after a `resync`, the running syncs are read back from the engine. Sync events that
  // arrive while that read is in flight are newer than its answer, so they win over it.
  let duringRead: { runs: Map<number, boolean>, progressed: Set<number> } | undefined
  async function syncBusy() {
    const seen = { runs: new Map<number, boolean>(), progressed: new Set<number>() }
    duringRead = seen
    try {
      const d = await api.diagnostics()
      if (duringRead !== seen) return
      const running = new Set(d.accounts.filter(a => a.sync.running > 0).map(a => a.id))
      for (const [id, on] of seen.runs) {
        if (on) running.add(id)
        else running.delete(id)
      }
      // A history fetch shows a progress bar without being a sync.
      const fetching = new Set(d.accounts.filter(a => a.busy.includes('history')).map(a => a.id))
      const live = (id: number) => running.has(id) || fetching.has(id) || seen.progressed.has(id)
      syncing.value = [...running]
      syncProgress.value = Object.fromEntries(Object.entries(syncProgress.value).filter(([id]) => live(Number(id))))
    } catch {
      // Left as is: the next sync of each account settles it.
    } finally {
      if (duringRead === seen) duringRead = undefined
    }
  }

  async function resync() {
    clearTimeout(flushTimer)
    flushTimer = undefined
    touched.clear()
    try {
      await loadAccounts()
      await loadMailboxes()
    } catch {
      // The next change or sync retries.
    }
    syncBusy()
    ops.refresh()
    outbox.refresh()
    followUps.refresh()
    backup.syncBusy()
    health.refreshAuth()
    logos.refreshPending()
    calendarTick.value++
    lastFlush = Date.now()
    tick.value++
  }

  await listen<EngineEvent>('engine', ({ payload: e }) => {
    // Not in EngineEvent yet on every build: matched on its tag alone.
    if ((e as { type: string }).type === 'resync') {
      resync()
      return
    }
    switch (e.type) {
      case 'sync_started':
        duringRead?.runs.set(e.account_id, true)
        if (!syncing.value.includes(e.account_id)) syncing.value = [...syncing.value, e.account_id]
        break
      case 'sync_progress':
        duringRead?.progressed.add(e.account_id)
        syncProgress.value = { ...syncProgress.value, [e.account_id]: { mailbox: e.mailbox, done: e.done, total: e.total } }
        break
      case 'sync_finished': {
        duringRead?.runs.set(e.account_id, false)
        duringRead?.progressed.delete(e.account_id)
        const { [e.account_id]: _done, ...rest } = syncProgress.value
        syncProgress.value = rest
        syncing.value = syncing.value.filter(id => id !== e.account_id)
        {
          // Told once when an account goes wrong (not at every sync while offline), or when the
          // user asked for this sync; meanwhile the sidebar shows it. Refused credentials have
          // their own banner.
          const askedByUser = takeAskedByUser(e.account_id)
          const wentWrong = health.noteSync(e.account_id, e.error)
          const refused = e.account_id in health.authProblems.value
          if (e.error && !refused && (askedByUser || (wentWrong && navigator.onLine))) {
            const email = accountById(e.account_id)?.email
            toast.add({
              title: email ? tr('common.toast.syncFailedFor', { email }) : tr('common.toast.syncFailed'),
              description: e.error,
              color: 'warning',
              icon: 'i-lucide-cloud-off'
            })
          }
        }
        loadAccounts()
        ops.refresh()
        // A reply may have arrived, clearing its follow-up.
        followUps.refresh()
        break
      }
      case 'outbox_changed':
        outbox.refresh()
        break
      case 'send_failed':
        toast.add({
          title: tr('common.toast.sendFailed', { subject: e.subject || tr('common.noSubject') }),
          description: `${e.error}\n${e.retrying ? tr('common.toast.willRetry') : tr('common.toast.waitsInOutbox')}`,
          color: 'error',
          icon: 'i-lucide-send',
          duration: 0,
          actions: [
            { label: tr('common.toast.edit'), color: 'neutral', variant: 'outline', onClick: () => editOutgoing(e.id) },
            { label: tr('common.toast.outbox'), color: 'neutral', variant: 'ghost', onClick: () => navigateTo('/outbox') }
          ]
        })
        break
      case 'follow_ups_due':
        followUps.refresh()
        break
      case 'logos_changed':
        logos.refreshPending()
        break
      case 'ops_changed':
        ops.refresh()
        break
      case 'backup_finished':
        backup.syncBusy()
        break
      case 'backup_progress':
      case 'restore_progress':
        if (!busy.value.includes(e.account_id)) busy.value = [...busy.value, e.account_id]
        progress.value = {
          kind: e.type === 'backup_progress' ? 'backup' : 'restore',
          accountId: e.account_id,
          mailbox: e.mailbox,
          done: e.done,
          total: e.total
        }
        break
      case 'changed':
        changed(e.account_id)
        break
      case 'calendar_changed':
        calendarTick.value++
        break
      case 'auth_changed':
        health.setAuth(e.account_id, e.error)
        break
      case 'ai_changed':
        ai.refresh()
        break
      case 'one_connection_advised':
        connectionAdvice.value = [...new Set([...connectionAdvice.value, e.account_id])]
        break
      case 'trust_changed':
        refinedTrust.note(e.message_id, e.trust)
        break
    }
  })

  // Back from sleep or online again: the engine is told at once, instead of waiting for its next
  // round (its connections are likely dead, and its timers did not run while asleep). Signs: the
  // network coming back, the window shown after a long while, or a timer firing far too late.
  const wake = () => api.wake().catch(() => {})
  window.addEventListener('online', wake)
  let hiddenAt = 0
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) hiddenAt = Date.now()
    else if (hiddenAt && Date.now() - hiddenAt > 10 * 60_000) wake()
  })
  const BEAT = 20_000
  let lastBeat = Date.now()
  setInterval(() => {
    const now = Date.now()
    if (now - lastBeat > BEAT + 60_000) wake()
    lastBeat = now
  }, BEAT)

  // From Windows notifications and system mailto: links.
  await listen<string>('mailto', ({ payload }) => composeMailto(payload))
  await listen<number>('open-reply', async ({ payload: id }) => {
    try {
      reply(await api.message(id) as MessageDetail)
    } catch (e) {
      toast.add({ title: tr('common.toast.messageNotFound'), description: String(e), color: 'error' })
    }
  })
  await listen<{ account_id: number, mailbox_id: number, thread_id: number }>('open-thread', ({ payload: t }) => {
    navigateTo(threadRoute(t))
  })
  await listen<{ id: number, start: number }>('open-event', ({ payload }) => {
    openEvent.value = payload
    navigateTo('/calendar')
  })
  await listen('open-unified', () => navigateTo('/mail/unified'))
  await listen('open-screener', () => navigateTo('/screener'))
})
