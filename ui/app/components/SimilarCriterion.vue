<script setup lang="ts">
/** Dimmed while unchecked but still usable: editing the field checks the criterion (see the page). */
const on = defineModel<boolean>({ required: true })
defineProps<{ label: string }>()
const slots = defineSlots<{ default?: () => unknown }>()
</script>

<template>
  <div class="flex flex-col gap-1.5">
    <SimilarCheck
      v-model="on"
      class="min-h-7 self-start text-[13.5px] font-semibold"
      :class="on ? 'text-highlighted' : 'text-default'"
    >
      {{ label }}
    </SimilarCheck>
    <div v-if="slots.default" class="ms-7.5 transition-opacity" :class="on ? '' : 'opacity-55'">
      <slot />
    </div>
  </div>
</template>
