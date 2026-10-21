<script setup lang="ts">
import { clock, commandStats, msLabel, num, tone } from '~/utils/diagnostics'
import type { CommandStat } from '~/utils/diagnostics'

const { t } = useI18n()
const { commands } = usePerf()

const filter = ref('')
const slowOnly = ref(false)
/** Slow: 95th percentile above this (ms). */
const SLOW = 150

type Key = keyof Pick<CommandStat, 'command' | 'n' | 'p50' | 'p95' | 'max' | 'last'>
const sortKey = ref<Key>('p95')
const descending = ref(true)
function sortBy(key: Key) {
  if (sortKey.value === key) descending.value = !descending.value
  else {
    sortKey.value = key
    descending.value = key !== 'command'
  }
}

const matches = (command: string) => !filter.value.trim() || command.toLowerCase().includes(filter.value.trim().toLowerCase())
const stats = computed(() => {
  const key = sortKey.value
  const sign = descending.value ? -1 : 1
  return commandStats(commands.value)
    .filter(s => matches(s.command) && (!slowOnly.value || s.p95 > SLOW))
    .sort((a, b) => sign * (key === 'command' ? a.command.localeCompare(b.command) : a[key] - b[key]))
})
const recent = computed(() => commands.value.filter(c => matches(c.command) && (!slowOnly.value || c.ms > SLOW)).slice(-200).reverse())

const columns: { key: Key, label: string }[] = [
  { key: 'command', label: 'app.diagnostics.commands.command' },
  { key: 'n', label: 'app.diagnostics.commands.n' },
  { key: 'p50', label: 'p50' },
  { key: 'p95', label: 'p95' },
  { key: 'max', label: 'app.diagnostics.commands.max' },
  { key: 'last', label: 'app.diagnostics.commands.last' }
]
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div class="flex flex-wrap items-center gap-3 border-b border-default px-4 py-2.5 sm:px-5">
      <UInput v-model="filter" icon="i-lucide-search" size="sm" :placeholder="t('app.diagnostics.commands.filter')" :aria-label="t('app.diagnostics.commands.filter')" class="w-64 max-w-full" />
      <USwitch v-model="slowOnly" size="sm" :label="t('app.diagnostics.commands.slowOnly', { ms: SLOW })" />
      <span class="ms-auto text-xs text-muted">{{ t('app.diagnostics.commands.kept', { n: num(commands.length) }, commands.length) }}</span>
    </div>
    <p v-if="!commands.length" class="p-5 text-sm text-muted">{{ t('app.diagnostics.commands.none') }}</p>
    <div v-else class="grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)_minmax(0,1fr)] lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] lg:grid-rows-1">
      <div class="min-h-0 overflow-auto border-default max-lg:border-b lg:border-e">
        <table class="w-full font-mono text-xs tabular-nums">
          <thead class="sticky top-0 bg-(--surface-card)">
            <tr class="text-left text-muted">
              <th v-for="c in columns" :key="c.key" class="px-3 py-2 font-medium" :class="c.key !== 'command' && 'text-right'">
                <button type="button" class="inline-flex items-center gap-1 hover:text-highlighted" @click="sortBy(c.key)">
                  {{ c.label.includes('.') ? t(c.label) : c.label }}
                  <UIcon v-if="sortKey === c.key" :name="descending ? 'i-lucide-arrow-down' : 'i-lucide-arrow-up'" class="size-3" />
                </button>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in stats" :key="s.command" class="cursor-pointer border-t border-default/60 hover:bg-elevated/60" @click="filter = s.command">
              <td class="max-w-0 truncate px-3 py-1.5 text-default" :title="s.command">
                {{ s.command }}
                <span v-if="s.failed" class="ms-1 text-error">{{ t('app.diagnostics.commands.failed', { n: s.failed }, s.failed) }}</span>
              </td>
              <td class="px-3 py-1.5 text-right text-muted">{{ num(s.n) }}</td>
              <td class="px-3 py-1.5 text-right" :class="tone(s.p50)">{{ msLabel(s.p50) }}</td>
              <td class="px-3 py-1.5 text-right" :class="tone(s.p95)">{{ msLabel(s.p95) }}</td>
              <td class="px-3 py-1.5 text-right" :class="tone(s.max)">{{ msLabel(s.max) }}</td>
              <td class="px-3 py-1.5 text-right" :class="tone(s.last)" :title="clock(s.lastAt)">{{ msLabel(s.last) }}</td>
            </tr>
          </tbody>
        </table>
        <p v-if="!stats.length" class="p-4 text-sm text-muted">{{ t('app.diagnostics.nothingMatches') }}</p>
      </div>
      <div class="min-h-0 overflow-auto">
        <p class="sticky top-0 bg-(--surface-card) px-3 py-2 text-xs font-medium text-muted">{{ t('app.diagnostics.commands.recent') }}</p>
        <div v-for="(c, i) in recent" :key="i" class="flex gap-3 px-3 py-0.5 font-mono text-xs tabular-nums">
          <span class="text-dimmed">{{ clock(c.at, true) }}</span>
          <span class="min-w-0 flex-1 truncate" :class="c.ok ? 'text-default' : 'text-error'">{{ c.command }}</span>
          <span :class="tone(c.ms)">{{ msLabel(c.ms) }}</span>
        </div>
      </div>
    </div>
  </div>
</template>
