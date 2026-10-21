<script setup lang="ts">
import { openUrl } from '@tauri-apps/plugin-opener'
import type { Invite } from '~/utils/types'
import type { InvitePerson } from '~/components/InviteAttendees.vue'
import type { Partstat } from '~/components/InviteAnswers.vue'

/** `accountId`: the invited account (its colour, "you"). `html`: the message body, where Google
 *  says what an update changed. */
const props = defineProps<{ messageId: number, invite: Invite, accountId: number, html?: string }>()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()
const { accountColor, isMine } = useMail()

const color = computed(() => accountColor(props.accountId))
const state = computed(() => inviteState(props.invite, props.html))
const canAnswer = computed(() => props.invite.method === 'REQUEST' && !props.invite.cancelled)

const ANSWERS: string[] = ['ACCEPTED', 'TENTATIVE', 'DECLINED']
/** Answer already sent: from facteur, or the PARTSTAT of one of my addresses in the event. */
const response = ref<string | null>(props.invite.my_response
  ?? props.invite.attendees.find(a => a.partstat && ANSWERS.includes(a.partstat) && isMine(a.address.email, props.accountId))?.partstat
  ?? null)
const sending = ref<Partstat | null>(null)

async function respond(partstat: Partstat) {
  sending.value = partstat
  try {
    await api.respondInvite(props.messageId, partstat)
    response.value = partstat
    const to = props.invite.organizer ? addressLabel(props.invite.organizer) : t('mail.invite.organizer')
    toast.add({ title: t('mail.invite.responseSent'), description: t('mail.invite.responseTo', { to }), icon: 'i-lucide-send' })
  } catch (e) {
    showError(e, t('mail.invite.responseFailed'))
  } finally {
    sending.value = null
  }
}

async function download() {
  const name = (props.invite.summary || 'invitation').slice(0, 60)
  try {
    if (await api.saveInvite(props.messageId, `${name}.ics`)) {
      toast.add({ title: t('mail.invite.eventSaved'), description: t('mail.invite.eventSavedDesc'), icon: 'i-lucide-calendar-plus' })
    }
  } catch (e) {
    showError(e)
  }
}

const GREEN = 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300'
const AMBER = 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300'
const RED = 'bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300'

const band = computed(() => {
  if (props.invite.cancelled) return { label: t('mail.invite.cancelled'), icon: 'i-lucide-calendar-x', tone: RED }
  if (state.value.kind === 'reply') return { label: t('mail.invite.reply'), icon: 'i-lucide-calendar', tone: 'bg-elevated text-toned' }
  if (!canAnswer.value) return { label: t('mail.invite.invitation'), icon: 'i-lucide-calendar', tone: color.value.soft }
  switch (response.value) {
    case 'ACCEPTED': return { label: t('mail.invite.band.accepted'), icon: 'i-lucide-circle-check', tone: GREEN }
    case 'TENTATIVE': return { label: t('mail.invite.band.tentative'), icon: 'i-lucide-circle-help', tone: AMBER }
    case 'DECLINED': return { label: t('mail.invite.band.declined'), icon: 'i-lucide-circle-x', tone: RED }
    default: return { label: t('mail.invite.band.pending'), icon: 'i-lucide-calendar', tone: color.value.soft }
  }
})

/** "Update · title": replaces the "this event has been changed" block of Google's e-mail. */
const updateTag = computed(() => {
  if (state.value.kind !== 'update') return null
  return state.value.change ? t('mail.invite.updatedWhat', { what: state.value.change }) : t('mail.invite.updated')
})

const sameDay = (a: Date, b: Date) => a.toDateString() === b.toDateString()
const hm = (d: Date, timeZone?: string) => dateFormat({ hour: '2-digit', minute: '2-digit', timeZone }).format(d)
const dayShort = (d: Date) => dateFormat({ day: 'numeric', month: 'short' }).format(d)

function duration(minutes: number): string {
  const h = Math.floor(minutes / 60)
  const m = minutes % 60
  if (!h) return t('mail.invite.durationM', { m })
  return m ? t('mail.invite.durationHM', { h, m: String(m).padStart(2, '0') }) : t('mail.invite.durationH', { h })
}

const span = computed(() => {
  const { start, end } = props.invite
  if (start === null) return null
  return { s: new Date(start * 1000), e: end !== null ? new Date(end * 1000) : null }
})

const when = computed(() => {
  if (!span.value) return null
  const { s, e } = span.value
  if (props.invite.all_day) {
    // iCalendar ends are exclusive: a one-day event ends at midnight the next day.
    const last = e ? new Date(e.getTime() - 1000) : s
    if (sameDay(s, last)) return { main: t('mail.invite.allDayOnly'), length: '' }
    const days = Math.round(((e?.getTime() ?? s.getTime()) - s.getTime()) / 86_400_000)
    return { main: t('mail.invite.range', { from: dayShort(s), to: dayShort(last) }), length: t('mail.invite.days', days) }
  }
  if (!e) return { main: hm(s), length: '' }
  const minutes = Math.round((e.getTime() - s.getTime()) / 60_000)
  if (sameDay(s, e)) return { main: `${hm(s)} – ${hm(e)}`, length: duration(minutes) }
  return {
    main: t('mail.invite.range', { from: `${dayShort(s)} ${hm(s)}`, to: `${dayShort(e)} ${hm(e)}` }),
    length: minutes < 24 * 60 ? duration(minutes) : t('mail.invite.days', Math.round(minutes / 1440))
  }
})

const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone
/** Hours are shown in the local zone. Named when the invitation gives its zone; when that zone
 *  reads differently, its own hours follow ("i.e. 02:00 – 05:00 New York time"). */
const zone = computed(() => {
  const tz = validZone(props.invite.tzid)
  if (!tz || !span.value || props.invite.all_day) return null
  const { s, e } = span.value
  const range = (z?: string) => (e ? `${hm(s, z)} – ${hm(e, z)}` : hm(s, z))
  if (tz === localZone || range(tz) === range()) return { label: t('mail.invite.zone', { city: zoneCity(tz) }), other: null }
  return { label: t('mail.invite.zone', { city: zoneCity(localZone) }), other: t('mail.invite.otherZoneTime', { time: range(tz), city: zoneCity(tz) }) }
})

const meeting = computed(() => meetingLink(props.invite))
const place = computed(() => {
  const l = props.invite.location?.trim()
  if (!l || (meeting.value && l.includes(meeting.value.url))) return null
  return l
})
const join = () => meeting.value && openUrl(meeting.value.url).catch(showError)

const notes = computed(() => inviteNotes(props.invite.description))
const notesOpen = ref(false)

const people = computed<InvitePerson[]>(() => {
  const org = props.invite.organizer?.email.toLowerCase()
  const list: InvitePerson[] = props.invite.attendees.map(a => ({
    address: a.address,
    status: a.partstat,
    organizer: a.address.email.toLowerCase() === org,
    me: isMine(a.address.email, props.accountId)
  }))
  // My row follows the answer just sent.
  for (const p of list) if (p.me && response.value) p.status = response.value
  if (props.invite.organizer && !list.some(p => p.organizer)) {
    list.unshift({ address: props.invite.organizer, status: null, organizer: true, me: isMine(org, props.accountId) })
  }
  // Organizer first, then me, then the others as invited.
  return list.map((p, i) => ({ p, i })).sort((a, b) => Number(b.p.organizer) - Number(a.p.organizer) || Number(b.p.me) - Number(a.p.me) || a.i - b.i).map(x => x.p)
})
const title = computed(() => props.invite.summary || t('mail.invite.untitled'))

const { hasCalendars, syncedSpan } = useCalendar()
/** The invitation's time, when it has one (all-day invitations are left out). A recurring one:
 *  its next occurrence not over yet (none known: nothing is said). */
const slot = computed(() => {
  const { all_day: allDay, recurring } = props.invite
  const start = recurring ? props.invite.next_start ?? null : props.invite.start
  const end = recurring ? props.invite.next_end ?? null : props.invite.end
  if (start === null || allDay || props.invite.cancelled) return null
  return { start, end: end !== null && end > start ? end : start + 3600, next: recurring && start !== props.invite.start }
})
/** The calendars only know the synced span: outside it, neither free nor busy is claimed. */
const known = computed(() => {
  const s = slot.value
  const span = syncedSpan.value
  return !!s && !!span && s.start >= span.from && s.end <= span.to
})
// The whole day, for the strip; conflicts are the events overlapping the invitation itself.
const { events: dayEvents } = useCalendarEvents(() => {
  if (!hasCalendars.value || !slot.value || !known.value) return null
  const day = dayStart(new Date(slot.value.start * 1000))
  return { from: unix(day), to: Math.max(unix(addDays(day, 1)), slot.value.end) }
}, 5000)
/** Other timed events of that day (long ones too); the invitation itself is often already in
 *  the calendar. */
const others = computed(() => (dayEvents.value ?? []).filter(e => e.uid !== props.invite.uid && e.status !== 'cancelled' && !e.all_day))
/** Those overlapping the invitation that take the time (not shown as free, not declined). */
const conflicts = computed(() => {
  const s = slot.value
  return s ? others.value.filter(e => e.busy !== false && e.start < s.end && e.end > s.start) : []
})
const availability = computed(() => {
  if (!slot.value || !known.value || dayEvents.value === null) return null
  const first = conflicts.value[0]
  if (!first) return { free: true, label: t('mail.invite.free') }
  const what = first.summary || t('tools.calendar.untitled')
  const more = conflicts.value.length - 1
  return { free: false, label: more ? t('mail.invite.conflictMore', { what, n: more }, more) : t('mail.invite.conflict', { what }) }
})
const nextLabel = computed(() => {
  const s = slot.value
  if (!s?.next) return null
  const d = new Date(s.start * 1000)
  return t('mail.invite.nextOccurrence', { date: `${dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(d)} · ${hm(d)}` })
})
</script>

<template>
  <section class="overflow-hidden rounded-[18px] border border-default bg-(--surface-card) lg:rounded-2xl lg:bg-(--surface-side)">
    <div class="flex min-h-9.5 items-center gap-2 border-b border-default/60 px-3.5 py-2 text-[12.5px] font-semibold lg:px-4" :class="band.tone">
      <UIcon :name="band.icon" class="size-3.75 shrink-0" />
      <span class="min-w-0 truncate">{{ band.label }}</span>
      <span class="flex-1" />
      <span v-if="invite.recurring" class="inline-flex shrink-0 items-center gap-1 text-[11.5px] font-semibold opacity-85">
        <UIcon name="i-lucide-repeat" class="size-3" />{{ $t('mail.invite.recurring') }}
      </span>
      <span v-if="updateTag" class="hidden max-w-[50%] shrink-0 items-center gap-1.25 truncate rounded-full px-2 py-0.5 text-[11.5px] font-semibold lg:inline-flex" :class="AMBER">
        <UIcon name="i-lucide-refresh-cw" class="size-3 shrink-0" />
        <span class="truncate">{{ updateTag }}</span>
      </span>
    </div>

    <div class="flex gap-3.5 px-3.5 pt-4 pb-3 lg:gap-4.5 lg:px-4 lg:pt-4.5 lg:pb-4">
      <InviteCalendarDay v-if="invite.start !== null" :start="invite.start" :band="color.solid" />
      <div class="flex min-w-0 flex-1 flex-col gap-1.5 lg:gap-2.5">
        <h3 class="text-lg leading-tight font-bold text-highlighted lg:text-xl" :class="{ 'line-through decoration-2': invite.cancelled }">{{ title }}</h3>

        <div class="flex flex-col gap-1.5 text-sm text-default lg:gap-1.75 lg:text-[13.5px]">
          <span v-if="when" class="flex items-start gap-2.25">
            <UIcon name="i-lucide-clock" class="mt-0.5 hidden size-4 shrink-0 text-muted lg:block" />
            <span>
              <b class="font-semibold text-highlighted">{{ when.main }}</b>
              <template v-if="when.length"> · {{ when.length }}</template>
              <span v-if="zone" class="text-dimmed"> · {{ zone.label }}</span>
              <span v-if="zone?.other" class="block text-xs text-dimmed lg:inline lg:text-[13.5px]"><span class="hidden lg:inline"> · </span>{{ zone.other }}</span>
            </span>
          </span>

          <span v-if="updateTag" class="inline-flex max-w-full items-center gap-1.25 self-start rounded-full px-2 py-0.5 text-[11.5px] font-semibold lg:hidden" :class="AMBER">
            <UIcon name="i-lucide-refresh-cw" class="size-3 shrink-0" />
            <span class="truncate">{{ updateTag }}</span>
          </span>

          <span v-if="meeting" class="hidden items-center gap-2.25 lg:flex">
            <UIcon name="i-lucide-video" class="size-4 shrink-0 text-muted" />
            <span class="min-w-0 flex-1 truncate">{{ meeting.provider }} · <span class="text-muted">{{ meeting.display }}</span></span>
            <button
              type="button"
              class="inline-flex h-7.5 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-[12.5px] font-semibold transition-opacity hover:opacity-85"
              :class="color.soft"
              @click="join"
            >
              {{ $t('mail.invite.join') }}
              <UIcon name="i-lucide-external-link" class="size-3.25" />
            </button>
          </span>

          <span v-if="place" class="flex items-start gap-2.25 text-toned">
            <UIcon name="i-lucide-map-pin" class="mt-0.5 size-4 shrink-0 text-muted" />
            <span class="min-w-0 break-words">{{ place }}</span>
          </span>

          <div v-if="invite.attendees.length" class="hidden lg:block">
            <InviteAttendees :people="people" :account-id="accountId" />
          </div>
          <span v-else-if="invite.organizer" class="flex items-center gap-2.25 text-toned">
            <UIcon name="i-lucide-user" class="size-4 shrink-0 text-muted" />
            {{ $t('mail.invite.organizedBy', { who: addressLabel(invite.organizer) }) }}
          </span>

          <p
            v-if="notes"
            role="button"
            tabindex="0"
            :aria-expanded="notesOpen"
            class="hidden cursor-pointer whitespace-pre-line text-[13px] text-toned lg:block"
            :class="notesOpen ? '' : 'line-clamp-3'"
            @click="notesOpen = !notesOpen"
            @keydown.enter.space.prevent="notesOpen = !notesOpen"
          >
            {{ notes }}
          </p>
        </div>
      </div>
    </div>

    <div class="flex flex-col lg:hidden">
      <button
        v-if="meeting"
        type="button"
        class="mx-3.5 flex min-h-12 items-center gap-2.5 rounded-xl px-3 py-1.5 text-left"
        :class="color.soft"
        @click="join"
      >
        <UIcon name="i-lucide-video" class="size-4.5 shrink-0" />
        <span class="flex min-w-0 flex-1 flex-col">
          <span class="text-sm font-semibold">{{ $t('mail.invite.joinProvider', { provider: meeting.provider }) }}</span>
          <span class="truncate text-xs opacity-85">{{ meeting.display }}</span>
        </span>
        <UIcon name="i-lucide-external-link" class="size-4 shrink-0" />
      </button>
      <InviteAttendees v-if="invite.attendees.length" :people="people" :account-id="accountId" rows class="px-3.5 pt-2 pb-1" />
      <p
        v-if="notes"
        role="button"
        tabindex="0"
        :aria-expanded="notesOpen"
        class="mx-3.5 mb-3 cursor-pointer whitespace-pre-line text-[13.5px] text-toned"
        :class="[notesOpen ? '' : 'line-clamp-4', !invite.attendees.length && 'mt-3']"
        @click="notesOpen = !notesOpen"
        @keydown.enter.space.prevent="notesOpen = !notesOpen"
      >
        {{ notes }}
      </p>
    </div>

    <div v-if="slot && availability" class="flex flex-col gap-2 px-3.5 pb-3 lg:px-4">
      <span
        class="inline-flex min-w-0 items-center gap-1.5 text-[13px] font-medium"
        :class="availability.free ? 'text-green-700 dark:text-green-400' : 'text-amber-700 dark:text-amber-400'"
      >
        <UIcon :name="availability.free ? 'i-lucide-calendar-check' : 'i-lucide-calendar-x'" class="size-4 shrink-0" />
        <span class="min-w-0 break-words">{{ availability.label }}</span>
      </span>
      <span v-if="nextLabel" class="text-xs text-dimmed">{{ nextLabel }}</span>
      <InviteDayStrip v-if="others.length" :start="slot.start" :end="slot.end" :events="others" :band="color.solid" />
    </div>

    <div
      class="flex flex-col gap-2.5 border-t border-default/60 bg-(--surface-side) px-3.5 pt-3 pb-3.5 lg:flex-row lg:items-center lg:gap-3 lg:bg-(--surface-card) lg:px-4 lg:py-3"
      :class="{ 'max-lg:mt-2': !invite.attendees.length && !notes }"
    >
      <template v-if="canAnswer">
        <span class="text-[12.5px] text-muted lg:text-[13px]">{{ $t('mail.invite.yourResponse') }}</span>
        <InviteAnswers :response="response" :sending="sending" @respond="respond" />
      </template>

      <span class="hidden flex-1 lg:block" />
      <UButton
        :label="$t('mail.invite.addToCalendar')"
        icon="i-lucide-calendar-plus"
        color="neutral"
        variant="ghost"
        class="h-11 justify-center rounded-xl border border-default bg-(--surface-card) text-sm font-medium lg:h-9.5 lg:rounded-[10px] lg:border-0 lg:bg-transparent lg:text-[13px]"
        @click="download"
      />
    </div>
  </section>
</template>
