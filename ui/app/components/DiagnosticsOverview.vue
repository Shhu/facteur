<script setup lang="ts">
import type { AccountDiagnostics, ConnectionDiagnostics, Diagnostics, ListenerStatus } from '~/composables/usePerf'
import { agoMs, durationLabel, msLabel, num, percent, spanLabel } from '~/utils/diagnostics'

const props = defineProps<{ diag: Diagnostics | null }>()
const { t } = useI18n()
const { samples, freezes, engine } = usePerf()
const { accounts, accountColor } = useMail()

// Durations under way tick with the clock, between two polls.
const now = ref(Date.now())
const ticker = setInterval(() => (now.value = Date.now()), 1000)
onBeforeUnmount(() => clearInterval(ticker))

const mb = (bytes: number) => (bytes / 1048576).toLocaleString(intlLocale(), { maximumFractionDigits: bytes < 10 * 1048576 ? 1 : 0 })

interface Tile { label: string, icon: string, value: string, unit?: string, detail: string, warn?: boolean, bars?: number[] }

const tiles = computed<Tile[]>(() => {
  const d = props.diag
  const list: Tile[] = []
  const rt = d?.runtime
  const avg = rt?.worker_busy.length ? rt.worker_busy.reduce((a, b) => a + b, 0) / rt.worker_busy.length : null
  list.push({
    label: t('app.diagnostics.tiles.runtime'),
    icon: 'i-lucide-cpu',
    value: rt ? (avg === null ? '–' : percent(avg)) : '–',
    detail: rt ? t('app.diagnostics.tiles.runtimeDetail', { workers: rt.workers, tasks: num(rt.alive_tasks), queue: rt.global_queue_depth }) : t('app.diagnostics.unavailable'),
    warn: !!rt && (rt.global_queue_depth > 4 || (avg ?? 0) > 0.8),
    bars: rt?.worker_busy
  })
  const s = d?.store
  if (s) {
    const hits = s.connections.reduce((n, c) => n + (c.cache_hits ?? 0), 0)
    const misses = s.connections.reduce((n, c) => n + (c.cache_misses ?? 0), 0)
    const busy = s.connections.filter(c => c.busy).length
    const free = s.page_count && s.freelist_count !== null ? s.freelist_count / s.page_count : null
    list.push({
      label: t('app.diagnostics.tiles.database'),
      icon: 'i-lucide-database',
      value: mb(s.file_bytes),
      unit: 'MB',
      detail: [
        t('app.diagnostics.tiles.wal', { size: mb(s.wal_bytes) }),
        free !== null ? t('app.diagnostics.tiles.free', { share: percent(free) }) : null,
        hits + misses ? t('app.diagnostics.tiles.cacheHits', { share: percent(hits / (hits + misses)) }) : null,
        t('app.diagnostics.tiles.connectionsBusy', { busy, total: s.connections.length })
      ].filter(Boolean).join(' · '),
      warn: s.wal_bytes > 64 * 1048576 || busy === s.connections.length
    })
  }
  const m = d?.memory
  list.push({
    label: t('app.diagnostics.tiles.memory'),
    icon: 'i-lucide-memory-stick',
    value: m ? mb(m.resident_bytes) : '–',
    unit: m ? 'MB' : undefined,
    detail: m ? (m.private_bytes !== null ? t('app.diagnostics.tiles.private', { size: mb(m.private_bytes) }) : t('app.diagnostics.tiles.resident')) : t('app.diagnostics.unavailable')
  })
  const last = samples.value.at(-1)
  list.push({
    label: t('app.diagnostics.tiles.heap'),
    icon: 'i-lucide-layers',
    value: last?.heap ? mb(last.heap) : '–',
    unit: last?.heap ? 'MB' : undefined,
    detail: last?.heap ? t('app.diagnostics.tiles.heapDetail') : t('app.diagnostics.unavailable')
  })
  const minute = samples.value.filter(x => x.at > now.value - 60_000)
  const maxLag = minute.reduce((n, x) => Math.max(n, x.lag), 0)
  list.push({
    label: t('app.diagnostics.tiles.lag'),
    icon: 'i-lucide-timer',
    value: last ? msLabel(last.lag) : '–',
    unit: 'ms',
    detail: t('app.diagnostics.tiles.lagMax', { ms: msLabel(maxLag) }),
    warn: maxLag > 200
  })
  const recent = freezes.value.filter(f => f.at > now.value - 60_000)
  list.push({
    label: t('app.diagnostics.tiles.freezes'),
    icon: 'i-lucide-snowflake',
    value: num(recent.length),
    detail: recent.length
      ? t('app.diagnostics.tiles.freezesDetail', { total: msLabel(recent.reduce((n, f) => n + f.ms, 0)), max: msLabel(Math.max(...recent.map(f => f.ms))) })
      : t('app.diagnostics.tiles.noFreeze'),
    warn: recent.some(f => f.ms > 200)
  })
  const kinds = d?.perf?.kinds
  if (kinds) {
    const of = (k: string) => kinds.find(x => x.kind === k)?.count ?? 0
    list.push({
      label: t('app.diagnostics.tiles.slowEngine'),
      icon: 'i-lucide-hourglass',
      value: num(of('sql') + of('wait') + of('hold')),
      detail: t('app.diagnostics.tiles.slowEngineDetail', { sql: of('sql'), wait: of('wait'), hold: of('hold') }),
      warn: of('wait') > 0
    })
  }
  if (d) {
    list.push({
      label: t('app.diagnostics.tiles.events'),
      icon: 'i-lucide-radio',
      value: num(d.events_queued),
      detail: t('app.diagnostics.tiles.eventsDetail', { n: d.event_receivers, recorded: num(engine.value.length) }),
      warn: d.events_queued > 64
    })
  }
  return list
})

function dotOf(account: AccountDiagnostics) {
  const known = accounts.value.find(a => a.id === account.id)
  return known ? accountColor(known.id).dot : 'bg-accented'
}

function connectionClass(c: ConnectionDiagnostics) {
  if (c.busy) return 'bg-info animate-pulse'
  return c.open ? 'bg-success' : 'bg-(--ui-border-accented)'
}

function connectionTitle(c: ConnectionDiagnostics) {
  const purpose = t(`app.diagnostics.purpose.${c.purpose}`)
  if (c.busy) return `${purpose} : ${t('app.diagnostics.connection.busy')}`
  if (c.open) return `${purpose} : ${t('app.diagnostics.connection.idle', { span: spanLabel(c.idle_secs ?? 0) })}`
  return `${purpose} : ${t('app.diagnostics.connection.closed')}`
}

const listenerColor = (l: ListenerStatus | null) => {
  switch (l?.state) {
    case 'listening': return 'success' as const
    case 'syncing': return 'info' as const
    case 'connecting': return 'neutral' as const
    case 'waiting': return 'warning' as const
    default: return 'neutral' as const
  }
}
</script>

<template>
  <div class="h-full overflow-y-auto p-4 sm:p-5">
    <p v-if="!diag" class="text-sm text-muted">{{ t('app.diagnostics.loading') }}</p>
    <template v-else>
      <div class="grid grid-cols-2 gap-2.5 md:grid-cols-4">
        <div
          v-for="tile in tiles"
          :key="tile.label"
          class="flex min-w-0 flex-col gap-1 rounded-xl border bg-(--surface-list) p-3"
          :class="tile.warn ? 'border-warning/50' : 'border-default'"
        >
          <div class="flex items-center gap-1.5 text-xs text-muted">
            <UIcon :name="tile.icon" class="size-3.5 shrink-0" />
            <span class="truncate">{{ tile.label }}</span>
            <UIcon v-if="tile.warn" name="i-lucide-triangle-alert" class="ms-auto size-3.5 shrink-0 text-warning" />
          </div>
          <div class="flex items-end gap-2">
            <p class="text-xl font-semibold tabular-nums text-highlighted">
              {{ tile.value }}<span v-if="tile.unit" class="ms-0.5 text-xs font-normal text-muted">{{ tile.unit }}</span>
            </p>
            <div v-if="tile.bars?.length" class="ms-auto flex h-6 items-end gap-0.5" :aria-label="t('app.diagnostics.tiles.workers')">
              <span
                v-for="(b, i) in tile.bars"
                :key="i"
                class="w-1.5 rounded-t-sm"
                :class="b > 0.8 ? 'bg-warning' : 'bg-primary'"
                :style="{ height: `${Math.max(8, b * 100)}%` }"
                :title="`#${i} ${percent(b)}`"
              />
            </div>
          </div>
          <p class="text-xs leading-snug text-muted">{{ tile.detail }}</p>
        </div>
      </div>

      <h3 class="mt-6 mb-2.5 text-xs font-semibold tracking-wide text-muted uppercase">{{ t('app.diagnostics.accounts') }}</h3>
      <p v-if="!diag.accounts.length" class="text-sm text-muted">{{ t('app.diagnostics.noAccount') }}</p>
      <div class="grid gap-2.5 lg:grid-cols-2">
        <article
          v-for="a in diag.accounts"
          :key="a.id"
          class="flex min-w-0 flex-col gap-3 rounded-xl border bg-(--surface-list) p-3.5 text-sm"
          :class="a.auth_refused || a.queue.failed ? 'border-error/40' : 'border-default'"
        >
          <header class="flex min-w-0 items-center gap-2">
            <span class="size-2.5 shrink-0 rounded-full" :class="dotOf(a)" />
            <span class="truncate font-semibold text-highlighted">{{ a.label || a.email }}</span>
            <span v-if="a.label" class="truncate text-xs text-muted">{{ a.email }}</span>
            <UBadge :label="`#${a.id}`" color="neutral" variant="subtle" size="sm" class="ms-auto shrink-0 font-mono" />
          </header>

          <UAlert v-if="a.auth_refused" color="error" variant="soft" icon="i-lucide-key-round" :title="t('app.diagnostics.authRefused')" :description="a.auth_refused" :ui="{ root: 'p-2.5', title: 'text-xs', description: 'text-xs' }" />

          <dl class="grid grid-cols-[6.5rem_1fr] gap-x-3 gap-y-2 text-[13px]">
            <dt class="text-muted">{{ t('app.diagnostics.connections') }}</dt>
            <dd class="flex flex-wrap gap-x-3 gap-y-1">
              <UTooltip v-for="c in a.connections" :key="c.purpose" :text="connectionTitle(c)">
                <span class="inline-flex items-center gap-1.5">
                  <span class="size-2 rounded-full" :class="connectionClass(c)" />
                  <span :class="c.open || c.busy ? 'text-default' : 'text-dimmed'">{{ t(`app.diagnostics.purpose.${c.purpose}`) }}</span>
                  <span v-if="c.open && c.idle_secs !== null" class="text-xs tabular-nums text-dimmed">{{ spanLabel(c.idle_secs) }}</span>
                </span>
              </UTooltip>
            </dd>

            <dt class="text-muted">{{ t('app.diagnostics.listener') }}</dt>
            <dd class="min-w-0">
              <div class="flex flex-wrap items-center gap-1.5">
                <UBadge
                  :label="a.listener ? t(`app.diagnostics.listenerState.${a.listener.state}`) : t(a.listening_task ? 'app.diagnostics.listenerState.starting' : 'app.diagnostics.listenerState.none')"
                  :color="listenerColor(a.listener)"
                  variant="subtle"
                  size="sm"
                />
                <span v-if="a.listener" class="text-xs text-muted">{{ t('app.diagnostics.since', { span: spanLabel((now - a.listener.since) / 1000) }) }}</span>
                <UBadge v-if="a.listener?.notify" label="NOTIFY" color="primary" variant="outline" size="sm" />
                <span v-if="a.listener?.retry_at" class="text-xs text-warning">{{ t('app.diagnostics.retryIn', { span: spanLabel((a.listener.retry_at - now) / 1000) }) }}</span>
                <span v-if="a.listener && a.listener.connections > 1" class="text-xs text-dimmed">{{ t('app.diagnostics.reconnections', { n: a.listener.connections - 1 }, a.listener.connections - 1) }}</span>
              </div>
              <p v-if="a.listener?.last_error" class="mt-1 text-xs break-words text-error">
                {{ a.listener.last_error }} <span class="text-dimmed">· {{ agoMs(a.listener.last_error_at ?? now, now) }}</span>
              </p>
            </dd>

            <dt class="text-muted">{{ t('app.diagnostics.sync') }}</dt>
            <dd class="min-w-0">
              <div v-if="a.sync.running_since" class="flex items-center gap-1.5 text-info">
                <UIcon name="i-lucide-loader-circle" class="size-3.5 animate-spin" />
                {{ t('app.diagnostics.syncRunning', { span: spanLabel((now - a.sync.running_since) / 1000), scope: t(`app.diagnostics.scope.${a.sync.running_scope}`) }) }}
                <span v-if="a.sync.running > 1" class="text-xs text-muted">(×{{ a.sync.running }})</span>
              </div>
              <div v-if="a.sync.last" class="flex flex-wrap items-center gap-x-1.5 text-default">
                <span>{{ t('app.diagnostics.syncLast', { ago: agoMs(a.sync.last.started + a.sync.last.ms, now) }) }}</span>
                <span class="tabular-nums" :class="a.sync.last.ms > 30_000 ? 'text-error' : a.sync.last.ms > 5000 ? 'text-warning' : 'text-muted'">{{ durationLabel(a.sync.last.ms) }}</span>
                <span class="text-muted">· {{ t(`app.diagnostics.scope.${a.sync.last.scope}`) }}</span>
                <span v-if="a.sync.last.new_messages" class="text-muted">· {{ t('app.diagnostics.newMessages', { n: a.sync.last.new_messages }, a.sync.last.new_messages) }}</span>
              </div>
              <p v-else-if="!a.sync.running_since" class="text-dimmed">{{ t('app.diagnostics.noSync') }}</p>
              <p v-if="a.sync.last?.error" class="mt-0.5 text-xs break-words text-error">{{ a.sync.last.error }}</p>
              <p class="mt-0.5 text-xs text-dimmed">{{ t('app.diagnostics.syncCount', { n: a.sync.count, failed: a.sync.failures }, a.sync.count) }}</p>
            </dd>

            <dt class="text-muted">{{ t('app.diagnostics.queue') }}</dt>
            <dd class="min-w-0">
              <div class="flex flex-wrap items-center gap-x-2">
                <span :class="a.queue.pending ? 'text-default' : 'text-dimmed'">{{ t('app.diagnostics.pending', { n: a.queue.pending }, a.queue.pending) }}</span>
                <span v-if="a.queue.failed" class="text-error">{{ t('app.diagnostics.failedOps', { n: a.queue.failed }, a.queue.failed) }}</span>
                <span v-if="a.queue.oldest_pending" class="text-xs text-muted">{{ t('app.diagnostics.oldest', { span: spanLabel(now / 1000 - a.queue.oldest_pending) }) }}</span>
                <UBadge v-if="a.queue.replaying" :label="t('app.diagnostics.replaying')" color="info" variant="subtle" size="sm" />
              </div>
              <p v-if="a.queue.last_error" class="mt-0.5 text-xs break-words text-warning">{{ a.queue.last_error }}</p>
            </dd>

            <template v-if="a.busy.length || a.calendar">
              <dt class="text-muted">{{ t('app.diagnostics.work') }}</dt>
              <dd class="flex flex-wrap items-center gap-1.5">
                <UBadge v-for="b in a.busy" :key="b" :label="t(`app.diagnostics.busy.${b}`)" color="info" variant="subtle" size="sm" />
                <span v-if="a.calendar?.ignored" class="text-xs text-dimmed">{{ t('app.diagnostics.calendarState.ignored') }}</span>
                <span v-else-if="a.calendar" class="text-xs" :class="a.calendar.state === 'ok' ? 'text-muted' : 'text-warning'" :title="a.calendar.message ?? undefined">
                  {{ t(`app.diagnostics.calendarState.${a.calendar.state}`) }}<template v-if="a.calendar.last_sync"> · {{ agoMs(a.calendar.last_sync * 1000, now) }}</template>
                  <template v-else-if="a.calendar.state === 'ok'"> · {{ t('app.diagnostics.calendarState.never') }}</template>
                </span>
              </dd>
            </template>
          </dl>
        </article>
      </div>
    </template>
  </div>
</template>
