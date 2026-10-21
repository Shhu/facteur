<script setup lang="ts">
import { QUOTA_WARNING, type Quota } from '~/utils/types'

const props = defineProps<{ quota: Quota }>()

const share = computed(() => Math.min(1, props.quota.used / props.quota.limit))
const percent = computed(() => new Intl.NumberFormat(intlLocale(), { style: 'percent', maximumFractionDigits: 0 }).format(share.value))
const full = computed(() => share.value >= QUOTA_WARNING)
const tone = computed(() => (share.value >= 0.98 ? 'bg-error' : full.value ? 'bg-warning' : 'bg-primary'))
</script>

<template>
  <div class="flex flex-col gap-2.5">
    <div class="flex items-baseline gap-2">
      <span class="flex-1 text-sm text-highlighted">
        {{ $t('settings.accounts.quota') }} ·
        <b class="font-semibold">{{ $t('settings.accounts.quotaUsed', { used: fileSize(quota.used), limit: fileSize(quota.limit) }) }}</b>
      </span>
      <span class="text-[13px] font-bold" :class="full ? 'text-warning' : 'text-muted'">{{ percent }}</span>
    </div>
    <div
      class="h-2 overflow-hidden rounded-full bg-elevated"
      role="meter"
      :aria-valuenow="Math.round(share * 100)"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-label="$t('settings.accounts.quota')"
    >
      <div class="h-full rounded-full transition-[width]" :class="tone" :style="{ width: `${share * 100}%` }" />
    </div>
    <span class="text-[12.5px]" :class="full ? 'text-warning' : 'text-muted'">
      {{ full ? $t('settings.accounts.quotaFull') : $t('settings.accounts.quotaHelp') }}
    </span>
  </div>
</template>
