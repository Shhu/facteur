<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import { useIntervalFn } from '@vueuse/core'
import type { OpState, OpView, OpsDetails } from '~/utils/types'

defineProps<{ collapsed?: boolean }>()

const { status, refresh, retry, discard } = useOps()
const { accounts, accountById } = useMail()
const showError = useErrorToast()
const { t } = useI18n()
const open = ref(false)

onMounted(refresh)

const failed = computed(() => status.value.failed.length)

const details = ref<OpsDetails | null>(null)
let detailsRequest = 0
async function loadDetails() {
  const request = ++detailsRequest
  try {
    const d = await api.opsDetails()
    if (request === detailsRequest) details.value = d
  } catch (e) {
    showError(e)
  }
}
watch(open, (isOpen) => {
  if (isOpen) loadDetails()
})
// Each queue change (operation sent, refused, added) refreshes the window.
watch(status, () => {
  if (open.value) loadDetails()
}, { deep: true })

// Clock for the undo countdown; an operation leaving its undo window changes state.
const now = ref(Date.now() / 1000)
const timer = useIntervalFn(() => {
  now.value = Date.now() / 1000
  if (details.value?.ops.some(o => o.state === 'held' && o.not_before !== null && o.not_before <= now.value)) loadDetails()
}, 1000, { immediate: false })
watch(open, isOpen => (isOpen ? timer.resume() : timer.pause()))

const groups = computed(() => {
  const ops = details.value?.ops ?? []
  return accounts.value
    .map(a => ({ account: a, ops: ops.filter(o => o.account_id === a.id), error: details.value?.errors[a.id] ?? null }))
    .filter(g => g.ops.length)
})

const stateLook: Record<OpState, { color: 'neutral' | 'info' | 'warning' | 'error', icon: string }> = {
  held: { color: 'info', icon: 'i-lucide-hourglass' },
  blocked: { color: 'neutral', icon: 'i-lucide-pause' },
  waiting: { color: 'neutral', icon: 'i-lucide-cloud-upload' },
  retrying: { color: 'warning', icon: 'i-lucide-refresh-cw' },
  failed: { color: 'error', icon: 'i-lucide-triangle-alert' }
}

const kindIcons: Record<string, string> = {
  store: 'i-lucide-flag', store_set: 'i-lucide-flag', mark_all_seen: 'i-lucide-mail-open',
  move: 'i-lucide-folder-input', move_set: 'i-lucide-folder-input',
  delete: 'i-lucide-trash-2', delete_set: 'i-lucide-trash-2', purge: 'i-lucide-trash-2',
  append: 'i-lucide-send', save_draft: 'i-lucide-file-pen', delete_draft: 'i-lucide-file-x',
  create: 'i-lucide-folder-plus', rename: 'i-lucide-folder-pen', delete_mailbox: 'i-lucide-folder-x',
  labels: 'i-lucide-tag'
}

function reason(o: OpView, accountError: string | null): string {
  switch (o.state) {
    case 'held':
      return t('app.ops.reason.held', { n: Math.max(0, Math.ceil((o.not_before ?? 0) - now.value)) })
    case 'blocked':
      return t('app.ops.reason.blocked', { n: o.blocked_by ?? '?' })
    case 'retrying':
      return t('app.ops.reason.retrying', { n: o.attempts, error: o.error ?? accountError ?? '?' })
    case 'failed':
      return t('app.ops.reason.failed', { n: o.attempts, error: o.error ?? '?' })
    default:
      return accountError ? t('app.ops.reason.waitingError', { error: accountError }) : t('app.ops.reason.waiting')
  }
}

function where(o: OpView): string | null {
  if (o.mailbox && o.dest) return `${o.mailbox} → ${o.dest}`
  return o.mailbox
}

const kindLabel = (kind: string) => (kindIcons[kind] ? t(`app.ops.kinds.${kind}`) : kind)

const sending = ref<number | null>(null)
async function sendNow(accountId: number) {
  sending.value = accountId
  try {
    // A failure comes with the engine's `sync_finished` event.
    await syncByUser([accountId])
  } finally {
    sending.value = null
    await refresh()
    await loadDetails()
  }
}

async function drop(o: OpView) {
  // A pending action is still meant to happen: ask before giving it up.
  if (!o.failed && !(await confirm(t('app.ops.discardConfirm'), { title: t('app.ops.discard'), kind: 'warning' }))) return
  await discard(o.id, o.created_at)
  await loadDetails()
}

async function again(o: OpView) {
  await retry(o.id)
  await loadDetails()
}
</script>

<template>
  <template v-if="failed || status.pending">
    <UTooltip
      :text="failed
        ? $t('app.ops.failedTooltip', failed)
        : $t('app.ops.pendingTooltip', status.pending)"
    >
      <UButton
        :icon="failed ? 'i-lucide-triangle-alert' : 'i-lucide-cloud-upload'"
        :color="failed ? 'warning' : 'neutral'"
        variant="ghost"
        :square="collapsed"
        :label="collapsed ? undefined : (failed ? $t('app.ops.failed', { n: failed }) : $t('app.ops.pending', { n: status.pending }))"
        :aria-label="collapsed ? (failed ? $t('app.ops.failed', { n: failed }) : $t('app.ops.pending', { n: status.pending })) : undefined"
        @click="open = true"
      />
    </UTooltip>
  </template>

  <!-- Outside the v-if: the window stays open (showing the empty queue) when the last operation
       goes, and never pops up by itself when the indicator comes back. -->
<UModal v-model:open="open" :title="$t('app.ops.title')" :description="$t('app.ops.description')" :ui="{ content: 'max-w-2xl' }">
    <template #body>
      <div v-if="!details" class="flex justify-center py-6">
        <UIcon name="i-lucide-loader-circle" class="size-5 animate-spin text-muted" />
      </div>
      <p v-else-if="!groups.length" class="py-4 text-center text-sm text-muted">{{ $t('app.ops.none') }}</p>
      <div v-else class="space-y-6">
        <section v-for="g in groups" :key="g.account.id" class="space-y-2">
          <div class="flex items-center gap-2">
            <h3 class="min-w-0 flex-1 truncate text-sm font-semibold text-highlighted">
              {{ accountName(g.account) }}
              <span class="font-normal text-muted">· {{ $t('app.ops.count', g.ops.length) }}</span>
            </h3>
            <UButton
              :label="$t('app.ops.sendNow')"
              icon="i-lucide-send-horizontal"
              size="xs"
              color="neutral"
              variant="outline"
              :loading="sending === g.account.id"
              @click="sendNow(g.account.id)"
            />
          </div>
          <UAlert
            v-if="g.error"
            :title="$t('app.ops.connectionProblem')"
            :description="g.error"
            color="warning"
            variant="subtle"
            icon="i-lucide-wifi-off"
          />
          <ul class="divide-y divide-default overflow-hidden rounded-lg border border-default">
            <li v-for="o in g.ops" :key="o.id" class="flex items-start gap-3 px-3 py-2.5">
              <UIcon :name="kindIcons[o.kind] ?? 'i-lucide-circle-dot'" class="mt-0.5 size-4 shrink-0 text-muted" />
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-2">
                  <p class="min-w-0 truncate text-sm font-medium text-highlighted">{{ o.label || kindLabel(o.kind) }}</p>
                  <UBadge
                    :label="$t(`app.ops.state.${o.state}`)"
                    :color="stateLook[o.state].color"
                    :icon="stateLook[o.state].icon"
                    variant="subtle"
                    size="sm"
                  />
                </div>
                <p class="truncate text-xs text-muted">
                  #{{ o.id }} · {{ kindLabel(o.kind) }}<template v-if="where(o)"> · {{ where(o) }}</template>
                  · {{ ago(o.created_at) }}<template v-if="o.size > 100_000"> · {{ fileSize(o.size * 3 / 4) }}</template>
                </p>
                <p class="mt-1 break-words text-xs" :class="o.state === 'failed' ? 'text-error' : o.state === 'retrying' ? 'text-warning' : 'text-default'">
                  {{ reason(o, g.error) }}
                </p>
              </div>
              <div class="flex shrink-0 gap-1">
                <UButton v-if="o.failed" :label="$t('app.ops.retry')" size="xs" color="neutral" variant="outline" @click="again(o)" />
                <UButton :label="$t('app.ops.discard')" size="xs" color="neutral" variant="ghost" @click="drop(o)" />
              </div>
            </li>
          </ul>
        </section>
      </div>
    </template>
  </UModal>
</template>
