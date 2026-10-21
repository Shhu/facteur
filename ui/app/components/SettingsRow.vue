<script setup lang="ts">
const props = defineProps<{
  label: string
  description?: string
  /** Other words the settings search finds it by. */
  keywords?: string
}>()
const shown = useSettingMatch(() => [props.label, props.description, props.keywords])
</script>

<template>
  <div v-show="shown" class="flex flex-col gap-3.5 px-5.5 py-4.5">
    <div class="flex items-center gap-x-6 gap-y-3 max-xl:flex-wrap">
      <slot name="leading" />
      <div class="flex min-w-60 flex-1 flex-col gap-0.5">
        <span class="flex items-baseline gap-2.5 text-sm font-semibold text-highlighted">
          {{ label }}
          <slot name="hint" />
        </span>
        <span v-if="description" class="text-[13px] text-muted">{{ description }}</span>
      </div>
      <slot />
    </div>
    <slot name="below" />
  </div>
</template>
