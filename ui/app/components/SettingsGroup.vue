<script setup lang="ts">
const props = defineProps<{
  title: string
  /** Other words the settings search finds the whole group by. */
  keywords?: string
}>()
const id = useId()
const shown = useSettingsSection(() => [props.title, props.keywords])
</script>

<template>
  <section v-show="shown" :aria-labelledby="id" class="flex flex-col gap-2">
    <h2 :id="id" class="mx-1.5 text-xs font-bold uppercase tracking-[0.06em] text-muted">{{ title }}</h2>
    <div class="divide-y divide-default overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none">
      <slot />
    </div>
  </section>
</template>
