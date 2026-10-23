<script setup lang="ts">
import type { CalendarEvent, EventAttendee, EventDraft, FoundEvent, Repeat, RepeatForm, SeriesRepeat } from '~/utils/types'

/** A new event (from `start`, or the next half hour; or what the local AI found in a message,
 *  `suggestion`) or an edit of `event`. */
const props = defineProps<{ event?: CalendarEvent | null, start?: Date | null, allDay?: boolean, suggestion?: FoundEvent | null }>()
const emit = defineEmits<{ saved: [id: number | null], deleted: [] }>()
const open = defineModel<boolean>('open', { required: true })

const { accountById } = useMail()
const { calendars, calendarColor } = useCalendar()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()

const LAST_KEY = 'facteur:calendar-last'
function lastCalendar(): number | null {
  try {
    const v = Number(localStorage.getItem(LAST_KEY))
    return Number.isFinite(v) && v > 0 ? v : null
  } catch {
    return null
  }
}

const writable = computed(() => calendars.value.filter(c => c.enabled && !c.read_only))
const calendarItems = computed(() => writable.value.map((c) => {
  const a = accountById(c.account_id)
  return { label: a ? `${c.name} · ${accountName(a)}` : c.name, value: c.id, color: calendarColor(c.id) }
}))

const pad = (n: number) => String(n).padStart(2, '0')
const dateText = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
const timeText = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`
function parse(date: string, time = '00:00'): Date | null {
  const [y, m, d] = date.split('-').map(Number)
  const [h, min] = time.split(':').map(Number)
  if (!y || !m || !d || h === undefined || min === undefined || Number.isNaN(h) || Number.isNaN(min)) return null
  return new Date(y, m - 1, d, h, min)
}

const form = reactive({
  summary: '',
  calendarId: undefined as number | undefined,
  allDay: false,
  startDate: '',
  startTime: '',
  endDate: '',
  endTime: '',
  location: '',
  description: ''
})
/** For an occurrence of a series: change it alone, or the whole series. */
const scope = ref<'occurrence' | 'series'>('occurrence')
const scopeItems = computed(() => [
  { label: t('tools.calendar.event.thisOccurrence'), value: 'occurrence' as const },
  { label: t('tools.calendar.event.wholeSeries'), value: 'series' as const }
])
/** Chosen once per new event: every attempt to create it sends the same key, so one whose
 *  answer was lost does not create it twice. */
let createKey = ''
const newKey = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, '0')).join('')
/** Outlook gives only a preview of an event's body: it is changed in Outlook. */
const descriptionLocked = computed(() => !!props.event && calendars.value.find(c => c.id === props.event!.calendar_id)?.source === 'microsoft')

const weekdayOf = (d: Date) => (d.getDay() + 6) % 7
const blankRepeat = (): RepeatForm => ({ kind: 'none', interval: 1, weekdays: [], byWeekday: false, end: 'never', until: '', count: 10 })
function toRepeat(f: RepeatForm): Repeat | null {
  if (f.kind === 'none' || f.kind === 'custom') return null
  return {
    freq: f.kind,
    interval: Math.max(1, Math.round(f.interval || 1)),
    weekdays: f.kind === 'weekly' ? [...f.weekdays] : [],
    by_weekday: f.kind === 'monthly' && f.byWeekday,
    until: f.end === 'until' && f.until ? f.until : null,
    count: f.end === 'count' ? Math.max(1, Math.round(f.count || 1)) : null
  }
}
function fromRepeat(r: SeriesRepeat, start: Date): RepeatForm {
  if (r.custom) return { ...blankRepeat(), kind: 'custom' }
  const rule = r.repeat
  if (!rule) return blankRepeat()
  return {
    kind: rule.freq,
    interval: rule.interval,
    weekdays: rule.freq === 'weekly' && !rule.weekdays.length ? [weekdayOf(start)] : [...rule.weekdays],
    byWeekday: rule.by_weekday,
    end: rule.until ? 'until' : rule.count ? 'count' : 'never',
    until: rule.until ?? '',
    count: rule.count ?? 10
  }
}
const repeat = ref<RepeatForm>(blankRepeat())
/** Guests as the recipient field writes them ("Name <address>"). */
const guests = ref<string[]>([])
function guestList(): EventAttendee[] {
  const before = props.event?.attendees ?? []
  return guests.value.map(parseRecipient).filter(a => a.email.includes('@')).map((a) => {
    const known = before.find(b => b.email.toLowerCase() === a.email.toLowerCase())
    return { email: a.email, name: a.name ?? known?.name ?? null, response: known?.response ?? null }
  })
}
/** The zone the form's times are in. */
const zone = ref(localZone())
/** The zone the event had, the computer's for one without. */
const zoneBefore = computed(() => validZone(props.event?.zone) ?? localZone())
const zoneChosen = ref(false)
const zoneShown = computed(() => !form.allDay && (zoneChosen.value || zone.value !== localZone()))
const zoneItems = computed(() => {
  const at = Math.floor(Date.now() / 1000)
  return allZones().map(z => ({ label: `${z.replace(/_/g, ' ')} (${zoneOffsetLabel(z, at)})`, value: z }))
})
/** Where the times fall on the computer's clock, when the event is set in another zone. */
const localHint = computed(() => {
  if (form.allDay || zone.value === localZone()) return null
  const start = instantAt(form.startDate, form.startTime, zone.value)
  if (start === null) return null
  const at = new Date(start * 1000)
  return t('tools.calendar.event.localTime', { time: at.toLocaleString(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' }) })
})

/** Minutes before the start; `null`: the calendar's own (a new event). */
const reminders = ref<number[] | null>(null)
const targetSource = computed(() => calendars.value.find(c => c.id === (props.event?.calendar_id ?? form.calendarId))?.source)
/** Outlook keeps one reminder per event. */
const oneReminder = computed(() => targetSource.value === 'microsoft')
const reminderLabel = (m: number) => {
  const [key, n] = reminderKey(m)
  return t(key, { n }, n)
}
const reminderItems = computed(() => {
  const list = [...new Set([...REMINDER_CHOICES, ...(reminders.value ?? [])])].sort((a, b) => a - b)
  return list.map(m => ({ label: reminderLabel(m), value: m }))
})
const singleReminderItems = computed(() => [{ label: t('tools.calendar.event.noReminder'), value: -1 }, ...reminderItems.value])
const singleReminder = computed({
  get: () => (reminders.value === null ? undefined : (reminders.value.at(-1) ?? -1)),
  set: (v: number | undefined) => {
    reminders.value = v === undefined || v < 0 ? [] : [v]
  }
})
const manyReminders = computed({
  get: () => reminders.value ?? [],
  set: (v: number[]) => {
    reminders.value = [...v].sort((a, b) => a - b)
  }
})
const remindersPlaceholder = computed(() => t(reminders.value === null ? 'tools.calendar.event.remindersDefault' : 'tools.calendar.event.noReminder'))

/** The series' rule as read when the form opened on it (`null`: not read yet). */
const seriesRepeat = ref<SeriesRepeat | null>(null)
const loadingRepeat = ref(false)
let repeatAsked = 0
/** A new event, a single one (it may become a series), or a whole series once its rule is read. */
const repeatShown = computed(() => !props.event || !props.event.recurring || (scope.value === 'series' && !!seriesRepeat.value))
async function loadRepeat() {
  const e = props.event
  if (!e?.recurring || seriesRepeat.value || loadingRepeat.value) return
  const asked = ++repeatAsked
  loadingRepeat.value = true
  try {
    const r = await api.eventRepeat(e)
    if (asked !== repeatAsked) return
    seriesRepeat.value = r
    repeat.value = fromRepeat(r, new Date(e.start * 1000))
  } catch (err) {
    if (asked === repeatAsked) error.value = String(err)
  } finally {
    if (asked === repeatAsked) loadingRepeat.value = false
  }
}
watch(scope, s => s === 'series' && loadRepeat())
/** The series is rebuilt from new dates: those changed one by one go back to its schedule. */
const rebuilds = computed(() => {
  const e = props.event
  const before = seriesRepeat.value
  if (!e?.recurring || scope.value !== 'series' || !before) return false
  const start = new Date(e.start * 1000)
  // The first day as the event's own zone gives it, as the engine compares it.
  const day = e.all_day ? dateText(start) : wallFields(e.start, zoneBefore.value).date
  if (form.startDate !== day || form.allDay !== e.all_day) return true
  if (!form.allDay && zone.value !== zoneBefore.value) return true
  return JSON.stringify(toRepeat(repeat.value)) !== JSON.stringify(toRepeat(fromRepeat(before, start)))
})
const saving = ref(false)
const deleting = ref(false)
const error = ref<string | null>(null)
const editing = computed(() => !!props.event)

function nextHalfHour(): Date {
  const d = new Date()
  d.setSeconds(0, 0)
  d.setMinutes(d.getMinutes() < 30 ? 30 : 60)
  return d
}

function fill() {
  error.value = null
  scope.value = 'occurrence'
  confirmingDelete.value = false
  repeatAsked++
  loadingRepeat.value = false
  seriesRepeat.value = null
  repeat.value = blankRepeat()
  guests.value = (props.event?.attendees ?? []).filter(a => !a.organizer).map(a => addressFull({ name: a.name ?? null, email: a.email }))
  createKey = newKey()
  zoneChosen.value = false
  const e = props.event
  if (e) {
    zone.value = zoneBefore.value
    reminders.value = [...e.reminders]
    const start = new Date(e.start * 1000)
    // A whole day ends at the midnight after its last day: the form shows that last day, a
    // calendar day before (not 24 hours: the day clocks change has 23 or 25).
    const after = new Date(e.end * 1000)
    const end = new Date(Math.max(start.getTime(), addDays(dayStart(after), -1).getTime()))
    const from = e.all_day ? { date: dateText(start), time: timeText(start) } : wallFields(e.start, zone.value)
    const to = e.all_day ? { date: dateText(end), time: timeText(end) } : wallFields(e.end, zone.value)
    Object.assign(form, {
      summary: e.summary,
      calendarId: e.calendar_id,
      allDay: e.all_day,
      startDate: from.date,
      startTime: from.time,
      endDate: to.date,
      endTime: to.time,
      location: e.location ?? '',
      description: e.description ?? ''
    })
    return
  }
  zone.value = localZone()
  reminders.value = null
  const found = props.suggestion
  const allDay = found?.start != null ? found.all_day : !!props.allDay
  const start = found?.start != null ? new Date(found.start * 1000) : props.start ?? nextHalfHour()
  const end = found?.end != null && found.end > found.start! ? new Date(found.end * 1000) : new Date(start.getTime() + 3600_000)
  // A whole day ends at the midnight after its last day: the form shows that last day.
  const last = allDay ? new Date(Math.max(start.getTime(), addDays(dayStart(end), -1).getTime())) : end
  const lastUsed = lastCalendar()
  Object.assign(form, {
    summary: found?.title ?? '',
    calendarId: writable.value.find(c => c.id === lastUsed)?.id ?? writable.value[0]?.id,
    allDay,
    startDate: dateText(start),
    startTime: timeText(start),
    endDate: dateText(last),
    endTime: timeText(end),
    location: found?.place ?? '',
    description: ''
  })
}
watch(open, isOpen => isOpen && fill(), { immediate: true })

// Moving the start keeps the length. Synchronous: `fill` sets the end after the start, over
// what this made of it.
watch(() => [form.startDate, form.startTime] as const, ([date, time], [oldDate, oldTime]) => {
  if (!oldDate || !open.value) return
  const before = parse(oldDate, form.allDay ? '00:00' : oldTime)
  const after = parse(date, form.allDay ? '00:00' : time)
  const end = parse(form.endDate, form.allDay ? '00:00' : form.endTime)
  if (!before || !after || !end) return
  const moved = new Date(end.getTime() + (after.getTime() - before.getTime()))
  form.endDate = dateText(moved)
  if (!form.allDay) form.endTime = timeText(moved)
  // Weekly on the start's day only: it follows the start.
  const r = repeat.value
  if (r.kind === 'weekly' && r.weekdays.length === 1 && r.weekdays[0] === weekdayOf(before)) r.weekdays = [weekdayOf(after)]
}, { flush: 'sync' })

function draft(): EventDraft | null {
  if (form.allDay) {
    const start = parse(form.startDate)
    const last = parse(form.endDate)
    if (!start || !last) return null
    return {
      summary: form.summary,
      location: form.location || null,
      description: form.description || null,
      start: unix(start),
      end: unix(addDays(last, 1)),
      all_day: true,
      repeat: toRepeat(repeat.value),
      attendees: guestList(),
      reminders: reminders.value
    }
  }
  const start = instantAt(form.startDate, form.startTime, zone.value)
  const end = instantAt(form.endDate, form.endTime, zone.value)
  if (start === null || end === null) return null
  // The zone is sent once changed: an event left in its own keeps it, a new one in the computer's
  // is written as before.
  const before = props.event ? validZone(props.event.zone) : null
  const sentZone = zone.value === zoneBefore.value ? before : zone.value
  return {
    summary: form.summary,
    location: form.location || null,
    description: form.description || null,
    start,
    end,
    all_day: false,
    repeat: toRepeat(repeat.value),
    attendees: guestList(),
    zone: sentZone,
    reminders: reminders.value
  }
}

async function save() {
  if (saving.value) return
  const d = draft()
  if (!d) {
    error.value = t('tools.calendar.event.badTime')
    return
  }
  if (d.end <= d.start) {
    error.value = t('tools.calendar.event.endBeforeStart')
    return
  }
  // The rule sent replaces the series': it must have been read first.
  if (props.event?.recurring && scope.value === 'series' && !seriesRepeat.value) {
    error.value = t('tools.calendar.repeat.notRead')
    return
  }
  if (!props.event && form.calendarId === undefined) {
    error.value = t('tools.calendar.event.noCalendar')
    return
  }
  saving.value = true
  error.value = null
  try {
    if (props.event) {
      await api.updateEvent(props.event, d, scope.value === 'series')
      emit('saved', props.event.id)
    } else {
      const id = await api.createEvent(form.calendarId!, d, createKey)
      try {
        localStorage.setItem(LAST_KEY, String(form.calendarId))
      } catch {
        // localStorage may be unavailable.
      }
      emit('saved', id)
    }
    open.value = false
  } catch (e) {
    // Closed while saving: the error would go unseen in the form.
    if (open.value) error.value = String(e)
    else showError(e)
  } finally {
    saving.value = false
  }
}

/** Asked in the form itself: a confirmation dialog would open under this one. */
const confirmingDelete = ref(false)
const deleteQuestion = computed(() => {
  const e = props.event
  if (!e?.recurring) return t('tools.calendar.event.deleteConfirm')
  return t(scope.value === 'series' ? 'tools.calendar.event.deleteSeriesConfirm' : 'tools.calendar.event.deleteOccurrenceConfirm')
})

async function remove() {
  const e = props.event
  if (!e || deleting.value) return
  const series = e.recurring && scope.value === 'series'
  deleting.value = true
  try {
    await api.deleteEvent(e, series)
    toast.add({ title: t('tools.calendar.event.deleted'), description: e.summary || t('tools.calendar.untitled'), icon: 'i-lucide-trash-2' })
    emit('deleted')
    open.value = false
  } catch (err) {
    showError(err)
  } finally {
    deleting.value = false
  }
}
</script>

<template>
  <UModal
    v-model:open="open"
    :title="editing ? t('tools.calendar.event.editTitle') : t('tools.calendar.event.newTitle')"
    :ui="{ content: 'max-w-lg' }"
  >
    <template #body>
      <form class="flex flex-col gap-4" @submit.prevent="save">
        <div v-if="event?.recurring" class="flex items-center gap-2.5 rounded-xl bg-elevated/50 px-3 py-2.5">
          <UIcon name="i-lucide-repeat" class="size-4 shrink-0 text-muted" />
          <URadioGroup v-model="scope" :items="scopeItems" orientation="horizontal" :aria-label="t('tools.calendar.event.scope')" />
        </div>
        <UInput v-model="form.summary" :placeholder="t('tools.calendar.event.summary')" autofocus size="lg" class="w-full" />

        <UFormField v-if="!editing" :label="t('tools.calendar.event.calendar')">
          <USelect v-model="form.calendarId" :items="calendarItems" class="w-full" :placeholder="t('tools.calendar.event.noCalendar')">
            <template #leading>
              <span v-if="form.calendarId !== undefined" class="size-2.5 rounded-[3px]" :style="{ background: calendarColor(form.calendarId) }" />
            </template>
            <template #item-leading="{ item }">
              <span class="size-2.5 rounded-[3px]" :style="{ background: item.color }" />
            </template>
          </USelect>
        </UFormField>

        <USwitch v-model="form.allDay" :label="t('tools.calendar.allDay')" />

        <div class="flex flex-col gap-2">
          <div class="flex items-end gap-2">
            <UFormField :label="t('tools.calendar.event.start')" class="flex-1">
              <UInput v-model="form.startDate" type="date" class="w-full" />
            </UFormField>
            <UInput v-if="!form.allDay" v-model="form.startTime" type="time" class="w-32" :aria-label="t('tools.calendar.event.startTime')" />
          </div>
          <div class="flex items-end gap-2">
            <UFormField :label="t('tools.calendar.event.end')" class="flex-1">
              <UInput v-model="form.endDate" type="date" class="w-full" />
            </UFormField>
            <UInput v-if="!form.allDay" v-model="form.endTime" type="time" class="w-32" :aria-label="t('tools.calendar.event.endTime')" />
          </div>
          <UButton
            v-if="!form.allDay && !zoneShown"
            :label="t('tools.calendar.event.zone')"
            icon="i-lucide-globe"
            color="neutral"
            variant="link"
            size="sm"
            class="-ms-1.5 self-start px-1.5 text-muted"
            @click="zoneChosen = true"
          />
          <template v-if="zoneShown">
            <USelectMenu
              v-model="zone"
              :items="zoneItems"
              value-key="value"
              virtualize
              icon="i-lucide-globe"
              :search-input="{ placeholder: t('tools.calendar.event.searchZone'), icon: 'i-lucide-search' }"
              :aria-label="t('tools.calendar.event.zone')"
              class="w-full"
            />
            <p v-if="localHint" class="text-[13px] text-muted">{{ localHint }}</p>
          </template>
        </div>

        <USelectMenu
          v-if="oneReminder"
          v-model="singleReminder"
          :items="singleReminderItems"
          value-key="value"
          icon="i-lucide-bell"
          :search-input="false"
          :placeholder="remindersPlaceholder"
          :aria-label="t('tools.calendar.event.reminders')"
          class="w-full"
        />
        <USelectMenu
          v-else
          v-model="manyReminders"
          :items="reminderItems"
          value-key="value"
          multiple
          icon="i-lucide-bell"
          :search-input="false"
          :placeholder="remindersPlaceholder"
          :aria-label="t('tools.calendar.event.reminders')"
          class="w-full"
        />

        <CalendarRepeat v-if="repeatShown" v-model="repeat" :start="parse(form.startDate)" :custom="!!seriesRepeat?.custom" />
        <div v-else-if="loadingRepeat" class="flex items-center gap-2 text-sm text-muted">
          <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" />
          {{ t('tools.calendar.repeat.reading') }}
        </div>
        <p v-if="rebuilds" class="flex items-start gap-2 text-[13px] text-muted">
          <UIcon name="i-lucide-info" class="mt-0.5 size-4 shrink-0" />
          {{ t('tools.calendar.repeat.rebuilds') }}
        </p>

        <div class="flex items-start gap-2">
          <UIcon name="i-lucide-users" class="mt-2.5 size-4 shrink-0 text-muted" />
          <RecipientInput v-model="guests" :label="t('tools.calendar.event.guests')" :placeholder="t('tools.calendar.event.guestsPlaceholder')" class="min-w-0 flex-1" />
        </div>
        <p v-if="guests.length" class="-mt-2 ps-6 text-[13px] text-muted">{{ t(editing ? 'tools.calendar.event.guestsUpdated' : 'tools.calendar.event.guestsInvited') }}</p>

        <UInput v-model="form.location" icon="i-lucide-map-pin" :placeholder="t('tools.calendar.event.location')" class="w-full" />
        <UFormField :help="descriptionLocked ? t('tools.calendar.event.descriptionInOutlook') : undefined">
          <UTextarea
            v-model="form.description"
            :placeholder="t('tools.calendar.event.description')"
            :rows="4"
            autoresize
            :maxrows="12"
            :disabled="descriptionLocked"
            class="w-full"
          />
        </UFormField>

        <p v-if="error" class="text-sm text-error">{{ error }}</p>

        <div v-if="confirmingDelete" class="flex flex-wrap items-center gap-2 rounded-xl bg-error/10 px-3 py-2.5">
          <UIcon name="i-lucide-triangle-alert" class="size-4 shrink-0 text-error" />
          <span class="min-w-0 flex-1 text-sm text-highlighted">{{ deleteQuestion }}</span>
          <UButton :label="t('common.cancel')" color="neutral" variant="ghost" size="sm" @click="confirmingDelete = false" />
          <UButton :label="t('tools.calendar.event.delete')" icon="i-lucide-trash-2" color="error" size="sm" :loading="deleting" @click="remove" />
        </div>

        <div v-else class="flex items-center gap-2">
          <UButton
            v-if="editing"
            :label="t('tools.calendar.event.delete')"
            icon="i-lucide-trash-2"
            color="error"
            variant="ghost"
            @click="confirmingDelete = true"
          />
          <span class="flex-1" />
          <UButton :label="t('common.cancel')" color="neutral" variant="ghost" @click="open = false" />
          <UButton type="submit" :label="t('tools.calendar.event.save')" :loading="saving" />
        </div>
      </form>
    </template>
  </UModal>
</template>
