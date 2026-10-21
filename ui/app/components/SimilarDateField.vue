<script setup lang="ts">
/** `YYYY-MM-DD`, empty for no bound; opens the system date picker. */
const model = defineModel<string>({ required: true })
const props = withDefaults(defineProps<{
  placeholder: string
  label: string
  min?: string
  max?: string
  size?: 'md' | 'lg'
}>(), { min: undefined, max: undefined, size: 'md' })
const { t } = useI18n()

const input = useTemplateRef<HTMLInputElement>('input')
const text = computed(() => {
  if (!model.value) return props.placeholder
  const [y, m, d] = model.value.split('-').map(Number)
  return dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(new Date(y!, m! - 1, d!))
})

function open() {
  const el = input.value
  if (!el) return
  try {
    el.showPicker()
  } catch {
    // Older web views: focusing the field still lets the user type the date.
    el.focus()
  }
}
</script>

<template>
  <div
    class="relative flex min-w-0 flex-1 items-center rounded-[10px] bg-elevated/60 ring-1 ring-inset ring-default transition-colors focus-within:ring-primary"
    :class="size === 'lg' ? 'h-12' : 'h-10'"
  >
    <button
      type="button"
      class="flex h-full min-w-0 flex-1 items-center gap-2 rounded-[10px] ps-3 text-start text-[13px] focus-visible:outline-2 focus-visible:outline-primary"
      :class="[model ? 'text-highlighted' : 'text-muted', model ? 'pe-1' : 'pe-3']"
      :aria-label="`${label} · ${text}`"
      @click="open"
    >
      <UIcon name="i-lucide-calendar" class="size-4 shrink-0" />
      <span class="truncate">{{ text }}</span>
    </button>
    <UButton
      v-if="model"
      icon="i-lucide-x"
      size="xs"
      color="neutral"
      variant="ghost"
      class="me-1.5"
      :class="size === 'lg' ? 'size-9 justify-center' : ''"
      :aria-label="t('tools.cleanup.clearDate')"
      @click="model = ''"
    />
    <input
      ref="input"
      v-model="model"
      type="date"
      tabindex="-1"
      aria-hidden="true"
      :min="min"
      :max="max"
      class="pointer-events-none absolute inset-0 opacity-0"
    >
  </div>
</template>
