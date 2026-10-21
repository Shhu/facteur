<script setup lang="ts">
import { clock } from '~/utils/diagnostics'

const props = defineProps<{
  points: { at: number, value: number }[]
  unit: string
  format: (v: number) => string
  threshold?: number
}>()

const W = 600
const H = 120
const PAD = 4

const range = computed(() => {
  const values = props.points.map(p => p.value)
  const max = Math.max(props.threshold ?? 0, ...values, 1)
  const first = props.points[0]?.at ?? 0
  const last = props.points.at(-1)?.at ?? first + 1
  return { max: max * 1.1, first, span: Math.max(1, last - first) }
})
const x = (at: number) => ((at - range.value.first) / range.value.span) * W
const y = (v: number) => H - PAD - (v / range.value.max) * (H - PAD * 2)
const path = computed(() => props.points.map((p, i) => `${i ? 'L' : 'M'}${x(p.at).toFixed(1)},${y(p.value).toFixed(1)}`).join(''))
const area = computed(() => (props.points.length > 1 ? `${path.value}L${W},${H}L0,${H}Z` : ''))

const hovered = ref<number | null>(null)
function onMove(e: PointerEvent) {
  const box = (e.currentTarget as SVGElement).getBoundingClientRect()
  const at = range.value.first + ((e.clientX - box.left) / box.width) * range.value.span
  let best = 0
  props.points.forEach((p, i) => {
    if (Math.abs(p.at - at) < Math.abs(props.points[best]!.at - at)) best = i
  })
  hovered.value = props.points.length ? best : null
}
const point = computed(() => (hovered.value === null ? null : props.points[hovered.value] ?? null))
</script>

<template>
  <div class="relative">
    <svg
      :viewBox="`0 0 ${W} ${H}`"
      preserveAspectRatio="none"
      class="h-28 w-full overflow-visible text-primary"
      role="img"
      @pointermove="onMove"
      @pointerleave="hovered = null"
    >
      <line v-if="threshold" x1="0" :x2="W" :y1="y(threshold)" :y2="y(threshold)" class="stroke-(--ui-border-accented)" stroke-dasharray="4 4" vector-effect="non-scaling-stroke" />
      <path v-if="area" :d="area" fill="currentColor" fill-opacity="0.1" />
      <path :d="path" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" vector-effect="non-scaling-stroke" />
      <line v-if="point" :x1="x(point.at)" :x2="x(point.at)" y1="0" :y2="H" class="stroke-(--ui-border-accented)" vector-effect="non-scaling-stroke" />
    </svg>
    <span
      v-if="point"
      class="pointer-events-none absolute top-0 rounded-md bg-inverted px-1.5 py-0.5 font-mono text-[11px] whitespace-nowrap text-inverted tabular-nums"
      :class="x(point.at) > W / 2 ? '-translate-x-full' : ''"
      :style="{ left: `${(x(point.at) / W) * 100}%` }"
    >{{ clock(point.at) }} · {{ format(point.value) }} {{ unit }}</span>
  </div>
</template>
