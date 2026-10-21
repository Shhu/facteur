<script setup lang="ts">
import type { DropdownMenuItem, TabsItem } from '@nuxt/ui'
import { useMediaQuery } from '@vueuse/core'
import type { ThreadFilter } from '~/utils/types'

const props = defineProps<{
  title: string
  unread: number
  accountId: number | null
  unified: boolean
  doneView: boolean
  showFilter: boolean
}>()
const filter = defineModel<ThreadFilter>('filter', { required: true })
const searchOpen = defineModel<boolean>('searchOpen', { default: false })

const { accounts, accountById, accountColor, mailboxByRole, unifiedUnread } = useMail()
const { unreadOf } = useFolderCounts()
const { t } = useI18n()
const desktop = useMediaQuery('(min-width: 1024px)')

const filterItems = computed<TabsItem[]>(() => [
  { label: t('mail.page.filter.all'), value: 'all' },
  { label: t('mail.page.filter.unread'), value: 'unread' },
  { label: t('mail.page.filter.flagged'), value: 'flagged' }
])

const account = computed(() => (props.accountId !== null ? accountById(props.accountId) : null))

const subtitle = computed(() => {
  const parts: string[] = []
  if (account.value) parts.push(accountName(account.value))
  else if (accounts.value.length > 1) parts.push(t('mail.list.accounts', accounts.value.length))
  else if (accounts.value[0]) parts.push(accountName(accounts.value[0]))
  if (props.unread) parts.push(t('mail.list.unread', props.unread))
  return parts.join(' · ')
})
const dots = computed(() =>
  props.accountId !== null ? [accountColor(props.accountId).dot] : accounts.value.slice(0, 4).map(a => accountColor(a.id).dot)
)

const switchItems = computed<DropdownMenuItem[]>(() => [
  ...(accounts.value.length > 1
    ? [{ label: t('mail.page.unified'), icon: 'i-lucide-inbox', to: '/mail/unified', current: props.unified, unread: unifiedUnread.value }]
    : []),
  ...accounts.value.map((a) => {
    const inbox = mailboxByRole('inbox', a.id)
    return {
      label: accountName(a),
      avatar: {
        src: a.avatar ?? undefined,
        alt: accountName(a),
        text: initials(accountName(a)),
        class: accountColor(a.id).soft,
        ui: { fallback: 'text-inherit font-bold' }
      },
      to: inbox ? `/mail/${inbox.id}` : '/mail/unified',
      current: props.accountId === a.id,
      unread: inbox ? unreadOf(inbox) : 0
    }
  })
])

const doneToggle = computed(() => ({
  label: props.doneView ? t('mail.page.toDo') : t('mail.page.done'),
  icon: props.doneView ? 'i-lucide-inbox' : 'i-lucide-check-check',
  to: props.doneView ? '/mail/unified' : '/mail/done'
}))
</script>

<template>
  <div class="flex flex-col gap-3">
    <div v-if="desktop" class="flex min-h-9 items-center gap-2.5">
      <div class="flex min-w-0 flex-1 flex-col">
        <h1 class="truncate text-[17px] font-bold text-highlighted">{{ title }}</h1>
        <span v-if="subtitle" class="flex min-w-0 items-center gap-1.5 text-xs text-muted">
          <span class="flex shrink-0 -space-x-px">
            <span v-for="(dot, i) in dots" :key="i" class="size-[7px] rounded-full ring-1 ring-(--list-bg)" :class="dot" />
          </span>
          <span class="truncate">{{ subtitle }}</span>
        </span>
      </div>
      <UTooltip v-if="unified" :text="doneToggle.label">
        <UButton
          :icon="doneToggle.icon"
          :to="doneToggle.to"
          :aria-label="doneToggle.label"
          size="sm"
          color="neutral"
          :variant="doneView ? 'soft' : 'ghost'"
        />
      </UTooltip>
      <UTabs
        v-if="showFilter"
        v-model="filter"
        :items="filterItems"
        :content="false"
        size="xs"
        color="neutral"
        variant="pill"
        class="shrink-0"
        :ui="{
          list: 'w-auto gap-0.5 rounded-[10px] bg-elevated p-[3px]',
          indicator: 'inset-y-[3px] rounded-[7px] bg-default shadow-xs dark:bg-accented',
          trigger: 'h-7 grow-0 rounded-[7px] px-2.5 text-xs font-medium data-[state=active]:font-semibold data-[state=active]:text-highlighted'
        }"
      />
    </div>

    <template v-else>
      <div class="flex items-center gap-2">
        <UDropdownMenu :items="switchItems" :content="{ align: 'start' }" :ui="{ content: 'min-w-60' }">
          <button
            type="button"
            class="flex h-11 min-w-0 items-center gap-2 rounded-full border border-default bg-(--surface-card) ps-1.5 pe-3 text-sm font-semibold text-highlighted"
          >
            <img v-if="account?.avatar" :src="account.avatar" :alt="accountName(account)" class="size-[30px] shrink-0 rounded-full object-cover">
            <span
              v-else-if="account"
              class="flex size-[30px] shrink-0 items-center justify-center rounded-full text-[11px] font-bold"
              :class="accountColor(account.id).solid"
            >{{ initials(accountName(account)) }}</span>
            <span v-else class="flex size-[30px] shrink-0 items-center justify-center rounded-full bg-primary text-inverted">
              <UIcon name="i-lucide-inbox" class="size-4" />
            </span>
            <span class="truncate">{{ account ? accountName(account) : $t('mail.page.unified') }}</span>
            <UIcon name="i-lucide-chevron-down" class="size-4 shrink-0 text-muted" />
          </button>
          <template #item-trailing="{ item }">
            <span v-if="item.unread" class="text-xs font-semibold text-muted">{{ item.unread }}</span>
            <UIcon v-if="item.current" name="i-lucide-check" class="size-4 text-primary" />
          </template>
        </UDropdownMenu>
        <div class="flex-1" />
        <UButton
          v-if="unified"
          :icon="doneToggle.icon"
          :label="doneToggle.label"
          :to="doneToggle.to"
          color="neutral"
          :variant="doneView ? 'soft' : 'outline'"
          class="h-11 rounded-full px-3.5 ring-default"
          :class="!doneView && 'bg-(--surface-card)'"
        />
        <UButton
          icon="i-lucide-search"
          color="neutral"
          :variant="searchOpen ? 'soft' : 'outline'"
          square
          :aria-label="$t('mail.page.search')"
          :aria-pressed="searchOpen"
          class="size-11 justify-center rounded-[14px] ring-default"
          :class="searchOpen ? 'text-highlighted' : 'bg-(--surface-card) text-muted'"
          :ui="{ leadingIcon: 'size-5' }"
          @click="searchOpen = !searchOpen"
        />
      </div>
      <div class="flex min-w-0 items-center gap-2.5">
        <h1 class="truncate text-[28px] leading-tight font-bold tracking-tight text-highlighted">{{ title }}</h1>
        <span v-if="unread" class="shrink-0 rounded-full bg-primary/15 px-2.5 py-0.5 text-[13px] font-bold text-primary">{{ unread }}</span>
      </div>
      <UTabs
        v-if="showFilter"
        v-model="filter"
        :items="filterItems"
        :content="false"
        color="neutral"
        variant="pill"
        :ui="{
          list: 'w-auto gap-2 rounded-none bg-transparent p-0',
          indicator: 'inset-y-0 rounded-full shadow-none',
          trigger: 'h-9 grow-0 rounded-full px-4 text-[13px] font-medium after:absolute after:inset-x-0 after:-inset-y-1 data-[state=active]:font-semibold data-[state=inactive]:bg-(--surface-card) data-[state=inactive]:text-default data-[state=inactive]:ring-1 data-[state=inactive]:ring-default data-[state=inactive]:ring-inset'
        }"
      />
    </template>
  </div>
</template>
