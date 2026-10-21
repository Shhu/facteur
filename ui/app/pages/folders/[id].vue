<script setup lang="ts">
import { useMediaQuery } from '@vueuse/core'
import { folderLabel, folderLook, moveTargets } from '~/components/SimilarFolderMenu.vue'
import type { FolderSummary } from '~/utils/types'

const PREVIEW = 6
const route = useRoute()
const { mailboxes, accountById, accountColor, mailboxByRole } = useMail()
const showError = useErrorToast()
const listStale = useListStale()
const toast = useToast()
const { t } = useI18n()
const desktop = useMediaQuery('(min-width: 1024px)')
const back = useScreenBack()

const mailboxId = computed(() => Number(route.params.id))
const summary = ref<FolderSummary | null>(null)
const failed = ref(false)
const expanded = ref(false)

type Fate = 'trash' | 'move'
const fate = ref<Fate>('trash')
const dest = ref<number | undefined>()

watch(mailboxId, async (id) => {
  summary.value = null
  failed.value = false
  expanded.value = false
  fate.value = 'trash'
  dest.value = undefined
  if (!Number.isInteger(id)) {
    failed.value = true
    return
  }
  try {
    const s = await api.folderSummary(id)
    if (id !== mailboxId.value) return
    summary.value = s
    // Gmail: trashing would remove the messages from all their labels, inbox included.
    // Default to removing only this label (moving them to "All Mail").
    if (s.gmail) fate.value = 'move'
  } catch (e) {
    if (id !== mailboxId.value) return
    failed.value = true
    showError(e, t('tools.folder.notFound'))
  }
}, { immediate: true })

const name = computed(() => (summary.value ? folderLabel(summary.value.mailbox) : ''))
const account = computed(() => (summary.value ? accountById(summary.value.mailbox.account_id) : null))
const color = computed(() => accountColor(summary.value?.mailbox.account_id ?? 0))
const doomed = computed(() => new Set([mailboxId.value, ...(summary.value?.children.map(c => c.id) ?? [])]))
const fmt = computed(() => new Intl.NumberFormat(intlLocale()))

/** Special folders, and folders holding one, are never deleted (the engine refuses them too):
 *  the screen then explains why, with no destructive button. */
const special = computed(() => {
  const s = summary.value
  if (!s) return null
  return s.mailbox.role ? s.mailbox : s.children.find(c => c.role) ?? null
})
const specialText = computed(() => {
  const sp = special.value
  if (!sp) return ''
  return sp.id === summary.value?.mailbox.id
    ? t('tools.folder.system', { name: name.value })
    : t('tools.folder.systemChild', { name: folderLabel(sp) })
})
/** Trash and junk are emptied instead, on their own screen. */
const purgeLabel = computed(() => {
  const role = summary.value?.mailbox.role
  return role === 'trash' ? t('app.folderMenu.emptyTrash') : role === 'junk' ? t('app.folderMenu.emptyJunk') : null
})
const openPurge = () => navigateTo(`/purge/${mailboxId.value}`, { replace: true })

/** Messages on the server when known, else those in facteur. */
const total = computed(() => summary.value?.server_count ?? summary.value?.local ?? 0)
const partial = computed(() => {
  const s = summary.value
  return !!s && s.server_count !== null && s.server_count > s.local
})
const countLabel = computed(() => `${t('tools.folder.messages', total.value)} · ${fileSize(summary.value?.bytes ?? 0)}`)

const longDay = (ts: number) => dateFormat({ day: 'numeric', month: 'long', year: 'numeric' }).format(new Date(ts * 1000))
const shortDay = (ts: number) => dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(new Date(ts * 1000))

const sameDay = computed(() => {
  const s = summary.value
  return !!s?.oldest && !!s.newest && new Date(s.oldest * 1000).toDateString() === new Date(s.newest * 1000).toDateString()
})

const period = computed(() => {
  const s = summary.value
  if (!s?.oldest || !s.newest) return ''
  return sameDay.value ? longDay(s.oldest) : t('tools.folder.range', { from: longDay(s.oldest), to: longDay(s.newest) })
})

const listDetail = computed(() => {
  const s = summary.value
  if (!s?.oldest) return ''
  if (sameDay.value) return desktop.value ? t('tools.folder.allOn', { date: longDay(s.oldest) }, s.local) : shortDay(s.oldest)
  return s.local > s.recent.length ? t('tools.folder.latest') : ''
})
const more = computed(() => (summary.value ? summary.value.local - summary.value.recent.length : 0))
const shown = computed(() => {
  const recent = summary.value?.recent ?? []
  return expanded.value || desktop.value ? recent : recent.slice(0, PREVIEW)
})

const facts = computed(() => {
  const s = summary.value
  if (!s) return []
  const amber = 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400'
  return [
    { key: 'unread', n: s.unread, label: t('tools.folder.unread', s.unread), icon: 'i-lucide-mail', tone: 'bg-teal-100 text-teal-700 dark:bg-teal-400/14 dark:text-teal-400' },
    { key: 'flagged', n: s.flagged, label: t('tools.folder.flagged', s.flagged), icon: 'i-lucide-pin', tone: amber },
    { key: 'attachments', n: s.with_attachments, label: t('tools.folder.withAttachments', s.with_attachments), icon: 'i-lucide-paperclip', tone: amber }
  ]
})

const senders = computed(() => summary.value?.top_senders ?? [])
const senderMax = computed(() => Math.max(1, ...senders.value.map(s => s.count)))
/** Bars only when the counts differ: equal ones would all fill the width for nothing. */
const senderBars = computed(() => new Set(senders.value.map(s => s.count)).size > 1)

const choices = computed(() => [
  { value: 'trash' as const, label: t('tools.folder.trash'), description: t('tools.folder.trashDesc'), icon: 'i-lucide-trash', tone: 'bg-red-100 text-red-700 dark:bg-red-500/14 dark:text-red-300' },
  { value: 'move' as const, label: t('tools.folder.move'), description: t('tools.folder.moveDesc'), icon: 'i-lucide-folder-input', tone: 'bg-sky-100 text-sky-700 dark:bg-sky-500/16 dark:text-sky-300' }
])

/** Arrow keys move the choice, as in a native radio group. */
function onChoiceKey(e: KeyboardEvent) {
  if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) return
  e.preventDefault()
  const group = (e.currentTarget as HTMLElement).parentElement
  fate.value = fate.value === 'trash' ? 'move' : 'trash'
  nextTick(() => group?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus())
}

const destItems = computed(() => {
  const s = summary.value
  if (!s) return []
  return moveTargets(mailboxes.value, s.mailbox.account_id)
    .filter(m => !doomed.value.has(m.id) && m.role !== 'trash')
    .map(m => ({ value: m.id, label: folderLabel(m), ...folderLook(m) }))
})
watch(destItems, (items) => {
  if (dest.value === undefined) {
    const s = summary.value
    const preferred = s && (s.gmail
      ? mailboxByRole('all', s.mailbox.account_id)
      : mailboxByRole('archive', s.mailbox.account_id) ?? mailboxByRole('all', s.mailbox.account_id))
    dest.value = items.find(i => i.value === preferred?.id)?.value ?? items[0]?.value
  }
})
const destination = computed(() => destItems.value.find(i => i.value === dest.value))
const selectUi = (mobile: boolean) => ({
  // Leading and trailing in the flow (not laid over the text): the button keeps its own padding.
  base: mobile ? 'h-13 w-full gap-2.5 rounded-[14px] px-3.5 ring-primary' : 'h-10.5 w-75 gap-2.5 rounded-[10px] px-3 ring-primary',
  leading: 'static ps-0',
  trailing: 'static pe-0',
  content: 'max-h-[min(26rem,var(--reka-combobox-content-available-height,26rem))] min-w-70 rounded-[14px] p-1.5',
  viewport: 'p-0',
  item: 'h-9.5 items-center px-2.5 before:rounded-[9px] data-highlighted:not-data-disabled:before:bg-elevated data-[state=checked]:before:bg-primary/12',
  trailingIcon: 'text-muted'
})

const sentence = computed(() => {
  const s = summary.value
  if (!s) return ''
  const what = t(s.children.length ? 'tools.folder.willDeleteTree' : 'tools.folder.willDelete', { name: name.value })
  const n = fmt.value.format(total.value)
  const then = !total.value
    ? ''
    : fate.value === 'trash'
      ? t('tools.folder.toTrash', { n }, total.value)
      : destination.value ? t('tools.folder.toDest', { n, folder: destination.value.label }, total.value) : ''
  return then ? t('tools.folder.sentence', { what, then }) : t('tools.folder.sentenceShort', { what })
})

const deleting = ref(false)
async function remove() {
  const s = summary.value
  if (!s || deleting.value || special.value) return
  deleting.value = true
  try {
    await api.deleteFolder(s.mailbox.id, fate.value === 'move' ? (dest.value ?? null) : null)
    listStale.bump()
    toast.add({ title: t('tools.folder.deleted', { name: s.mailbox.display_name }), icon: 'i-lucide-folder-x' })
    const inbox = mailboxByRole('inbox', s.mailbox.account_id)
    await navigateTo(inbox ? `/mail/${inbox.id}` : '/', { replace: true })
  } catch (e) {
    showError(e, t('tools.shared.deleteFailed'))
  } finally {
    deleting.value = false
  }
}

const card = 'border border-default bg-(--surface-card) shadow-xs dark:shadow-none'
const folderTone = 'bg-amber-100 text-amber-700 dark:bg-amber-500/16 dark:text-amber-400'
const choiceOn = 'bg-teal-50 ring-2 ring-primary dark:bg-primary/7'
const radioOn = 'border-[6px] border-primary bg-primary shadow-[inset_0_0_0_3px_var(--surface-card)]'
const radioOff = 'border-[1.5px] border-(--ui-text-dimmed)'
/** With `btn-danger` (main.css): solid red in both themes. */
const deleteButton = computed(() => ({
  label: summary.value?.children.length ? t('tools.folder.deleteMany', { n: summary.value.children.length + 1 }) : t('tools.folder.title'),
  icon: 'i-lucide-folder-x',
  color: 'error' as const,
  loading: deleting.value,
  disabled: fate.value === 'move' && dest.value === undefined
}))
</script>

<template>
  <UDashboardPanel id="folder-delete" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <UDashboardNavbar :title="t('tools.folder.title')" class="max-lg:hidden">
        <template #leading>
          <UButton icon="i-lucide-arrow-left" color="neutral" variant="ghost" :aria-label="t('tools.shared.back')" @click="back" />
        </template>
      </UDashboardNavbar>
      <div class="flex shrink-0 items-center gap-1 border-b border-default bg-default px-2 py-2.5 lg:hidden dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-arrow-left"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl text-highlighted"
          :aria-label="t('tools.shared.back')"
          @click="back"
        />
        <h1 class="text-[17px] font-semibold text-highlighted">{{ t('tools.folder.title') }}</h1>
      </div>
    </template>

    <template #body>
      <UEmpty
        v-if="failed"
        icon="i-lucide-folder-x"
        :title="t('tools.folder.notFound')"
        :description="t('tools.shared.unavailableHint')"
        :actions="[{ label: t('tools.shared.back'), icon: 'i-lucide-arrow-left', color: 'neutral', variant: 'subtle', onClick: back }]"
        variant="naked"
        class="h-full"
      />
      <div v-else-if="!summary" class="flex h-full items-center justify-center text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="me-2 size-4 animate-spin" /> {{ t('tools.folder.loading') }}
      </div>

      <div v-else-if="desktop" class="mx-auto flex w-full max-w-245 flex-col gap-4 px-8 py-6 xl:px-0">
        <section :aria-label="name" :class="card" class="flex items-center gap-4.5 rounded-[18px] px-6 py-5">
          <span class="flex size-14 shrink-0 items-center justify-center rounded-2xl" :class="folderTone">
            <UIcon name="i-lucide-folder" class="size-6.5" />
          </span>
          <div class="flex min-w-0 flex-1 flex-col gap-1.5">
            <span class="truncate text-[22px] font-bold text-highlighted">{{ name }}</span>
            <div class="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1 text-[13px] text-muted">
              <span v-if="account" class="inline-flex min-w-0 items-center gap-1.5 rounded-full py-0.5 ps-0.75 pe-2.5 text-[12.5px] font-semibold" :class="color.soft">
                <img v-if="account.avatar" :src="account.avatar" alt="" class="size-5 shrink-0 rounded-full object-cover">
                <span v-else class="flex size-5 shrink-0 items-center justify-center rounded-full text-[9px] font-bold" :class="color.solid">{{ initials(accountName(account)) }}</span>
                <span class="truncate">{{ accountName(account) }}</span>
              </span>
              <span v-if="period">{{ period }}</span>
            </div>
            <span v-if="summary.children.length" class="text-[13px] text-muted">
              {{ t('tools.folder.children', { list: summary.children.map(c => c.display_name).join(', ') }) }}
            </span>
          </div>
          <p class="flex shrink-0 flex-col items-end">
            <span class="text-[52px] leading-[0.95] font-bold tracking-[-0.03em] tabular-nums text-highlighted">{{ fmt.format(total) }}</span>
            <span class="text-[13px] text-muted">{{ countLabel }}</span>
          </p>
        </section>

        <p v-if="partial" class="-mt-1 px-1 text-xs text-dimmed">
          {{ t('tools.folder.partial', { n: fmt.format(summary.local) }) }}
        </p>

        <template v-if="summary.local">
          <div class="flex flex-wrap gap-2">
            <span v-for="f in facts" :key="f.key" class="inline-flex h-8 items-center gap-2 rounded-full border border-default bg-(--surface-card) ps-2 pe-3 text-[13px] text-muted">
              <span class="flex size-5 shrink-0 items-center justify-center rounded-md" :class="f.tone">
                <UIcon :name="f.icon" class="size-3" />
              </span>
              <span><b class="font-bold tabular-nums" :class="f.n ? 'text-highlighted' : 'text-muted'">{{ fmt.format(f.n) }}</b> {{ f.label }}</span>
            </span>
          </div>

          <div class="grid grid-cols-2 gap-4">
            <section aria-labelledby="folder-senders" :class="card" class="flex max-h-96 flex-col overflow-hidden rounded-2xl">
              <h2 id="folder-senders" class="px-4.5 pt-3.5 pb-2 text-[15px] font-semibold text-highlighted">{{ t('tools.folder.senders') }}</h2>
              <ul class="min-h-0 overflow-y-auto">
                <li v-for="s in senders" :key="s.email" class="flex items-center gap-3 border-t border-default px-4.5 py-2.25">
                  <MailAvatar :address="{ name: s.name, email: s.email }" :account-id="summary.mailbox.account_id" class="size-8! text-[11px]!" />
                  <div class="flex min-w-0 flex-1 flex-col">
                    <span class="truncate text-[13.5px] font-semibold text-highlighted">{{ s.name || s.email }}</span>
                    <span v-if="s.name" class="truncate text-xs text-muted">{{ s.email }}</span>
                    <span v-if="senderBars" class="mt-1.5 block h-1.25 overflow-hidden rounded-full bg-elevated" aria-hidden="true">
                      <span class="block h-full rounded-full bg-(--viz-series-1)" :style="{ width: `${(s.count / senderMax) * 100}%`, minWidth: '4px' }" />
                    </span>
                  </div>
                  <span class="shrink-0 rounded-full bg-elevated px-2 py-px text-xs font-bold tabular-nums text-muted">{{ fmt.format(s.count) }}</span>
                </li>
              </ul>
            </section>

            <section aria-labelledby="folder-list" :class="card" class="flex max-h-96 flex-col overflow-hidden rounded-2xl">
              <h2 id="folder-list" class="px-4.5 pt-3.5 pb-2 text-[15px] font-semibold text-highlighted">
                {{ t('tools.folder.list') }}<span v-if="listDetail" class="font-normal text-muted"> · {{ listDetail }}</span>
              </h2>
              <ul class="min-h-0 overflow-y-auto">
                <li v-for="m in shown" :key="m.id" class="flex items-center gap-3 border-t border-default px-4.5 py-2.25">
                  <div class="flex min-w-0 flex-1 flex-col gap-px">
                    <span class="truncate text-[13.5px] text-highlighted">{{ m.subject || t('common.noSubject') }}</span>
                    <span class="truncate text-xs text-muted">{{ addressLabel(m.from) }}</span>
                  </div>
                  <span v-if="!sameDay" class="shrink-0 text-xs tabular-nums text-dimmed">{{ shortDate(m.date) }}</span>
                </li>
                <li v-if="more > 0" class="border-t border-default px-4.5 py-2 text-xs text-muted">
                  {{ t('tools.folder.more', { n: fmt.format(more) }, more) }}
                </li>
              </ul>
            </section>
          </div>
        </template>
        <p v-else-if="!total" :class="card" class="rounded-2xl p-6 text-center text-sm text-muted">{{ t('tools.folder.empty') }}</p>

        <section v-if="special" role="note" :class="card" class="flex items-center gap-3.5 rounded-2xl px-5 py-4.5">
          <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-elevated text-toned">
            <UIcon name="i-lucide-lock" class="size-4.5" />
          </span>
          <div class="flex min-w-0 flex-1 flex-col gap-0.5">
            <span class="text-sm font-semibold text-highlighted">{{ t('tools.folder.systemTitle') }}</span>
            <span class="text-[13px] text-muted">{{ specialText }}</span>
          </div>
          <UButton v-if="purgeLabel" :label="purgeLabel" icon="i-lucide-trash-2" color="neutral" variant="outline" class="h-10.5 rounded-[11px] px-4 text-sm" @click="openPurge" />
          <UButton :label="t('tools.shared.back')" color="neutral" variant="outline" class="h-10.5 rounded-[11px] px-4 text-sm" @click="back" />
        </section>

        <section v-else aria-labelledby="folder-fate" :class="card" class="flex flex-col gap-3 rounded-2xl px-5 py-4.5">
          <template v-if="total">
            <h2 id="folder-fate" class="text-[15px] font-semibold text-highlighted">{{ t('tools.folder.fate', { n: fmt.format(total) }, total) }}</h2>
            <div role="radiogroup" aria-labelledby="folder-fate" class="grid grid-cols-2 gap-3">
              <button
                v-for="c in choices"
                :key="c.value"
                type="button"
                role="radio"
                :aria-checked="fate === c.value"
                :tabindex="fate === c.value ? 0 : -1"
                class="flex items-start gap-3 rounded-[14px] p-3.5 text-start ring-inset transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
                :class="fate === c.value ? choiceOn : 'ring-1 ring-default hover:bg-elevated/50'"
                @click="fate = c.value"
                @keydown="onChoiceKey"
              >
                <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px]" :class="c.tone">
                  <UIcon :name="c.icon" class="size-4.5" />
                </span>
                <span class="flex min-w-0 flex-1 flex-col gap-0.75">
                  <span class="text-sm font-semibold text-highlighted">{{ c.label }}</span>
                  <span class="text-[12.5px] text-muted">{{ c.description }}</span>
                </span>
                <span class="size-5 shrink-0 rounded-full" :class="fate === c.value ? radioOn : radioOff" aria-hidden="true" />
              </button>
            </div>
            <div v-if="fate === 'move'" class="flex items-center gap-2.5 ps-1">
              <span class="text-[13.5px] text-muted">{{ t('tools.folder.to') }}</span>
              <USelectMenu
                v-model="dest"
                :items="destItems"
                value-key="value"
                :search-input="destItems.length > 12 ? { placeholder: t('tools.cleanup.searchFolder'), icon: 'i-lucide-search' } : false"
                :ui="selectUi(false)"
                :aria-label="t('tools.folder.dest')"
                trailing-icon="i-lucide-chevron-down"
              >
                <template #leading>
                  <UIcon name="i-lucide-folder" class="size-4 shrink-0 text-primary" />
                </template>
                <template #default>
                  <span class="min-w-0 flex-1 truncate text-start text-[13.5px]" :class="destination ? 'text-highlighted' : 'text-muted'">
                    {{ destination?.label ?? t('tools.folder.chooseFolder') }}
                  </span>
                  <span v-if="account" class="flex max-w-28 shrink-0 items-center gap-1.25 text-xs text-muted">
                    <span class="size-1.75 shrink-0 rounded-full" :class="color.dot" />
                    <span class="truncate">{{ accountName(account) }}</span>
                  </span>
                </template>
                <template #item="{ item }">
                  <span class="flex w-full min-w-0 items-center gap-2.5">
                    <span class="flex size-6.5 shrink-0 items-center justify-center rounded-lg" :class="item.tone">
                      <UIcon :name="item.icon" class="size-3.5" />
                    </span>
                    <span class="min-w-0 flex-1 truncate text-[13.5px] text-highlighted">{{ item.label }}</span>
                    <UIcon v-if="item.value === dest" name="i-lucide-check" class="size-4 shrink-0 text-primary" />
                  </span>
                </template>
              </USelectMenu>
            </div>
            <p v-if="summary.gmail" class="ps-1 text-xs text-muted">
              {{ t('tools.folder.gmail', { all: roleLabels.all }) }}
            </p>
          </template>
          <div class="flex items-center gap-3" :class="total ? 'border-t border-default pt-3' : ''">
            <span class="min-w-0 flex-1 text-[13px] text-muted">{{ sentence }}</span>
            <UButton :label="t('common.cancel')" color="neutral" variant="outline" class="h-10.5 rounded-[11px] px-4 text-sm" @click="back" />
            <UButton v-bind="deleteButton" class="h-10.5 rounded-[11px] px-4.5 text-sm font-semibold btn-danger" @click="remove" />
          </div>
        </section>
      </div>

      <div v-else class="flex flex-col gap-3.5 px-4 pt-4 pb-6">
        <section :aria-label="name" :class="card" class="flex items-center gap-3.5 rounded-[18px] p-4">
          <span class="flex size-12 shrink-0 items-center justify-center rounded-[14px]" :class="folderTone">
            <UIcon name="i-lucide-folder" class="size-5.75" />
          </span>
          <div class="flex min-w-0 flex-1 flex-col gap-1.25">
            <span class="truncate text-lg font-bold text-highlighted">{{ name }}</span>
            <span v-if="account" class="inline-flex min-w-0 items-center gap-1.25 self-start rounded-full py-px ps-0.75 pe-2 text-xs font-semibold" :class="color.soft">
              <img v-if="account.avatar" :src="account.avatar" alt="" class="size-4.5 shrink-0 rounded-full object-cover">
              <span v-else class="flex size-4.5 shrink-0 items-center justify-center rounded-full text-[8px] font-bold" :class="color.solid">{{ initials(accountName(account)) }}</span>
              <span class="truncate">{{ accountName(account) }}</span>
            </span>
            <span v-if="summary.children.length" class="text-xs text-muted">
              {{ t('tools.folder.children', { list: summary.children.map(c => c.display_name).join(', ') }) }}
            </span>
          </div>
          <p class="flex shrink-0 flex-col items-end">
            <span class="text-[40px] leading-[0.95] font-bold tabular-nums text-highlighted">{{ fmt.format(total) }}</span>
            <span class="text-xs text-muted">{{ countLabel }}</span>
          </p>
        </section>

        <p v-if="partial" class="-mt-1 px-1 text-xs text-dimmed">
          {{ t('tools.folder.partial', { n: fmt.format(summary.local) }) }}
        </p>

        <section v-if="summary.local" aria-labelledby="folder-list-m" :class="card" class="overflow-hidden rounded-2xl">
          <h2 id="folder-list-m" class="px-3.5 pt-3.5 pb-2 text-[15px] font-semibold text-highlighted">
            {{ t('tools.folder.list') }}<span v-if="listDetail" class="font-normal text-muted"> · {{ listDetail }}</span>
          </h2>
          <div v-for="m in shown" :key="m.id" class="flex min-h-14 items-center gap-3 border-t border-default px-3.5 py-1.5">
            <MailAvatar :address="m.from" :account-id="m.account_id" class="size-8! text-[11px]!" />
            <div class="flex min-w-0 flex-1 flex-col gap-px">
              <span class="truncate text-[13.5px] text-highlighted">{{ m.subject || t('common.noSubject') }}</span>
              <span class="truncate text-xs text-muted">{{ addressLabel(m.from) }}</span>
            </div>
            <span v-if="!sameDay" class="shrink-0 text-[11.5px] tabular-nums text-dimmed">{{ shortDate(m.date) }}</span>
          </div>
          <button
            v-if="!expanded && (summary.recent.length > PREVIEW || more > 0)"
            type="button"
            class="h-12 w-full border-t border-default text-sm font-semibold text-primary"
            @click="expanded = true"
          >
            {{ t('tools.folder.seeAll', { n: fmt.format(total) }, total) }}
          </button>
          <p v-else-if="expanded && more > 0" class="border-t border-default px-3.5 py-2.5 text-xs text-muted">
            {{ t('tools.folder.more', { n: fmt.format(more) }, more) }}
          </p>
        </section>
        <p v-else-if="!total" :class="card" class="rounded-2xl p-6 text-center text-sm text-muted">{{ t('tools.folder.empty') }}</p>

        <section v-if="special" role="note" :class="card" class="flex flex-col gap-3 rounded-2xl p-4">
          <div class="flex items-start gap-3">
            <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-elevated text-toned">
              <UIcon name="i-lucide-lock" class="size-4.5" />
            </span>
            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
              <span class="text-[14.5px] font-semibold text-highlighted">{{ t('tools.folder.systemTitle') }}</span>
              <span class="text-[13px] text-muted">{{ specialText }}</span>
            </div>
          </div>
          <div class="flex gap-2">
            <UButton :label="t('tools.shared.back')" color="neutral" variant="outline" class="h-12.5 rounded-[14px] px-4.5 text-[15px]" @click="back" />
            <UButton v-if="purgeLabel" :label="purgeLabel" icon="i-lucide-trash-2" color="neutral" variant="outline" block class="h-12.5 flex-1 rounded-[14px] text-[15px]" @click="openPurge" />
          </div>
        </section>

        <section v-else-if="total" aria-labelledby="folder-fate-m" class="flex flex-col gap-2.5">
          <h2 id="folder-fate-m" class="mx-1 text-[15px] font-semibold text-highlighted">{{ t('tools.folder.fate', { n: fmt.format(total) }, total) }}</h2>
          <div role="radiogroup" aria-labelledby="folder-fate-m" class="flex flex-col gap-2.5">
            <button
              v-for="c in choices"
              :key="c.value"
              type="button"
              role="radio"
              :aria-checked="fate === c.value"
              :tabindex="fate === c.value ? 0 : -1"
              class="flex items-start gap-3 rounded-2xl p-3.5 text-start ring-inset focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
              :class="fate === c.value ? choiceOn : 'bg-(--surface-card) ring-1 ring-default'"
              @click="fate = c.value"
              @keydown="onChoiceKey"
            >
              <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px]" :class="c.tone">
                <UIcon :name="c.icon" class="size-4.5" />
              </span>
              <span class="flex min-w-0 flex-1 flex-col gap-0.75">
                <span class="text-[14.5px] font-semibold text-highlighted">{{ c.label }}</span>
                <span class="text-[12.5px] text-muted">{{ c.description }}</span>
              </span>
              <span class="size-5.5 shrink-0 rounded-full" :class="fate === c.value ? radioOn : radioOff" aria-hidden="true" />
            </button>
          </div>
          <USelectMenu
            v-if="fate === 'move'"
            v-model="dest"
            :items="destItems"
            value-key="value"
            :search-input="destItems.length > 12 ? { placeholder: t('tools.cleanup.searchFolder'), icon: 'i-lucide-search' } : false"
            :content="{ side: 'top', align: 'start', sideOffset: 8 }"
            :ui="selectUi(true)"
            :aria-label="t('tools.folder.dest')"
            trailing-icon="i-lucide-chevron-down"
          >
            <template #leading>
              <UIcon name="i-lucide-folder" class="size-4.25 shrink-0 text-primary" />
            </template>
            <template #default>
              <span class="flex min-w-0 flex-1 flex-col text-start leading-tight">
                <span class="text-xs text-muted">{{ t('tools.folder.toFolder') }}</span>
                <span class="truncate text-[15px] font-semibold" :class="destination ? 'text-highlighted' : 'text-muted'">
                  {{ destination?.label ?? t('tools.folder.chooseFolder') }}
                </span>
              </span>
            </template>
            <template #item="{ item }">
              <span class="flex w-full min-w-0 items-center gap-2.5">
                <span class="flex size-6.5 shrink-0 items-center justify-center rounded-lg" :class="item.tone">
                  <UIcon :name="item.icon" class="size-3.5" />
                </span>
                <span class="min-w-0 flex-1 truncate text-[13.5px] text-highlighted">{{ item.label }}</span>
                <UIcon v-if="item.value === dest" name="i-lucide-check" class="size-4 shrink-0 text-primary" />
              </span>
            </template>
          </USelectMenu>
          <p v-if="summary.gmail" class="mx-1 text-xs text-muted">
            {{ t('tools.folder.gmail', { all: roleLabels.all }) }}
          </p>
        </section>
      </div>
    </template>

    <template #footer>
      <div
        v-if="summary && !desktop && !special"
        class="flex flex-col gap-2.5 border-t border-default bg-default px-4 pt-3 pb-[calc(1.25rem+env(safe-area-inset-bottom))] shadow-[0_-8px_24px_rgba(0,0,0,0.12)] dark:bg-(--surface-side)"
      >
        <p class="text-[12.5px] leading-[1.4] text-muted">{{ sentence }}</p>
        <div class="flex gap-2">
          <UButton :label="t('common.cancel')" color="neutral" variant="outline" class="h-12.5 rounded-[14px] px-4.5 text-[15px]" @click="back" />
          <UButton v-bind="deleteButton" block class="h-12.5 flex-1 rounded-[14px] text-[15px] font-semibold btn-danger" @click="remove" />
        </div>
      </div>
    </template>
  </UDashboardPanel>
</template>
