<script setup lang="ts">
import type { CountedAddress } from '~/utils/types'

const props = defineProps<{
  items: CountedAddress[]
  color: string
}>()

const max = computed(() => Math.max(1, ...props.items.map(i => i.count)))
const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
</script>

<template>
  <ol class="flex flex-col gap-4">
    <li v-for="item in items" :key="item.email" class="flex items-center gap-3">
      <MailAvatar :address="{ name: item.name, email: item.email }" size="md" />
      <div class="flex min-w-0 flex-1 flex-col gap-1.5">
        <p class="flex min-w-0 items-baseline gap-2">
          <span class="truncate text-sm font-semibold text-highlighted">{{ item.name || item.email }}</span>
          <span v-if="item.name" class="truncate text-xs text-muted max-lg:hidden">{{ item.email }}</span>
        </p>
        <div class="flex items-center gap-2.5">
          <span class="h-2 flex-1 overflow-hidden rounded-full bg-elevated" aria-hidden="true">
            <span class="block h-full rounded-full" :style="{ width: `${(item.count / max) * 100}%`, minWidth: '4px', background: color }" />
          </span>
          <span class="min-w-6 shrink-0 text-right text-[13px] font-semibold tabular-nums text-highlighted">{{ fmt.format(item.count) }}</span>
        </div>
      </div>
    </li>
  </ol>
</template>
