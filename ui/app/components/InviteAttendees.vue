<script setup lang="ts">
import type { Address } from '~/utils/types'

export interface InvitePerson {
  address: Address
  /** PARTSTAT: `ACCEPTED`, `TENTATIVE`, `DECLINED`, `DELEGATED`, or null (no reply yet). */
  status: string | null
  organizer: boolean
  me: boolean
}

const props = defineProps<{ people: InvitePerson[], accountId: number, rows?: boolean }>()
const { t } = useI18n()

const STACKED = 5
const FIRST_ROWS = 4
const open = ref(false)

type Look = { label: string, tag: string, dot: string, icon: string | null }
const looks = computed<Record<string, Look>>(() => ({
  ACCEPTED: { label: t('mail.invite.tag.accepted'), tag: 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300', dot: 'bg-green-600', icon: 'i-lucide-check' },
  TENTATIVE: { label: t('mail.invite.tag.tentative'), tag: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300', dot: 'bg-amber-600', icon: null },
  DECLINED: { label: t('mail.invite.tag.declined'), tag: 'bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300', dot: 'bg-red-600', icon: 'i-lucide-x' },
  DELEGATED: { label: t('mail.invite.tag.delegated'), tag: 'bg-elevated text-toned', dot: 'bg-neutral-400 dark:bg-neutral-500', icon: null },
  NONE: { label: t('mail.invite.tag.none'), tag: 'bg-elevated text-muted', dot: 'bg-neutral-400 dark:bg-neutral-600', icon: null }
}))
/** An organizer missing from the attendees has no status to show. */
const look = (p: InvitePerson): Look | null => (p.status && looks.value[p.status]) || (p.organizer && !p.status ? null : looks.value.NONE!)

const role = (p: InvitePerson) => (p.me ? t('mail.invite.you') : p.organizer ? t('mail.invite.organizes') : '')

const summary = computed(() => {
  const count = (s: string | null) => props.people.filter(p => p.status === s).length
  const parts = [t('mail.invite.guests', props.people.length)]
  const yes = count('ACCEPTED')
  const maybe = count('TENTATIVE')
  const no = count('DECLINED')
  const none = props.people.filter(p => !p.status || p.status === 'NEEDS-ACTION').filter(p => !(p.organizer && !p.status)).length
  if (yes) parts.push(t('mail.invite.acceptCount', yes))
  if (maybe) parts.push(t('mail.invite.maybeCount', maybe))
  if (no) parts.push(t('mail.invite.declineCount', no))
  if (none) parts.push(t('mail.invite.noReplyCount', none))
  return parts.join(' · ')
})

const shown = computed(() => (open.value || props.people.length <= FIRST_ROWS + 1 ? props.people : props.people.slice(0, FIRST_ROWS)))
</script>

<template>
  <div v-if="!rows" class="flex flex-col gap-2">
    <div class="flex items-center gap-2.25">
      <UIcon name="i-lucide-users" class="size-4 shrink-0 text-muted" />
      <span class="flex shrink-0">
        <span
          v-for="(p, i) in people.slice(0, STACKED)"
          :key="p.address.email"
          class="relative -me-1.5 rounded-full ring-2 ring-(--surface-side)"
          :style="{ zIndex: STACKED - i }"
        >
          <MailAvatar :address="p.address" :account-id="accountId" class="size-7! text-[10px]!" />
          <span
            v-if="look(p)"
            class="absolute -end-0.75 -bottom-0.75 flex size-3.25 items-center justify-center rounded-full text-white ring-2 ring-(--surface-side)"
            :class="look(p)!.dot"
          >
            <UIcon v-if="look(p)!.icon" :name="look(p)!.icon!" class="size-2" />
            <span v-else-if="p.status === 'TENTATIVE'" class="text-[8px] leading-none font-bold">?</span>
          </span>
        </span>
        <span
          v-if="people.length > STACKED"
          class="relative flex size-7 items-center justify-center rounded-full bg-elevated text-[10px] font-bold text-muted ring-2 ring-(--surface-side)"
        >+{{ people.length - STACKED }}</span>
      </span>
      <span class="ms-2.5 min-w-0 truncate">{{ summary }}</span>
      <button
        type="button"
        class="ms-auto h-7 shrink-0 rounded-[7px] px-2 text-[12.5px] font-semibold text-primary hover:bg-primary/10"
        :aria-expanded="open"
        @click="open = !open"
      >
        {{ open ? $t('mail.invite.hideAll') : $t('mail.invite.seeAll') }}
      </button>
    </div>
    <ul v-if="open" class="ms-6.25 flex flex-col gap-0.5 rounded-[10px] border border-default bg-(--surface-card) p-1.5">
      <li v-for="p in people" :key="p.address.email" class="flex items-center gap-2.5 px-2 py-1.5 text-[13px]">
        <MailAvatar :address="p.address" :account-id="accountId" class="size-6! text-[9px]!" />
        <span class="min-w-0 flex-1 truncate text-highlighted" :title="p.address.email">
          {{ addressLabel(p.address) }}<span v-if="role(p)" class="text-muted"> · {{ role(p) }}</span>
        </span>
        <span v-if="look(p)" class="shrink-0 rounded-full px-2 py-px text-[11.5px] font-semibold" :class="look(p)!.tag">{{ look(p)!.label }}</span>
      </li>
    </ul>
  </div>

  <ul v-else class="flex flex-col">
    <li
      v-for="(p, i) in shown"
      :key="p.address.email"
      class="flex min-h-11 items-center gap-2.5"
      :class="i > 0 && 'border-t border-default/60'"
    >
      <MailAvatar :address="p.address" :account-id="accountId" class="size-7! text-[10px]!" />
      <span class="min-w-0 flex-1 truncate text-[13.5px] text-highlighted">
        {{ addressLabel(p.address) }}<span v-if="role(p)" class="text-muted"> · {{ role(p) }}</span>
      </span>
      <span v-if="look(p)" class="shrink-0 rounded-full px-2 py-px text-[11.5px] font-semibold" :class="look(p)!.tag">{{ look(p)!.label }}</span>
    </li>
    <li v-if="shown.length < people.length || open && people.length > FIRST_ROWS + 1" class="border-t border-default/60">
      <button type="button" class="flex min-h-11 w-full items-center gap-2 text-[13.5px] font-semibold text-primary" :aria-expanded="open" @click="open = !open">
        <UIcon :name="open ? 'i-lucide-chevron-up' : 'i-lucide-chevron-down'" class="size-4" />
        {{ open ? $t('mail.invite.hideAll') : $t('mail.invite.seeAllCount', { n: people.length }) }}
      </button>
    </li>
  </ul>
</template>
