<script setup lang="ts">
import type { Account } from '~/utils/types'

const props = withDefaults(defineProps<{
  account: Account
  touch?: boolean
}>(), { touch: false })
const { calendarsOf, statusOf, isIgnored, calendarColor, setEnabled, setIgnored, sync, isSyncing, setCaldavUrl, lastSync } = useCalendar()
const synced = useSyncedLabel()
const toast = useToast()
const { t } = useI18n()

const status = computed(() => statusOf(props.account.id))
const list = computed(() => calendarsOf(props.account.id))
const provider = computed(() => (props.account.auth.kind === 'oauth2' ? props.account.auth.provider : null))
/** Password accounts reach their calendars over CalDAV. */
const caldav = computed(() => provider.value === null)
const help = computed(() => t(provider.value === 'google' ? 'settings.accounts.calendar.helpGoogle' : provider.value === 'microsoft' ? 'settings.accounts.calendar.helpMicrosoft' : 'settings.accounts.calendar.helpCaldav'))
const syncing = computed(() => isSyncing(props.account.id))
const used = computed(() => !isIgnored(props.account.id))
const syncedLabel = computed(() => synced(lastSync(props.account.id)))

const url = ref('')
watch(() => status.value?.caldav_url, (u) => {
  url.value = u ?? ''
}, { immediate: true })
const changed = computed(() => url.value.trim() !== (status.value?.caldav_url ?? ''))
const saving = ref(false)
const field = useTemplateRef<{ inputRef?: HTMLInputElement }>('field')

async function save() {
  if (saving.value) return
  saving.value = true
  if (await setCaldavUrl(props.account.id, url.value)) {
    toast.add({ title: t('settings.accounts.calendar.saved'), icon: 'i-lucide-check' })
  }
  saving.value = false
}
</script>

<template>
  <div class="flex flex-col gap-3">
    <div class="flex items-center gap-3">
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="font-semibold text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ $t('settings.accounts.calendar.title') }}</span>
        <span class="text-[13px] text-muted">{{ used ? help : $t('settings.accounts.calendar.ignored') }}</span>
        <span v-if="used && (status?.state === 'ok' || status?.state === 'partial')" class="text-xs text-dimmed">{{ syncing ? $t('common.syncing') : syncedLabel }}</span>
      </div>
      <UTooltip v-if="used" :text="$t('settings.accounts.calendar.sync')">
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="ghost"
          :size="touch ? 'lg' : 'sm'"
          :class="touch ? 'size-11 justify-center' : ''"
          :loading="syncing"
          :aria-label="$t('settings.accounts.calendar.sync')"
          @click="sync(account.id)"
        />
      </UTooltip>
      <USwitch :model-value="used" :aria-label="$t('settings.accounts.calendar.use')" @update:model-value="(v: boolean) => setIgnored(account.id, !v)" />
    </div>

    <template v-if="used">
      <CalendarAccountState :account="account" :status="status" in-settings :touch="touch" @configure="field?.inputRef?.focus()" />

      <template v-if="list.length">
        <span class="text-xs font-bold uppercase tracking-wider text-muted">{{ $t('settings.accounts.calendar.shown') }}</span>
        <ul class="divide-y divide-default overflow-hidden rounded-xl border border-default">
          <li v-for="c in list" :key="c.id" class="flex items-center gap-3 px-3.5" :class="touch ? 'min-h-13' : 'min-h-11'">
            <span class="size-3 shrink-0 rounded-[4px]" :style="{ background: calendarColor(c.id) }" />
            <span class="min-w-0 flex-1 truncate text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ c.name }}</span>
            <UTooltip v-if="c.error" :text="c.error">
              <UIcon name="i-lucide-triangle-alert" class="size-3.5 shrink-0 text-warning" :aria-label="c.error" />
            </UTooltip>
            <UIcon v-if="c.read_only" name="i-lucide-lock" class="size-3.5 shrink-0 text-dimmed" :aria-label="$t('tools.calendar.readOnly')" />
            <USwitch :model-value="c.enabled" :aria-label="c.name" @update:model-value="(v: boolean) => setEnabled(c, v)" />
          </li>
        </ul>
      </template>

      <form v-if="caldav" class="flex flex-col gap-1.5" @submit.prevent="save">
        <label :for="`caldav-${account.id}`" class="text-xs font-bold uppercase tracking-wider text-muted">{{ $t('settings.accounts.calendar.caldavUrl') }}</label>
        <div class="flex gap-2">
          <UInput
            :id="`caldav-${account.id}`"
            ref="field"
            v-model="url"
            type="url"
            inputmode="url"
            autocomplete="off"
            spellcheck="false"
            :placeholder="$t('settings.accounts.calendar.caldavPlaceholder')"
            :size="touch ? 'lg' : 'md'"
            class="min-w-0 flex-1"
            :ui="{ base: touch ? 'h-11 rounded-xl' : 'h-9.5 rounded-[10px]' }"
          />
          <UButton
            type="submit"
            :label="$t('settings.accounts.calendar.save')"
            color="neutral"
            variant="outline"
            :size="touch ? 'lg' : 'md'"
            :class="touch ? 'min-h-11 rounded-xl' : 'rounded-[10px]'"
            :loading="saving"
            :disabled="!changed || saving"
          />
        </div>
        <span class="text-xs text-dimmed">{{ $t('settings.accounts.calendar.caldavHint') }}</span>
      </form>
    </template>
  </div>
</template>
