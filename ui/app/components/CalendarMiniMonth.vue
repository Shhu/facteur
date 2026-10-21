<script setup lang="ts">
/** Highlights the days shown in the main view, `from` to `to` (exclusive). */
const props = defineProps<{ from: Date, to: Date }>()
const date = defineModel<Date>({ required: true })
const now = useNow()
const { t } = useI18n()

const month = ref(new Date(date.value.getFullYear(), date.value.getMonth(), 1))
watch(date, (d) => {
  month.value = new Date(d.getFullYear(), d.getMonth(), 1)
})
const days = computed(() => monthGrid(month.value))
const title = computed(() => {
  const label = dateFormat({ month: 'long', year: 'numeric' }).format(month.value)
  return label.charAt(0).toUpperCase() + label.slice(1)
})
const weekdays = computed(() => days.value.slice(0, 7).map(d => dateFormat({ weekday: 'narrow' }).format(d)))
const today = computed(() => dayStart(new Date(now.value)))
const inRange = (d: Date) => d >= props.from && d < props.to
const move = (n: number) => {
  month.value = new Date(month.value.getFullYear(), month.value.getMonth() + n, 1)
}
</script>

<template>
  <div class="flex flex-col gap-1.5">
    <div class="flex items-center gap-1">
      <span class="min-w-0 flex-1 truncate ps-1 text-[13px] font-semibold text-highlighted">{{ title }}</span>
      <UButton icon="i-lucide-chevron-left" color="neutral" variant="ghost" size="xs" :aria-label="t('tools.calendar.previousMonth')" @click="move(-1)" />
      <UButton icon="i-lucide-chevron-right" color="neutral" variant="ghost" size="xs" :aria-label="t('tools.calendar.nextMonth')" @click="move(1)" />
    </div>
    <div class="grid grid-cols-7 text-center text-[11px]">
      <span v-for="(w, i) in weekdays" :key="i" class="py-0.5 font-semibold text-dimmed">{{ w }}</span>
      <button
        v-for="d in days"
        :key="dayKey(d)"
        type="button"
        class="mx-auto my-px flex size-6.5 items-center justify-center rounded-full tabular-nums transition-colors"
        :class="[
          sameDay(d, today) ? 'bg-primary font-bold text-inverted'
          : inRange(d) ? 'bg-primary/12 font-semibold text-highlighted'
            : d.getMonth() === month.getMonth() ? 'text-default hover:bg-elevated' : 'text-dimmed hover:bg-elevated'
        ]"
        :aria-label="dateFormat({ dateStyle: 'full' }).format(d)"
        :aria-current="sameDay(d, date) ? 'date' : undefined"
        @click="date = d"
      >
        {{ d.getDate() }}
      </button>
    </div>
  </div>
</template>
