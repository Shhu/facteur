<script setup lang="ts">
import type { RepeatForm } from '~/utils/types'

const props = defineProps<{ start: Date | null, custom?: boolean }>()
const model = defineModel<RepeatForm>({ required: true })
const { t } = useI18n()

/** Monday 0 to Sunday 6, as the engine counts. */
const weekdayOf = (d: Date) => (d.getDay() + 6) % 7
// 2024-01-01 was a Monday.
const weekdayDate = (i: number) => new Date(2024, 0, 1 + i)

const kindItems = computed(() => [
  { label: t('tools.calendar.repeat.none'), value: 'none' },
  ...(props.custom ? [{ label: t('tools.calendar.repeat.custom'), value: 'custom' }] : []),
  { label: t('tools.calendar.repeat.daily'), value: 'daily' },
  { label: t('tools.calendar.repeat.weekly'), value: 'weekly' },
  { label: t('tools.calendar.repeat.monthly'), value: 'monthly' },
  { label: t('tools.calendar.repeat.yearly'), value: 'yearly' }
])
const repeating = computed(() => model.value.kind !== 'none' && model.value.kind !== 'custom')
const unit = computed(() => t(`tools.calendar.repeat.every.${model.value.kind}`, model.value.interval))

const days = computed(() => Array.from({ length: 7 }, (_, i) => ({
  i,
  short: dateFormat({ weekday: 'narrow' }).format(weekdayDate(i)),
  long: dateFormat({ weekday: 'long' }).format(weekdayDate(i))
})))
function toggleDay(i: number) {
  const now = model.value.weekdays
  const next = now.includes(i) ? now.filter(d => d !== i) : [...now, i].sort()
  // Never empty: the series would fall on no day.
  if (next.length) model.value.weekdays = next
}

const monthlyItems = computed(() => {
  const d = props.start
  if (!d) return []
  const n = Math.floor((d.getDate() - 1) / 7) + 1
  const weekday = dateFormat({ weekday: 'long' }).format(d)
  return [
    { label: t('tools.calendar.repeat.monthDay', { day: d.getDate() }), value: false },
    { label: n === 5 ? t('tools.calendar.repeat.lastWeekday', { weekday }) : t(`tools.calendar.repeat.nthWeekday.${n}`, { weekday }), value: true }
  ]
})

const endItems = computed(() => [
  { label: t('tools.calendar.repeat.endNever'), value: 'never' },
  { label: t('tools.calendar.repeat.endUntil'), value: 'until' },
  { label: t('tools.calendar.repeat.endCount'), value: 'count' }
])

// A first choice of weekly falls on the start's day.
watch(() => model.value.kind, (kind) => {
  if (kind === 'weekly' && !model.value.weekdays.length && props.start) model.value.weekdays = [weekdayOf(props.start)]
})
</script>

<template>
  <div class="flex flex-col gap-2.5">
    <div class="flex items-center gap-2">
      <UIcon name="i-lucide-repeat" class="size-4 shrink-0 text-muted" />
      <USelect v-model="model.kind" :items="kindItems" class="min-w-40" :aria-label="t('tools.calendar.repeat.title')" />
      <template v-if="repeating">
        <span class="text-sm text-muted">{{ t(`tools.calendar.repeat.everyLabel.${model.kind}`) }}</span>
        <UInputNumber v-model="model.interval" :min="1" :max="999" class="w-24" :aria-label="t('tools.calendar.repeat.interval')" />
        <span class="text-sm text-muted">{{ unit }}</span>
      </template>
    </div>

    <p v-if="model.kind === 'custom'" class="text-[13px] text-muted">{{ t('tools.calendar.repeat.customHelp') }}</p>

    <div v-if="model.kind === 'weekly'" class="flex gap-1 pl-6">
      <UTooltip v-for="d in days" :key="d.i" :text="d.long">
        <UButton
          :label="d.short"
          :color="model.weekdays.includes(d.i) ? 'primary' : 'neutral'"
          :variant="model.weekdays.includes(d.i) ? 'solid' : 'outline'"
          size="sm"
          class="size-8 justify-center rounded-full"
          :aria-pressed="model.weekdays.includes(d.i)"
          :aria-label="d.long"
          @click="toggleDay(d.i)"
        />
      </UTooltip>
    </div>

    <URadioGroup v-if="model.kind === 'monthly'" v-model="model.byWeekday" :items="monthlyItems" class="pl-6" />

    <div v-if="repeating" class="flex items-center gap-2 pl-6">
      <USelect v-model="model.end" :items="endItems" class="min-w-36" :aria-label="t('tools.calendar.repeat.endLabel')" />
      <UInput v-if="model.end === 'until'" v-model="model.until" type="date" class="w-44" :aria-label="t('tools.calendar.repeat.endUntil')" />
      <template v-if="model.end === 'count'">
        <UInputNumber v-model="model.count" :min="1" :max="999" class="w-24" :aria-label="t('tools.calendar.repeat.endCount')" />
        <span class="text-sm text-muted">{{ t('tools.calendar.repeat.times', model.count) }}</span>
      </template>
    </div>
  </div>
</template>
