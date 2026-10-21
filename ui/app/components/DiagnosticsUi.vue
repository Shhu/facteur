<script setup lang="ts">
import { clock, msLabel, num, tone } from '~/utils/diagnostics'

const { t } = useI18n()
const { samples, freezes } = usePerf()

const lag = computed(() => samples.value.map(s => ({ at: s.at, value: s.lag })))
const heap = computed(() => samples.value.filter(s => s.heap !== null).map(s => ({ at: s.at, value: s.heap! / 1048576 })))
const lagStats = computed(() => {
  const values = samples.value.map(s => s.lag).sort((a, b) => a - b)
  if (!values.length) return null
  return { p95: values[Math.min(values.length - 1, Math.floor(values.length * 0.95))]!, max: values.at(-1)! }
})
const freezeStats = computed(() => {
  const list = freezes.value
  return { n: list.length, total: list.reduce((n, f) => n + f.ms, 0), max: list.reduce((n, f) => Math.max(n, f.ms), 0) }
})
const mb = (v: number) => v.toLocaleString(intlLocale(), { maximumFractionDigits: 1 })
</script>

<template>
  <div class="h-full overflow-y-auto p-4 sm:p-5">
    <div class="grid gap-3 lg:grid-cols-2">
      <section class="rounded-xl border border-default bg-(--surface-list) p-3.5">
        <header class="mb-2 flex items-baseline gap-2">
          <h3 class="text-sm font-semibold text-highlighted">{{ t('app.diagnostics.ui.lag') }}</h3>
          <span v-if="lagStats" class="ms-auto text-xs text-muted tabular-nums">p95 {{ msLabel(lagStats.p95) }} ms · max {{ msLabel(lagStats.max) }} ms</span>
        </header>
        <DiagnosticsSparkline :points="lag" unit="ms" :format="msLabel" :threshold="50" />
      </section>
      <section class="rounded-xl border border-default bg-(--surface-list) p-3.5">
        <header class="mb-2 flex items-baseline gap-2">
          <h3 class="text-sm font-semibold text-highlighted">{{ t('app.diagnostics.ui.heap') }}</h3>
          <span v-if="heap.length" class="ms-auto text-xs text-muted tabular-nums">{{ mb(heap.at(-1)!.value) }} MB</span>
        </header>
        <DiagnosticsSparkline v-if="heap.length" :points="heap" unit="MB" :format="mb" />
        <p v-else class="py-6 text-center text-sm text-muted">{{ t('app.diagnostics.unavailable') }}</p>
      </section>
    </div>

    <section class="mt-4">
      <header class="mb-2 flex flex-wrap items-baseline gap-x-3">
        <h3 class="text-sm font-semibold text-highlighted">{{ t('app.diagnostics.ui.freezes') }}</h3>
        <span class="text-xs text-muted">{{ t('app.diagnostics.ui.freezesDetail', { n: num(freezeStats.n), total: msLabel(freezeStats.total), max: msLabel(freezeStats.max) }) }}</span>
      </header>
      <p v-if="!freezes.length" class="text-sm text-muted">{{ t('app.diagnostics.ui.noFreeze') }}</p>
      <div class="max-h-[50vh] overflow-y-auto rounded-xl border border-default" :class="!freezes.length && 'hidden'">
        <div v-for="(f, i) in freezes.slice().reverse()" :key="i" class="flex items-center gap-3 border-b border-default/60 px-3 py-1 font-mono text-xs tabular-nums last:border-b-0">
          <span class="text-dimmed">{{ clock(f.at, true) }}</span>
          <span class="h-1.5 rounded-full bg-warning/60" :style="{ width: `${Math.min(100, f.ms / 10)}%` }" />
          <span class="ms-auto" :class="tone(f.ms)">{{ msLabel(f.ms) }} ms</span>
        </div>
      </div>
    </section>
  </div>
</template>
