<script setup lang="ts">
import type { CommandPaletteGroup } from '@nuxt/ui'
import { useMediaQuery } from '@vueuse/core'
import type { PaletteEntry } from '~/components/PaletteIcon.vue'
import type { MessageSummary } from '~/utils/types'

const open = useState('command-palette', () => false)
const term = ref('')
const { accounts, accountId, accountById, mailboxes, unifiedUnread, syncAll } = useMail()
const { calendarsAvailable } = useCalendar()
const { compose, open: composing } = useComposer()
const { undoLast } = useUndo()
const { list: saved, add: addSaved } = useSavedSearches()
const colorMode = useColorMode()
const toast = useToast()
const { t, locale } = useI18n()
const desktop = useMediaQuery('(min-width: 1024px)')
const root = useTemplateRef<HTMLElement>('root')

defineShortcuts({
  meta_k: { usingInput: true, handler: () => { if (!composing.value) open.value = !open.value } }
})

function go(to: string | { path: string, query?: Record<string, string> }) {
  open.value = false
  navigateTo(to)
}

function run(fn: () => unknown) {
  open.value = false
  fn()
}

const query = computed(() => term.value.trim())
/** Parsed like the engine does, for the chips only: the query is sent as typed. */
const parsed = computed(() => parseSearch(term.value))

function focusInput() {
  nextTick(() => root.value?.querySelector('input')?.focus())
}
function clear() {
  term.value = ''
  focusInput()
}
/** In the interface's language (the engine understands French and English operators). */
const operators = computed(() => operatorShortcuts(locale.value))
const footerOperators = computed(() => operatorShortcuts(locale.value, true))
function insertOperator(op: string) {
  term.value = appendOperator(term.value, op)
  focusInput()
}

function searchEverywhere() {
  if (query.value) go({ path: '/mail/unified', query: { q: query.value } })
}
const alreadySaved = computed(() => saved.value.some(s => s.query === query.value))
function saveSearch() {
  const q = query.value
  if (!q || alreadySaved.value) return
  run(async () => {
    await addSaved('', q)
    toast.add({ title: t('mail.searchHelp.savedToast'), description: t('mail.searchHelp.savedToastDesc'), icon: 'i-lucide-bookmark' })
  })
}
/** Ctrl/⌘ Enter: the whole search, whatever row is highlighted (the list ignores modified Enter). */
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && query.value) {
    e.preventDefault()
    searchEverywhere()
  }
}

/** Operators and their values while one is typed, as under the search fields. */
const { items: suggestions } = useSearchSuggestions(term, () => open.value, () => null)
function complete(insert: string) {
  term.value = completeOperator(term.value, insert)
  focusInput()
}

const found = ref<MessageSummary[]>([])
const searching = ref(false)
let timer: ReturnType<typeof setTimeout> | undefined
watch(term, (q) => {
  clearTimeout(timer)
  // `from:` without its value: searching it would only be a slow lookup of a word nobody means.
  if (q.trim().length < 2 || incompleteOperator(q)) {
    found.value = []
    return
  }
  timer = setTimeout(async () => {
    searching.value = true
    try {
      const lists = await Promise.all(accounts.value.map(a => api.search(a.id, q)))
      if (term.value === q) found.value = lists.flat().sort((a, b) => b.date - a.date).slice(0, 8)
    } catch {
      found.value = []
    } finally {
      searching.value = false
    }
  }, 200)
})
watch(open, (o) => {
  if (!o) term.value = ''
})

const actionItems = computed<PaletteEntry[]>(() => [
  { label: t('app.palette.newMessage'), icon: 'i-lucide-square-pen', tone: 'teal', kbds: ['c'], onSelect: () => run(() => accountId.value !== null && compose()) },
  { label: t('app.palette.syncAll'), icon: 'i-lucide-refresh-cw', tone: 'blue', onSelect: () => run(syncAll) },
  { label: t('app.palette.undoLast'), icon: 'i-lucide-undo-2', tone: 'orange', kbds: ['z'], onSelect: () => run(undoLast) },
  {
    label: colorMode.value === 'dark' ? t('app.palette.lightTheme') : t('app.palette.darkTheme'),
    icon: colorMode.value === 'dark' ? 'i-lucide-sun' : 'i-lucide-moon',
    tone: 'amber',
    onSelect: () => run(() => { colorMode.preference = colorMode.value === 'dark' ? 'light' : 'dark' })
  }
])

const screenerWaiting = computed(() => mailboxes.value.filter(m => m.role === 'screener').reduce((n, m) => n + m.total, 0))
const pageItems = computed<PaletteEntry[]>(() => [
  { label: t('app.palette.home'), icon: 'i-lucide-house', onSelect: () => go('/') },
  { label: t('app.palette.unified'), icon: 'i-lucide-inbox', tone: 'primary', badge: unifiedUnread.value || undefined, onSelect: () => go('/mail/unified') },
  { label: t('app.palette.done'), icon: 'i-lucide-check-check', tone: 'emerald', onSelect: () => go('/mail/done') },
  // Only once an account filters its new senders; the count is of the messages waiting.
  ...(accounts.value.some(a => a.screener_since !== null)
    ? [{ label: t('app.palette.screener'), icon: roleIcons.screener, tone: 'blue' as const, badge: screenerWaiting.value || undefined, onSelect: () => go('/screener') }]
    : []),
  ...(calendarsAvailable.value ? [{ label: t('app.palette.calendar'), icon: 'i-lucide-calendar', tone: 'orange' as const, onSelect: () => go('/calendar') }] : []),
  { label: t('app.palette.outbox'), icon: 'i-lucide-send-horizontal', tone: 'sky', onSelect: () => go('/outbox') },
  { label: t('app.palette.rules'), icon: 'i-lucide-workflow', tone: 'indigo', onSelect: () => go('/rules') },
  { label: t('app.palette.settings'), icon: 'i-lucide-settings', onSelect: () => go('/settings') }
])

// The account's name goes in `suffix` so that typing it finds the entry; it is drawn as a pill.
const statsItems = computed<PaletteEntry[]>(() => accounts.value.map(a => ({
  label: t('app.palette.statsGroup'),
  suffix: accountName(a),
  account: a.id,
  accountAvatar: true,
  onSelect: () => go(`/stats/${a.id}`)
})))

const savedItems = computed<PaletteEntry[]>(() => saved.value.map(s => ({
  label: s.name,
  suffix: s.query,
  hint: s.name !== s.query ? s.query : undefined,
  icon: 'i-lucide-search',
  onSelect: () => go({ path: '/mail/unified', query: { q: s.query } })
})))

/** Every selectable folder of every account, once. A role names one folder per account: should
 *  two folders share it, the next ones keep their own name rather than read the same. */
const folderItems = computed<PaletteEntry[]>(() => {
  const several = accounts.value.length > 1
  const ids = new Set<number>()
  const roles = new Set<string>()
  const out: PaletteEntry[] = []
  for (const m of mailboxes.value) {
    if (!m.selectable || ids.has(m.id)) continue
    ids.add(m.id)
    const roleKey = `${m.account_id}:${m.role}`
    const byRole = m.role !== null && !roles.has(roleKey)
    if (byRole) roles.add(roleKey)
    const account = accountById(m.account_id)
    out.push({
      label: byRole ? roleLabels[m.role!] : m.display_name,
      suffix: account ? accountName(account) : undefined,
      account: several && account ? m.account_id : undefined,
      icon: m.role ? roleIcons[m.role] : 'i-lucide-folder',
      tone: m.role === 'flagged' ? 'amber' : 'neutral',
      onSelect: () => go(`/mail/${m.id}`)
    })
  }
  return out
})

const groups = computed<CommandPaletteGroup<PaletteEntry>[]>(() => {
  const out: CommandPaletteGroup<PaletteEntry>[] = []
  if (suggestions.value.length) {
    out.push({
      id: 'suggest',
      label: t('mail.searchSuggest.label'),
      ignoreFilter: true,
      items: suggestions.value.map(s => ({
        label: s.label,
        icon: s.operator ? 'i-lucide-text-cursor-input' : (s.icon ?? 'i-lucide-search'),
        tone: 'primary',
        hint: s.hint,
        onSelect: () => complete(s.insert)
      }))
    })
  }
  if (found.value.length) {
    out.push({
      id: 'messages',
      label: t('app.palette.messages'),
      // Already sorted and filtered by the engine.
      ignoreFilter: true,
      items: found.value.map(m => ({
        label: m.subject || t('common.noSubject'),
        slot: 'message',
        message: m,
        ui: { item: 'min-h-14' },
        onSelect: () => go(threadRoute(m))
      }))
    })
  }
  out.push({ id: 'actions', label: t('app.palette.actions'), items: actionItems.value })
  out.push({ id: 'pages', label: t('app.palette.goTo'), items: pageItems.value })
  out.push({ id: 'stats', label: t('app.palette.statsGroup'), items: statsItems.value })
  if (savedItems.value.length) out.push({ id: 'saved', label: t('app.palette.savedSearches'), items: savedItems.value })
  out.push({ id: 'folders', label: t('app.palette.folders'), items: folderItems.value })
  if (query.value) {
    out.push({
      id: 'search',
      label: t('app.palette.search'),
      // Always offered while something is typed, after whatever matched.
      ignoreFilter: true,
      items: [
        { label: t('app.palette.searchEverywhere', { q: query.value }), icon: 'i-lucide-search', tone: 'primary', kbds: ['meta', 'enter'], onSelect: searchEverywhere },
        {
          label: alreadySaved.value ? t('mail.searchHelp.alreadySaved') : t('app.palette.saveSearch'),
          icon: alreadySaved.value ? 'i-lucide-bookmark-check' : 'i-lucide-bookmark',
          tone: 'amber',
          disabled: alreadySaved.value,
          onSelect: saveSearch
        }
      ]
    })
  }
  return out
})

const ITEM = [
  'min-h-11 items-center gap-3 px-2.5 py-1.5 text-sm cursor-pointer rounded-[11px] before:inset-0 before:rounded-[11px]',
  'data-highlighted:not-data-disabled:before:bg-primary-50 dark:data-highlighted:not-data-disabled:before:bg-primary/10',
  'data-highlighted:not-data-disabled:before:ring-1 data-highlighted:not-data-disabled:before:ring-inset',
  'data-highlighted:not-data-disabled:before:ring-primary-300 dark:data-highlighted:not-data-disabled:before:ring-primary/40'
].join(' ')

const paletteUi = computed(() => ({
  // Filters (order 1) sit between the input and the list (order 2), the footer comes last
  // (order 3): the footer slot's wrapper is `contents`, so its children are laid out here.
  root: 'flex-1 divide-y-0',
  input: 'w-full border-b border-default',
  content: 'order-2 min-h-0 flex-1',
  viewport: desktop.value ? 'h-[420px] max-h-[52dvh] flex-none divide-y-0 px-2 pt-1.5 pb-2.5' : 'divide-y-0 px-2 pt-1 pb-[calc(1rem+env(safe-area-inset-bottom))]',
  group: 'p-0',
  label: 'flex items-baseline gap-2 px-2.5 pt-3 pb-1.5 text-[11px] font-bold tracking-[0.06em] text-muted uppercase',
  item: ITEM,
  empty: 'px-5 py-12 text-sm text-muted',
  footer: 'contents'
}))

const inputProps = {
  ui: {
    base: 'h-[58px] ps-[50px] pe-24 text-base text-highlighted',
    leading: 'ps-[18px]',
    leadingIcon: 'size-5 text-primary',
    trailing: 'gap-1.5 pe-3.5'
  }
}

const modalUi = computed(() => desktop.value
  ? {
      overlay: 'bg-neutral-900/30 dark:bg-black/60',
      content: [
        'top-[9dvh] translate-y-0 max-w-[640px] divide-y-0 overflow-hidden rounded-[18px] bg-default ring-0',
        'border border-default dark:border-neutral-700/70',
        'shadow-[0_28px_70px_color-mix(in_oklab,var(--ui-color-neutral-900)_25%,transparent)] dark:shadow-[0_28px_70px_rgb(0_0_0/0.6)]'
      ].join(' ')
    }
  : { content: 'divide-y-0 bg-neutral-100 dark:bg-neutral-950' })
</script>

<template>
  <UModal v-model:open="open" :fullscreen="!desktop" :title="$t('app.palette.title')" :ui="modalUi">
    <template #content>
      <div ref="root" class="flex min-h-0 flex-1 flex-col" :class="{ 'pt-[env(safe-area-inset-top)]': !desktop }">
        <template v-if="!desktop">
          <div class="flex items-center gap-2.5 pt-3.5 pe-3 pb-2.5 ps-4">
            <label class="flex h-12 min-w-0 flex-1 items-center gap-2.5 rounded-[14px] border-[1.5px] border-primary-300 bg-default ps-3.5 pe-1 dark:border-primary/50">
              <UIcon name="i-lucide-search" class="size-[19px] flex-none text-primary" />
              <span class="sr-only">{{ $t('app.palette.title') }}</span>
              <input
                :value="term"
                type="text"
                enterkeyhint="search"
                autocomplete="off"
                autocapitalize="off"
                spellcheck="false"
                :placeholder="$t('app.palette.placeholderShort')"
                class="min-w-0 flex-1 bg-transparent text-base text-highlighted outline-none placeholder:text-dimmed"
                @input="term = ($event.target as HTMLInputElement).value"
                @keydown.enter.prevent="searchEverywhere"
              >
              <UIcon v-if="searching" name="i-lucide-loader-circle" class="size-4 flex-none animate-spin text-dimmed" />
              <button
                v-if="term"
                type="button"
                class="flex size-10 flex-none items-center justify-center rounded-[10px] text-muted active:bg-elevated"
                :aria-label="$t('app.palette.clear')"
                @click="clear"
              >
                <UIcon name="i-lucide-x" class="size-4" />
              </button>
            </label>
            <button type="button" class="h-11 flex-none px-1.5 text-[15px] font-semibold text-primary-700 dark:text-primary-300" @click="open = false">
              {{ $t('app.palette.cancel') }}
            </button>
          </div>
          <div class="flex flex-none gap-2 overflow-x-auto px-4 pt-0.5 pb-3.5 [scrollbar-width:none]" :aria-label="$t('app.palette.operators')" role="group">
            <button
              v-for="op in operators"
              :key="op"
              type="button"
              class="relative h-9 flex-none rounded-full border border-default bg-default px-3 font-mono text-[13px] text-primary-700 after:absolute after:inset-x-0 after:-inset-y-1 active:bg-elevated dark:text-primary-300"
              @click="insertOperator(op)"
            >
              {{ op }}
            </button>
          </div>
        </template>

        <div v-if="!desktop && !query" class="min-h-0 flex-1 overflow-y-auto">
          <PaletteMobileHome :actions="actionItems" :pages="pageItems" :saved="savedItems" :stats="statsItems" />
        </div>
        <UCommandPalette
          v-else
          v-model:search-term="term"
          :groups="groups"
          :loading="searching"
          :placeholder="$t('app.palette.placeholder')"
          :input="desktop ? inputProps : false"
          :close="desktop"
          :ui="paletteUi"
          class="min-h-0"
          @keydown="onKeydown"
          @update:open="(o: boolean) => { if (!o) open = false }"
        >
          <template v-if="desktop" #close>
            <UButton
              v-if="term"
              icon="i-lucide-x"
              color="neutral"
              variant="ghost"
              size="sm"
              :aria-label="$t('app.palette.clear')"
              @click="clear"
            />
            <UKbd value="escape" />
          </template>

          <template #group-label="{ group, label }">
            <span>{{ label }}</span>
            <span v-if="group.id === 'messages'" class="font-normal text-dimmed">{{ found.length }}</span>
          </template>

          <template #item="{ item }">
            <PaletteRow :item="item" />
          </template>

          <template #message="{ item }">
            <PaletteMessageRow :message="item.message" :terms="parsed.terms" />
          </template>

          <template #empty>
            {{ $t('app.palette.noResults', { q: query }) }}
          </template>

          <template #footer>
            <PaletteFilters v-if="parsed.filtered" :chips="parsed.chips" class="order-1" />
            <div
              v-if="desktop"
              class="order-3 flex items-center gap-3.5 border-t border-default bg-neutral-50 px-4 py-2.5 text-xs text-muted dark:bg-neutral-950/40"
            >
              <span class="inline-flex items-center gap-1.5">
                <UKbd value="arrowup" size="sm" /><UKbd value="arrowdown" size="sm" />{{ $t('app.palette.navigate') }}
              </span>
              <span class="inline-flex items-center gap-1.5">
                <UKbd value="enter" size="sm" />{{ $t('app.palette.open') }}
              </span>
              <span class="ms-auto">{{ $t('app.palette.operators') }}</span>
              <span class="flex items-center gap-1">
                <button
                  v-for="op in footerOperators"
                  :key="op"
                  type="button"
                  class="h-6 rounded-md border border-default bg-default px-2 font-mono text-[11.5px] text-primary-700 hover:bg-elevated dark:text-primary-300"
                  @click="insertOperator(op)"
                >
                  {{ op }}
                </button>
              </span>
            </div>
          </template>
        </UCommandPalette>
      </div>
    </template>
  </UModal>
</template>
