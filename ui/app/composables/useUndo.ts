/** Must match UNDO_SECS in engine.rs. */
const UNDO_MS = 8000

/** For the `z` key. */
let last: { token: number, at: number } | null = null
/** A second `z` or click on a token still in flight does nothing. */
const inFlight = new Set<number>()
const toasts = new Map<number, string | number>()
const undoneHandlers = new Map<number, () => void>()

export function useUndo() {
  const toast = useToast()
  const showError = useErrorToast()
  // Taken now: `useState` needs the app's context, gone after an await.
  const listStale = useListStale()

  async function undo(token: number) {
    if (inFlight.has(token)) return
    inFlight.add(token)
    // Used up before the engine answers: neither `z` nor the toast can send it twice.
    if (last?.token === token) last = null
    const id = toasts.get(token)
    toasts.delete(token)
    if (id !== undefined) toast.remove(id)
    const onUndone = undoneHandlers.get(token)
    undoneHandlers.delete(token)
    try {
      await api.undo(token)
      toast.add({ title: tr('common.undone'), icon: 'i-lucide-undo-2' })
      listStale.bump()
      onUndone?.()
    } catch (e) {
      showError(e, tr('common.undoFailed'))
    } finally {
      inFlight.delete(token)
    }
  }

  function done(token: number | null, title: string, icon = 'i-lucide-check', onUndone?: () => void) {
    if (token === null) return
    // One at a time, as in Gmail: a new action replaces the previous toast (whose action can no
    // longer be undone from it) instead of stacking up.
    for (const [previous, id] of toasts) {
      toast.remove(id)
      toasts.delete(previous)
      undoneHandlers.delete(previous)
    }
    const added = toast.add({
      title,
      icon,
      duration: UNDO_MS - 500,
      orientation: 'horizontal',
      close: false,
      ui: { root: 'py-2 ps-3 pe-2 gap-2.5', icon: 'size-4', title: 'text-sm', actions: 'gap-1' },
      actions: [{ label: tr('common.undo'), icon: 'i-lucide-undo-2', size: 'xs', color: 'primary', variant: 'soft', onClick: () => { undo(token) } }]
    })
    toasts.set(token, added.id)
    if (onUndone) undoneHandlers.set(token, onUndone)
    setTimeout(() => {
      toasts.delete(token)
      undoneHandlers.delete(token)
    }, UNDO_MS)
    last = { token, at: Date.now() }
  }

  function undoLast() {
    if (last && Date.now() - last.at < UNDO_MS) undo(last.token)
  }

  return { done, undo, undoLast }
}
