<script setup lang="ts">
const props = defineProps<{ accountId: number }>()

/** Below this, the sync is too quick for a bar to make sense. */
const THRESHOLD = 100

const { syncProgress } = useMail()
const p = computed(() => {
  const value = syncProgress.value[props.accountId]
  return value && value.total > THRESHOLD ? value : null
})
const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
</script>

<template>
  <div v-if="p" class="mt-1 space-y-1">
    <UProgress :model-value="p.done" :max="p.total" size="xs" />
    <p class="truncate text-xs text-muted">{{ p.mailbox }} · {{ fmt.format(p.done) }} / {{ fmt.format(p.total) }}</p>
  </div>
</template>
