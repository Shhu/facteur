<script setup lang="ts">
/** A checkbox drawn like UCheckbox, for inside a control that is itself the checkbox (a button
 *  with `aria-pressed` or `role="checkbox"`): UCheckbox is a button and can't nest in one. */
defineProps<{ checked: boolean }>()
</script>

<template>
  <span
    aria-hidden="true"
    class="flex size-4 shrink-0 items-center justify-center overflow-hidden rounded-sm ring ring-inset"
    :class="checked ? 'bg-primary ring-primary text-inverted' : 'bg-default ring-accented'"
  >
    <UIcon v-if="checked" name="i-lucide-check" class="size-3" />
  </span>
</template>
