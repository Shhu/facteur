<script setup lang="ts">
import type { SearchChip } from '~/utils/searchOperators'

defineProps<{ chips: SearchChip[] }>()
</script>

<template>
  <div class="flex flex-wrap items-center gap-1.5 border-b border-default px-[18px] py-2.5 max-lg:px-4">
    <span class="me-0.5 text-xs text-muted">{{ $t('app.palette.filters') }}</span>
    <template v-for="(chip, i) in chips" :key="i">
      <span
        v-if="chip.kind === 'word'"
        class="inline-flex h-[26px] max-w-full items-center rounded-full bg-elevated px-2.5 text-[12.5px] text-highlighted"
      >
        <span class="truncate">{{ chip.value }}</span>
      </span>
      <span
        v-else
        class="inline-flex h-[26px] max-w-full items-center gap-1 rounded-full bg-primary-100 px-2.5 text-[12.5px] text-primary-700 dark:bg-primary/14 dark:text-primary-300"
      >
        <b class="font-semibold">{{ chip.kind === 'flag' ? $t(`app.palette.filter.${chip.flag}`) : $t(`app.palette.filter.${chip.operator}`) }}</b>
        <span v-if="chip.kind === 'operator'" class="truncate">{{ chip.value }}</span>
      </span>
    </template>
  </div>
</template>
