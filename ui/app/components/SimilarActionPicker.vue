<script lang="ts">
export type SimilarAction = 'move' | 'delete'
</script>

<script setup lang="ts">
/** `block`: the two options share the full width (mobile). */
const model = defineModel<SimilarAction>({ required: true })
defineProps<{ block?: boolean }>()
const { t } = useI18n()

const options = computed(() => [
  { value: 'move' as const, label: t('tools.cleanup.move'), icon: 'i-lucide-folder-input' },
  { value: 'delete' as const, label: t('tools.cleanup.delete'), icon: 'i-lucide-trash-2' }
])
const buttons = useTemplateRef<HTMLButtonElement[]>('buttons')
function flip() {
  const next = model.value === 'move' ? 1 : 0
  model.value = options.value[next]!.value
  buttons.value?.[next]?.focus()
}
</script>

<template>
  <div
    role="radiogroup"
    :aria-label="t('tools.cleanup.action')"
    class="gap-0.5 rounded-[11px] bg-elevated p-[3px]"
    :class="block ? 'grid grid-cols-2 rounded-xl' : 'flex shrink-0'"
    @keydown.right.prevent="flip"
    @keydown.left.prevent="flip"
    @keydown.down.prevent="flip"
    @keydown.up.prevent="flip"
  >
    <button
      v-for="o in options"
      ref="buttons"
      :key="o.value"
      type="button"
      role="radio"
      :aria-checked="model === o.value"
      :tabindex="model === o.value ? 0 : -1"
      class="inline-flex items-center justify-center gap-1.75 font-semibold transition-colors focus-visible:outline-2 focus-visible:outline-primary"
      :class="[
        block ? 'h-10 rounded-[9px] text-sm' : 'h-9 rounded-lg px-3.5 text-[13.5px]',
        model !== o.value
          ? 'text-muted hover:text-default'
          : o.value === 'delete'
            ? 'bg-error/12 text-error dark:bg-error/16'
            : 'bg-default text-highlighted shadow-xs dark:bg-accented dark:shadow-none'
      ]"
      @click="model = o.value"
    >
      <UIcon :name="o.icon" class="size-4" />
      {{ o.label }}
    </button>
  </div>
</template>
