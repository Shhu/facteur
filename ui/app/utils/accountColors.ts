import type { Account } from '~/utils/types'

/** Classes are written out in full so Tailwind generates them. */
export interface AccountColor {
  solid: string
  soft: string
  ring: string
  dot: string
}

/** Key order is the automatic assignment order. */
export const ACCOUNT_COLORS = {
  orange: {
    solid: 'bg-orange-600 text-white dark:bg-orange-400 dark:text-orange-950',
    soft: 'bg-orange-100 text-orange-700 dark:bg-orange-500/20 dark:text-orange-300',
    ring: 'ring-orange-300 dark:ring-orange-400/60',
    dot: 'bg-orange-600 dark:bg-orange-400'
  },
  pink: {
    solid: 'bg-pink-600 text-white dark:bg-pink-400 dark:text-pink-950',
    soft: 'bg-pink-100 text-pink-700 dark:bg-pink-500/20 dark:text-pink-300',
    ring: 'ring-pink-300 dark:ring-pink-400/60',
    dot: 'bg-pink-600 dark:bg-pink-400'
  },
  violet: {
    solid: 'bg-violet-600 text-white dark:bg-violet-400 dark:text-violet-950',
    soft: 'bg-violet-100 text-violet-700 dark:bg-violet-500/20 dark:text-violet-300',
    ring: 'ring-violet-300 dark:ring-violet-400/60',
    dot: 'bg-violet-600 dark:bg-violet-400'
  },
  blue: {
    solid: 'bg-blue-600 text-white dark:bg-blue-400 dark:text-blue-950',
    soft: 'bg-blue-100 text-blue-700 dark:bg-blue-500/20 dark:text-blue-300',
    ring: 'ring-blue-300 dark:ring-blue-400/60',
    dot: 'bg-blue-600 dark:bg-blue-400'
  },
  emerald: {
    solid: 'bg-emerald-600 text-white dark:bg-emerald-400 dark:text-emerald-950',
    soft: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/20 dark:text-emerald-300',
    ring: 'ring-emerald-300 dark:ring-emerald-400/60',
    dot: 'bg-emerald-600 dark:bg-emerald-400'
  },
  amber: {
    solid: 'bg-amber-600 text-white dark:bg-amber-400 dark:text-amber-950',
    soft: 'bg-amber-100 text-amber-700 dark:bg-amber-500/20 dark:text-amber-300',
    ring: 'ring-amber-300 dark:ring-amber-400/60',
    dot: 'bg-amber-600 dark:bg-amber-400'
  },
  cyan: {
    solid: 'bg-cyan-600 text-white dark:bg-cyan-400 dark:text-cyan-950',
    soft: 'bg-cyan-100 text-cyan-700 dark:bg-cyan-500/20 dark:text-cyan-300',
    ring: 'ring-cyan-300 dark:ring-cyan-400/60',
    dot: 'bg-cyan-600 dark:bg-cyan-400'
  },
  red: {
    solid: 'bg-red-600 text-white dark:bg-red-400 dark:text-red-950',
    soft: 'bg-red-100 text-red-700 dark:bg-red-500/20 dark:text-red-300',
    ring: 'ring-red-300 dark:ring-red-400/60',
    dot: 'bg-red-600 dark:bg-red-400'
  },
  lime: {
    solid: 'bg-lime-600 text-white dark:bg-lime-400 dark:text-lime-950',
    soft: 'bg-lime-100 text-lime-700 dark:bg-lime-500/20 dark:text-lime-300',
    ring: 'ring-lime-300 dark:ring-lime-400/60',
    dot: 'bg-lime-600 dark:bg-lime-400'
  },
  fuchsia: {
    solid: 'bg-fuchsia-600 text-white dark:bg-fuchsia-400 dark:text-fuchsia-950',
    soft: 'bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-500/20 dark:text-fuchsia-300',
    ring: 'ring-fuchsia-300 dark:ring-fuchsia-400/60',
    dot: 'bg-fuchsia-600 dark:bg-fuchsia-400'
  }
} satisfies Record<string, AccountColor>

export type AccountColorName = keyof typeof ACCOUNT_COLORS
export const ACCOUNT_COLOR_NAMES = Object.keys(ACCOUNT_COLORS) as AccountColorName[]

export function accountColorName(account: Pick<Account, 'id' | 'color'>, accounts: Pick<Account, 'id'>[]): AccountColorName {
  if (account.color && account.color in ACCOUNT_COLORS) return account.color as AccountColorName
  const rank = Math.max(0, accounts.findIndex(a => a.id === account.id))
  return ACCOUNT_COLOR_NAMES[rank % ACCOUNT_COLOR_NAMES.length]!
}

