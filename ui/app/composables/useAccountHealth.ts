import type { Account } from '~/utils/types'

/** A requested sync reports its outcome even when the error is unchanged; a background sync only
 *  reports when an account goes wrong. */
const askedByUser = new Set<number>()

/** An answer older than the one applied is dropped. */
let authRequest = 0
let authApplied = 0

/** Errors come with the engine's `sync_finished` event, not from here: no second toast. */
export async function syncByUser(ids: number[]) {
  for (const id of ids) askedByUser.add(id)
  await Promise.allSettled(ids.map(id => api.sync(id)))
}

/** Forgotten once read. */
export function takeAskedByUser(id: number): boolean {
  return askedByUser.delete(id)
}

export function useAccountHealth() {
  const syncErrors = useState<Record<number, string>>('sync-errors', () => ({}))
  const authProblems = useState<Record<number, string>>('auth-problems', () => ({}))

  async function refreshAuth() {
    const mine = ++authRequest
    try {
      const problems = await api.authProblems()
      if (mine < authApplied) return
      authApplied = mine
      authProblems.value = problems
    } catch {
      // Indicator only: the next event updates it.
    }
  }

  function setAuth(accountId: number, error: string | null) {
    const { [accountId]: _old, ...rest } = authProblems.value
    authProblems.value = error ? { ...rest, [accountId]: error } : rest
  }

  /** True when the account just went wrong (it was fine before). */
  function noteSync(accountId: number, error: string | null): boolean {
    const before = syncErrors.value[accountId]
    const { [accountId]: _old, ...rest } = syncErrors.value
    syncErrors.value = error ? { ...rest, [accountId]: error } : rest
    return !!error && before === undefined
  }

  function troubled(accounts: Account[]) {
    return accounts
      .filter(a => a.id in authProblems.value || a.id in syncErrors.value)
      .map(a => ({ account: a, auth: authProblems.value[a.id] ?? null, sync: syncErrors.value[a.id] ?? null }))
  }

  return { syncErrors, authProblems, refreshAuth, setAuth, noteSync, troubled }
}
