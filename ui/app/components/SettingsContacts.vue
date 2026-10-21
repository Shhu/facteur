<script setup lang="ts">
import type { Account, AddressBookStatus } from '~/utils/types'

const props = withDefaults(defineProps<{
  account: Account
  touch?: boolean
}>(), { touch: false })
const { calendarTick, reconnect, reconnecting } = useCalendar()
const synced = useSyncedLabel()
const showError = useErrorToast()
const { t } = useI18n()

const status = ref<AddressBookStatus | null>(null)
const syncing = ref(false)

async function load() {
  try {
    status.value = (await api.addressBookStatus()).find(s => s.account_id === props.account.id) ?? null
  } catch {
    // Older engine, or none yet: nothing to show.
  }
}
// The engine emits `calendar_changed` after an address book sync too.
watch(calendarTick, load, { immediate: true })

async function sync() {
  if (syncing.value) return
  syncing.value = true
  try {
    await api.syncAddressBooks(props.account.id)
    await load()
  } catch (e) {
    showError(e)
  } finally {
    syncing.value = false
  }
}

const provider = computed(() => (props.account.auth.kind === 'oauth2' ? props.account.auth.provider : null))
const help = computed(() => t(provider.value === 'google' ? 'settings.accounts.contacts.helpGoogle' : provider.value === 'microsoft' ? 'settings.accounts.contacts.helpMicrosoft' : 'settings.accounts.contacts.helpCarddav'))
const line = computed(() => {
  const s = status.value
  if (syncing.value) return t('common.syncing')
  if (!s) return t('settings.accounts.contacts.notYet')
  if (s.state === 'ok') return `${t('settings.accounts.contacts.count', { n: s.contacts }, s.contacts)} · ${synced(s.last_sync)}`
  // No address book here (most mail servers have none): said quietly, nothing to fix.
  if (s.state === 'not_configured' || s.state === 'unavailable') return s.message
  return null
})
const problem = computed(() => (status.value && (status.value.state === 'needs_reconnect' || status.value.state === 'error') && !syncing.value ? status.value : null))
</script>

<template>
  <div class="flex flex-col gap-3">
    <div class="flex items-center gap-3">
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="font-semibold text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ $t('settings.accounts.contacts.title') }}</span>
        <span class="text-[13px] text-muted">{{ help }}</span>
        <span v-if="line" class="text-xs text-dimmed">{{ line }}</span>
      </div>
      <UTooltip :text="$t('settings.accounts.contacts.sync')">
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="ghost"
          :size="touch ? 'lg' : 'sm'"
          :class="touch ? 'size-11 justify-center' : ''"
          :loading="syncing"
          :aria-label="$t('settings.accounts.contacts.sync')"
          @click="sync"
        />
      </UTooltip>
    </div>

    <UAlert
      v-if="problem"
      :description="problem.message ?? undefined"
      :color="problem.state === 'error' ? 'error' : 'neutral'"
      variant="subtle"
      icon="i-lucide-contact"
      :actions="problem.state === 'needs_reconnect' && account.auth.kind === 'oauth2'
        ? [{ label: $t('settings.accounts.contacts.reconnect'), color: 'neutral', variant: 'outline', loading: reconnecting === account.id, onClick: () => reconnect(account) }]
        : []"
    />
  </div>
</template>
