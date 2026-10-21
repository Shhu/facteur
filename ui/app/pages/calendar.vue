<script setup lang="ts">
import { useMediaQuery } from '@vueuse/core'
import type { CalendarEvent } from '~/utils/types'

const { accounts } = useMail()
const { calendars, loaded, hasCalendars, statusOf, isIgnored, isSyncing, sync } = useCalendar()
const { open: composing } = useComposer()
const desktop = useMediaQuery('(min-width: 1024px)')
const now = useNow()
const back = useScreenBack()
const { t } = useI18n()

type View = 'month' | 'week' | 'day'
const VIEW_KEY = 'facteur:calendar-view'
function savedView(): View {
  try {
    const v = localStorage.getItem(VIEW_KEY)
    if (v === 'month' || v === 'week' || v === 'day') return v
  } catch {
    // localStorage may be unavailable.
  }
  return 'week'
}
const view = ref<View>(savedView())
watch(view, (v) => {
  try {
    localStorage.setItem(VIEW_KEY, v)
  } catch {
    // localStorage may be unavailable.
  }
})
const viewItems = computed(() => [
  { label: t('tools.calendar.view.month'), value: 'month' as const },
  { label: t('tools.calendar.view.week'), value: 'week' as const },
  { label: t('tools.calendar.view.day'), value: 'day' as const }
])
const shownView = computed<View>(() => (desktop.value ? view.value : 'week'))

const date = ref(dayStart(new Date(now.value)))
const today = computed(() => dayStart(new Date(now.value)))

const days = computed<Date[]>(() => {
  switch (shownView.value) {
    case 'day': return [date.value]
    case 'month': return monthGrid(date.value)
    default: {
      const start = weekStart(date.value)
      return Array.from({ length: 7 }, (_, i) => addDays(start, i))
    }
  }
})
const rangeStart = computed(() => days.value[0]!)
const rangeEnd = computed(() => addDays(days.value.at(-1)!, 1))
const { events } = useCalendarEvents(() => ({ from: unix(rangeStart.value), to: unix(rangeEnd.value) }))
const list = computed(() => events.value ?? [])

function move(step: number) {
  const d = date.value
  if (shownView.value === 'month') date.value = new Date(d.getFullYear(), d.getMonth() + step, 1)
  else date.value = addDays(d, step * (shownView.value === 'day' ? 1 : 7))
}
function goToday() {
  date.value = today.value
  if (!desktop.value) nextTick(() => scrollToDay(today.value))
}
function pickDay(d: Date) {
  date.value = d
  view.value = 'day'
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1)
const title = computed(() => {
  const d = date.value
  if (shownView.value === 'month') return capital(dateFormat({ month: 'long', year: 'numeric' }).format(d))
  if (shownView.value === 'day') return capital(dateFormat({ weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' }).format(d))
  const f = dateFormat({ day: 'numeric', month: desktop.value ? 'long' : 'short', year: 'numeric' })
  return f.formatRange(rangeStart.value, addDays(rangeEnd.value, -1))
})
const weekNumber = computed(() => (shownView.value === 'month' ? null : t('tools.calendar.weekNumber', { n: isoWeek(days.value[Math.min(3, days.value.length - 1)]!) })))
const stepLabels = computed(() => ({
  month: [t('tools.calendar.previousMonth'), t('tools.calendar.nextMonth')],
  week: [t('tools.calendar.previousWeek'), t('tools.calendar.nextWeek')],
  day: [t('tools.calendar.previousDay'), t('tools.calendar.nextDay')]
}[shownView.value]))

const selected = ref<{ event: CalendarEvent, el: HTMLElement | null } | null>(null)
/** When the popover last closed: the click that closed it, landing on a free spot of the week,
 *  must not also start an event there. */
let popoverClosedAt = 0
const popoverOpen = computed({
  get: () => desktop.value && selected.value !== null,
  set: (open: boolean) => {
    if (!open) {
      selected.value = null
      popoverClosedAt = Date.now()
    }
  }
})
const drawerOpen = computed({
  get: () => !desktop.value && selected.value !== null,
  set: (open: boolean) => {
    if (!open) selected.value = null
  }
})
/** An event to show, asked for from elsewhere (a reminder's notification): its day, then the
 *  event itself once loaded. */
const asked = useState<{ id: number, start: number } | null>('calendar-open-event', () => null)
watch(asked, (a) => {
  if (a) date.value = dayStart(new Date(a.start * 1000))
}, { immediate: true })
watch([asked, list], ([a, events]) => {
  const found = a && events.find(e => e.id === a.id)
  if (!found) return
  asked.value = null
  selected.value = { event: found, el: null }
}, { immediate: true })

function select(event: CalendarEvent, el: HTMLElement | null = null) {
  if (selected.value && eventKey(selected.value.event) === eventKey(event)) selected.value = null
  else selected.value = { event, el }
}
// After a sync, follow the same occurrence (its row may be new); close only if it is gone.
watch(list, (l) => {
  const s = selected.value
  if (!s) return
  const key = eventKey(s.event)
  const fresh = l.find(e => eventKey(e) === key)
  if (!fresh) selected.value = null
  else if (fresh !== s.event) selected.value = { event: fresh, el: s.el }
})

const canCreate = computed(() => calendars.value.some(c => c.enabled && !c.read_only))
const form = reactive({ open: false, event: null as CalendarEvent | null, start: null as Date | null, allDay: false })
function newEvent(start: Date | null = null, allDay = false) {
  if (!canCreate.value) return
  Object.assign(form, { open: true, event: null, start, allDay })
}
function createAt(start: Date) {
  // A click beside an open event closes it first.
  if (selected.value || Date.now() - popoverClosedAt < 400) {
    selected.value = null
    return
  }
  newEvent(start)
}
function editSelected() {
  const e = selected.value?.event
  if (!e) return
  selected.value = null
  Object.assign(form, { open: true, event: e, start: null, allDay: false })
}

const busyDays = computed(() => new Set(days.value.filter(d => list.value.some(e => onDay(e, d))).map(dayKey)))
function scrollToDay(d: Date) {
  document.getElementById(`agenda-${dayKey(d)}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}
function tapDay(d: Date) {
  date.value = d
  scrollToDay(d)
}
const listOpen = ref(false)

const troubled = computed(() => accounts.value.filter(a => !isIgnored(a.id) && (statusOf(a.id)?.state ?? 'ok') !== 'ok'))
const empty = computed(() => loaded.value && !hasCalendars.value)

const free = () => !composing.value && !overlayOpen()
defineShortcuts({
  t: () => free() && goToday(),
  j: () => free() && move(1),
  k: () => free() && move(-1),
  arrowright: () => free() && move(1),
  arrowleft: () => free() && move(-1),
  n: () => free() && newEvent(),
  m: () => free() && (view.value = 'month'),
  w: () => free() && (view.value = 'week'),
  d: () => free() && (view.value = 'day')
})
</script>

<template>
  <UDashboardPanel id="calendar" :ui="{ body: 'gap-0 bg-(--surface-read) p-0 sm:gap-0 sm:p-0 lg:overflow-hidden' }">
    <template #header>
      <UDashboardNavbar :title="t('tools.calendar.title')" class="max-lg:hidden">
        <template #right>
          <div class="flex items-center gap-1.5">
            <UTooltip v-if="canCreate" :text="t('tools.calendar.event.newTitle')" :kbds="['n']">
              <UButton :label="t('tools.calendar.event.new')" icon="i-lucide-plus" class="me-1 rounded-[10px]" @click="newEvent()" />
            </UTooltip>
            <UButton :label="t('tools.calendar.today')" color="neutral" variant="outline" class="rounded-[10px]" @click="goToday" />
            <UTooltip :text="stepLabels[0]" :kbds="['k']">
              <UButton icon="i-lucide-chevron-left" color="neutral" variant="ghost" :aria-label="stepLabels[0]" @click="move(-1)" />
            </UTooltip>
            <UTooltip :text="stepLabels[1]" :kbds="['j']">
              <UButton icon="i-lucide-chevron-right" color="neutral" variant="ghost" :aria-label="stepLabels[1]" @click="move(1)" />
            </UTooltip>
            <h2 class="ms-1 min-w-0 truncate text-[15px] font-semibold text-highlighted" aria-live="polite">{{ title }}</h2>
            <span v-if="weekNumber" class="shrink-0 rounded-full bg-elevated px-2 py-0.5 text-xs font-medium text-muted">{{ weekNumber }}</span>
          </div>
          <SettingsSegmented v-model="view" :items="viewItems" :label="t('tools.calendar.view.label')" class="ms-2" />
        </template>
      </UDashboardNavbar>

      <div class="flex shrink-0 items-center gap-1 border-b border-default bg-default px-2 py-2.5 lg:hidden dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-arrow-left"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl text-highlighted"
          :aria-label="t('tools.shared.back')"
          @click="back"
        />
        <h1 class="min-w-0 flex-1 truncate text-[17px] font-semibold text-highlighted">{{ t('tools.calendar.title') }}</h1>
        <UButton :label="t('tools.calendar.today')" color="neutral" variant="ghost" size="lg" class="min-h-11 rounded-xl" @click="goToday" />
        <UButton
          v-if="canCreate"
          icon="i-lucide-plus"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl"
          :aria-label="t('tools.calendar.event.newTitle')"
          @click="newEvent()"
        />
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl"
          :loading="isSyncing()"
          :aria-label="t('tools.calendar.refresh')"
          @click="sync(null)"
        />
        <UButton
          icon="i-lucide-list-filter"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl"
          :aria-label="t('tools.calendar.calendars')"
          @click="listOpen = true"
        />
      </div>
    </template>

    <template #body>
      <div v-if="desktop" class="flex min-h-0 flex-1">
        <aside class="flex w-64 shrink-0 flex-col gap-5 overflow-y-auto border-e border-default bg-(--surface-side) px-3.5 py-4 xl:w-68" :aria-label="t('tools.calendar.calendars')">
          <!-- Month view: the main grid is the month, so the mini month marks no range. -->
          <CalendarMiniMonth v-model="date" :from="rangeStart" :to="shownView === 'month' ? rangeStart : rangeEnd" />
          <CalendarList />
        </aside>
        <main class="relative flex min-h-0 min-w-0 flex-1 flex-col bg-(--surface-card)">
          <div v-if="empty" class="flex flex-1 items-center justify-center p-8">
            <UEmpty
              icon="i-lucide-calendar-off"
              :title="t('tools.calendar.empty.title')"
              :description="t('tools.calendar.empty.description')"
              variant="naked"
              class="max-w-md"
            />
          </div>
          <CalendarMonth v-else-if="shownView === 'month'" :month="date" :days="days" :events="list" :selected="selected?.event.id ?? null" @select="select" @pick="pickDay" />
          <CalendarWeek v-else :key="shownView" :days="days" :events="list" :selected="selected?.event.id ?? null" @select="select" @pick="pickDay" @create="createAt" />
        </main>

        <UPopover
          v-model:open="popoverOpen"
          :reference="selected?.el ?? undefined"
          :content="{ side: 'right', align: 'start', sideOffset: 8, collisionPadding: 12 }"
          :ui="{ content: 'w-88 max-w-[calc(100vw-2rem)] rounded-2xl p-4' }"
        >
          <template #content>
            <CalendarEventDetails v-if="selected" :event="selected.event" @close="selected = null" @edit="editSelected" />
          </template>
        </UPopover>
      </div>

      <template v-else>
        <div class="sticky top-0 z-10 flex flex-col gap-1.5 border-b border-default bg-default px-2 pt-2 pb-2.5 dark:bg-(--surface-side)">
          <div class="flex items-center gap-1 px-1">
            <span class="min-w-0 flex-1 truncate text-sm font-semibold text-highlighted" aria-live="polite">{{ title }}</span>
            <span v-if="weekNumber" class="shrink-0 text-xs text-muted">{{ weekNumber }}</span>
          </div>
          <div class="flex items-center">
            <UButton icon="i-lucide-chevron-left" color="neutral" variant="ghost" class="size-11 shrink-0 justify-center" :aria-label="stepLabels[0]" @click="move(-1)" />
            <div class="grid min-w-0 flex-1 grid-cols-7">
              <button
                v-for="d in days"
                :key="dayKey(d)"
                type="button"
                class="flex min-h-14 flex-col items-center justify-center gap-0.5 rounded-xl"
                :class="sameDay(d, date) ? 'bg-primary/12' : ''"
                :aria-label="dayHeading(d, today)"
                :aria-current="sameDay(d, date) ? 'date' : undefined"
                @click="tapDay(d)"
              >
                <span class="text-[11px] font-semibold uppercase" :class="sameDay(d, today) ? 'text-primary' : 'text-muted'">{{ dateFormat({ weekday: 'narrow' }).format(d) }}</span>
                <span
                  class="flex size-7.5 items-center justify-center rounded-full text-[15px] font-semibold tabular-nums"
                  :class="sameDay(d, today) ? 'bg-primary text-inverted' : 'text-highlighted'"
                >{{ d.getDate() }}</span>
                <span class="size-1 rounded-full" :class="busyDays.has(dayKey(d)) ? 'bg-(--ui-text-muted)' : 'bg-transparent'" />
              </button>
            </div>
            <UButton icon="i-lucide-chevron-right" color="neutral" variant="ghost" class="size-11 shrink-0 justify-center" :aria-label="stepLabels[1]" @click="move(1)" />
          </div>
        </div>

        <div class="px-4 pt-3.5 pb-8">
          <div v-if="empty" class="flex flex-col gap-3 py-6">
            <UEmpty
              icon="i-lucide-calendar-off"
              :title="t('tools.calendar.empty.title')"
              :description="t('tools.calendar.empty.description')"
              variant="naked"
            />
            <CalendarAccountState v-for="a in troubled" :key="a.id" :account="a" :status="statusOf(a.id)" touch />
          </div>
          <CalendarAgenda v-else :days="days" :events="list" @select="(e) => select(e)" />
        </div>

        <UDrawer v-model:open="drawerOpen" handle-only :ui="{ container: 'pb-[calc(1rem+env(safe-area-inset-bottom))]' }">
          <template #body>
            <CalendarEventDetails v-if="selected" :event="selected.event" touch @edit="editSelected" />
          </template>
        </UDrawer>
        <UDrawer v-model:open="listOpen" :title="t('tools.calendar.calendars')" handle-only :ui="{ container: 'pb-[calc(1rem+env(safe-area-inset-bottom))]' }">
          <template #body>
            <CalendarList touch />
          </template>
        </UDrawer>
      </template>

      <CalendarEventForm v-model:open="form.open" :event="form.event" :start="form.start" :all-day="form.allDay" />
    </template>
  </UDashboardPanel>
</template>
