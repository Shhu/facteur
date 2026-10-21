<script setup lang="ts">
import type { ContextMenuItem } from '@nuxt/ui'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useElementSize, useOnline } from '@vueuse/core'
import { flushDraftBeforeQuit } from '~/composables/useComposer'
import { flushOutboxBeforeQuit } from '~/composables/useOutbox'
import { confirm } from '~/composables/useConfirm'
import type { Account, Mailbox, Role } from '~/utils/types'

const { accounts, accountId, mailboxes, isSyncing, syncProgress, loadAccounts, loadMailboxes, syncAll } = useMail()
const { compose, composeMailto, open: composing } = useComposer()
const saved = useSavedSearches()
saved.load()
const showError = useErrorToast()
const listStale = useListStale()
const route = useRouter().currentRoute
const { t } = useI18n()

await loadAccounts()
watch(() => accounts.value.map(a => a.id).join(), () => loadMailboxes(), { immediate: true })

const sidebarOpen = ref(false)

// The last list stays mounted (frozen on its route) while the other screens open over it.
trackMailOverlay()
const { mailRoute, pageRoute, covered, close: closeOverlay, forget: forgetMail } = useMailOverlay()
const currentRoute = useRouter().currentRoute
const onMail = computed(() => isMailRoute(currentRoute.value))
// Once covered, a list whose folder or account was deleted has nothing left to show: forget it.
watch([mailboxes, accounts, covered], () => {
  const r = mailRoute.value
  if (!r || !covered.value) return
  const id = Number(r.params.mailbox)
  const gone = !accounts.value.length || (Number.isInteger(id) && mailboxes.value.length > 0 && !mailboxes.value.some(m => m.id === id))
  if (gone) forgetMail()
})
/** The overlay starts where the sidebar ends (0 on mobile, where it is hidden). */
const sidebar = useTemplateRef('sidebar')
const { width: sidebarWidth } = useElementSize(() => (sidebar.value ? document.getElementById('facteur-sidebar-nav') : null), undefined, { box: 'border-box' })

const { autoBackup } = useBackup()
let backupTimer: ReturnType<typeof setInterval> | undefined
onMounted(async () => {
  // The mailto: link facteur was launched with (as the default mail client).
  const link = await api.takeLaunchMailto().catch(() => null)
  if (link) composeMailto(link)
  autoBackup()
  backupTimer = setInterval(autoBackup, 3600_000)
})
onBeforeUnmount(() => clearInterval(backupTimer))

// One close guard for the whole app: save the open draft, then let just-sent messages leave
// (flushOutboxBeforeQuit). Tauri hands the close to JS as soon as a close-requested listener
// exists, and destroys the window once it returns without preventDefault (needs the
// `core:window:allow-destroy` permission). The listener is removed first, so a second attempt
// closes natively if destroying is refused. A repeated close is held while the draft is saved,
// and quits at once while waiting for the outbox.
let stopCloseGuard: (() => void) | null = null
let closeGuardGone = false
let quitPhase: 'idle' | 'draft' | 'outbox' = 'idle'
onMounted(async () => {
  try {
    const unlisten = await getCurrentWindow().onCloseRequested(async (event) => {
      if (quitPhase === 'draft') {
        event.preventDefault()
        return
      }
      if (quitPhase === 'outbox') return
      quitPhase = 'draft'
      try {
        if (!(await flushDraftBeforeQuit())) {
          event.preventDefault()
          return
        }
        quitPhase = 'outbox'
        await flushOutboxBeforeQuit(n => toast.add({ title: t('mail.quit.sending', { n }, n), description: t('mail.quit.sendingHint'), icon: 'i-lucide-send', duration: 0 }))
        stopCloseGuard?.()
        stopCloseGuard = null
      } finally {
        quitPhase = 'idle'
      }
    })
    if (closeGuardGone) unlisten()
    else stopCloseGuard = unlisten
  } catch {
    // Not running in a Tauri window.
  }
})
onBeforeUnmount(() => {
  closeGuardGone = true
  stopCloseGuard?.()
  stopCloseGuard = null
})

const progressAccount = computed(() => {
  const entry = Object.entries(syncProgress.value).find(([, p]) => p.total > 100)
  return entry ? Number(entry[0]) : null
})

/** The oldest sync among the accounts: it tells whether everything is up to date. */
const oldestSync = computed(() => {
  const times = accounts.value.map(a => a.last_sync)
  return times.length && !times.includes(null) ? Math.min(...(times as number[])) : null
})
const now = useNow()
const lastSync = computed(() => {
  void now.value // shortDate is relative to today
  return oldestSync.value === null ? t('common.neverSynced') : t('app.sidebar.lastSync', { date: shortDate(oldestSync.value) })
})
const { syncErrors, authProblems } = useAccountHealth()
const { enabled: perfEnabled, open: perfOpen } = usePerf()
const online = useOnline()
const troubled = computed(() => Object.keys(syncErrors.value).length > 0 || Object.keys(authProblems.value).length > 0)
const syncTooltip = computed(() => {
  if (!online.value) return t('app.ops.health.offline')
  return troubled.value ? `${t('app.ops.health.syncProblems')} · ${lastSync.value}` : lastSync.value
})
const synced = useSyncedLabel()
const since = useSinceLabel()
const sinceLabel = computed(() => since(oldestSync.value))
const syncedDetail = (at: number | null) => (at === null ? t('common.neverSynced') : `${synced(at)} · ${since(at)}`)

/** Mobile: these screens take the whole screen, without the tab bar (purge and folder deletion
 *  keep their confirmation bar at the bottom). */
const messageOpen = computed(() => (route.value.path.startsWith('/mail/') && !!route.value.query.thread) || route.value.path.startsWith('/settings/account/') || route.value.path.startsWith('/purge/') || route.value.path.startsWith('/folders/'))

const menuAccount = ref<Account | null>(null)
const menuFolder = ref<Mailbox | null>(null)
const menuSaved = ref<string | null>(null)

function pickTarget(e: MouseEvent) {
  const target = e.target as HTMLElement | null
  // The id is on the label; the click may land on the row's icon or badge.
  const row = target?.closest('a, button')
  const tagged = (row?.querySelector('[data-account-id], [data-mailbox-id], [data-saved-search]') ?? target?.closest('[data-account-id], [data-mailbox-id], [data-saved-search]')) as HTMLElement | null
  menuSaved.value = tagged?.dataset.savedSearch ?? null
  const folderId = Number(tagged?.dataset.mailboxId)
  const accountId = Number(tagged?.dataset.accountId)
  menuFolder.value = mailboxes.value.find(m => m.id === folderId) ?? null
  menuAccount.value = menuFolder.value ? null : (accounts.value.find(a => a.id === accountId) ?? null)
}

const folderMenu = (m: Mailbox): ContextMenuItem[][] => [
  [{ type: 'label', label: m.role ? roleLabels[m.role] : m.display_name }],
  [
    { label: t('app.folderMenu.open'), icon: 'i-lucide-folder-open', disabled: !m.selectable, onSelect: () => navigateTo(`/mail/${m.id}`) },
    { label: t('app.folderMenu.newSubfolder'), icon: 'i-lucide-folder-plus', disabled: !!m.role || !m.delimiter, onSelect: () => openFolderDialog('create', m.account_id, m) },
    { label: t('app.folderMenu.rename'), icon: 'i-lucide-pencil', disabled: !!m.role, onSelect: () => openFolderDialog('rename', m.account_id, m) },
    // Trash and junk get emptied, "Refused" goes to the trash, other special folders cannot be deleted.
    m.role === 'refused'
      ? {
          label: t('app.folderMenu.trashAll'),
          icon: 'i-lucide-trash-2',
          color: 'error',
          disabled: !m.total,
          onSelect: () => trashAll(m)
        }
      : m.role === 'trash' || m.role === 'junk'
      ? {
          label: t(m.role === 'trash' ? 'app.folderMenu.emptyTrash' : 'app.folderMenu.emptyJunk'),
          icon: 'i-lucide-trash-2',
          color: 'error',
          onSelect: () => navigateTo(`/purge/${m.id}`)
        }
      : {
          label: m.role ? t('app.folderMenu.special') : t('app.folderMenu.delete'),
          icon: 'i-lucide-folder-x',
          color: 'error',
          disabled: !!m.role,
          onSelect: () => navigateTo(`/folders/${m.id}`)
        }
  ]
]

const accountMenu = computed<ContextMenuItem[][]>(() => {
  if (menuSaved.value !== null) {
    const q = menuSaved.value
    return [[{ type: 'label', label: q }], [{ label: t('app.folderMenu.deleteSearch'), icon: 'i-lucide-trash-2', color: 'error', onSelect: () => saved.remove(q) }]]
  }
  if (menuFolder.value) return folderMenu(menuFolder.value)
  const a = menuAccount.value
  if (!a) return []
  const unread = mailboxes.value.filter(m => m.account_id === a.id).reduce((n, m) => n + m.unread, 0)
  return [
    [{ type: 'label', label: accountName(a) }],
    [
      { label: t('app.accountMenu.markAllRead'), icon: 'i-lucide-mail-open', disabled: !unread, onSelect: () => markAllRead(a, unread) },
      { label: t('app.accountMenu.rename'), icon: 'i-lucide-pencil', onSelect: () => openRename(a) },
      { label: t('app.accountMenu.newFolder'), icon: 'i-lucide-folder-plus', onSelect: () => openFolderDialog('create', a.id, null) },
      { label: t('app.accountMenu.stats'), icon: 'i-lucide-chart-column', onSelect: () => navigateTo(`/stats/${a.id}`) }
    ],
    [
      { label: t('app.accountMenu.moveUp'), icon: 'i-lucide-arrow-up', disabled: accounts.value[0]?.id === a.id, onSelect: () => moveAccount(a.id, -1) },
      { label: t('app.accountMenu.moveDown'), icon: 'i-lucide-arrow-down', disabled: accounts.value.at(-1)?.id === a.id, onSelect: () => moveAccount(a.id, 1) }
    ]
  ]
})
const { moveBy: moveAccount } = useAccountOrder()

async function trashAll(m: Mailbox) {
  const name = m.role ? roleLabels[m.role] : m.display_name
  const ok = await confirm(t('app.folderMenu.trashAllConfirm', { n: m.total, name }, m.total), {
    title: t('app.folderMenu.trashAll'),
    kind: 'warning'
  })
  if (!ok) return
  try {
    done(await api.trashFolder(m.id), t('app.folderMenu.trashedAll', { n: m.total }, m.total), 'i-lucide-trash-2')
  } catch (e) {
    showError(e)
  }
}

async function markAllRead(a: Account, unread: number) {
  const ok = await confirm(t('app.accountMenu.markAllReadConfirm', { n: unread, account: accountName(a) }, unread), {
    title: t('app.accountMenu.markAllRead'),
    kind: 'info'
  })
  if (!ok) return
  try {
    await api.markAccountRead(a.id)
  } catch (e) {
    showError(e)
  }
}

const renaming = ref<Account | null>(null)
const newLabel = ref('')

function openRename(a: Account) {
  renaming.value = a
  newLabel.value = a.label ?? ''
}

async function saveLabel() {
  const a = renaming.value
  if (!a) return
  try {
    await api.setAccountLabel(a.id, newLabel.value)
    await loadAccounts()
    renaming.value = null
  } catch (e) {
    showError(e)
  }
}

const folderDialog = ref<{ mode: 'create' | 'rename', accountId: number, folder: Mailbox | null } | null>(null)
const folderName = ref('')
const savingFolder = ref(false)

function openFolderDialog(mode: 'create' | 'rename', accountId: number, folder: Mailbox | null) {
  folderDialog.value = { mode, accountId, folder }
  folderName.value = mode === 'rename' && folder ? folder.display_name : ''
}

async function saveFolder() {
  const d = folderDialog.value
  if (!d || savingFolder.value) return
  savingFolder.value = true
  try {
    if (d.mode === 'create') {
      const id = await api.createFolder(d.accountId, d.folder?.id ?? null, folderName.value)
      await loadMailboxes()
      await navigateTo(`/mail/${id}`)
    } else if (d.folder) {
      await api.renameFolder(d.folder.id, folderName.value)
      await loadMailboxes()
    }
    folderDialog.value = null
  } catch (e) {
    showError(e)
  } finally {
    savingFolder.value = false
  }
}

const DRAG_TYPE = 'application/x-facteur-messages'
const { done, undoLast } = useUndo()
const toast = useToast()
let dropRow: HTMLElement | null = null

function folderUnder(e: DragEvent): { mailbox: Mailbox, row: HTMLElement } | null {
  const row = (e.target as HTMLElement | null)?.closest('a') as HTMLElement | null
  const id = Number((row?.querySelector('[data-mailbox-id]') as HTMLElement | null)?.dataset.mailboxId)
  const mailbox = mailboxes.value.find(m => m.id === id)
  return row && mailbox?.selectable ? { mailbox, row } : null
}

/** Folders a drop cannot move into: Pinned and Later hold their messages through a flag and a
 *  date (a plain move would lose them), Drafts holds only facteur's own drafts. The reason is
 *  given on drop. */
const NO_DROP: Partial<Record<Role, string>> = {
  flagged: 'app.drop.pinned',
  snoozed: 'app.drop.later',
  drafts: 'app.drop.drafts'
}
const refused = (m: Mailbox) => (m.role ? NO_DROP[m.role] : undefined)

function highlight(row: HTMLElement | null) {
  if (dropRow === row) return
  dropRow?.classList.remove('ring-2', 'ring-primary', 'bg-primary/10')
  row?.classList.add('ring-2', 'ring-primary', 'bg-primary/10')
  dropRow = row
}

function onDragOver(e: DragEvent) {
  if (!e.dataTransfer?.types.includes(DRAG_TYPE)) return
  const target = folderUnder(e)
  // A refused folder still takes the drop (to explain why), without the highlight.
  highlight(target && !refused(target.mailbox) ? target.row : null)
  if (target) {
    e.preventDefault()
    e.dataTransfer.dropEffect = 'move'
  }
}

async function onDrop(e: DragEvent) {
  highlight(null)
  const target = folderUnder(e)
  const raw = e.dataTransfer?.getData(DRAG_TYPE)
  if (!target || !raw) return
  e.preventDefault()
  const { accountId: from, ids, box } = JSON.parse(raw) as { accountId: number | null, ids: number[], box?: number | null }
  const m = target.mailbox
  const reason = refused(m)
  if (reason) {
    toast.add({ title: t('app.drop.impossible'), description: t(reason), color: 'warning', icon: 'i-lucide-folder-x' })
    return
  }
  if (from !== m.account_id) {
    toast.add({ title: t('app.drop.impossible'), description: t('app.drop.otherAccount'), color: 'warning', icon: 'i-lucide-folder-x' })
    return
  }
  try {
    done(await api.moveTo(ids, m.id, box), t('app.drop.moved', { folder: m.role ? roleLabels[m.role] : m.display_name }), 'i-lucide-folder-input')
    listStale.bump()
  } catch (err) {
    showError(err)
  }
}
// Nothing while composing, or while a dialog, menu or the palette is open: a key typed outside
// the text must not act behind it.
const shortcutsOff = () => composing.value || overlayOpen()
defineShortcuts({
  c: () => !shortcutsOff() && accountId.value !== null && compose(),
  z: () => !shortcutsOff() && undoLast(),
  // Ctrl+Z (Cmd+Z on a Mac) too, while the action's toast is up. Not in a text field: there it
  // stays the field's own undo (defineShortcuts skips them).
  meta_z: () => !shortcutsOff() && undoLast()
})
</script>

<template>
  <!-- Widths in pixels: a column keeps the width it was given when the window is resized. -->
  <UDashboardGroup
    storage="local"
    storage-key="facteur"
    unit="px"
    :class="accounts.length && !messageOpen ? 'max-lg:bottom-[calc(4.5rem+env(safe-area-inset-bottom))]' : ''"
  >
    <UDashboardSidebar
      v-if="accounts.length"
      id="nav"
      ref="sidebar"
      v-model:open="sidebarOpen"
      collapsible
      resizable
      :default-size="260"
      :min-size="200"
      :max-size="420"
      :ui="{
        root: 'bg-(--surface-side)',
        content: 'bg-(--surface-side)',
        header: 'h-auto px-3.5 pt-4 pb-3',
        body: 'gap-0 px-2.5 py-1',
        footer: 'border-t border-default px-3.5 py-3'
      }"
    >
      <template #header="{ collapsed }">
        <UTooltip :text="$t('app.sidebar.newMessage')" :disabled="!collapsed" :content="{ side: 'right' }">
          <UButton
            icon="i-lucide-pen-line"
            :label="collapsed ? undefined : $t('app.sidebar.newMessage')"
            :aria-label="collapsed ? $t('app.sidebar.newMessage') : undefined"
            :square="collapsed"
            :block="!collapsed"
            class="h-10.5 justify-center rounded-xl font-semibold"
            :class="collapsed ? 'mx-auto w-10.5' : ''"
            @click="compose()"
          />
        </UTooltip>
      </template>

      <template #default="{ collapsed }">
        <UContextMenu :items="accountMenu" :disabled="!menuAccount && !menuFolder && menuSaved === null">
          <!-- Any link closes the drawer (mobile), the current folder's too: the route doesn't change. -->
          <div
            @click="(e: MouseEvent) => { if ((e.target as Element | null)?.closest('a[href]')) sidebarOpen = false }"
            @contextmenu.capture="pickTarget"
            @dragover="onDragOver"
            @dragleave="(e: DragEvent) => { if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node)) highlight(null) }"
            @drop="onDrop"
          >
            <SidebarNav :collapsed="collapsed" />
          </div>
        </UContextMenu>
      </template>

      <template #footer="{ collapsed }">
        <!-- The footer slot is a row: stack the progress and the buttons here. -->
        <div class="flex w-full min-w-0 flex-col gap-1">
        <div v-if="!collapsed && progressAccount !== null" class="w-full min-w-0 px-1">
          <SyncProgress :account-id="progressAccount" />
        </div>
        <!-- Folded, the bar is too narrow: the buttons stack. -->
        <div class="flex w-full min-w-0 items-center gap-1" :class="collapsed ? 'flex-col' : ''">
          <UTooltip :content="{ side: collapsed ? 'right' : 'top' }" :ui="{ content: 'h-auto max-w-80 flex-col items-start gap-1 py-2' }">
            <UButton
              :icon="collapsed ? 'i-lucide-refresh-cw' : undefined"
              color="neutral"
              variant="ghost"
              :loading="isSyncing"
              :label="collapsed ? undefined : (isSyncing ? $t('common.syncing') : sinceLabel)"
              :aria-label="collapsed ? (isSyncing ? $t('common.syncing') : syncTooltip) : syncTooltip"
              :square="collapsed"
              class="min-w-0 rounded-[10px] text-[13px] font-normal text-muted"
              :class="collapsed ? '' : 'h-9.5 flex-1'"
              :ui="{ label: 'truncate' }"
              @click="syncAll"
            >
              <template v-if="!collapsed && !isSyncing" #leading>
                <span class="mx-1 size-2 shrink-0 rounded-full" :class="troubled || !online ? 'bg-warning' : oldestSync === null ? 'bg-accented' : 'bg-green-600 dark:bg-green-500'" />
              </template>
            </UButton>
            <template #content>
              <span class="text-xs font-semibold text-highlighted">{{ $t('app.sidebar.syncDetailsTitle') }}</span>
              <span v-if="!online" class="text-xs text-warning">{{ $t('app.ops.health.offline') }}</span>
              <span v-for="a in accounts" :key="a.id" class="flex flex-col text-xs">
                <span class="text-default">{{ $t('app.sidebar.syncDetailsAccount', { name: accountName(a), when: syncedDetail(a.last_sync) }) }}</span>
                <span v-if="authProblems[a.id] ?? syncErrors[a.id]" class="text-warning">{{ authProblems[a.id] ?? syncErrors[a.id] }}</span>
              </span>
              <span class="text-xs text-muted">{{ $t('app.sidebar.syncDetailsClick') }}</span>
            </template>
          </UTooltip>
          <OpsIndicator :collapsed="collapsed" />
          <UTooltip v-if="perfEnabled" :text="$t('app.diagnostics.open')" :content="{ side: collapsed ? 'right' : 'top' }">
            <UButton
              icon="i-lucide-activity"
              color="neutral"
              variant="ghost"
              square
              class="rounded-[10px] text-muted"
              :class="perfOpen && 'bg-elevated text-highlighted'"
              :aria-label="$t('app.diagnostics.open')"
              @click="perfOpen = !perfOpen"
            />
          </UTooltip>
          <UTooltip :text="$t('app.sidebar.settings')">
            <UButton icon="i-lucide-settings" color="neutral" variant="ghost" square to="/settings" class="rounded-[10px] text-muted" :aria-label="$t('app.sidebar.settings')" />
          </UTooltip>
          <UDashboardSidebarCollapse color="neutral" variant="ghost" class="rounded-[10px] text-muted max-lg:hidden" />
        </div>
        </div>
      </template>
    </UDashboardSidebar>

    <!-- The list, kept under the other screens: inert (no focus, no clicks) while covered. A box of
         its own (not `display: contents`): the list's resize handle measures its width against
         this parent, and a box-less one (width 0) sent it straight to its maximum. -->
    <div v-if="mailRoute" class="flex min-w-0 flex-1" :inert="covered">
      <NuxtPage :route="mailRoute" />
    </div>
    <PageOverlay v-if="!onMail && pageRoute" :floating="covered" :inset="sidebarWidth" @close="closeOverlay">
      <NuxtPage :route="pageRoute" />
    </PageOverlay>

    <MobileTabBar v-if="accounts.length && !messageOpen" v-model:sidebar-open="sidebarOpen" />

    <Composer />
    <WhenPicker />
    <CommandPalette />
    <AttachmentViewer />

    <UModal
      :open="folderDialog !== null"
      :title="folderDialog?.mode === 'rename' ? $t('app.folderDialog.renameTitle') : folderDialog?.folder ? $t('app.folderDialog.newIn', { name: folderDialog.folder.display_name }) : $t('app.folderDialog.new')"
      :ui="{ content: 'max-w-md' }"
      @update:open="(o) => { if (!o) folderDialog = null }"
    >
      <template #body>
        <form class="flex gap-2" @submit.prevent="saveFolder">
          <UInput v-model="folderName" :placeholder="$t('app.folderDialog.placeholder')" :aria-label="$t('app.folderDialog.placeholder')" autofocus class="flex-1" />
          <UButton type="submit" :label="folderDialog?.mode === 'rename' ? $t('app.folderDialog.rename') : $t('app.folderDialog.create')" :loading="savingFolder" :disabled="!folderName.trim()" />
        </form>
      </template>
    </UModal>

    <UModal
      :open="renaming !== null"
      :title="$t('app.renameAccount.title')"
      :description="renaming ? $t('app.renameAccount.description', { email: renaming.email }) : undefined"
      :ui="{ content: 'max-w-md' }"
      @update:open="(o) => { if (!o) renaming = null }"
    >
      <template #body>
        <form class="flex gap-2" @submit.prevent="saveLabel">
          <UInput v-model="newLabel" :placeholder="renaming?.email" :aria-label="$t('app.renameAccount.label')" autofocus class="flex-1" />
          <UButton type="submit" :label="$t('app.renameAccount.submit')" />
        </form>
      </template>
    </UModal>
  </UDashboardGroup>
</template>
