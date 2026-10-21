import { getVersion } from '@tauri-apps/api/app'
import { invoke } from '~/utils/ipc'
import { listen } from '@tauri-apps/api/event'
import { flushDraftBeforeQuit } from '~/composables/useComposer'
import { flushOutboxBeforeQuit } from '~/composables/useOutbox'

interface Available {
  version: string
  notes: string
}

/** Only the Windows exe updates itself; the Android app is installed by hand for now. */
const supported = typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent)

const version = ref<string | null>(null)
const available = shallowRef<Available | null>(null)
const checking = ref(false)
const installing = ref(false)
/** 0 to 1; null: size unknown. */
const progress = ref<number | null>(null)
let listening = false

/** The new exe is downloaded, its signature checked, it replaces this one, and facteur restarts. */
export function useUpdater() {
  const toast = useToast()
  const showError = useErrorToast()

  if (version.value === null) getVersion().then((v) => { version.value = v }).catch(() => {})
  if (supported && !listening) {
    listening = true
    listen<{ done: number, total: number | null }>('update-progress', ({ payload }) => {
      progress.value = payload.total ? Math.min(1, payload.done / payload.total) : null
    }).catch(() => {})
  }

  /** `manual`: the user is told the outcome either way. */
  async function checkNow(manual = false) {
    if (!supported || checking.value || installing.value) return
    checking.value = true
    try {
      available.value = await invoke<Available | null>('update_check')
      if (manual && !available.value) toast.add({ title: tr('settings.about.upToDate'), icon: 'i-lucide-check' })
    } catch (e) {
      if (manual) showError(e, tr('settings.about.checkFailed'))
      else console.warn('update check failed', e)
    } finally {
      checking.value = false
    }
  }

  async function install() {
    if (!available.value || installing.value) return
    installing.value = true
    progress.value = 0
    try {
      // Downloaded and checked first: the composer stays usable meanwhile, so the flushes
      // below run right before the restart, not before a download that can take minutes.
      await invoke('update_download')
      // The restart quits without the window's close guard: save the open draft first, and
      // stay when its save failed and the user keeps it.
      if (!await flushDraftBeforeQuit()) return
      // Nor the outbox's: the messages just sent leave before the restart.
      await flushOutboxBeforeQuit()
      await invoke('update_apply')
    } catch (e) {
      showError(e, tr('settings.about.installFailed'))
    } finally {
      invoke('update_discard').catch(() => {})
      installing.value = false
      progress.value = null
    }
  }

  return { supported, version, available, checking, installing, progress, checkNow, install }
}
