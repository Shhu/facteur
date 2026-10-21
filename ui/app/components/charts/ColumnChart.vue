<script setup lang="ts">
export interface ColumnSeries {
  name: string
  color: string
  values: number[]
}

const props = withDefaults(defineProps<{
  categories: string[]
  /** Full titles for the tooltip; the labels otherwise. */
  titles?: string[]
  series: ColumnSeries[]
  height?: number
  labelEvery?: number
  summary: string
}>(), { height: 180, labelEvery: 1, titles: undefined })

const root = useTemplateRef('root')
const width = ref(0)
let observer: ResizeObserver | undefined
onMounted(() => {
  observer = new ResizeObserver(([entry]) => {
    width.value = entry?.contentRect.width ?? 0
  })
  if (root.value) observer.observe(root.value)
})
onBeforeUnmount(() => observer?.disconnect())

const M = { top: 8, right: 4, bottom: 20, left: 34 }
const innerW = computed(() => Math.max(0, width.value - M.left - M.right))
const innerH = computed(() => props.height - M.top - M.bottom)

/** "Round" step: 1, 2, 2.5 or 5 × 10ⁿ. */
function niceStep(raw: number) {
  const pow = 10 ** Math.floor(Math.log10(raw))
  const f = raw / pow
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10) * pow
}
const scale = computed(() => {
  const max = Math.max(1, ...props.series.flatMap(s => s.values))
  const step = max < 4 ? 1 : niceStep(max / 4)
  const top = Math.ceil(max / step) * step
  const ticks: number[] = []
  for (let v = 0; v <= top + step / 2; v += step) ticks.push(v)
  return { top, ticks }
})
const y = (v: number) => M.top + innerH.value - (v / scale.value.top) * innerH.value

const { t } = useI18n()
const compact = computed(() => new Intl.NumberFormat(intlLocale(), { notation: 'compact', maximumFractionDigits: 1 }))
const full = computed(() => new Intl.NumberFormat(intlLocale()))

const band = computed(() => innerW.value / Math.max(1, props.categories.length))
const GAP = 2
const barW = computed(() => {
  const k = props.series.length
  return Math.max(2, Math.min(24, (band.value * 0.72 - (k - 1) * GAP) / k))
})

function column(x: number, value: number) {
  const base = M.top + innerH.value
  const top = y(value)
  const h = base - top
  if (h <= 0) return ''
  const w = barW.value
  const r = Math.min(4, w / 2, h)
  return `M${x},${base}V${top + r}Q${x},${top} ${x + r},${top}H${x + w - r}Q${x + w},${top} ${x + w},${top + r}V${base}Z`
}

function groupX(i: number) {
  const k = props.series.length
  const groupW = k * barW.value + (k - 1) * GAP
  return M.left + i * band.value + (band.value - groupW) / 2
}

const active = ref<number | null>(null)
const tipLeft = computed(() => {
  if (active.value === null) return 0
  const center = M.left + (active.value + 0.5) * band.value
  // Keeps the tooltip inside the frame.
  return Math.min(Math.max(center, 70), Math.max(70, width.value - 70))
})
</script>

<template>
  <div class="viz-root">
    <div v-if="series.length > 1" class="mb-2 flex flex-wrap gap-4 text-xs text-muted">
      <span v-for="s in series" :key="s.name" class="flex items-center gap-1.5">
        <span class="inline-block size-2.5 rounded-xs" :style="{ background: s.color }" />
        {{ s.name }}
      </span>
    </div>

    <div ref="root" class="relative" :style="{ height: `${height}px` }">
      <svg v-if="width" :width="width" :height="height" role="img" :aria-label="summary" class="block">
        <g>
          <template v-for="tick in scale.ticks" :key="tick">
            <line :x1="M.left" :x2="width - M.right" :y1="y(tick)" :y2="y(tick)" :stroke="tick === 0 ? 'var(--viz-axis)' : 'var(--viz-grid)'" stroke-width="1" shape-rendering="crispEdges" />
            <text :x="M.left - 6" :y="y(tick)" text-anchor="end" dominant-baseline="middle" class="viz-tick">{{ compact.format(tick) }}</text>
          </template>
        </g>
        <rect
          v-if="active !== null"
          :x="M.left + active * band"
          :y="M.top"
          :width="band"
          :height="innerH"
          fill="var(--viz-hover)"
        />
        <g v-for="(s, si) in series" :key="s.name">
          <path
            v-for="(v, i) in s.values"
            :key="i"
            :d="column(groupX(i) + si * (barW + GAP), v)"
            :fill="s.color"
          />
        </g>
        <template v-for="(c, i) in categories" :key="c + i">
          <text
            v-if="i % labelEvery === 0"
            :x="M.left + (i + 0.5) * band"
            :y="height - 4"
            text-anchor="middle"
            class="viz-tick"
          >{{ c }}</text>
        </template>
        <!-- Hit areas: the whole band, wider than the column -->
        <rect
          v-for="(c, i) in categories"
          :key="`hit${i}`"
          :x="M.left + i * band"
          :y="M.top"
          :width="band"
          :height="innerH"
          fill="transparent"
          tabindex="0"
          class="outline-none"
          :aria-label="t('tools.chart.point', { title: titles?.[i] ?? c, values: series.map(s => `${s.name} ${full.format(s.values[i] ?? 0)}`).join(', ') })"
          @pointerenter="active = i"
          @pointerleave="active = null"
          @focus="active = i"
          @blur="active = null"
        />
      </svg>

      <div
        v-if="active !== null"
        class="pointer-events-none absolute top-0 z-10 -translate-x-1/2 rounded-md border border-default bg-default px-2.5 py-1.5 text-xs shadow-md"
        :style="{ left: `${tipLeft}px` }"
      >
        <p class="mb-1 text-muted">{{ titles?.[active] ?? categories[active] }}</p>
        <p v-for="s in series" :key="s.name" class="flex items-center gap-2 whitespace-nowrap">
          <span class="inline-block h-0.5 w-3 rounded-full" :style="{ background: s.color }" />
          <span class="font-semibold text-highlighted">{{ full.format(s.values[active] ?? 0) }}</span>
          <span class="text-muted">{{ s.name }}</span>
        </p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.viz-tick {
  fill: var(--viz-muted);
  font-size: 10px;
  font-variant-numeric: tabular-nums;
}
</style>
