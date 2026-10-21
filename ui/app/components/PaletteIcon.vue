<script lang="ts">
import type { CommandPaletteItem } from '@nuxt/ui'

/** Tints of the palette's icon squares. Full class strings so Tailwind generates them; `primary`
 *  and `neutral` follow the accent and tint chosen in the settings. */
export const PALETTE_TONES = {
  primary: 'bg-primary-100 text-primary-700 dark:bg-primary-400/15 dark:text-primary-300',
  neutral: 'bg-elevated text-default',
  teal: 'bg-teal-100 text-teal-700 dark:bg-teal-400/15 dark:text-teal-400',
  blue: 'bg-blue-100 text-blue-600 dark:bg-blue-500/16 dark:text-blue-400',
  orange: 'bg-orange-100 text-orange-700 dark:bg-orange-500/16 dark:text-orange-300',
  amber: 'bg-amber-100 text-amber-700 dark:bg-amber-500/16 dark:text-amber-400',
  emerald: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/16 dark:text-emerald-400',
  sky: 'bg-sky-100 text-sky-700 dark:bg-sky-500/16 dark:text-sky-400',
  indigo: 'bg-indigo-100 text-indigo-700 dark:bg-indigo-500/20 dark:text-indigo-300'
} as const

export type PaletteTone = keyof typeof PALETTE_TONES

export interface PaletteEntry extends CommandPaletteItem {
  tone?: PaletteTone
  badge?: number
  account?: number
  accountAvatar?: boolean
  hint?: string
}
</script>

<script setup lang="ts">
withDefaults(defineProps<{
  icon: string
  tone?: PaletteTone
  large?: boolean
}>(), { tone: 'neutral' })
</script>

<template>
  <span
    class="flex flex-none items-center justify-center"
    :class="[PALETTE_TONES[tone], large ? 'size-8 rounded-[10px]' : 'size-[30px] rounded-[9px]']"
    aria-hidden="true"
  >
    <UIcon :name="icon" :class="large ? 'size-[17px]' : 'size-4'" />
  </span>
</template>
