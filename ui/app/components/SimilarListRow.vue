<script setup lang="ts">
/** Without an editor (`edit`), tapping the row toggles the checkbox. */
const checked = defineModel<boolean | undefined>({ default: undefined })
const props = defineProps<{
  label: string
  value?: string
  icon?: string
  editable?: boolean
}>()
const emit = defineEmits<{ edit: [] }>()

const on = computed(() => checked.value !== false)
function tap() {
  if (props.editable) emit('edit')
  else if (checked.value !== undefined) checked.value = !checked.value
}
</script>

<template>
  <div class="flex min-h-15 items-center gap-3 px-3.5 py-2 active:bg-elevated/60" @click="tap">
    <SimilarCheck
      v-if="checked !== undefined"
      :model-value="checked"
      touch
      :label="label"
      @update:model-value="(v: boolean) => { checked = v }"
    />
    <span v-else class="flex size-5.5 shrink-0 items-center justify-center text-muted">
      <UIcon :name="icon ?? 'i-lucide-calendar'" class="size-5" />
    </span>
    <component
      :is="editable ? 'button' : 'div'"
      :type="editable ? 'button' : undefined"
      class="flex min-w-0 flex-1 flex-col gap-0.5 ps-1 text-start"
    >
      <span class="text-[14.5px] font-semibold" :class="on ? 'text-highlighted' : 'text-default'">{{ label }}</span>
      <span v-if="value" class="truncate text-[12.5px]" :class="on ? 'text-default' : 'text-dimmed'">{{ value }}</span>
    </component>
    <UIcon v-if="editable" name="i-lucide-chevron-right" class="size-4.5 shrink-0 text-dimmed" />
    <slot />
  </div>
</template>
