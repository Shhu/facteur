<script setup lang="ts">
const props = defineProps<{
  items: { label: string, detail?: string, value: number }[]
  color?: string
}>()

const max = computed(() => Math.max(1, ...props.items.map(i => i.value)))
const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
</script>

<template>
  <ol class="viz-root space-y-2.5">
    <li v-for="item in items" :key="item.label + (item.detail ?? '')" class="text-sm">
      <div class="mb-1 flex items-baseline gap-2">
        <span class="truncate text-highlighted">{{ item.label }}</span>
        <span v-if="item.detail" class="truncate text-xs text-dimmed">{{ item.detail }}</span>
      </div>
      <div class="flex items-center gap-2">
        <div class="h-2 flex-1">
          <div
            class="h-2 rounded-r-[4px]"
            :style="{ width: `${(item.value / max) * 100}%`, background: color ?? 'var(--viz-series-1)', minWidth: '2px' }"
          />
        </div>
        <span class="w-10 shrink-0 text-right text-xs tabular-nums text-muted">{{ fmt.format(item.value) }}</span>
      </div>
    </li>
  </ol>
</template>
