<script setup lang="ts">
import type { PerfRecord } from '~/composables/usePerf'
import { clock, engineGroups, msLabel, num, tone } from '~/utils/diagnostics'

const { t } = useI18n()
const { engine } = usePerf()

const KINDS: PerfRecord['kind'][] = ['sql', 'wait', 'hold', 'imap']
const shown = ref<Set<PerfRecord['kind']>>(new Set(KINDS))
function toggle(kind: PerfRecord['kind']) {
  const next = new Set(shown.value)
  if (next.has(kind)) next.delete(kind)
  else next.add(kind)
  shown.value = next
}
const filter = ref('')
type Sort = 'total' | 'max' | 'count' | 'lastAt'
const sort = ref<Sort>('total')
const sorts: { value: Sort, label: string }[] = [
  { value: 'total', label: 'app.diagnostics.engine.total' },
  { value: 'max', label: 'app.diagnostics.engine.max' },
  { value: 'count', label: 'app.diagnostics.engine.count' },
  { value: 'lastAt', label: 'app.diagnostics.engine.latest' }
]

const counts = computed(() => Object.fromEntries(KINDS.map(k => [k, engine.value.filter(e => e.kind === k).length])) as Record<PerfRecord['kind'], number>)
const groups = computed(() => {
  const q = filter.value.trim().toLowerCase()
  return engineGroups(engine.value)
    .filter(g => shown.value.has(g.kind) && (!q || g.what.toLowerCase().includes(q)))
    .sort((a, b) => b[sort.value] - a[sort.value])
})
const opened = ref<string | null>(null)

const kindClass: Record<PerfRecord['kind'], string> = {
  sql: 'text-sky-600 dark:text-sky-400',
  wait: 'text-error',
  hold: 'text-warning',
  imap: 'text-primary'
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div class="flex flex-wrap items-center gap-2 border-b border-default px-4 py-2.5 sm:px-5">
      <UButton
        v-for="k in KINDS"
        :key="k"
        :label="`${t(`app.diagnostics.engine.kind.${k}`)} · ${num(counts[k])}`"
        size="xs"
        :color="shown.has(k) ? 'primary' : 'neutral'"
        :variant="shown.has(k) ? 'soft' : 'outline'"
        @click="toggle(k)"
      />
      <UInput v-model="filter" icon="i-lucide-search" size="sm" :placeholder="t('app.diagnostics.engine.filter')" :aria-label="t('app.diagnostics.engine.filter')" class="w-64 max-w-full" />
      <div class="ms-auto flex items-center gap-1 text-xs text-muted">
        <span>{{ t('app.diagnostics.engine.sort') }}</span>
        <UButton v-for="s in sorts" :key="s.value" :label="t(s.label)" size="xs" color="neutral" :variant="sort === s.value ? 'soft' : 'ghost'" @click="sort = s.value" />
      </div>
    </div>
    <div class="min-h-0 flex-1 overflow-y-auto">
      <p v-if="!engine.length" class="p-5 text-sm text-muted">{{ t('app.diagnostics.engine.none') }}</p>
      <p v-else-if="!groups.length" class="p-5 text-sm text-muted">{{ t('app.diagnostics.nothingMatches') }}</p>
      <div v-for="g in groups" :key="g.key" class="border-b border-default/60">
        <button
          type="button"
          class="grid w-full grid-cols-[3rem_minmax(0,1fr)_auto] items-start gap-3 px-4 py-2 text-left font-mono text-xs hover:bg-elevated/60 sm:px-5"
          :aria-expanded="opened === g.key"
          @click="opened = opened === g.key ? null : g.key"
        >
          <span class="font-bold" :class="kindClass[g.kind]">{{ g.kind }}</span>
          <span class="break-words text-default" :class="opened === g.key ? '' : 'line-clamp-2'">{{ g.what }}</span>
          <span class="flex gap-3 text-right tabular-nums whitespace-nowrap">
            <span class="text-muted" :title="t('app.diagnostics.engine.count')">×{{ num(g.count) }}</span>
            <span :class="tone(g.total)" :title="t('app.diagnostics.engine.total')">Σ {{ msLabel(g.total) }}</span>
            <span :class="tone(g.max)" :title="t('app.diagnostics.engine.max')">↑ {{ msLabel(g.max) }}</span>
            <span class="w-16 text-dimmed">{{ clock(g.lastAt) }}</span>
          </span>
        </button>
        <div v-if="opened === g.key" class="max-h-64 overflow-y-auto bg-elevated/40 px-4 pb-2 sm:px-5">
          <div v-for="(e, i) in g.events" :key="i" class="flex gap-3 py-0.5 ps-[3.75rem] font-mono text-xs tabular-nums">
            <span class="text-dimmed">{{ clock(e.at, true) }}</span>
            <span :class="tone(e.ms)">{{ msLabel(e.ms) }} ms</span>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
