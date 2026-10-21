<script setup lang="ts">
import type { AccountColorName } from '~/utils/accountColors'
import type { Account, Mailbox } from '~/utils/types'
import { sidebarRow as row } from '~/components/SidebarFolder.vue'
import type { FolderNode } from '~/components/SidebarFolder.vue'

/** The surrounding layout handles the context menu and dropping messages on a folder, from the
 *  data-* attributes carried by the row labels. */
defineProps<{ collapsed?: boolean }>()

const { accounts, accountColor, mailboxes, unifiedUnread } = useMail()
const outbox = useOutbox()
const saved = useSavedSearches()
const { calendarsAvailable } = useCalendar()
const { syncErrors, authProblems } = useAccountHealth()
function trouble(id: number): { text: string, icon: string, color: string } | null {
  const auth = authProblems.value[id]
  if (auth) return { text: auth, icon: 'i-lucide-key-round', color: 'text-error' }
  const sync = syncErrors.value[id]
  if (sync) return { text: sync, icon: 'i-lucide-cloud-off', color: 'text-warning' }
  return null
}
// The router's route, not useRoute(): outside the pages, Nuxt's copy follows only the pages
// that finish loading, and going back to the list kept under the other screens loads none (the
// unified inbox stayed unselected).
const route = useRouter().currentRoute
const { t } = useI18n()

/** Account colour as text (unread count), as a border (line along its folders), and on the
 *  selected folder (row, then its count). Written out in full so that Tailwind generates them. */
const ACCENTS: Record<AccountColorName, { text: string, line: string, active: string, activeCount: string }> = {
  orange: { text: 'text-orange-700 dark:text-orange-300', line: 'border-orange-300 dark:border-orange-400/45', active: 'bg-orange-500/14 font-semibold text-orange-700 dark:text-orange-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-orange-600 text-white dark:bg-orange-400 dark:text-orange-950' },
  pink: { text: 'text-pink-700 dark:text-pink-300', line: 'border-pink-300 dark:border-pink-400/45', active: 'bg-pink-500/14 font-semibold text-pink-700 dark:text-pink-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-pink-600 text-white dark:bg-pink-400 dark:text-pink-950' },
  violet: { text: 'text-violet-700 dark:text-violet-300', line: 'border-violet-300 dark:border-violet-400/45', active: 'bg-violet-500/14 font-semibold text-violet-700 dark:text-violet-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-violet-600 text-white dark:bg-violet-400 dark:text-violet-950' },
  blue: { text: 'text-blue-700 dark:text-blue-300', line: 'border-blue-300 dark:border-blue-400/45', active: 'bg-blue-500/14 font-semibold text-blue-700 dark:text-blue-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-blue-600 text-white dark:bg-blue-400 dark:text-blue-950' },
  emerald: { text: 'text-emerald-700 dark:text-emerald-300', line: 'border-emerald-300 dark:border-emerald-400/45', active: 'bg-emerald-500/14 font-semibold text-emerald-700 dark:text-emerald-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-emerald-600 text-white dark:bg-emerald-400 dark:text-emerald-950' },
  amber: { text: 'text-amber-700 dark:text-amber-300', line: 'border-amber-300 dark:border-amber-400/45', active: 'bg-amber-500/14 font-semibold text-amber-700 dark:text-amber-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-amber-600 text-white dark:bg-amber-400 dark:text-amber-950' },
  cyan: { text: 'text-cyan-700 dark:text-cyan-300', line: 'border-cyan-300 dark:border-cyan-400/45', active: 'bg-cyan-500/14 font-semibold text-cyan-700 dark:text-cyan-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-cyan-600 text-white dark:bg-cyan-400 dark:text-cyan-950' },
  red: { text: 'text-red-700 dark:text-red-300', line: 'border-red-300 dark:border-red-400/45', active: 'bg-red-500/14 font-semibold text-red-700 dark:text-red-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-red-600 text-white dark:bg-red-400 dark:text-red-950' },
  lime: { text: 'text-lime-700 dark:text-lime-300', line: 'border-lime-300 dark:border-lime-400/45', active: 'bg-lime-500/14 font-semibold text-lime-700 dark:text-lime-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-lime-600 text-white dark:bg-lime-400 dark:text-lime-950' },
  fuchsia: { text: 'text-fuchsia-700 dark:text-fuchsia-300', line: 'border-fuchsia-300 dark:border-fuchsia-400/45', active: 'bg-fuchsia-500/14 font-semibold text-fuchsia-700 dark:text-fuchsia-300', activeCount: 'flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold bg-fuchsia-600 text-white dark:bg-fuchsia-400 dark:text-fuchsia-950' }
}

const badge = (n: number) => (n > 0 ? String(n) : undefined)

// Badges: unread messages, or every message of the folder (setting). The total is the server's
// count when known: the cache may only hold the most recent part of a large folder.
const { mode: countMode, unreadOf, totalOf } = useFolderCounts()
const compactCount = computed(() => new Intl.NumberFormat(intlLocale(), { notation: 'compact', maximumFractionDigits: 1 }))
/** "1.2k", and "1.2k+" when the cache holds only part of the folder. */
const totalBadge = ({ n, partial }: { n: number, partial: boolean }) => (n > 0 ? `${compactCount.value.format(n)}${partial ? '+' : ''}` : undefined)
const unifiedTotal = computed(() => {
  const totals = mailboxes.value.filter(m => m.role === 'inbox').map(totalOf)
  return totalBadge({ n: totals.reduce((sum, t) => sum + t.n, 0), partial: totals.some(t => t.partial) })
})
function countBadge(m: Mailbox | undefined): string | undefined {
  if (!m) return undefined
  if (countMode.value === 'total') return totalBadge(totalOf(m))
  // New senders: everything there waits for a decision, read or not (the cache holds it all).
  if (m.role === 'screener') return badge(totalOf(m).n)
  return m.role !== 'trash' && m.role !== 'junk' && m.role !== 'snoozed' && m.role !== 'refused' ? badge(unreadOf(m)) : undefined
}

interface NavLink {
  key: string
  label: string
  icon: string
  to: string | { path: string, query: Record<string, string> }
  active: boolean
  count?: string
  countClass?: string
  /** Marks a saved search for the context menu. */
  savedQuery?: string
}

const links = computed<NavLink[]>(() => [
  { key: 'home', label: t('app.sidebar.home'), icon: 'i-lucide-house', to: '/', active: route.value.path === '/' },
  {
    key: 'unified',
    label: t('app.sidebar.unified'),
    icon: 'i-lucide-inbox',
    to: '/mail/unified',
    count: countMode.value === 'total' ? unifiedTotal.value : badge(unifiedUnread.value),
    countClass: 'text-primary',
    active: (route.value.params.mailbox === 'unified' || route.value.params.mailbox === 'done') && !route.value.query.q
  },
  // Once the engine knows calendars (see useCalendar).
  ...(calendarsAvailable.value
    ? [{ key: 'calendar', label: t('app.sidebar.calendar'), icon: 'i-lucide-calendar', to: '/calendar', active: route.value.path === '/calendar' }]
    : []),
  // Saved searches open in the unified inbox.
  ...saved.list.value.map(s => ({
    key: `saved:${s.query}`,
    label: s.name,
    icon: 'i-lucide-search',
    to: { path: '/mail/unified', query: { q: s.query } },
    savedQuery: s.query,
    active: route.value.query.q === s.query
  })),
  // Only while something waits: scheduled or failed sends.
  ...(outbox.visible.value.length
    ? [{
        key: 'outbox',
        label: t('app.sidebar.outbox'),
        icon: outbox.failed.value ? 'i-lucide-send-horizontal' : 'i-lucide-calendar-clock',
        to: '/outbox',
        count: badge(outbox.visible.value.length),
        countClass: outbox.failed.value ? 'text-error' : 'text-muted',
        active: route.value.path === '/outbox'
      }]
    : [])
])

function folderNode(m: Mailbox): FolderNode {
  return {
    mailbox: m,
    label: m.role ? roleLabels[m.role] : m.display_name,
    icon: m.role ? roleIcons[m.role] : 'i-lucide-folder',
    iconClass: m.role === 'flagged' ? 'text-amber-600 dark:text-amber-400' : 'text-muted',
    count: countBadge(m),
    active: route.value.params.mailbox === String(m.id),
    ...folderAccent(m.account_id),
    children: []
  }
}

function folderAccent(accountId: number) {
  const a = accounts.value.find(x => x.id === accountId)
  const accent = a ? ACCENTS[accountColorName(a, accounts.value)] : undefined
  return { activeClass: accent?.active, activeCountClass: accent?.activeCount }
}

/** Regular folders as a tree by delimiter: "Clients/2024" under "Clients". Non-selectable
 *  containers ([Gmail]…) stay as nodes without a link. */
function folderTree(folders: Mailbox[]): FolderNode[] {
  const byName = new Map<string, FolderNode>()
  const roots: FolderNode[] = []
  for (const m of [...folders].sort((x, y) => x.name.localeCompare(y.name))) {
    const node = folderNode(m)
    byName.set(m.name, node)
    const d = m.delimiter
    const parent = d && m.name.includes(d) ? byName.get(m.name.slice(0, m.name.lastIndexOf(d))) : undefined
    if (parent) parent.children.push(node)
    else roots.push(node)
  }
  // A container with neither link nor children is useless.
  return roots.filter(r => r.mailbox.selectable || r.children.length)
}

function flatten(nodes: FolderNode[], depth = 0): { node: FolderNode, depth: number }[] {
  return nodes.flatMap(n => [
    ...(n.mailbox.selectable ? [{ node: n, depth }] : []),
    ...flatten(n.children, depth + 1)
  ])
}

const openAccounts = ref(new Set<number>())
function toggleAccount(id: number) {
  if (!openAccounts.value.delete(id)) openAccounts.value.add(id)
}
// Open when it is the only account, or holds the folder shown.
const onlyAccount = computed(() => (accounts.value.length === 1 ? accounts.value[0]!.id : null))
const shownAccount = computed(() => mailboxes.value.find(m => route.value.params.mailbox === String(m.id))?.account_id ?? null)
watch([onlyAccount, shownAccount], (ids) => {
  for (const id of ids) if (id !== null) openAccounts.value.add(id)
}, { immediate: true })

/** Account of the avatar editor (kept once closed, for its closing animation). */
const avatarAccountId = ref<number | null>(null)
const avatarAccount = computed(() => accounts.value.find(a => a.id === avatarAccountId.value) ?? null)
const avatarOpen = ref(false)
function editAvatar(id: number) {
  avatarAccountId.value = id
  avatarOpen.value = true
}

const ACCOUNT_DRAG = 'application/x-facteur-account'
const { moveTo } = useAccountOrder()
const draggedAccount = ref<number | null>(null)
const dropAt = ref<{ id: number, after: boolean } | null>(null)

function onAccountDragStart(e: DragEvent, id: number) {
  if (!e.dataTransfer) return
  e.dataTransfer.setData(ACCOUNT_DRAG, String(id))
  e.dataTransfer.effectAllowed = 'move'
  draggedAccount.value = id
}
function onAccountDragOver(e: DragEvent, id: number) {
  // Messages dragged onto a folder are not this.
  if (draggedAccount.value === null || !e.dataTransfer?.types.includes(ACCOUNT_DRAG)) return
  e.preventDefault()
  e.dataTransfer.dropEffect = 'move'
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
  dropAt.value = { id, after: e.clientY > box.top + box.height / 2 }
}
/** Left for good, not just onto one of the row's own children. */
function onAccountDragLeave(e: DragEvent) {
  if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node | null)) dropAt.value = null
}
function onAccountDragEnd() {
  draggedAccount.value = null
  dropAt.value = null
}
function onAccountDrop(e: DragEvent) {
  const id = draggedAccount.value
  const at = dropAt.value
  onAccountDragEnd()
  if (id === null || !at || at.id === id) return
  e.preventDefault()
  const others = accounts.value.filter(a => a.id !== id)
  const index = others.findIndex(a => a.id === at.id)
  if (index >= 0) moveTo(id, index + (at.after ? 1 : 0))
}

const accountRows = computed(() => accounts.value.map((a: Account) => {
  const own = mailboxes.value.filter(m => m.account_id === a.id)
  const folders = [
    ...own.filter(m => m.role && m.selectable).map(folderNode),
    ...folderTree(own.filter(m => !m.role))
  ]
  return {
    account: a,
    name: accountName(a),
    initials: initials(accountName(a)),
    solid: accountColor(a.id).solid,
    accent: ACCENTS[accountColorName(a, accounts.value)],
    count: countBadge(own.find(m => m.role === 'inbox')),
    open: openAccounts.value.has(a.id),
    folders,
    flat: flatten(folders)
  }
}))

/** On opening an account's popover, focus its first folder rather than the avatar button: focus
 *  showed that button's tooltip, over the folder's count. */
function focusFirstFolder(e: Event) {
  e.preventDefault()
  const content = e.target as HTMLElement | null
  content?.querySelector<HTMLElement>('a')?.focus()
}
</script>

<template>
  <nav :aria-label="$t('app.tabs.mailboxes')" class="flex flex-col gap-0.5">
    <UTooltip
      v-for="link in links"
      :key="link.key"
      :text="link.label"
      :disabled="!collapsed"
      :delay-duration="0"
      :content="{ side: 'right' }"
    >
      <NuxtLink
        :to="link.to"
        :class="[row.base, 'h-9', collapsed ? 'justify-center' : 'px-2.5', link.active ? row.active : [row.idle, 'font-medium']]"
      >
        <UIcon :name="link.icon" class="size-4.5 shrink-0" :class="link.active ? '' : 'text-muted'" />
        <span :class="collapsed ? 'sr-only' : 'flex-1 truncate'" :data-saved-search="link.savedQuery">{{ link.label }}</span>
        <span
          v-if="!collapsed && link.count"
          :class="link.active ? row.activeCount : ['shrink-0 text-xs font-bold', link.countClass]"
        >{{ link.count }}</span>
      </NuxtLink>
    </UTooltip>

    <p v-if="!collapsed" class="mx-2.5 mt-4 mb-1.5 text-[11px] font-bold tracking-[0.06em] text-muted uppercase">
      {{ $t('app.sidebar.accounts') }}
    </p>
    <div v-else class="mx-2 my-2.5 border-t border-default" />

    <template v-for="entry in accountRows" :key="entry.account.id">
      <template v-if="!collapsed">
        <!-- Two buttons: the avatar opens its editor, the rest folds the account. The row is
             dragged to reorder the accounts. -->
        <div
          draggable="true"
          :class="[row.base, 'relative h-10 ps-1.5 font-semibold text-highlighted hover:bg-elevated', draggedAccount === entry.account.id ? 'opacity-50' : '']"
          @dragstart="onAccountDragStart($event, entry.account.id)"
          @dragover="onAccountDragOver($event, entry.account.id)"
          @dragleave="onAccountDragLeave"
          @drop="onAccountDrop"
          @dragend="onAccountDragEnd"
        >
          <span
            v-if="dropAt?.id === entry.account.id && draggedAccount !== entry.account.id"
            class="pointer-events-none absolute inset-x-2 h-0.5 rounded-full bg-primary"
            :class="dropAt.after ? '-bottom-px' : '-top-px'"
          />
          <UTooltip :text="$t('app.avatar.edit')" :delay-duration="600">
            <button
              type="button"
              class="group/avatar shrink-0 rounded-full p-1 focus-visible:outline-2 focus-visible:outline-primary"
              :aria-label="`${$t('app.avatar.edit')} · ${entry.name}`"
              @click="editAvatar(entry.account.id)"
            >
              <span
                class="flex size-6.5 items-center justify-center overflow-hidden rounded-full text-[10px] font-bold transition-opacity group-hover/avatar:opacity-80"
                :class="entry.solid"
              >
                <img v-if="entry.account.avatar" :src="entry.account.avatar" alt="" draggable="false" class="size-full object-cover">
                <template v-else>{{ entry.initials }}</template>
              </span>
            </button>
          </UTooltip>
          <button
            type="button"
            class="flex h-full min-w-0 flex-1 items-center gap-2.5 rounded-e-[10px] pe-2.5 text-start focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary"
            :aria-expanded="entry.open"
            @click="toggleAccount(entry.account.id)"
          >
            <span class="flex-1 truncate" :data-account-id="entry.account.id">{{ entry.name }}</span>
            <UTooltip v-if="trouble(entry.account.id)" :text="trouble(entry.account.id)!.text">
              <UIcon :name="trouble(entry.account.id)!.icon" class="size-3.5 shrink-0" :class="trouble(entry.account.id)!.color" :aria-label="trouble(entry.account.id)!.text" />
            </UTooltip>
            <span v-if="entry.count" class="shrink-0 text-xs font-bold" :class="entry.accent.text">{{ entry.count }}</span>
            <UIcon name="i-lucide-chevron-down" class="size-4 shrink-0 text-dimmed transition-transform" :class="entry.open ? 'rotate-180' : ''" />
          </button>
        </div>
        <div v-if="entry.open" class="ms-5.5 mb-1 flex flex-col gap-px border-s-2 ps-2.5" :class="entry.accent.line">
          <SidebarFolder v-for="node in entry.folders" :key="node.mailbox.id" :node="node" />
        </div>
      </template>

      <UPopover v-else :content="{ side: 'right', align: 'start', alignOffset: 2, onOpenAutoFocus: focusFirstFolder }">
        <button
          type="button"
          draggable="true"
          :class="[row.base, 'relative h-10 justify-center hover:bg-elevated', draggedAccount === entry.account.id ? 'opacity-50' : '']"
          @dragstart="onAccountDragStart($event, entry.account.id)"
          @dragover="onAccountDragOver($event, entry.account.id)"
          @dragleave="onAccountDragLeave"
          @drop="onAccountDrop"
          @dragend="onAccountDragEnd"
        >
          <span
            v-if="dropAt?.id === entry.account.id && draggedAccount !== entry.account.id"
            class="pointer-events-none absolute inset-x-2 h-0.5 rounded-full bg-primary"
            :class="dropAt.after ? '-bottom-px' : '-top-px'"
          />
          <span class="flex size-6.5 shrink-0 items-center justify-center overflow-hidden rounded-full text-[10px] font-bold" :class="entry.solid">
            <img v-if="entry.account.avatar" :src="entry.account.avatar" alt="" draggable="false" class="size-full object-cover">
            <template v-else>{{ entry.initials }}</template>
          </span>
          <span
            v-if="trouble(entry.account.id)"
            class="pointer-events-none absolute end-2 top-1.5 size-2 rounded-full ring-2 ring-(--surface-side)"
            :class="authProblems[entry.account.id] ? 'bg-error' : 'bg-warning'"
          />
          <span class="sr-only" :data-account-id="entry.account.id">{{ entry.name }}</span>
        </button>
        <template #content="{ close }">
          <div class="flex max-h-[70vh] w-60 flex-col gap-px overflow-y-auto p-1.5">
            <div class="flex items-center gap-1 ps-2.5">
              <p class="flex-1 truncate py-1.5 text-xs font-semibold text-highlighted">{{ entry.name }}</p>
              <UTooltip :text="$t('app.avatar.edit')">
                <UButton
                  icon="i-lucide-image-up"
                  color="neutral"
                  variant="ghost"
                  size="xs"
                  :aria-label="$t('app.avatar.edit')"
                  @click="close(); editAvatar(entry.account.id)"
                />
              </UTooltip>
            </div>
            <NuxtLink
              v-for="{ node, depth } in entry.flat"
              :key="node.mailbox.id"
              :to="`/mail/${node.mailbox.id}`"
              :class="[row.base, 'h-8 pe-2.5', node.active ? node.activeClass ?? row.active : row.idle]"
              :style="{ paddingInlineStart: `${0.625 + depth * 1.5}rem` }"
              @click="close()"
            >
              <UIcon :name="node.icon" class="size-4 shrink-0" :class="node.active ? '' : node.iconClass" />
              <span class="flex-1 truncate">{{ node.label }}</span>
              <span v-if="node.count" :class="node.active ? node.activeCountClass ?? row.activeCount : 'shrink-0 text-xs font-semibold text-muted'">{{ node.count }}</span>
            </NuxtLink>
          </div>
        </template>
      </UPopover>
    </template>

    <UTooltip :text="$t('app.sidebar.addAccount')" :disabled="!collapsed" :delay-duration="0" :content="{ side: 'right' }">
      <NuxtLink to="/setup" :class="[row.base, 'h-9.5 text-muted hover:bg-elevated hover:text-highlighted', collapsed ? 'justify-center' : 'px-2.5']">
        <!-- Solid in primary while adding: the slot of the account being added. -->
        <span
          class="flex size-6.5 shrink-0 items-center justify-center rounded-full border-[1.5px]"
          :class="route.path === '/setup' ? 'border-solid border-primary text-primary' : 'border-dashed border-accented'"
        >
          <UIcon name="i-lucide-plus" class="size-3.5" />
        </span>
        <span :class="collapsed ? 'sr-only' : 'truncate'">{{ $t('app.sidebar.addAccount') }}</span>
      </NuxtLink>
    </UTooltip>
    <AccountAvatarModal v-if="avatarAccount" v-model:open="avatarOpen" :account="avatarAccount" />
  </nav>
</template>
