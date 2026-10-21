import { shallowReactive } from 'vue'

// Logos are requested in batches (a 100-row list makes one call) and cached per domain: every
// address of a brand shares its logo. `null`: no logo (or personal address, or logos turned off).

/** LRU bound. */
const MAX_DOMAINS = 500
/** Re-requests of a domain still without a logo, on `logos_changed`. */
const MAX_RETRIES = 3

/** Reactive per key: an avatar only tracks its own domain. */
const cache = shallowReactive(new Map<string, string | null>())
const recency = new Set<string>()
/** Domain → one address of it, for the next batch. */
const queue = new Map<string, string>()
const inflight = new Set<string>()
/** Still without a logo: the engine may still be looking. Asked again on `logos_changed`, only a
 *  few times so domains that have none do not loop forever. */
const pending = new Map<string, { email: string, tries: number }>()
let scheduled = false

const domainOf = (email: string) => email.slice(email.lastIndexOf('@') + 1)

function touch(domain: string) {
  recency.delete(domain)
  recency.add(domain)
}

function remember(domain: string, logo: string | null) {
  cache.set(domain, logo)
  touch(domain)
  while (recency.size > MAX_DOMAINS) {
    const oldest = recency.values().next().value!
    recency.delete(oldest)
    cache.delete(oldest)
    pending.delete(oldest)
  }
}

function schedule() {
  if (scheduled || !queue.size) return
  scheduled = true
  setTimeout(flush, 0)
}

async function flush() {
  scheduled = false
  const batch = [...queue]
  queue.clear()
  if (!batch.length) return
  for (const [domain] of batch) inflight.add(domain)
  try {
    const found = await api.senderLogos(batch.map(([, email]) => email))
    for (const [domain, email] of batch) {
      const logo = found[email] ?? null
      remember(domain, logo)
      if (logo) {
        pending.delete(domain)
        continue
      }
      const tries = pending.get(domain)?.tries ?? 0
      if (tries < MAX_RETRIES && pending.size < MAX_DOMAINS) pending.set(domain, { email, tries })
      else pending.delete(domain)
    }
  } catch {
    // Logos are optional: on error the initials stay.
  } finally {
    for (const [domain] of batch) inflight.delete(domain)
  }
}

export function useSenderLogos() {
  function logoOf(email: string | null | undefined): string | null {
    return email ? cache.get(domainOf(email.toLowerCase())) ?? null : null
  }

  function request(email: string | undefined) {
    const e = email?.toLowerCase()
    if (!e) return
    const domain = domainOf(e)
    if (cache.has(domain)) {
      touch(domain)
      return
    }
    if (queue.has(domain) || inflight.has(domain)) return
    queue.set(domain, e)
    schedule()
  }

  function refreshPending() {
    for (const [domain, p] of pending) {
      if (inflight.has(domain)) continue
      pending.set(domain, { ...p, tries: p.tries + 1 })
      queue.set(domain, p.email)
    }
    schedule()
  }

  function reset() {
    cache.clear()
    recency.clear()
    pending.clear()
  }

  return { logoOf, request, refreshPending, reset }
}
