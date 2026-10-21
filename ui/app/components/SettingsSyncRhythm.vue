<script setup lang="ts">
import type { Account } from '~/utils/types'

/** Setting `sync_every:<account>`, minutes "inbox,system,custom". A sync asked for by hand, and
 *  the return from sleep, always sync everything. */
const props = withDefaults(defineProps<{
  account: Account
  touch?: boolean
}>(), { touch: false })
const showError = useErrorToast()
const { t } = useI18n()

const DEFAULTS = [1, 5, 15]
const key = computed(() => `sync_every:${props.account.id}`)
const every = ref<number[]>([...DEFAULTS])

watch(key, async (k) => {
  const saved = (await api.getSetting(k).catch(() => null)) ?? ''
  every.value = DEFAULTS.map((d, i) => {
    const n = Number(saved.split(',')[i])
    return Number.isInteger(n) && n > 0 ? n : d
  })
}, { immediate: true })

async function set(index: number, minutes: number) {
  const next = every.value.slice()
  next[index] = minutes
  every.value = next
  await api.setSetting(key.value, next.join(',')).catch(showError)
}

/** Setting `single_connection:<account>`: for servers that cut sessions when a mailbox has
 *  several. No listening then: the inbox is checked at its rhythm. */
const singleKey = computed(() => `single_connection:${props.account.id}`)
const single = ref(false)
watch(singleKey, async (k) => {
  single.value = (await api.getSetting(k).catch(() => null)) === '1'
}, { immediate: true })
async function setSingle(on: boolean) {
  single.value = on
  await api.setSetting(singleKey.value, on ? '1' : '').catch(showError)
}

const label = (m: number) => (m < 60 ? t('settings.accounts.sync.minutes', { n: m }) : t('settings.accounts.sync.hours', { n: m / 60 }))
const rows = computed(() => [
  { title: t('settings.accounts.sync.inbox'), help: t('settings.accounts.sync.inboxHelp'), options: [1, 2, 5, 10, 15] },
  { title: t('settings.accounts.sync.system'), help: t('settings.accounts.sync.systemHelp'), options: [5, 10, 15, 30, 60] },
  { title: t('settings.accounts.sync.custom'), help: t('settings.accounts.sync.customHelp'), options: [15, 30, 60, 120, 360] }
].map((row, i) => ({ ...row, items: row.options.map(m => ({ label: label(m), value: m })), value: every.value[i] })))
</script>

<template>
  <div class="flex flex-col gap-3">
    <div class="flex flex-col gap-0.5">
      <span class="font-semibold text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ $t('settings.accounts.sync.label') }}</span>
      <span class="text-[13px] text-muted">{{ $t('settings.accounts.sync.help') }}</span>
    </div>
    <div v-for="(row, i) in rows" :key="row.title" class="flex flex-wrap items-center gap-x-4 gap-y-1.5">
      <div class="flex min-w-48 flex-1 flex-col">
        <span class="text-[13px] font-medium text-default">{{ row.title }}</span>
        <span class="text-xs text-muted">{{ row.help }}</span>
      </div>
      <USelect
        :model-value="row.value"
        :items="row.items"
        :size="touch ? 'lg' : 'sm'"
        class="w-28"
        :aria-label="row.title"
        @update:model-value="(v: number) => set(i, v)"
      />
    </div>
    <div class="flex items-center gap-x-4">
      <div class="flex min-w-48 flex-1 flex-col">
        <span class="text-[13px] font-medium text-default">{{ $t('settings.accounts.sync.single') }}</span>
        <span class="text-xs text-muted">{{ $t('settings.accounts.sync.singleHelp') }}</span>
      </div>
      <USwitch :model-value="single" :aria-label="$t('settings.accounts.sync.single')" @update:model-value="setSingle" />
    </div>
  </div>
</template>
