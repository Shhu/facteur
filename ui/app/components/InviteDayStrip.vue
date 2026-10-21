<script setup lang="ts">
import type { CalendarEvent } from '~/utils/types'

const props = defineProps<{ start: number, end: number, events: CalendarEvent[], band: string }>()
const { eventColor } = useCalendar()
const { t } = useI18n()

const HOUR = 3600
/** `t` (Unix seconds) on a whole hour of the local clock: the one before, or after (`up`).
 *  In a zone half an hour off UTC, ticks still fall on 14:00, not 14:30. */
function localHour(t: number, up: boolean): number {
  const d = new Date(t * 1000)
  const on = d.getMinutes() === 0 && d.getSeconds() === 0
  d.setMinutes(0, 0, 0)
  return unix(d) + (up && !on ? HOUR : 0)
}
const span = computed(() => {
  const dayFirst = dayStart(new Date(props.start * 1000))
  const day = unix(dayFirst)
  const from = Math.max(day, localHour(props.start - 2 * HOUR, false))
  const to = Math.min(unix(addDays(dayFirst, 1)) + HOUR, localHour(props.end + 2 * HOUR, true))
  return { from, to }
})
const pct = (t: number) => `${((Math.min(Math.max(t, span.value.from), span.value.to) - span.value.from) / (span.value.to - span.value.from)) * 100}%`
const width = (s: number, e: number) => `calc(${pct(e)} - ${pct(s)} - 2px)`

/** Events in sight, each in the first free lane (three at most). */
const lanes = computed(() => {
  const ends: number[] = []
  const out: { event: CalendarEvent, lane: number }[] = []
  for (const e of props.events.filter(e => e.end > span.value.from && e.start < span.value.to).sort((a, b) => a.start - b.start)) {
    let lane = ends.findIndex(end => end <= e.start)
    if (lane === -1) {
      if (ends.length >= 3) continue
      lane = ends.length
      ends.push(e.end)
    } else ends[lane] = e.end
    out.push({ event: e, lane })
  }
  return out
})
const laneCount = computed(() => Math.max(1, ...lanes.value.map(l => l.lane + 1)))
const ticks = computed(() => {
  const list: number[] = []
  for (let h = span.value.from; h <= span.value.to; h += HOUR) list.push(h)
  return list
})
const labelStep = computed(() => (ticks.value.length > 7 ? 2 : 1))
</script>

<template>
  <div class="flex flex-col gap-1" role="img" :aria-label="t('mail.invite.yourDay')">
    <div class="relative" :style="{ height: `${24 + laneCount * 20}px` }">
      <span v-for="h in ticks" :key="h" class="absolute inset-y-0 w-px bg-(--ui-border)" :style="{ left: pct(h) }" />
      <span
        class="absolute top-0 h-5.5 truncate rounded-md px-1.5 text-[11px] leading-5.5 font-semibold"
        :class="band"
        :style="{ left: pct(start), width: width(start, end) }"
      >{{ timeOf(start) }}</span>
      <span
        v-for="l in lanes"
        :key="`${l.event.id}-${l.event.start}`"
        class="cal-tint absolute h-4.5 truncate rounded-[5px] border-s-3 border-(--ev) px-1 text-[10.5px] leading-4.5 font-medium text-highlighted"
        :style="{ top: `${26 + l.lane * 20}px`, left: pct(l.event.start), width: width(l.event.start, l.event.end), '--ev': eventColor(l.event) }"
        :title="`${eventTime(l.event)} · ${l.event.summary}`"
      >{{ l.event.summary || t('tools.calendar.untitled') }}</span>
    </div>
    <div class="relative h-3.5 text-[10.5px] tabular-nums text-dimmed">
      <span
        v-for="(h, i) in ticks"
        v-show="i % labelStep === 0 || i === ticks.length - 1"
        :key="h"
        class="absolute"
        :class="i === 0 ? '' : i === ticks.length - 1 ? '-translate-x-full' : '-translate-x-1/2'"
        :style="{ left: pct(h) }"
      >{{ timeOf(h) }}</span>
    </div>
  </div>
</template>
