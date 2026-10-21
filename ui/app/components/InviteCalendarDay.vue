<script setup lang="ts">
const props = defineProps<{ start: number, band: string }>()

const date = computed(() => new Date(props.start * 1000))
const month = computed(() => {
  const m = dateFormat({ month: 'short' }).format(date.value).toLocaleUpperCase(intlLocale())
  // Another year: said in the band, the page shows no year otherwise.
  return date.value.getFullYear() === new Date().getFullYear() ? m : `${m} ${date.value.getFullYear()}`
})
const day = computed(() => dateFormat({ day: 'numeric' }).format(date.value))
const weekday = computed(() => dateFormat({ weekday: 'long' }).format(date.value))
const weekdayShort = computed(() => dateFormat({ weekday: 'short' }).format(date.value))
</script>

<template>
  <div aria-hidden="true" class="w-16 shrink-0 self-start overflow-hidden rounded-[13px] border border-default bg-(--surface-side) text-center lg:w-18 lg:rounded-[14px] lg:bg-(--surface-card)">
    <div class="py-0.75 text-[10.5px] font-bold tracking-[0.08em] lg:py-1 lg:text-[11px]" :class="band">{{ month }}</div>
    <div class="pt-0.75 text-[26px] leading-[1.1] font-bold text-highlighted lg:pt-1 lg:text-[30px]">{{ day }}</div>
    <div class="truncate px-1 pb-1.25 text-[11px] text-muted lg:pb-1.5 lg:text-[11.5px]">
      <span class="lg:hidden">{{ weekdayShort }}</span>
      <span class="hidden lg:inline">{{ weekday }}</span>
    </div>
  </div>
</template>
