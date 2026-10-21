import { documentDir, join } from '@tauri-apps/api/path'
import type { BackupStatus, RestoreRequest } from '~/utils/types'

const KEY = 'facteur:backup'
const DAY = 24 * 3600
/** An incomplete backup is tried again after this, not a day later. */
const HOUR = 3600

interface Settings {
  dest: string | null
  auto: boolean
}

export interface BackupProgress {
  kind: 'backup' | 'restore'
  accountId: number
  mailbox: string
  done: number
  total: number
}

/** Settings persistence is wired once, however many callers. */
let persisting = false
/** Later hourly attempts fail quietly; the settings show the failure until a backup succeeds. */
let autoFailureToasted = false

function readSettings(): Settings {
  try {
    return { dest: null, auto: true, ...JSON.parse(localStorage.getItem(KEY) ?? '{}') }
  } catch {
    return { dest: null, auto: true }
  }
}

export function useBackup() {
  const settings = useState<Settings>('backup-settings', readSettings)
  const status = useState<BackupStatus[]>('backup-status', () => [])
  /** The engine is the source of truth: the UI may have been reloaded mid-operation. */
  const busy = useState<number[]>('backup-busy', () => [])
  const looping = useState('backup-looping', () => false)
  const running = computed(() => looping.value || busy.value.length > 0)
  const progress = useState<BackupProgress | null>('backup-progress', () => null)
  /** Cleared by the next complete backup. */
  const autoFailure = useState<{ at: number, message: string } | null>('backup-auto-failure', () => null)
  const { accounts } = useMail()
  const showError = useErrorToast()
  const toast = useToast()

  if (!persisting) {
    persisting = true
    watch(settings, (s) => {
      try {
        localStorage.setItem(KEY, JSON.stringify(s))
      } catch {
        // Storage unavailable: the setting lasts for the session.
      }
    }, { deep: true })
  }

  async function dest(): Promise<string> {
    settings.value.dest ??= await join(await documentDir(), 'facteur-sauvegardes')
    return settings.value.dest
  }

  async function syncBusy() {
    try {
      busy.value = await api.backupBusy()
    } catch {
      busy.value = []
    }
    if (!busy.value.length && !looping.value) progress.value = null
  }

  /** `quiet`: the error is returned, not toasted. */
  async function refresh(quiet = false): Promise<string | null> {
    try {
      status.value = await api.backupStatus(await dest())
      return null
    } catch (e) {
      if (!quiet) showError(e)
      return String(e)
    }
  }

  /** `auto`: the hourly automatic backup, silent on success, reporting a failure once per session. */
  async function backupAll(ids = accounts.value.map(a => a.id), auto = false) {
    if (looping.value) return
    // An account already running (started before a reload, say) is not started again.
    await syncBusy()
    ids = ids.filter(id => !busy.value.includes(id))
    if (!ids.length) return
    looping.value = true
    let downloaded = 0
    const failures: string[] = []
    try {
      const target = await dest()
      for (const id of ids) {
        try {
          downloaded += (await api.backup(id, target)).downloaded
        } catch (e) {
          failures.push(`${accounts.value.find(a => a.id === id)?.email ?? id} : ${e}`)
        }
      }
    } catch (e) {
      failures.push(String(e))
    } finally {
      looping.value = false
      await syncBusy()
      await refresh(auto)
    }
    if (failures.length) {
      reportFailure(failures.join('\n'), auto)
      return
    }
    autoFailure.value = null
    if (!auto) {
      toast.add({
        title: tr('common.backup.done'),
        description: downloaded ? tr('common.backup.newMessages', { n: downloaded }) : tr('common.backup.upToDate'),
        icon: 'i-lucide-hard-drive'
      })
    }
  }

  function reportFailure(message: string, auto: boolean) {
    if (auto) {
      autoFailure.value = { at: Date.now() / 1000, message }
      if (autoFailureToasted) return
      autoFailureToasted = true
    }
    toast.add({
      title: tr('common.backup.incomplete'),
      description: auto ? `${message}\n${tr('common.backup.autoFailedHint')}` : message,
      color: 'error',
      icon: 'i-lucide-hard-drive'
    })
  }

  /** Never backed up or over a day ago; an incomplete last backup (messages the server did not
   *  send) is due after an hour. */
  async function autoBackup() {
    if (!settings.value.auto || looping.value) return
    await syncBusy()
    const error = await refresh(true)
    if (error) {
      reportFailure(error, true)
      return
    }
    const now = Date.now() / 1000
    const due = status.value
      .filter(s => !busy.value.includes(s.account_id) && (!s.manifest || now - s.manifest.last_backup > (s.manifest.failed ? HOUR : DAY)))
      .map(s => s.account_id)
    if (due.length) await backupAll(due, true)
  }

  async function restore(source: string, accountId: number, request: RestoreRequest) {
    if (running.value) return null
    looping.value = true
    try {
      const stats = await api.restore(await dest(), source, accountId, request)
      api.sync(accountId).catch(() => {})
      return stats
    } catch (e) {
      showError(e, tr('common.backup.restoreFailed'))
      return null
    } finally {
      looping.value = false
      await syncBusy()
    }
  }

  return { settings, status, running, busy, progress, autoFailure, dest, refresh, syncBusy, backupAll, autoBackup, restore }
}
