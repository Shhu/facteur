<script setup lang="ts">
import type { Address } from '~/utils/types'

/** `size`: Tailwind size classes (36 px by default). `row`: message list rows (38 px, 42 px on
 *  touch layouts below `lg`). `accountId`: preferred when telling whether the address is ours. */
const props = withDefaults(defineProps<{
  address: Address | null | undefined
  size?: 'xs' | 'md' | 'row' | 'lg'
  accountId?: number
}>(), { size: 'md' })
const box = computed(() => ({
  xs: 'size-5 text-[9px]',
  md: 'size-9 text-xs',
  row: 'size-[38px] text-[13px] max-lg:size-[42px] max-lg:text-sm',
  lg: 'size-12 text-sm'
})[props.size])
const { accounts, isMine, accountColor } = useMail()
const logos = useSenderLogos()

const email = computed(() => props.address?.email.toLowerCase())

/** Our account owning this address (alias included), the message's own account first. */
const own = computed(() => {
  const e = email.value
  if (!e) return null
  if (props.accountId !== undefined && isMine(e, props.accountId)) return accounts.value.find(a => a.id === props.accountId) ?? null
  return accounts.value.find(a => isMine(e, a.id)) ?? null
})
const photo = computed(() => own.value?.avatar ?? null)
const logo = computed(() => logos.logoOf(email.value))

// Our addresses have no brand logo: no need to fetch one.
watch(email, (e) => {
  if (!isMine(e)) logos.request(e)
}, { immediate: true })

/** Soft tints for other senders, picked from a hash of the address. Full class strings so
 *  Tailwind generates them; each pair stays readable in light and dark mode. */
const TINTS = [
  'bg-indigo-100 text-indigo-700 dark:bg-indigo-500/20 dark:text-indigo-300',
  'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/20 dark:text-emerald-300',
  'bg-sky-100 text-sky-700 dark:bg-sky-500/20 dark:text-sky-300',
  'bg-violet-100 text-violet-700 dark:bg-violet-500/20 dark:text-violet-300',
  'bg-rose-100 text-rose-700 dark:bg-rose-500/20 dark:text-rose-300',
  'bg-amber-100 text-amber-700 dark:bg-amber-500/20 dark:text-amber-300',
  'bg-teal-100 text-teal-700 dark:bg-teal-500/20 dark:text-teal-300',
  'bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-500/20 dark:text-fuchsia-300',
  'bg-lime-100 text-lime-700 dark:bg-lime-500/20 dark:text-lime-300',
  'bg-cyan-100 text-cyan-700 dark:bg-cyan-500/20 dark:text-cyan-300',
  'bg-orange-100 text-orange-700 dark:bg-orange-500/20 dark:text-orange-300',
  'bg-blue-100 text-blue-700 dark:bg-blue-500/20 dark:text-blue-300'
]
const tint = computed(() => (own.value ? accountColor(own.value.id).soft : TINTS[avatarHue(props.address) % TINTS.length]))
const letters = computed(() => (own.value ? initials(accountName(own.value)) : initials(props.address)))
const label = computed(() => (props.address ? addressLabel(props.address) : ''))
</script>

<template>
  <img v-if="photo" :src="photo" :alt="label" class="shrink-0 select-none rounded-full object-cover" :class="box">
  <!-- White background: many logos are transparent and dark. -->
  <img
    v-else-if="logo"
    :src="logo"
    :alt="label"
    :title="label"
    class="shrink-0 select-none rounded-full bg-white object-contain ring-1 ring-default"
    :class="[box, size === 'xs' ? 'p-0.5' : 'p-1.5']"
  >
  <div
    v-else
    class="flex shrink-0 select-none items-center justify-center rounded-full font-bold"
    :class="[box, tint]"
    :title="label || undefined"
  >
    {{ letters }}
  </div>
</template>
