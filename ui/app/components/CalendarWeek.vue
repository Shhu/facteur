<script setup lang="ts">
import { useElementSize } from '@vueuse/core'
import type { TimedBlock } from '~/utils/calendar'
import type { CalendarEvent } from '~/utils/types'

const props = defineProps<{ days: Date[], events: CalendarEvent[], selected: number | null }>()
const emit = defineEmits<{ select: [event: CalendarEvent, el: HTMLElement], pick: [day: Date], create: [start: Date] }>()
const { eventColor } = useCalendar()
const now = useNow()
const { t } = useI18n()

const scroller = useTemplateRef<HTMLElement>('scroller')
const head = useTemplateRef<HTMLElement>('head')
const { height: scrollHeight } = useElementSize(scroller)
const { height: headHeight } = useElementSize(head)
/** 7:00 to 21:00 fill what is left under the headers, within reason. */
const hourH = computed(() => Math.round(Math.min(72, Math.max(40, (scrollHeight.value - headHeight.value) / 14))))

const cols = computed(() => ({ gridTemplateColumns: `3.5rem repeat(${props.days.length}, minmax(0, 1fr))` }))
const today = computed(() => dayStart(new Date(now.value)))
const nowMinutes = computed(() => {
  const d = new Date(now.value)
  return d.getHours() * 60 + d.getMinutes()
})

const blocks = computed(() => props.days.map(d => layoutDay(props.events, d, 22)))
const band = computed(() => layoutBand(props.events, props.days))
const lanes = computed(() => Math.max(0, ...band.value.map(b => b.lane + 1)))

const hourLabel = (h: number) => {
  const twelve = dateFormat({ hour: 'numeric' }).resolvedOptions().hour12
  return dateFormat(twelve ? { hour: 'numeric' } : { hour: '2-digit', minute: '2-digit' }).format(new Date(2026, 0, 1, h))
}
const weekday = (d: Date) => dateFormat({ weekday: props.days.length > 1 ? 'short' : 'long' }).format(d)
const isWeekend = (d: Date) => d.getDay() === 0 || d.getDay() === 6
const past = (e: CalendarEvent) => e.end * 1000 < now.value

// Opens at 7:00, or lower when the current hour would be out of sight.
let scrolled = false
watch([hourH, () => props.days.length], () => {
  const el = scroller.value
  if (!el || !scrollHeight.value || scrolled) return
  scrolled = true
  const visible = (scrollHeight.value - headHeight.value) / hourH.value
  const nowHour = nowMinutes.value / 60
  const showsToday = props.days.some(d => sameDay(d, today.value))
  const top = showsToday && (nowHour > 7 + visible - 1 || nowHour < 7) ? Math.max(0, nowHour - visible / 3) : 7
  // A little above the hour, so that its label shows whole.
  el.scrollTop = Math.max(0, top * hourH.value - 10)
}, { flush: 'post' })

const select = (e: CalendarEvent, ev: MouseEvent) => emit('select', e, ev.currentTarget as HTMLElement)
/** A click on a free spot starts an event there, on the half hour above. */
function createAt(d: Date, ev: MouseEvent) {
  const minutes = Math.floor(((ev.offsetY / hourH.value) * 60) / 30) * 30
  emit('create', new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, Math.min(minutes, 23 * 60 + 30)))
}
const blockStyle = (b: TimedBlock) => ({
  top: `${(b.top / 60) * hourH.value}px`,
  height: `${Math.max(18, ((b.bottom - b.top) / 60) * hourH.value - 2)}px`,
  left: `calc(${(b.col / b.cols) * 100}% + 2px)`,
  width: `calc(${(b.span / b.cols) * 100}% - 5px)`,
  '--ev': eventColor(b.event)
})
const blockHeight = (b: TimedBlock) => ((b.bottom - b.top) / 60) * hourH.value
/** Room for the title and the time on their own lines (the title on two when there is more). */
const tall = (b: TimedBlock) => blockHeight(b) >= 36
</script>

<template>
  <div ref="scroller" class="relative h-full min-h-0 overflow-y-auto">
    <div ref="head" class="sticky top-0 z-20 border-b border-default bg-(--surface-card)">
      <div class="grid" :style="cols">
        <span />
        <button
          v-for="d in days"
          :key="dayKey(d)"
          type="button"
          class="flex min-w-0 flex-col items-center gap-0.5 border-s border-default pt-2 pb-1.5 transition-colors hover:bg-elevated/50"
          :aria-label="dayHeading(d, today)"
          :disabled="days.length === 1"
          @click="emit('pick', d)"
        >
          <span class="truncate text-[11px] font-semibold uppercase tracking-wide" :class="sameDay(d, today) ? 'text-primary' : 'text-muted'">{{ weekday(d) }}</span>
          <span
            class="flex size-8 items-center justify-center rounded-full text-[17px] font-semibold tabular-nums"
            :class="sameDay(d, today) ? 'bg-primary text-inverted' : 'text-highlighted'"
          >{{ d.getDate() }}</span>
        </button>
      </div>
      <div
        v-if="lanes"
        class="grid pb-1"
        :style="{ ...cols, gridTemplateRows: `repeat(${lanes}, 1.625rem)` }"
      >
        <span class="pe-2 pt-1 text-end text-[10.5px] text-dimmed" :style="{ gridColumn: 1, gridRow: 1 }">{{ t('tools.calendar.allDayShort') }}</span>
        <span v-for="(d, i) in days" :key="dayKey(d)" class="border-s border-default" :style="{ gridColumn: i + 2, gridRow: `1 / ${lanes + 1}` }" />
        <button
          v-for="b in band"
          :key="`${b.event.id}-${b.event.start}`"
          type="button"
          class="cal-tint relative z-1 mx-0.75 my-0.5 flex min-w-0 items-center gap-1 rounded-md border-s-3 border-(--ev) px-1.5 text-start text-xs font-medium text-highlighted transition-shadow hover:shadow-sm"
          :class="[selected === b.event.id && 'ring-2 ring-(--ev)', b.event.status === 'cancelled' && 'line-through opacity-60', past(b.event) && 'opacity-70']"
          :style="{ gridColumn: `${b.from + 2} / ${b.to + 3}`, gridRow: b.lane + 1, '--ev': eventColor(b.event) }"
          @click="select(b.event, $event)"
        >
          <UIcon v-if="b.continuesBefore" name="i-lucide-chevron-left" class="size-3 shrink-0 text-muted" />
          <span class="truncate">{{ b.event.summary || t('tools.calendar.untitled') }}</span>
          <UIcon v-if="b.continuesAfter" name="i-lucide-chevron-right" class="ms-auto size-3 shrink-0 text-muted" />
        </button>
      </div>
    </div>

    <div class="relative grid" :style="{ ...cols, height: `${24 * hourH}px` }">
      <div class="relative">
        <span
          v-for="h in 23"
          :key="h"
          class="absolute end-2 -translate-y-1/2 text-[11px] tabular-nums text-dimmed"
          :style="{ top: `${h * hourH}px` }"
        >{{ hourLabel(h) }}</span>
      </div>
      <div
        v-for="(d, i) in days"
        :key="dayKey(d)"
        class="relative border-s border-default"
        :class="isWeekend(d) && days.length > 1 ? 'bg-elevated/30' : ''"
        :style="{ backgroundImage: 'linear-gradient(to bottom, var(--ui-border) 1px, transparent 1px)', backgroundSize: `100% ${hourH}px` }"
        @click.self="createAt(d, $event)"
      >
        <button
          v-for="b in blocks[i]"
          :key="`${b.event.id}-${b.event.start}`"
          type="button"
          class="cal-tint absolute flex flex-col overflow-hidden rounded-md border-s-3 border-(--ev) px-1.5 text-start text-xs leading-tight text-highlighted ring-1 ring-(--surface-card) transition-shadow hover:z-10 hover:shadow-md"
          :class="[
            tall(b) ? 'py-1' : 'justify-center',
            selected === b.event.id && 'z-10 ring-2 ring-(--ev)',
            b.event.status === 'cancelled' && 'opacity-60',
            b.event.status === 'tentative' && 'border-dashed',
            past(b.event) && 'opacity-70'
          ]"
          :style="blockStyle(b)"
          @click="select(b.event, $event)"
        >
          <template v-if="tall(b)">
            <span class="font-semibold" :class="[blockHeight(b) >= 56 ? 'line-clamp-2' : 'truncate', b.event.status === 'cancelled' && 'line-through']">{{ b.event.summary || t('tools.calendar.untitled') }}</span>
            <span class="truncate text-[11px] text-toned">{{ eventTime(b.event) }}</span>
            <span v-if="eventPlace(b.event) && b.bottom - b.top >= 75" class="truncate text-[11px] text-muted">{{ eventPlace(b.event) }}</span>
          </template>
          <span v-else class="truncate">
            <span class="font-semibold" :class="b.event.status === 'cancelled' && 'line-through'">{{ b.event.summary || t('tools.calendar.untitled') }}</span>
            <span class="text-[11px] text-toned"> · {{ timeOf(b.event.start) }}</span>
          </span>
        </button>

        <div
          v-if="sameDay(d, today)"
          class="pointer-events-none absolute inset-x-0 z-15 h-0.5 bg-red-500"
          :style="{ top: `${(nowMinutes / 60) * hourH}px` }"
          aria-hidden="true"
        >
          <span class="absolute -start-1.25 -top-1 size-2.5 rounded-full bg-red-500" />
        </div>
      </div>
    </div>
  </div>
</template>
