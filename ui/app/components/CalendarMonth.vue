<script setup lang="ts">
import { useElementSize } from '@vueuse/core'
import type { CalendarEvent } from '~/utils/types'

const props = defineProps<{ month: Date, days: Date[], events: CalendarEvent[], selected: number | null }>()
const emit = defineEmits<{ select: [event: CalendarEvent, el: HTMLElement], pick: [day: Date] }>()
const { eventColor } = useCalendar()
const now = useNow()
const { t } = useI18n()

const grid = useTemplateRef<HTMLElement>('grid')
const { height } = useElementSize(grid)
/** Event rows that fit in a cell, under the day number. */
const fits = computed(() => Math.max(1, Math.floor((height.value / 6 - 30) / 21)))

const today = computed(() => dayStart(new Date(now.value)))
const weekdays = computed(() => props.days.slice(0, 7).map(d => dateFormat({ weekday: 'short' }).format(d)))
const byDay = computed(() => props.days.map(d => props.events
  .filter(e => onDay(e, d))
  .sort((a, b) => Number(inBand(b)) - Number(inBand(a)) || a.start - b.start)))
const shown = (i: number) => {
  const list = byDay.value[i] ?? []
  // The "+N" line takes the place of the last event.
  return list.length > fits.value ? list.slice(0, fits.value - 1) : list
}
const hidden = (i: number) => (byDay.value[i]?.length ?? 0) - shown(i).length
const outside = (d: Date) => d.getMonth() !== props.month.getMonth()

const select = (e: CalendarEvent, ev: MouseEvent) => emit('select', e, ev.currentTarget as HTMLElement)
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div class="grid grid-cols-7 border-b border-default">
      <span v-for="w in weekdays" :key="w" class="py-2 text-center text-[11px] font-semibold uppercase tracking-wide text-muted">{{ w }}</span>
    </div>
    <div ref="grid" class="grid min-h-0 flex-1 grid-cols-7 grid-rows-6">
      <div
        v-for="(d, i) in days"
        :key="dayKey(d)"
        class="flex min-h-0 min-w-0 flex-col gap-px overflow-hidden border-default px-1 pt-1 pb-0.5"
        :class="[i % 7 && 'border-s', i >= 7 && 'border-t', outside(d) && 'bg-elevated/30']"
      >
        <button
          type="button"
          class="mb-0.5 flex size-6.5 items-center justify-center self-center rounded-full text-[12.5px] font-semibold tabular-nums transition-colors"
          :class="sameDay(d, today) ? 'bg-primary text-inverted' : outside(d) ? 'text-dimmed hover:bg-elevated' : 'text-highlighted hover:bg-elevated'"
          :aria-label="dayHeading(d, today)"
          @click="emit('pick', d)"
        >
          {{ d.getDate() }}
        </button>
        <template v-for="e in shown(i)" :key="`${e.id}-${e.start}`">
          <button
            v-if="inBand(e)"
            type="button"
            class="cal-tint flex h-5 min-w-0 shrink-0 items-center rounded-[5px] border-s-3 border-(--ev) px-1.5 text-start text-[11.5px] font-medium text-highlighted"
            :class="[selected === e.id && 'ring-2 ring-(--ev)', e.status === 'cancelled' && 'line-through opacity-60']"
            :style="{ '--ev': eventColor(e) }"
            @click="select(e, $event)"
          >
            <span class="truncate">{{ e.summary || t('tools.calendar.untitled') }}</span>
          </button>
          <button
            v-else
            type="button"
            class="flex h-5 min-w-0 shrink-0 items-center gap-1.5 rounded-[5px] px-1 text-start text-[11.5px] text-default hover:bg-elevated/70"
            :class="[selected === e.id && 'bg-elevated', e.status === 'cancelled' && 'line-through opacity-60']"
            @click="select(e, $event)"
          >
            <span class="size-2 shrink-0 rounded-full" :style="{ background: eventColor(e) }" />
            <span class="shrink-0 tabular-nums text-muted">{{ timeOf(e.start) }}</span>
            <span class="truncate font-medium">{{ e.summary || t('tools.calendar.untitled') }}</span>
          </button>
        </template>
        <button
          v-if="hidden(i) > 0"
          type="button"
          class="h-5 shrink-0 rounded-[5px] px-1 text-start text-[11.5px] font-semibold text-muted hover:bg-elevated/70"
          @click="emit('pick', d)"
        >
          {{ t('tools.calendar.more', { n: hidden(i) }, hidden(i)) }}
        </button>
      </div>
    </div>
  </div>
</template>
