<script setup lang="ts">
import { watchDebounced } from '@vueuse/core'
import type { CalendarEvent, EventHit, EventSearch } from '~/utils/types'

/** Loads the shared search, again after a calendar sync. */
const emit = defineEmits<{ select: [event: CalendarEvent] }>()
const { query, active } = useCalendarSearch()
const { calendarTick, eventColor, calendarById, syncedSpan } = useCalendar()
const now = useNow()
const { t } = useI18n()

const result = ref<EventSearch | null>(null)
let request = 0
async function load() {
  const mine = ++request
  if (!active.value) {
    result.value = null
    return
  }
  try {
    const found = await api.searchCalendarEvents(query.value)
    if (mine === request) result.value = found
  } catch {
    if (mine === request) result.value = { hits: [], more: false }
  }
}
watchDebounced(() => JSON.stringify(query.value), load, { debounce: 150, immediate: true })
watch(calendarTick, load)

const groups = computed(() => {
  const hits = result.value?.hits ?? []
  const upcoming = hits.filter(h => h.event.end * 1000 > now.value)
  const past = hits.filter(h => h.event.end * 1000 <= now.value)
  return [
    { key: 'upcoming', label: t('tools.calendar.search.upcoming'), hits: upcoming },
    { key: 'past', label: t('tools.calendar.search.past'), hits: past }
  ].filter(g => g.hits.length)
})

const thisYear = computed(() => new Date(now.value).getFullYear())
function dateParts(h: EventHit) {
  const d = new Date(h.event.start * 1000)
  return {
    day: d.getDate(),
    month: dateFormat({ month: 'short', ...(d.getFullYear() === thisYear.value ? {} : { year: 'numeric' }) }).format(d),
    weekday: dateFormat({ weekday: 'short' }).format(d)
  }
}
const dayLabel = (h: EventHit) => dateFormat({ weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' }).format(new Date(h.event.start * 1000))
const spanText = computed(() => {
  const s = syncedSpan.value
  if (!s) return null
  const f = dateFormat({ day: 'numeric', month: 'long', year: 'numeric' })
  return t('tools.calendar.search.span', { from: f.format(new Date(s.from * 1000)), to: f.format(new Date((s.to - 1) * 1000)) })
})
</script>

<template>
  <div class="flex flex-col gap-5">
    <p v-if="result && !result.hits.length" class="px-1 py-6 text-center text-sm text-muted">{{ t('tools.calendar.search.none') }}</p>
    <section v-for="g in groups" :key="g.key" :aria-label="g.label">
      <h2 class="mb-1.5 px-1 text-[13px] font-semibold text-muted uppercase">{{ g.label }}</h2>
      <ul class="divide-y divide-default overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none">
        <li v-for="h in g.hits" :key="`${h.event.id}-${h.event.start}`">
          <button
            type="button"
            class="flex min-h-14 w-full items-stretch gap-3 px-3.5 py-2.5 text-start hover:bg-elevated/50 active:bg-elevated/60"
            :class="g.key === 'past' && 'opacity-70'"
            :aria-label="`${h.event.summary || t('tools.calendar.untitled')}, ${dayLabel(h)}`"
            @click="emit('select', h.event)"
          >
            <span class="flex w-12 shrink-0 flex-col items-center justify-center leading-tight tabular-nums">
              <span class="text-[11px] text-muted uppercase">{{ dateParts(h).weekday }}</span>
              <span class="text-lg font-semibold text-highlighted">{{ dateParts(h).day }}</span>
              <span class="text-[11px] text-muted">{{ dateParts(h).month }}</span>
            </span>
            <span class="w-1 shrink-0 rounded-full" :class="h.event.status === 'tentative' && 'opacity-50'" :style="{ background: eventColor(h.event) }" />
            <span class="flex min-w-0 flex-1 flex-col justify-center gap-0.5">
              <span class="truncate text-[15px] font-medium text-highlighted" :class="h.event.status === 'cancelled' && 'line-through'">{{ h.event.summary || t('tools.calendar.untitled') }}</span>
              <span class="truncate text-[12.5px] text-muted">
                {{ [eventTime(h.event), eventPlace(h.event), calendarById(h.event.calendar_id)?.name].filter(Boolean).join(' · ') }}
              </span>
              <span v-if="h.occurrences > 1" class="flex items-center gap-1 text-[12px] text-dimmed">
                <UIcon name="i-lucide-repeat" class="size-3.5" />
                {{ t('tools.calendar.search.occurrences', { n: h.occurrences }) }}
              </span>
            </span>
          </button>
        </li>
      </ul>
    </section>
    <p v-if="result?.more" class="px-1 text-[13px] text-muted">{{ t('tools.calendar.search.more', { n: result.hits.length }) }}</p>
    <p v-if="result && spanText" class="px-1 text-[12.5px] text-dimmed">{{ spanText }}</p>
  </div>
</template>
