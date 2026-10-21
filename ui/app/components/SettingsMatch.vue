<script setup lang="ts">
/** A block of a settings card built by hand, that the settings search treats as one row. */
const props = defineProps<{
  text: string
}>()
const shown = useSettingMatch(() => [props.text])
</script>

<template>
  <div v-show="shown">
    <slot />
  </div>
</template>
