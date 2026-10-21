import type { Ref } from 'vue'
import type { Address } from '~/utils/types'
import { datePresets, folderRole, folderValue, lastToken, suggestOperators, valueContext } from '~/utils/searchOperators'

export interface SearchSuggestion {
  insert: string
  label: string
  hint?: string
  icon?: string
  /** An operator's name (shown as typed) rather than one of its values. */
  operator: boolean
}

/** What to offer while a search query is typed: operators while their name is being typed (`is:`,
 *  `de`…), then their values: contacts after `de:`, `à:`, `avec:`; folders after `dans:`; dates
 *  after `après:`, `avant:`. Shared by the search fields and the command palette. Off
 *  (`enabled`), nothing is offered nor asked of the engine. */
export function useSearchSuggestions(query: Ref<string>, enabled: () => boolean, accountId: () => number | null | undefined) {
  const { t, locale } = useI18n()
  const { mailboxes, accounts, accountById } = useMail()
  /** Contact names by address, for the blocks of the fields (shared by every search field). */
  const names = useState<Record<string, string>>('search-contact-names', () => ({}))

  const word = computed(() => lastToken(query.value))
  const context = computed(() => (enabled() ? valueContext(word.value) : null))

  const contacts = ref<Address[]>([])
  /** Latest request: an older answer arriving after it is ignored. */
  let request = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  const contactQuery = computed(() => {
    const c = context.value
    return c && ['from', 'to', 'with'].includes(c.operator) ? c.value : null
  })
  watch(contactQuery, (q) => {
    clearTimeout(timer)
    const n = ++request
    if (q === null) {
      contacts.value = []
      return
    }
    // An empty query gives the recent correspondents.
    timer = setTimeout(async () => {
      try {
        const found = await api.contacts(q.trim())
        if (n !== request) return
        contacts.value = found
        for (const a of found) if (a.name) names.value[a.email.toLowerCase()] = a.name
      } catch {
        if (n === request) contacts.value = []
      }
    }, q.trim() ? 150 : 0)
  })
  onBeforeUnmount(() => clearTimeout(timer))

  /** Every folder the search may name, once per `in:` value: a role is one word for all accounts. */
  const folders = computed(() => {
    const several = accounts.value.length > 1
    const out = new Map<string, SearchSuggestion & { owners: Set<number> }>()
    for (const m of mailboxes.value) {
      // Pinned messages are `is:pinned`, whatever folder holds them.
      if (!m.selectable || m.role === 'flagged' || (accountId() != null && m.account_id !== accountId())) continue
      const value = folderValue(m, locale.value)
      const item = out.get(value)
      if (item) {
        item.owners.add(m.account_id)
        continue
      }
      // A role's word stands for the role (also a folder only named so, as for the engine).
      const role = folderRole(value)
      out.set(value, {
        insert: value,
        label: role ? roleLabels[role] : m.display_name,
        icon: role ? roleIcons[role] : 'i-lucide-folder',
        operator: false,
        owners: new Set([m.account_id])
      })
    }
    return [...out.values()].map(({ owners, ...item }) => {
      const owner = several && owners.size === 1 ? accountById([...owners][0]!) : undefined
      return { ...item, hint: owner ? accountName(owner) : undefined }
    })
  })

  const fold = (s: string) => s.toLowerCase().normalize('NFD').replace(/\p{Diacritic}/gu, '')
  const day = (iso: string) => dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(new Date(`${iso}T00:00`))

  function values(): SearchSuggestion[] {
    const c = context.value
    if (!c) return []
    const typed = fold(c.value)
    const as = (value: string) => `${c.prefix}${value}`
    if (contactQuery.value !== null) {
      return contacts.value
        .filter(a => fold(a.email) !== typed)
        .slice(0, 8)
        .map(a => ({ insert: as(a.email), label: a.name || a.email, hint: a.name ? a.email : undefined, icon: 'i-lucide-user-round', operator: false }))
    }
    if (c.operator === 'in') {
      return folders.value
        .filter(f => fold(f.insert) !== typed && (fold(f.label).includes(typed) || fold(f.insert).includes(typed)))
        .slice(0, 8)
        .map(f => ({ ...f, insert: as(f.insert) }))
    }
    if (c.operator === 'after' || c.operator === 'before') {
      return datePresets()
        .map(p => ({ insert: as(p.value), label: t(`mail.searchSuggest.dates.${p.key}`), hint: day(p.value), icon: 'i-lucide-calendar', operator: false }))
        .filter(p => p.insert.slice(c.prefix.length) !== c.value && (p.insert.slice(c.prefix.length).startsWith(c.value) || fold(p.label).includes(typed)))
    }
    return []
  }

  const items = computed<SearchSuggestion[]>(() => {
    if (!enabled()) return []
    const operators = suggestOperators(word.value, locale.value)
      .map(s => ({ insert: s.insert, label: s.insert, hint: t(`mail.searchSuggest.${s.key}`), operator: true }))
    return [...operators, ...values()]
  })

  return { items, word, context }
}
