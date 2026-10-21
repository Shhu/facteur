<script setup lang="ts">
import type { Account } from '~/utils/types'

/** `null` means "all accounts" (only offered with `allowAll`). */
const model = defineModel<number | null>({ required: true })
const props = withDefaults(defineProps<{
  allowAll?: boolean
  size?: 'md' | 'lg'
  label?: string
}>(), { allowAll: false, size: 'md', label: undefined })
const { t } = useI18n()
const { accounts, accountColor } = useMail()

interface Item { id: number | null, label: string, email: string, account: Account | null, class?: string }

const items = computed<Item[]>(() => [
  ...(props.allowAll ? [{ id: null, label: t('app.accountSelect.all'), email: t('app.accountSelect.count', accounts.value.length), account: null }] : []),
  ...accounts.value.map(a => ({ id: a.id, label: accountName(a), email: a.email, account: a }))
].map(i => ({ ...i, class: i.id === model.value ? 'font-semibold before:bg-primary/10' : undefined })))
const selected = computed({
  get: () => items.value.find(i => i.id === model.value) ?? items.value[0],
  set: (item: Item | undefined) => {
    if (item) model.value = item.id
  }
})
const current = computed(() => selected.value?.account ?? null)
const large = computed(() => props.size === 'lg')

const allAvatar = 'flex shrink-0 items-center justify-center rounded-full bg-(--ui-color-neutral-800) font-bold text-white dark:bg-(--ui-color-neutral-700) dark:text-(--ui-color-neutral-50)'
</script>

<template>
  <USelectMenu
    v-model="selected"
    :items="items"
    variant="none"
    :search-input="{ placeholder: t('app.accountSelect.search'), icon: 'i-lucide-search', variant: 'soft', ui: { base: 'h-9.5 rounded-lg' } }"
    :content="{ align: 'end', sideOffset: 8 }"
    :filter-fields="['label', 'email']"
    :aria-label="label ?? t('tools.stats.account')"
    :ui="{
      base: 'w-full p-0 rounded-xl focus-visible:ring-2 focus-visible:ring-primary',
      trailing: 'hidden',
      content: 'min-w-85 max-h-[min(24rem,var(--reka-combobox-content-available-height,24rem))] rounded-xl p-1.5',
      input: 'border-0 mb-1.5',
      group: 'p-0',
      viewport: 'max-h-75',
      item: 'rounded-lg before:rounded-lg'
    }"
    :class="large ? 'w-full' : 'w-80'"
  >
    <template #default>
      <span
        class="flex w-full min-w-0 items-center gap-2.5 rounded-xl ps-2 pe-3 text-left ring-1 ring-inset"
        :class="[large ? 'h-13 ps-2.5' : 'h-11', current ? [accountColor(current.id).soft, accountColor(current.id).ring] : 'bg-(--surface-card) ring-(--ui-border)']"
      >
        <UAvatar
          v-if="current"
          :src="current.avatar ?? undefined"
          :text="initials(accountName(current))"
          :alt="accountName(current)"
          :size="large ? 'md' : 'sm'"
          :class="accountColor(current.id).solid"
          :ui="{ fallback: ['text-inherit font-bold', large ? 'text-[11px]' : 'text-[10px]'] }"
        />
        <span v-else :class="[allAvatar, large ? 'size-8 text-sm' : 'size-7 text-xs']" aria-hidden="true">∑</span>
        <span class="flex min-w-0 flex-1 flex-col leading-tight">
          <span v-if="label" class="text-xs text-muted">{{ label }}</span>
          <span class="truncate font-semibold text-highlighted" :class="large ? 'text-[15px]' : 'text-sm'">{{ selected?.label }}</span>
          <span v-if="!label" class="truncate text-muted" :class="large ? 'text-xs' : 'text-[11.5px]'">{{ selected?.email }}</span>
        </span>
        <UIcon name="i-lucide-chevron-down" class="size-4 shrink-0 text-muted" />
      </span>
    </template>
    <template #item="{ item }">
      <span class="flex min-h-8 w-full min-w-0 items-center gap-2.5 py-0.5">
        <UAvatar
          v-if="item.account"
          :src="item.account.avatar ?? undefined"
          :text="initials(item.label)"
          :alt="item.label"
          size="sm"
          :class="accountColor(item.account.id).solid"
          :ui="{ fallback: 'text-inherit text-[10px] font-bold' }"
        />
        <span v-else :class="[allAvatar, 'size-7 text-xs']" aria-hidden="true">∑</span>
        <span class="flex min-w-0 flex-1 flex-col leading-tight">
          <span class="truncate text-sm text-highlighted">{{ item.label }}</span>
          <span class="truncate text-xs font-normal text-muted">{{ item.email }}</span>
        </span>
        <UIcon v-if="item.id === model" name="i-lucide-check" class="size-4 shrink-0 text-primary" />
      </span>
    </template>
  </USelectMenu>
</template>
