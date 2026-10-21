<script setup lang="ts">
/**
 * One-sentence summary of when mail arrives: the busiest day and the busiest hours. A tie is
 * never resolved by taking the first value: tied days that follow each other become a range,
 * and the hours become the smallest window holding most of the mail. Without a clear peak,
 * that half of the sentence is left out.
 */
const props = defineProps<{
  /** Received messages per weekday, Monday first. */
  weekdays: number[]
  /** Received messages per hour, 0 to 23. */
  hours: number[]
}>()
const { t } = useI18n()

// January 1st 2024 is a Monday.
const weekdayName = (i: number) => dateFormat({ weekday: 'long' }).format(new Date(2024, 0, 1 + i))

/** Busiest day, or run of consecutive tied days (wrapping from Sunday to Monday). */
const day = computed<{ from: number, to: number } | null>(() => {
  const w = props.weekdays
  const max = Math.max(0, ...w)
  if (!max) return null
  const top = new Set(w.flatMap((v, i) => (v === max ? [i] : [])))
  if (top.size === w.length) return null
  const starts = [...top].filter(i => !top.has((i - 1 + w.length) % w.length))
  if (starts.length !== 1) return null
  let to = starts[0]!
  while (top.has((to + 1) % w.length)) to = (to + 1) % w.length
  return { from: starts[0]!, to }
})

/** Smallest window of up to 6 hours (wrapping past midnight) holding at least half of the mail.
 *  When several windows of that length tie, a longer one is tried; none: no clear peak. */
const hour = computed<{ from: number, length: number } | null>(() => {
  const h = props.hours
  const n = h.length
  const total = h.reduce((a, b) => a + b, 0)
  if (!total) return null
  for (let length = 1; length <= 6; length++) {
    const sums = Array.from({ length: n }, (_, start) => {
      let sum = 0
      for (let k = 0; k < length; k++) sum += h[(start + k) % n]!
      return sum
    })
    const best = Math.max(...sums)
    if (best * 2 < total) continue
    const at = sums.flatMap((s, i) => (s === best ? [i] : []))
    if (at.length === 1) return { from: at[0]!, length }
  }
  return null
})

const keypath = computed(() => (day.value && hour.value ? 'tools.stats.insightDayHour' : day.value ? 'tools.stats.insightDay' : 'tools.stats.insightHour'))
const hourText = computed(() => {
  const w = hour.value
  if (!w) return ''
  return w.length === 1
    ? t('tools.stats.aroundHour', { h: w.from })
    : t('tools.stats.betweenHours', { from: w.from, to: (w.from + w.length) % 24 })
})
</script>

<template>
  <div
    v-if="day || hour"
    class="flex items-center gap-3 rounded-[14px] border border-violet-200 bg-violet-50 px-4.5 py-3.5 text-sm text-default max-lg:items-start max-lg:p-3.5 max-lg:leading-[1.45] dark:border-violet-500/25 dark:bg-violet-500/8"
  >
    <span class="flex size-8 shrink-0 items-center justify-center rounded-[10px] bg-violet-100 text-violet-700 max-lg:size-7.5 dark:bg-violet-500/20 dark:text-violet-300">
      <UIcon name="i-lucide-sparkles" class="size-4" />
    </span>
    <i18n-t :keypath="keypath" tag="p" scope="global">
      <template #day>
        <i18n-t v-if="day && day.from === day.to" keypath="tools.stats.busiestDay" scope="global">
          <template #day>
            <b class="font-semibold text-highlighted">{{ weekdayName(day.from) }}</b>
          </template>
        </i18n-t>
        <i18n-t v-else-if="day" keypath="tools.stats.busiestDays" scope="global">
          <template #range>
            <b class="font-semibold text-highlighted">{{ t('tools.stats.dayRange', { from: weekdayName(day.from), to: weekdayName(day.to) }) }}</b>
          </template>
        </i18n-t>
      </template>
      <template #hour>
        <b class="font-semibold text-highlighted">{{ hourText }}</b>
      </template>
    </i18n-t>
  </div>
</template>
