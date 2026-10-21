<script setup lang="ts">
import type { PaletteEntry } from '~/components/PaletteIcon.vue'

defineProps<{
  actions: PaletteEntry[]
  pages: PaletteEntry[]
  saved: PaletteEntry[]
  stats: PaletteEntry[]
}>()
const { accountById, accountColor } = useMail()

const heading = 'mx-1 text-xs font-bold tracking-[0.06em] text-muted uppercase'
</script>

<template>
  <div class="flex flex-col gap-[18px] px-4 pt-1 pb-[calc(1.5rem+env(safe-area-inset-bottom))]">
    <section aria-labelledby="palette-m-actions" class="flex flex-col gap-2">
      <h2 id="palette-m-actions" :class="heading">{{ $t('app.palette.actions') }}</h2>
      <div class="grid grid-cols-2 gap-2.5">
        <button
          v-for="(a, i) in actions"
          :key="i"
          type="button"
          class="flex min-h-[76px] flex-col items-start justify-between gap-2.5 rounded-2xl border border-default bg-default p-3 text-start text-sm font-semibold text-highlighted shadow-xs active:bg-elevated dark:shadow-none"
          @click="a.onSelect?.($event)"
        >
          <PaletteIcon :icon="a.icon!" :tone="a.tone" large />
          {{ a.label }}
        </button>
      </div>
    </section>

    <section aria-labelledby="palette-m-pages" class="flex flex-col gap-2">
      <h2 id="palette-m-pages" :class="heading">{{ $t('app.palette.goTo') }}</h2>
      <div class="divide-y divide-default overflow-hidden rounded-2xl border border-default bg-default shadow-xs dark:shadow-none">
        <button
          v-for="(p, i) in pages"
          :key="i"
          type="button"
          class="flex min-h-[50px] w-full items-center gap-3 px-3.5 text-start active:bg-elevated"
          @click="p.onSelect?.($event)"
        >
          <PaletteIcon :icon="p.icon!" :tone="p.tone" />
          <span class="flex-1 truncate text-[15px] text-highlighted">{{ p.label }}</span>
          <span
            v-if="p.badge"
            class="rounded-full bg-primary-100 px-2 py-px text-xs font-bold text-primary-700 dark:bg-primary/14 dark:text-primary-300"
          >{{ p.badge }}</span>
          <UIcon name="i-lucide-chevron-right" class="size-4 flex-none text-dimmed" />
        </button>
      </div>
    </section>

    <section v-if="saved.length" aria-labelledby="palette-m-saved" class="flex flex-col gap-2">
      <h2 id="palette-m-saved" :class="heading">{{ $t('app.palette.savedSearches') }}</h2>
      <div class="divide-y divide-default overflow-hidden rounded-2xl border border-default bg-default shadow-xs dark:shadow-none">
        <button
          v-for="(s, i) in saved"
          :key="i"
          type="button"
          class="flex min-h-[50px] w-full items-center gap-3 px-3.5 text-start active:bg-elevated"
          @click="s.onSelect?.($event)"
        >
          <PaletteIcon :icon="s.icon!" :tone="s.tone" />
          <span class="flex min-w-0 flex-1 flex-col">
            <span class="truncate text-[15px] text-highlighted">{{ s.label }}</span>
            <span v-if="s.hint && s.hint !== s.label" class="truncate font-mono text-xs text-muted">{{ s.hint }}</span>
          </span>
          <UIcon name="i-lucide-chevron-right" class="size-4 flex-none text-dimmed" />
        </button>
      </div>
    </section>

    <section v-if="stats.length" aria-labelledby="palette-m-stats" class="flex flex-col gap-2">
      <h2 id="palette-m-stats" :class="heading">{{ $t('app.palette.statsGroup') }}</h2>
      <div class="flex flex-wrap gap-2">
        <template v-for="(s, i) in stats" :key="i">
          <button
            v-if="s.account !== undefined && accountById(s.account)"
            type="button"
            class="flex h-11 min-w-[calc((100%-1rem)/3)] flex-1 basis-0 items-center gap-2 rounded-full ps-1.5 pe-2.5 text-[13px] font-semibold"
            :class="accountColor(s.account).soft"
            :aria-label="$t('app.palette.stats', { account: accountName(accountById(s.account)!) })"
            @click="s.onSelect?.($event)"
          >
            <span
              class="flex size-[30px] flex-none items-center justify-center rounded-full text-[11px] font-bold"
              :class="accountColor(s.account).solid"
              aria-hidden="true"
            >{{ initials(accountName(accountById(s.account)!)) }}</span>
            <span class="truncate">{{ accountName(accountById(s.account)!) }}</span>
          </button>
        </template>
      </div>
    </section>
  </div>
</template>
