<script setup lang="ts">
import type { CalendarEvent } from '~/utils/types'

/** Each day has an anchor (`agenda-<day>`) that the day strip above scrolls to. */
const props = defineProps<{ days: Date[], events: CalendarEvent[] }>()
const emit = defineEmits<{ select: [event: CalendarEvent] }>()
const { eventColor, calendarById } = useCalendar()
const now = useNow()
const { t } = useI18n()

const today = computed(() => dayStart(new Date(now.value)))
const groups = computed(() => props.days.map(d => ({
  day: d,
  events: props.events
    .filter(e => onDay(e, d))
    .sort((a, b) => Number(inBand(b)) - Number(inBand(a)) || a.start - b.start)
})))
const past = (e: CalendarEvent) => e.end * 1000 < now.value
function times(e: CalendarEvent, d: Date): [string, string] {
  if (inBand(e)) return [t('tools.calendar.allDayShort'), '']
  const from = unix(d)
  const to = unix(addDays(d, 1))
  return [
    e.start < from ? '…' : timeOf(e.start),
    e.end > to ? '…' : e.end > e.start ? timeOf(e.end) : ''
  ]
}
</script>

<template>
  <div class="flex flex-col gap-4">
    <section v-for="g in groups" :id="`agenda-${dayKey(g.day)}`" :key="dayKey(g.day)" class="scroll-mt-3" :aria-label="dayHeading(g.day, today)">
      <h2 class="mb-1.5 flex items-baseline gap-2 px-1">
        <span class="text-[15px] font-semibold" :class="sameDay(g.day, today) ? 'text-primary' : 'text-highlighted'">{{ dayHeading(g.day, today) }}</span>
        <span v-if="sameDay(g.day, today) || sameDay(g.day, addDays(today, 1))" class="text-[13px] text-muted">
          {{ dateFormat({ weekday: 'short', day: 'numeric', month: 'short' }).format(g.day) }}
        </span>
      </h2>
      <p v-if="!g.events.length" class="px-1 text-[13px] text-dimmed">{{ t('tools.calendar.nothing') }}</p>
      <ul v-else class="divide-y divide-default overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none">
        <li v-for="e in g.events" :key="`${e.id}-${e.start}`">
          <button
            type="button"
            class="flex min-h-14 w-full items-stretch gap-3 px-3.5 py-2.5 text-start active:bg-elevated/60"
            :class="past(e) && 'opacity-60'"
            @click="emit('select', e)"
          >
            <span class="flex w-11 shrink-0 flex-col justify-center text-[12.5px] leading-tight tabular-nums">
              <span class="font-semibold text-highlighted">{{ times(e, g.day)[0] }}</span>
              <span class="text-muted">{{ times(e, g.day)[1] }}</span>
            </span>
            <span class="w-1 shrink-0 rounded-full" :class="e.status === 'tentative' && 'opacity-50'" :style="{ background: eventColor(e) }" />
            <span class="flex min-w-0 flex-1 flex-col justify-center gap-0.5">
              <span class="truncate text-[15px] font-medium text-highlighted" :class="e.status === 'cancelled' && 'line-through'">{{ e.summary || t('tools.calendar.untitled') }}</span>
              <span class="truncate text-[12.5px] text-muted">
                {{ [eventPlace(e), calendarById(e.calendar_id)?.name].filter(Boolean).join(' · ') }}
              </span>
            </span>
          </button>
        </li>
      </ul>
    </section>
  </div>
</template>
