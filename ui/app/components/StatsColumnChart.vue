<script setup lang="ts">
/** Colours come from the --viz-* variables (an ancestor must carry `.viz-root`); every text uses
 *  the text colours, never a series colour. */
export interface StatsSeries {
  name: string
  color: string
  values: number[]
}

const props = withDefaults(defineProps<{
  categories: string[]
  /** Full titles for the tooltip; the labels otherwise. */
  titles?: string[]
  series: StatsSeries[]
  height?: number
  yAxis?: boolean
  labelEvery?: number
  labelLast?: boolean
  barMax?: number
  fill?: number
  peak?: boolean
  summary: string
}>(), { titles: undefined, height: 200, yAxis: false, labelEvery: 1, labelLast: false, barMax: 18, fill: 0.72, peak: false })

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

const AXIS_H = 22
const M = computed(() => ({ top: props.peak ? 20 : 6, right: 0, bottom: AXIS_H, left: props.yAxis ? 34 : 0 }))
const innerW = computed(() => Math.max(0, width.value - M.value.left - M.value.right))
const innerH = computed(() => props.height - M.value.top - M.value.bottom)
const base = computed(() => M.value.top + innerH.value)

/** "Round" step: 1, 2, 2.5 or 5 × 10ⁿ. */
function niceStep(raw: number) {
  const pow = 10 ** Math.floor(Math.log10(raw))
  const f = raw / pow
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10) * pow
}
const scale = computed(() => {
  const max = Math.max(1, ...props.series.flatMap(s => s.values))
  const step = max <= 4 ? 1 : niceStep(max / 4)
  const top = Math.ceil(max / step) * step
  const ticks: number[] = []
  for (let v = 0; v <= top + step / 2; v += step) ticks.push(v)
  // Without value labels, a couple of guide lines are enough.
  return { top, ticks: props.yAxis ? ticks : ticks.filter((_, i) => i === 0 || i === ticks.length - 1 || (ticks.length > 3 && i === Math.floor((ticks.length - 1) / 2))) }
})
const y = (v: number) => base.value - (v / scale.value.top) * innerH.value

const compact = computed(() => new Intl.NumberFormat(intlLocale(), { notation: 'compact', maximumFractionDigits: 1 }))
const full = computed(() => new Intl.NumberFormat(intlLocale()))
const { t } = useI18n()

const GAP = 2
const band = computed(() => innerW.value / Math.max(1, props.categories.length))
const barW = computed(() => {
  const k = props.series.length
  const room = Math.min(band.value * props.fill, band.value - 2) - (k - 1) * GAP
  return Math.max(2, Math.min(props.barMax, room / k))
})
function groupX(i: number) {
  const k = props.series.length
  const groupW = k * barW.value + (k - 1) * GAP
  return M.value.left + i * band.value + (band.value - groupW) / 2
}

function column(x: number, value: number) {
  const top = y(value)
  const h = base.value - top
  if (h <= 0) return ''
  const w = barW.value
  const r = Math.min(4, w / 2, h)
  return `M${x},${base.value}V${top + r}Q${x},${top} ${x + r},${top}H${x + w - r}Q${x + w},${top} ${x + w},${top + r}V${base.value}Z`
}

/** Columns holding the highest value of the first series (several on a tie). */
const peaks = computed(() => {
  const values = props.series[0]?.values ?? []
  const max = Math.max(0, ...values)
  return new Set(props.peak && max > 0 ? values.flatMap((v, i) => (v === max ? [i] : [])) : [])
})
/** Sparse labels (hours) sit at the start of their band, the last one at the end. */
function labelX(i: number) {
  if (props.labelEvery === 1) return M.value.left + (i + 0.5) * band.value
  return M.value.left + (i === props.categories.length - 1 ? i + 1 : i) * band.value
}
function hoverBand(i: number) {
  const x0 = M.value.left + i * band.value
  const x1 = x0 + band.value
  const top = M.value.top
  const r = Math.min(8, band.value / 2)
  return `M${x0},${base.value}V${top + r}Q${x0},${top} ${x0 + r},${top}H${x1 - r}Q${x1},${top} ${x1},${top + r}V${base.value}Z`
}
const showLabel = (i: number) => i % props.labelEvery === 0 || (props.labelLast && i === props.categories.length - 1)

const active = ref<number | null>(null)
function onEnter(e: PointerEvent, i: number) {
  if (e.pointerType === 'mouse') active.value = i
}
function onLeave(e: PointerEvent) {
  // Touch: the tooltip stays until another tap, it would vanish as soon as the finger lifts.
  if (e.pointerType === 'mouse') active.value = null
}
function onTap(e: PointerEvent, i: number) {
  if (e.pointerType === 'mouse') return
  active.value = active.value === i ? null : i
}
const tip = useTemplateRef('tip')
const tipW = ref(140)
watch(active, async () => {
  await nextTick()
  if (tip.value) tipW.value = tip.value.offsetWidth
})
const tipLeft = computed(() => {
  if (active.value === null) return 0
  const center = M.value.left + (active.value + 0.5) * band.value
  const half = tipW.value / 2 + 4
  return Math.min(Math.max(center, half), Math.max(half, width.value - half))
})
const tipBottom = computed(() => {
  if (active.value === null) return 0
  const i = active.value
  const max = Math.max(0, ...props.series.map(s => s.values[i] ?? 0))
  return props.height - y(max) + (peaks.value.has(i) ? 26 : 10)
})
</script>

<template>
  <div ref="root" class="relative" :style="{ height: `${height}px` }">
    <svg v-if="width" :width="width" :height="height" role="img" :aria-label="summary" class="block overflow-visible">
      <g>
        <template v-for="tick in scale.ticks" :key="tick">
          <line
            :x1="M.left"
            :x2="width - M.right"
            :y1="Math.round(y(tick)) + 0.5"
            :y2="Math.round(y(tick)) + 0.5"
            :stroke="tick === 0 ? 'var(--viz-axis)' : 'var(--viz-grid)'"
            stroke-width="1"
          />
          <text v-if="yAxis" :x="M.left - 10" :y="y(tick)" text-anchor="end" dominant-baseline="middle" class="stats-tick">{{ compact.format(tick) }}</text>
        </template>
      </g>
      <path v-if="active !== null" :d="hoverBand(active)" fill="var(--viz-hover)" />
      <g v-for="(s, si) in series" :key="s.name">
        <path
          v-for="(v, i) in s.values"
          :key="i"
          :d="column(groupX(i) + si * (barW + GAP), v)"
          :fill="s.color"
        />
      </g>
      <template v-for="i in peaks" :key="`peak${i}`">
        <text
          :x="M.left + (i + 0.5) * band"
          :y="y(series[0]!.values[i]!) - 6"
          text-anchor="middle"
          class="stats-peak"
        >{{ full.format(series[0]!.values[i]!) }}</text>
      </template>
      <template v-for="(c, i) in categories" :key="c + i">
        <text
          v-if="showLabel(i)"
          :x="labelX(i)"
          :y="height - 5"
          :text-anchor="labelEvery > 1 ? (i === categories.length - 1 ? 'end' : 'start') : 'middle'"
          :class="peaks.has(i) ? 'stats-label-peak' : 'stats-label'"
        >{{ c }}</text>
      </template>
      <!-- Hit areas: the whole band, wider than the column -->
      <rect
        v-for="(c, i) in categories"
        :key="`hit${i}`"
        :x="M.left + i * band"
        :y="0"
        :width="band"
        :height="height"
        fill="transparent"
        tabindex="0"
        class="cursor-default outline-none"
        :aria-label="t('tools.chart.point', { title: titles?.[i] ?? c, values: series.map(s => `${s.name} ${full.format(s.values[i] ?? 0)}`).join(', ') })"
        @pointerenter="onEnter($event, i)"
        @pointerleave="onLeave"
        @pointerdown="onTap($event, i)"
        @focus="active = i"
        @blur="active = null"
      />
    </svg>

    <div
      v-if="active !== null"
      ref="tip"
      class="pointer-events-none absolute z-10 flex -translate-x-1/2 flex-col gap-1.5 whitespace-nowrap rounded-[10px] border border-default bg-default px-3 py-2.5 text-[12.5px] shadow-lg dark:bg-elevated"
      :style="{ left: `${tipLeft}px`, bottom: `${tipBottom}px` }"
    >
      <p class="font-semibold text-highlighted first-letter:uppercase">{{ titles?.[active] ?? categories[active] }}</p>
      <p v-for="s in series" :key="s.name" class="flex items-center gap-2 text-default">
        <span class="inline-block size-2.25 shrink-0 rounded-[3px]" :style="{ background: s.color }" />
        {{ s.name }}
        <span class="ms-auto ps-3 font-semibold tabular-nums text-highlighted">{{ full.format(s.values[active] ?? 0) }}</span>
      </p>
    </div>
  </div>
</template>

<style scoped>
.stats-tick,
.stats-label,
.stats-label-peak {
  fill: var(--ui-text-muted);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
}
.stats-label-peak {
  fill: var(--ui-text-highlighted);
  font-weight: 700;
}
.stats-peak {
  fill: var(--ui-text-highlighted);
  font-size: 12px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}
</style>
