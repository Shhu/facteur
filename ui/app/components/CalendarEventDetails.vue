<script setup lang="ts">
import { openUrl } from '@tauri-apps/plugin-opener'
import type { Partstat } from '~/components/InviteAnswers.vue'
import type { InvitePerson } from '~/components/InviteAttendees.vue'
import type { CalendarEvent, EventAnswer } from '~/utils/types'

const props = defineProps<{ event: CalendarEvent, touch?: boolean }>()
const emit = defineEmits<{ close: [], edit: [] }>()
const { accountById, isMine } = useMail()
const { calendarById, eventColor } = useCalendar()
const showError = useErrorToast()
const { t } = useI18n()

const e = computed(() => props.event)
const color = computed(() => eventColor(e.value))
const title = computed(() => e.value.summary.trim() || t('tools.calendar.untitled'))
const calendar = computed(() => calendarById(e.value.calendar_id))
const account = computed(() => accountById(e.value.account_id))

const meeting = computed(() => meetingLink(e.value))
const place = computed(() => {
  const l = e.value.location?.trim()
  if (!l || (meeting.value && l.includes(meeting.value.url))) return null
  return l
})
const placeIsUrl = computed(() => !!place.value && /^https?:\/\/\S+$/i.test(place.value))
const notes = computed(() => {
  const text = inviteNotes(e.value.description)
  return text ? linkify(text) : null
})
const notesOpen = ref(false)
watch(() => e.value.id, () => {
  notesOpen.value = false
})

/** "Name <address>", "mailto:address" or a bare address. */
const organizer = computed(() => {
  const o = e.value.organizer?.replace(/^mailto:/i, '').trim()
  if (!o) return null
  return o.replace(/\s*<[^>]+>$/, '') || o
})

const open = (url: string) => openUrl(url).catch(showError)
/** Links someone else wrote (description, location): anyone can put an event in your calendar by
 *  inviting you, so they ask first, like links of an untrustworthy message. Own events do not. */
const openLink = useOpenLink()
const ownEvent = computed(() => {
  const o = e.value.organizer?.replace(/^mailto:/i, '').trim()
  return !o || isMine(o.match(/<([^>]+)>$/)?.[1] ?? o)
})
const openWritten = (url: string, text?: string) => openLink(url, { text, cautious: !ownEvent.value && 'event' })
const openMap = () => place.value && open(`https://www.openstreetmap.org/search?query=${encodeURIComponent(place.value)}`)

const rowIcon = 'mt-0.5 size-4 shrink-0 text-muted'

const STATUS: Record<string, string> = { accepted: 'ACCEPTED', tentative: 'TENTATIVE', declined: 'DECLINED' }
const people = computed<InvitePerson[]>(() => e.value.attendees.map(a => ({
  address: { name: a.name ?? null, email: a.email },
  status: (a.response && STATUS[a.response]) || null,
  organizer: !!a.organizer,
  me: isMine(a.email)
})))

/** "10 min before", "1 h before", "1 day before". */
const reminders = computed(() => e.value.reminders.map((m) => {
  const [key, n] = reminderKey(m)
  return t(key, { n }, n)
}).join(', '))

/** The event's own time, when set in another zone than the computer's. */
const ownTime = computed(() => {
  const zone = validZone(e.value.zone)
  if (e.value.all_day || !zone || zone === localZone()) return null
  const time = new Date(e.value.start * 1000).toLocaleString(undefined, { timeZone: zone, weekday: 'short', hour: '2-digit', minute: '2-digit' })
  return t('tools.calendar.event.inZone', { time, zone: zoneCity(zone) })
})

/** Google and Outlook give their own event page; a CalDAV event's URL is whatever its author
 *  wrote, so it opens like the other links they wrote. */
const link = computed(() => {
  if (!e.value.url) return null
  switch (calendar.value?.source) {
    case 'google': return { label: t('tools.calendar.openGoogle'), provider: true }
    case 'microsoft': return { label: t('tools.calendar.openOutlook'), provider: true }
    default: return { label: t('tools.calendar.openLink'), provider: false }
  }
})
function openEventLink() {
  const url = e.value.url
  if (!url || !link.value) return
  if (link.value.provider) open(url)
  else openWritten(url)
}

/** Answering a series answers every date of it, unless asked otherwise. */
const wholeSeries = ref(true)
/** Answers on their way, by event: shared, so that a card closed and opened again meanwhile
 *  does not send a second one. */
const answering = useState<Record<number, Partstat>>('calendar-answering', () => ({}))
const sending = computed(() => answering.value[e.value.id] ?? null)
const toast = useToast()
const partstat = computed(() => (e.value.response && e.value.response !== 'needs_action' ? e.value.response.toUpperCase() : null))
const answerOf: Record<Partstat, EventAnswer> = { ACCEPTED: 'accepted', TENTATIVE: 'tentative', DECLINED: 'declined' }
async function respond(p: Partstat) {
  const id = e.value.id
  if (answering.value[id]) return
  answering.value = { ...answering.value, [id]: p }
  try {
    await api.respondEvent(e.value, answerOf[p], e.value.recurring && wholeSeries.value)
    toast.add({ title: t('mail.invite.responseSent'), description: title.value, icon: 'i-lucide-check' })
  } catch (err) {
    showError(err, t('mail.invite.responseFailed'))
  } finally {
    const { [id]: _, ...rest } = answering.value
    answering.value = rest
  }
}
</script>

<template>
  <div class="flex flex-col gap-3.5" :class="touch ? 'text-[15px]' : 'text-sm'">
    <div class="flex items-start gap-3">
      <span class="mt-1.5 size-3 shrink-0 rounded-[4px]" :style="{ background: color }" />
      <div class="flex min-w-0 flex-1 flex-col gap-1">
        <h2
          class="font-semibold leading-snug text-highlighted"
          :class="[touch ? 'text-lg' : 'text-base', e.status === 'cancelled' && 'line-through decoration-2']"
        >
          {{ title }}
        </h2>
        <span class="text-[13px] text-toned first-letter:uppercase">{{ eventWhen(e) }}</span>
        <span v-if="ownTime" class="flex items-center gap-1.5 text-[13px] text-muted">
          <UIcon name="i-lucide-globe" class="size-3.5 shrink-0" />
          {{ ownTime }}
        </span>
      </div>
      <UBadge v-if="e.status === 'tentative'" :label="t('tools.calendar.tentative')" color="warning" variant="subtle" class="shrink-0" />
      <UBadge v-else-if="e.status === 'cancelled'" :label="t('tools.calendar.cancelled')" color="error" variant="subtle" class="shrink-0" />
      <UButton
        v-if="!touch"
        icon="i-lucide-x"
        color="neutral"
        variant="ghost"
        size="sm"
        class="-me-1.5 -mt-1 shrink-0"
        :aria-label="t('app.overlay.close')"
        @click="emit('close')"
      />
    </div>

    <div class="flex flex-col gap-2.5 text-default">
      <span v-if="meeting" class="flex items-start gap-2.5">
        <UIcon name="i-lucide-video" :class="rowIcon" />
        <span class="min-w-0 flex-1 truncate">{{ meeting.provider }} · <span class="text-muted">{{ meeting.display }}</span></span>
        <UButton :label="t('mail.invite.join')" trailing-icon="i-lucide-external-link" size="xs" color="neutral" variant="outline" class="-my-0.5 shrink-0" @click="open(meeting.url)" />
      </span>

      <span v-if="place" class="flex items-start gap-2.5">
        <UIcon name="i-lucide-map-pin" :class="rowIcon" />
        <button v-if="placeIsUrl" type="button" class="min-w-0 break-all text-start text-primary hover:underline" @click="openWritten(place)">{{ place }}</button>
        <span v-else class="min-w-0 flex-1 break-words">{{ place }}</span>
        <UTooltip v-if="!placeIsUrl" :text="t('tools.calendar.openMap')">
          <UButton icon="i-lucide-map" size="xs" color="neutral" variant="ghost" class="-my-0.5 shrink-0" :aria-label="t('tools.calendar.openMap')" @click="openMap" />
        </UTooltip>
      </span>

      <InviteAttendees v-if="people.length" :people="people" :account-id="e.account_id" class="min-w-0" />
      <span v-else-if="organizer" class="flex items-start gap-2.5">
        <UIcon name="i-lucide-user" :class="rowIcon" />
        <span class="min-w-0 break-words">{{ t('mail.invite.organizedBy', { who: organizer }) }}</span>
      </span>

      <span v-if="reminders" class="flex items-start gap-2.5">
        <UIcon name="i-lucide-bell" :class="rowIcon" />
        <span class="min-w-0 break-words">{{ reminders }}</span>
      </span>

      <span class="flex items-start gap-2.5">
        <UIcon name="i-lucide-calendar" :class="rowIcon" />
        <span class="min-w-0 break-words">
          {{ calendar?.name ?? t('tools.calendar.title') }}<template v-if="account"> · <span class="text-muted">{{ accountName(account) }}</span></template>
        </span>
      </span>

      <div v-if="notes" class="cursor-pointer rounded-xl bg-elevated/50 px-3 py-2.5 text-[13px] text-toned" @click="notesOpen = !notesOpen">
        <p class="whitespace-pre-line break-words" :class="notesOpen ? '' : 'line-clamp-6'">
          <template v-for="(part, i) in notes" :key="i">
            <a v-if="part.href" :href="part.href" class="break-all text-primary hover:underline" @click.prevent.stop="openWritten(part.href, part.text)">{{ part.text }}</a>
            <template v-else>{{ part.text }}</template>
          </template>
        </p>
      </div>
    </div>

    <div v-if="e.respondable" class="flex flex-col gap-2">
      <span class="text-xs font-semibold text-muted">{{ t('mail.invite.yourResponse') }}</span>
      <InviteAnswers :response="partstat" :sending="sending" @respond="respond" />
      <USwitch v-if="e.recurring" v-model="wholeSeries" :label="t('tools.calendar.event.answerSeries')" size="sm" />
    </div>

    <div v-if="e.editable" class="flex items-center gap-2" :class="touch && 'flex-col items-stretch'">
      <UButton
        :label="t('tools.calendar.event.edit')"
        icon="i-lucide-pencil"
        color="neutral"
        variant="outline"
        :size="touch ? 'lg' : 'sm'"
        :class="touch ? 'min-h-11 justify-center rounded-xl' : 'rounded-[9px]'"
        @click="emit('edit')"
      />
    </div>

    <UButton
      v-if="link"
      :label="link.label"
      icon="i-lucide-external-link"
      color="neutral"
      variant="outline"
      :size="touch ? 'lg' : 'sm'"
      :block="touch"
      :class="touch ? 'min-h-11 justify-center rounded-xl' : 'self-start rounded-[9px]'"
      @click="openEventLink"
    />
  </div>
</template>
