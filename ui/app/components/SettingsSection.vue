<script setup lang="ts">
/** `id` is the anchor of the section menu. */
const props = defineProps<{
  id: string
  title: string
  description: string
  icon: string
  tone: string
  /** Other words the settings search finds the whole section by. */
  keywords?: string
}>()
const shown = useSettingsSection(() => [props.title, props.keywords], props.id)
</script>

<template>
  <section v-show="shown" :id="id" :aria-labelledby="`${id}-title`" class="flex scroll-mt-8 flex-col gap-4">
    <div class="flex items-center gap-3.5">
      <span class="flex size-10.5 shrink-0 items-center justify-center rounded-xl" :class="tone">
        <UIcon :name="icon" class="size-5" />
      </span>
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <h2 :id="`${id}-title`" class="text-[19px] font-bold text-highlighted">{{ title }}</h2>
        <p class="text-[13px] text-muted">{{ description }}</p>
      </div>
      <slot name="actions" />
    </div>
    <slot />
  </section>
</template>
