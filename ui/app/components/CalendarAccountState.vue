<script setup lang="ts">
import type { Account, CalendarStatus } from '~/utils/types'

const props = defineProps<{ account: Account, status: CalendarStatus | null, inSettings?: boolean, touch?: boolean }>()
const emit = defineEmits<{ configure: [] }>()
const { reconnect, reconnecting, setIgnored } = useCalendar()
const { t } = useI18n()
const toast = useToast()

const state = computed(() => props.status?.state ?? 'ok')
const settingsLink = computed(() => `/settings/account/${props.account.id}`)

const view = computed(() => {
  switch (state.value) {
    case 'needs_reconnect': return { icon: 'i-lucide-key-round', tone: 'text-warning', text: t('tools.calendar.state.needsReconnect') }
    // The engine may say why (a server found elsewhere, whose address must be entered).
    case 'not_configured': return { icon: 'i-lucide-calendar-search', tone: 'text-muted', text: props.status?.message || t('tools.calendar.state.notConfigured') }
    case 'unavailable': return { icon: 'i-lucide-hourglass', tone: 'text-muted', text: t('tools.calendar.state.unavailable') }
    case 'partial': return { icon: 'i-lucide-triangle-alert', tone: 'text-warning', text: props.status?.message || t('tools.calendar.state.partial') }
    case 'error': return { icon: 'i-lucide-circle-alert', tone: 'text-error', text: props.status?.message || t('tools.calendar.state.error') }
    default: return null
  }
})
const size = computed(() => (props.touch ? 'md' : 'xs'))
const ignorable = computed(() => !props.inSettings && state.value !== 'partial')

/** The tooltip never shows on touch, and the account (maybe the calendar tab with it) leaves
 *  the screen: say where to turn it back on, and offer to undo. */
async function ignore() {
  const id = props.account.id
  if (!await setIgnored(id, true)) return
  toast.add({
    title: t('tools.calendar.state.ignored'),
    description: t('tools.calendar.state.ignoreHint'),
    icon: 'i-lucide-eye-off',
    actions: [{ label: t('common.undo'), icon: 'i-lucide-undo-2', size: 'xs', color: 'primary', variant: 'soft', onClick: () => { setIgnored(id, false) } }]
  })
}
</script>

<template>
  <div v-if="view" class="flex flex-col gap-2 rounded-xl border border-default bg-(--surface-side) px-3 py-2.5" :class="touch ? 'text-sm' : 'text-[12.5px]'">
    <span class="flex items-start gap-2 text-toned">
      <UIcon :name="view.icon" class="mt-0.5 size-3.75 shrink-0" :class="view.tone" />
      <span class="min-w-0 break-words">{{ view.text }}</span>
    </span>
    <div v-if="state === 'needs_reconnect' || state === 'not_configured' || state === 'error' || ignorable" class="flex flex-wrap gap-1.5">
      <UButton
        v-if="state === 'needs_reconnect' && account.auth.kind === 'oauth2'"
        :label="t('settings.accounts.reconnect')"
        icon="i-lucide-refresh-ccw"
        :size="size"
        color="neutral"
        variant="outline"
        class="rounded-lg bg-(--surface-card)"
        :loading="reconnecting === account.id"
        @click="reconnect(account)"
      />
      <template v-else-if="state === 'not_configured'">
        <UButton
          v-if="inSettings"
          :label="t('tools.calendar.state.configure')"
          icon="i-lucide-link"
          :size="size"
          color="neutral"
          variant="outline"
          class="rounded-lg bg-(--surface-card)"
          @click="emit('configure')"
        />
        <UButton
          v-else
          :label="t('tools.calendar.state.configure')"
          icon="i-lucide-settings"
          :size="size"
          color="neutral"
          variant="outline"
          class="rounded-lg bg-(--surface-card)"
          :to="settingsLink"
        />
      </template>
      <UButton
        v-else-if="state === 'error' && !inSettings"
        :label="t('tools.calendar.state.openSettings')"
        icon="i-lucide-settings"
        :size="size"
        color="neutral"
        variant="ghost"
        :to="settingsLink"
      />
      <UTooltip v-if="ignorable" :text="t('tools.calendar.state.ignoreHint')">
        <UButton
          :label="t('tools.calendar.state.ignore')"
          icon="i-lucide-eye-off"
          :size="size"
          color="neutral"
          variant="ghost"
          @click="ignore"
        />
      </UTooltip>
    </div>
  </div>
</template>
