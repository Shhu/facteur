<script setup lang="ts" generic="T extends string">
const props = defineProps<{
  items: { label: string, value: T, icon?: string }[]
  label: string
}>()
const model = defineModel<T>({ required: true })

const buttons = useTemplateRef<HTMLButtonElement[]>('buttons')

function move(step: number) {
  const i = props.items.findIndex(o => o.value === model.value)
  const next = (i + step + props.items.length) % props.items.length
  model.value = props.items[next]!.value
  buttons.value?.[next]?.focus()
}
</script>

<template>
  <div
    role="radiogroup"
    :aria-label="label"
    class="flex shrink-0 gap-0.5 rounded-[10px] bg-elevated p-[3px]"
    @keydown.right.prevent="move(1)"
    @keydown.down.prevent="move(1)"
    @keydown.left.prevent="move(-1)"
    @keydown.up.prevent="move(-1)"
  >
    <button
      v-for="o in items"
      ref="buttons"
      :key="o.value"
      type="button"
      role="radio"
      :aria-checked="model === o.value"
      :tabindex="model === o.value ? 0 : -1"
      class="inline-flex h-8 items-center gap-1.5 rounded-[7px] px-3 text-[13px] transition-colors focus-visible:outline-2 focus-visible:outline-primary"
      :class="model === o.value
        ? 'bg-default font-semibold text-highlighted shadow-xs dark:bg-accented dark:shadow-none'
        : 'font-medium text-muted hover:text-default'"
      @click="model = o.value"
    >
      <UIcon v-if="o.icon" :name="o.icon" class="size-4" />
      {{ o.label }}
    </button>
  </div>
</template>
