<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import { createReusableTemplate, useMediaQuery } from '@vueuse/core'
import type { BackupManifest, BackupMessage, Catalog, CatalogEntry, RestoreFilter, RestoreStats, RestoreTarget } from '~/utils/types'

const PAGE = 100

const route = useRoute()
const { accounts } = useMail()
const backup = useBackup()
const { running, progress } = backup
const showError = useErrorToast()
const { t } = useI18n()
/** Below `lg` the folders are a drawer; below `xl` the preview opens over the list. */
const wide = useMediaQuery('(min-width: 1024px)')
const wider = useMediaQuery('(min-width: 1280px)')
const foldersOpen = ref(false)
const [DefineFolders, ReuseFolders] = createReusableTemplate()

const backups = ref<BackupManifest[]>([])
const source = ref<string | undefined>((route.query.source as string) || undefined)
const targetAccount = ref<number | undefined>()

onMounted(async () => {
  try {
    backups.value = await api.listBackups(await backup.dest())
  } catch (e) {
    showError(e)
  }
  source.value ??= backups.value[0]?.dir
  // The source from the link was known before the list: find its account now.
  targetAccount.value = accountOf(backups.value.find(b => b.dir === source.value))?.id ?? targetAccount.value
})

function accountOf(b: BackupManifest | undefined) {
  return b ? accounts.value.find(a => a.email.toLowerCase() === b.email.toLowerCase()) : undefined
}

const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
const sourceItems = computed(() => backups.value.map((b) => {
  const account = accountOf(b)
  return {
  label: account ? accountName(account) : b.email,
  value: b.dir,
  description: t('tools.restore.sourceDetail', { n: fmt.value.format(b.messages), date: shortDate(b.last_backup) }, b.messages)
  }
}))
const targetItems = computed(() => accounts.value.map(a => ({ label: accountName(a), value: a.id })))

type Preset = 'all' | 'today' | 'week' | 'month' | 'year' | 'custom'
const preset = ref<Preset>('all')
const customFrom = ref('')
const customTo = ref('')
const query = ref('')
const flaggedOnly = ref(false)
const folders = ref<string[]>([])

const presetItems = computed(() => [
  { label: t('tools.restore.presetAll'), value: 'all' },
  { label: t('tools.restore.presetToday'), value: 'today' },
  { label: t('tools.restore.presetWeek'), value: 'week' },
  { label: t('tools.restore.presetMonth'), value: 'month' },
  { label: t('tools.restore.presetYear'), value: 'year' },
  { label: t('tools.restore.presetCustom'), value: 'custom' }
])

const dayStart = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 1000
/** A typed "YYYY-MM-DD" date, in the local time zone. */
const parseDay = (s: string) => {
  const [y, m, d] = s.split('-').map(Number)
  return y && m && d ? new Date(y, m - 1, d) : null
}

const range = computed<{ since: number | null, until: number | null }>(() => {
  const now = new Date()
  const today = dayStart(now)
  switch (preset.value) {
    case 'today': return { since: today, until: null }
    case 'week': return { since: today - 6 * 86_400, until: null }
    case 'month': return { since: today - 29 * 86_400, until: null }
    case 'year': return { since: new Date(now.getFullYear(), 0, 1).getTime() / 1000, until: null }
    case 'custom': {
      const from = parseDay(customFrom.value)
      const to = parseDay(customTo.value)
      // "Until" is inclusive: stop at the start of the next day.
      return { since: from ? dayStart(from) : null, until: to ? dayStart(to) + 86_400 : null }
    }
    default: return { since: null, until: null }
  }
})

const filter = computed<RestoreFilter>(() => ({
  folders: folders.value,
  since: range.value.since,
  until: range.value.until,
  query: query.value.trim() || null,
  flagged_only: flaggedOnly.value
}))

const catalog = ref<Catalog | null>(null)
const entries = ref<CatalogEntry[]>([])
/** Source and filter the catalog shown was read for: what a restoration then sends. */
const shown = ref<{ source: string, filter: RestoreFilter } | null>(null)
const loading = ref(false)
/** Source or filter changed, the new catalog not read yet: the one shown (and its count) is stale. */
const resetting = ref(false)
/** Last catalog read requested: an older answer arriving after it is ignored. */
let catalogRequest = 0

let timer: ReturnType<typeof setTimeout> | undefined
async function load(reset = true) {
  const from = source.value
  if (!from) {
    resetting.value = false
    return
  }
  // Extra pages extend the catalog shown: none while a new one is loading.
  if (!reset && (resetting.value || !shown.value)) return
  const request = reset ? ++catalogRequest : catalogRequest
  const wanted: RestoreFilter = reset ? { ...filter.value, folders: [...filter.value.folders] } : shown.value!.filter
  loading.value = true
  try {
    const page = await api.backupCatalog(await backup.dest(), from, wanted, PAGE, reset ? 0 : entries.value.length)
    if (request !== catalogRequest) return
    catalog.value = page
    entries.value = reset ? page.entries : [...entries.value, ...page.entries]
    if (reset) shown.value = { source: from, filter: wanted }
  } catch (e) {
    if (request === catalogRequest) showError(e)
  } finally {
    if (request === catalogRequest) {
      loading.value = false
      if (reset) resetting.value = false
    }
  }
}
watch([source, filter], () => {
  clearTimeout(timer)
  // Invalidate the read in progress: it was for the old criteria.
  catalogRequest++
  resetting.value = true
  timer = setTimeout(() => load(), 200)
}, { immediate: true, deep: true })
onBeforeUnmount(() => clearTimeout(timer))

function more(e: Event) {
  const el = e.target as HTMLElement
  if (!loading.value && !resetting.value && catalog.value && entries.value.length < catalog.value.matching
    && el.scrollTop + el.clientHeight > el.scrollHeight - 400) load(false)
}

const folderName = (path: string) => catalog.value?.folders.find(f => f.path === path)?.name ?? path

function toggleFolder(path: string) {
  folders.value = folders.value.includes(path) ? folders.value.filter(f => f !== path) : [...folders.value, path]
}

/** Messages ticked one by one. Empty: everything the filter matches is restored. */
const picked = ref(new Set<number>())
const preview = ref<BackupMessage | null>(null)
const previewId = ref<number | null>(null)

function pick(id: number) {
  const next = new Set(picked.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  picked.value = next
}

async function open(entry: CatalogEntry) {
  if (!source.value) return
  previewId.value = entry.id
  try {
    const message = await api.backupMessage(await backup.dest(), source.value, entry.id)
    if (previewId.value === entry.id) preview.value = message
  } catch (e) {
    showError(e)
  }
}
const previewOpen = computed({
  get: () => !wider.value && preview.value !== null,
  set: (open: boolean) => {
    if (!open) {
      preview.value = null
      previewId.value = null
    }
  }
})

// New backup: start over, targeting the backed-up account if it still exists.
// Declared after the state it resets.
watch(source, (dir) => {
  targetAccount.value = accountOf(backups.value.find(b => b.dir === dir))?.id ?? accounts.value[0]?.id
  picked.value = new Set()
  preview.value = null
  previewId.value = null
  folders.value = []
}, { immediate: true })

const count = computed(() => (picked.value.size ? picked.value.size : (catalog.value?.matching ?? 0)))
const summary = computed(() => {
  if (picked.value.size) return t('tools.restore.picked', picked.value.size)
  const c = catalog.value
  if (!c) return ''
  return t('tools.restore.matching', { size: fileSize(c.matching_bytes) }, c.matching)
})

const target = ref<RestoreTarget>('new_folder')
/** Same name as the folder the engine creates, in the UI language. */
const restoreFolder = computed(() => t('tools.restore.folderName'))
const targetOptions = computed(() => [
  {
    label: t('tools.restore.newFolder'),
    value: 'new_folder',
    description: t('tools.restore.newFolderDesc', { folder: `${restoreFolder.value} <date>` })
  },
  {
    label: t('tools.restore.original'),
    value: 'original',
    description: t('tools.restore.originalDesc')
  }
])
const result = ref<RestoreStats | null>(null)
const resultDetail = computed(() => {
  const r = result.value
  if (!r) return undefined
  const parts: string[] = []
  if (r.skipped) parts.push(t('tools.restore.skipped', r.skipped))
  if (r.failed) parts.push(t('tools.restore.failed', { list: r.failures.join(' · ') }, r.failed))
  return parts.length ? parts.join(' ') : undefined
})

const progressLabel = computed(() => {
  const p = progress.value
  return p && p.kind === 'restore' ? `${p.mailbox} · ${fmt.value.format(p.done)}/${fmt.value.format(p.total)}` : t('tools.restore.connecting')
})

async function run() {
  // The count to confirm and the filter sent must be those of the catalog on screen.
  const basis = shown.value
  if (!basis || resetting.value || basis.source !== source.value || targetAccount.value === undefined || !count.value) return
  const n = count.value
  const to = accounts.value.find(a => a.id === targetAccount.value)?.email
  const where = target.value === 'original' ? t('tools.restore.whereOriginal') : t('tools.restore.whereNew', { folder: restoreFolder.value })
  const ok = await confirm(t('tools.restore.confirm', { to: to ?? '', where }, n), {
    title: t('tools.restore.confirmTitle'),
    kind: 'warning'
  })
  // Changed while the question was asked: not what was confirmed any more.
  if (!ok || shown.value !== basis || resetting.value) return
  result.value = await backup.restore(basis.source, targetAccount.value, {
    filter: basis.filter,
    ids: picked.value.size ? [...picked.value] : null,
    target: target.value
  })
}
</script>

<template>
  <UDashboardPanel id="restore">
    <template #header>
      <UDashboardNavbar :title="t('tools.restore.title')">
        <template #leading>
          <UButton icon="i-lucide-arrow-left" color="neutral" variant="ghost" to="/settings" :aria-label="t('tools.restore.backToSettings')" />
        </template>
      </UDashboardNavbar>

      <div class="flex flex-wrap items-center gap-2 border-b border-default px-4 py-2 sm:px-6">
        <span class="text-sm text-muted">{{ t('tools.restore.backupOf') }}</span>
        <USelect v-model="source" :items="sourceItems" :placeholder="t('tools.restore.noBackup')" :aria-label="t('tools.restore.backupOf')" class="w-full sm:w-72" />
        <UIcon name="i-lucide-arrow-right" class="size-4 text-dimmed max-sm:hidden" />
        <span class="text-sm text-muted">{{ t('tools.restore.towards') }}</span>
        <USelect v-model="targetAccount" :items="targetItems" :aria-label="t('tools.restore.towards')" class="w-full sm:w-64" />
      </div>

      <div class="flex flex-wrap items-center gap-2 border-b border-default px-4 py-2 sm:px-6">
        <UTabs v-model="preset" :items="presetItems" :content="false" size="xs" variant="pill" class="max-w-full overflow-x-auto" />
        <template v-if="preset === 'custom'">
          <UInput v-model="customFrom" type="date" size="sm" :aria-label="t('tools.shared.from')" />
          <span class="text-sm text-muted">{{ t('tools.shared.between') }}</span>
          <UInput v-model="customTo" type="date" size="sm" :aria-label="t('tools.shared.to')" />
        </template>
        <UInput v-model="query" icon="i-lucide-search" :placeholder="t('tools.restore.query')" :aria-label="t('tools.restore.query')" size="sm" class="w-full sm:ms-auto sm:w-56" />
        <UCheckbox v-model="flaggedOnly" :label="t('tools.restore.flaggedOnly')" />
      </div>
    </template>

    <template #body>
      <DefineFolders>
        <button
          type="button"
          class="mb-1 flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-elevated/50"
          :class="!folders.length ? 'font-medium text-highlighted' : 'text-toned'"
          :aria-pressed="!folders.length"
          @click="folders = []"
        >
          <UIcon name="i-lucide-folders" class="size-4 shrink-0" />
          <span class="flex-1">{{ t('tools.restore.allFolders') }}</span>
        </button>
        <button
          v-for="f in catalog?.folders ?? []"
          :key="f.path"
          type="button"
          class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-elevated/50"
          :class="f.matching ? 'text-default' : 'text-dimmed'"
          role="checkbox"
          :aria-checked="folders.includes(f.path)"
          @click="toggleFolder(f.path)"
        >
          <CheckMark :checked="folders.includes(f.path)" />
          <span class="min-w-0 flex-1 truncate" :title="f.name">{{ f.name }}</span>
          <span class="shrink-0 text-xs text-muted">{{ fmt.format(f.matching) }}<template v-if="f.matching !== f.total">/{{ fmt.format(f.total) }}</template></span>
        </button>
      </DefineFolders>

      <div v-if="!backups.length" class="flex h-full flex-col items-center justify-center gap-2 text-sm text-muted">
        <UIcon name="i-lucide-hard-drive" class="size-8" />
        <p>{{ t('tools.restore.empty') }}</p>
        <UButton :label="t('tools.restore.backupSettings')" to="/settings" color="neutral" variant="outline" size="sm" />
      </div>

      <div v-else class="-mx-4 -my-4 grid h-[calc(100%+2rem)] grid-cols-1 sm:-mx-6 sm:-my-6 lg:grid-cols-[14rem_minmax(0,1fr)] xl:grid-cols-[14rem_minmax(0,1fr)_minmax(0,1fr)]">
        <aside v-if="wide" class="overflow-y-auto border-r border-default p-3">
          <ReuseFolders />
        </aside>

        <section class="flex min-h-0 flex-col xl:border-r xl:border-default">
          <div class="flex items-center gap-2 border-b border-default px-4 py-2 text-sm">
            <UButton
              v-if="!wide"
              icon="i-lucide-folders"
              :label="folders.length ? t('tools.restore.foldersCount', { n: folders.length }, folders.length) : t('tools.restore.allFolders')"
              size="sm"
              color="neutral"
              variant="outline"
              class="shrink-0"
              @click="foldersOpen = true"
            />
            <span class="min-w-0 truncate text-muted">{{ summary }}</span>
            <UButton v-if="picked.size" :label="t('tools.restore.uncheckAll')" size="xs" color="neutral" variant="link" @click="picked = new Set()" />
            <UIcon v-if="loading" name="i-lucide-loader-circle" class="ms-auto size-4 animate-spin text-muted" />
          </div>
          <div class="min-h-0 flex-1 overflow-y-auto" @scroll.passive="more">
            <p v-if="catalog && !entries.length" class="p-6 text-center text-sm text-muted">{{ t('tools.shared.noMatch') }}</p>
            <div
              v-for="e in entries"
              :key="e.id"
              class="flex cursor-pointer items-center gap-3 border-b border-default px-4 py-2"
              :class="previewId === e.id ? 'bg-elevated' : picked.has(e.id) ? 'bg-primary/10' : 'hover:bg-elevated/50'"
              @click="open(e)"
            >
              <UCheckbox :model-value="picked.has(e.id)" :aria-label="t('tools.restore.check')" @click.stop @update:model-value="pick(e.id)" />
              <!-- The row opens the preview on a click anywhere; this button is its keyboard way. -->
              <button type="button" class="block min-w-0 flex-1 cursor-pointer text-start" :aria-current="previewId === e.id || undefined" @click.stop="open(e)">
                <span class="flex items-baseline gap-2">
                  <span class="truncate text-sm text-highlighted">{{ addressLabel(e.from) }}</span>
                  <UIcon v-if="e.flags.includes('F')" name="i-lucide-star" class="size-3.5 shrink-0 text-warning" />
                  <span class="ms-auto shrink-0 text-xs text-muted">{{ shortDate(e.received) }}</span>
                </span>
                <span class="block truncate text-sm">{{ e.subject || t('common.noSubject') }}</span>
                <span class="block truncate text-xs text-muted">{{ folderName(e.folder) }} · {{ fileSize(e.size) }}</span>
              </button>
            </div>
          </div>
        </section>

        <section v-if="wider" class="min-h-0 overflow-y-auto p-4">
          <div v-if="preview" class="space-y-3">
            <div>
              <h2 class="text-lg font-semibold text-highlighted">{{ preview.subject || t('common.noSubject') }}</h2>
              <p class="text-sm text-muted">{{ preview.from ? addressFull(preview.from) : '' }} · {{ longDate(preview.date) }}</p>
              <p class="truncate text-xs text-muted">{{ t('tools.restore.to', { to: preview.to.map(addressLabel).join(', ') }) }}</p>
            </div>
            <div v-if="preview.attachments.length" class="flex flex-wrap gap-1">
              <UBadge v-for="a in preview.attachments" :key="a.index" color="neutral" variant="outline" icon="i-lucide-paperclip">
                {{ a.filename }} · {{ fileSize(a.size) }}
              </UBadge>
            </div>
            <MailFrame :html="preview.html" :plain="preview.is_plain" :allow-remote="false" />
          </div>
          <div v-else class="flex h-full flex-col items-center justify-center gap-2 text-sm text-dimmed">
            <UIcon name="i-lucide-mail-search" class="size-8" />
            {{ t('tools.restore.previewHint') }}
          </div>
        </section>
      </div>

      <USlideover v-model:open="foldersOpen" side="left" :title="t('tools.restore.folders')" :ui="{ content: 'max-w-xs', body: 'p-3 sm:p-3' }">
        <template #body>
          <ReuseFolders />
        </template>
      </USlideover>

      <USlideover v-model:open="previewOpen" :title="preview?.subject || t('common.noSubject')" :ui="{ content: 'max-w-2xl', body: 'space-y-3' }">
        <template #body>
          <template v-if="preview">
            <div>
              <p class="text-sm text-muted">{{ preview.from ? addressFull(preview.from) : '' }} · {{ longDate(preview.date) }}</p>
              <p class="truncate text-xs text-muted">{{ t('tools.restore.to', { to: preview.to.map(addressLabel).join(', ') }) }}</p>
            </div>
            <div v-if="preview.attachments.length" class="flex flex-wrap gap-1">
              <UBadge v-for="a in preview.attachments" :key="a.index" color="neutral" variant="outline" icon="i-lucide-paperclip">
                {{ a.filename }} · {{ fileSize(a.size) }}
              </UBadge>
            </div>
            <MailFrame :html="preview.html" :plain="preview.is_plain" :allow-remote="false" />
          </template>
        </template>
      </USlideover>
    </template>

    <template v-if="backups.length" #footer>
      <div class="flex flex-wrap items-center gap-4 border-t border-default px-4 py-3 sm:px-6">
        <URadioGroup v-model="target" :items="targetOptions" :orientation="wide ? 'horizontal' : 'vertical'" class="min-w-0 flex-1 basis-72" />
        <div class="flex shrink-0 items-center gap-3">
          <div v-if="running" class="w-40 space-y-1 sm:w-56">
            <p class="truncate text-xs text-muted">{{ progressLabel }}</p>
            <UProgress :model-value="progress?.kind === 'restore' && progress.total ? progress.done : null" :max="progress?.total || 100" size="xs" />
          </div>
          <UButton
            icon="i-lucide-archive-restore"
            :label="t('tools.restore.run', count)"
            :loading="running"
            :disabled="running || resetting || !count || targetAccount === undefined"
            @click="run"
          />
        </div>
      </div>
      <UAlert
        v-if="result"
        class="mx-4 mb-3 sm:mx-6"
        :color="result.failed ? 'warning' : 'success'"
        variant="subtle"
        :icon="result.failed ? 'i-lucide-triangle-alert' : 'i-lucide-circle-check'"
        :title="t('tools.restore.done', { folder: result.target }, result.messages)"
        :description="resultDetail"
        :close="true"
        @update:open="result = null"
      />
    </template>
  </UDashboardPanel>
</template>
