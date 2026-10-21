<script setup lang="ts">
import type { PaletteEntry } from '~/components/PaletteIcon.vue'

const props = defineProps<{ item: PaletteEntry }>()
const { accountById, accountColor } = useMail()
const avatarAccount = computed(() => (props.item.accountAvatar && props.item.account !== undefined ? accountById(props.item.account) : null))
</script>

<template>
  <span
    v-if="avatarAccount"
    class="flex size-[30px] flex-none items-center justify-center rounded-full text-[11px] font-bold"
    :class="accountColor(avatarAccount.id).solid"
    aria-hidden="true"
  >{{ initials(accountName(avatarAccount)) }}</span>
  <PaletteIcon v-else-if="item.icon" :icon="item.icon" :tone="item.tone" />
  <span class="flex min-w-0 flex-1 items-baseline gap-2">
    <span class="truncate text-sm text-highlighted">{{ item.label }}</span>
    <span v-if="item.hint" class="truncate font-mono text-xs text-muted">{{ item.hint }}</span>
  </span>
  <span
    v-if="item.badge"
    class="flex-none rounded-full bg-primary-100 px-2 py-px text-[11.5px] font-bold text-primary-700 dark:bg-primary/14 dark:text-primary-300"
  >{{ item.badge }}</span>
  <PaletteAccountPill v-if="item.account !== undefined" :account-id="item.account" />
  <span v-if="item.kbds?.length" class="hidden flex-none items-center gap-0.5 lg:inline-flex">
    <UKbd v-for="(k, i) in item.kbds" :key="i" :value="typeof k === 'object' ? k.value : k" />
  </span>
  <UKbd value="enter" color="primary" class="hidden flex-none lg:group-data-highlighted:inline-flex" />
</template>
