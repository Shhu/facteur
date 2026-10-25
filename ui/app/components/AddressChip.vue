<script setup lang="ts">
import { createReusableTemplate, useMediaQuery } from '@vueuse/core'
import { confirm } from '~/composables/useConfirm'
import type { Address, Category } from '~/utils/types'

// Several roots (the card defined once, a sheet or a popover): attributes go to the button.
defineOptions({ inheritAttrs: false })
const props = defineProps<{
  address: Address
  accountId: number
  /** Sender: offers rules, blocking and screening. */
  sender?: boolean
  strong?: boolean
  /** A message's header: the name alone with a chevron (the avatar is beside it), and the card
   *  in a sheet from the bottom on mobile. */
  header?: boolean
}>()
const { t } = useI18n()
const { isMine } = useMail()
const { compose } = useComposer()
const toast = useToast()
const showError = useErrorToast()

const open = ref(false)
const email = computed(() => props.address.email.toLowerCase())
/** One of our own addresses (account or alias): no rule, no blocking. */
const mine = computed(() => isMine(email.value))

const category = ref<Category | null>(null)
watch(open, async (o) => {
  if (o && props.sender && !mine.value) category.value = await api.senderCategory(props.address.email).catch(() => null)
})

function go(to: string) {
  open.value = false
  navigateTo(to)
}

async function copy() {
  try {
    await navigator.clipboard.writeText(props.address.email)
    toast.add({ title: t('mail.address.copied'), description: props.address.email, icon: 'i-lucide-copy' })
  } catch {
    toast.add({ title: t('mail.address.copyFailed'), color: 'error' })
  }
}

function write() {
  open.value = false
  compose([addressFull(props.address)])
}

async function classify(c: Category) {
  try {
    await api.setSenderCategory(props.address.email, c)
    category.value = c
    toast.add({ title: t('mail.address.classified', { who: addressLabel(props.address), category: t(`mail.page.category.${c}`) }), icon: 'i-lucide-tags' })
  } catch (e) {
    showError(e)
  }
}

async function block() {
  open.value = false
  if (!await confirm(t('mail.address.blockConfirm', { email: props.address.email }), { title: t('mail.address.block'), kind: 'warning' })) return
  try {
    await api.blockSender(props.address.email)
    // Their inbox messages went to the junk folder: the list reads them again.
    useListStale().bump()
    toast.add({ title: t('mail.address.blocked'), description: props.address.email, icon: 'i-lucide-ban' })
  } catch (e) {
    showError(e)
  }
}

const search = computed(() => `avec:${props.address.email}`)
const wide = useMediaQuery('(min-width: 1024px)')
const sheet = computed(() => props.header && !wide.value)
const [DefineCard, Card] = createReusableTemplate()
const [DefineHeader, HeaderButton] = createReusableTemplate()
const categories: { value: Category, icon: string }[] = [
  { value: 'people', icon: 'i-lucide-users' },
  { value: 'notification', icon: 'i-lucide-bell' },
  { value: 'newsletter', icon: 'i-lucide-newspaper' }
]
</script>

<template>
  <DefineCard>
    <div class="flex items-center gap-3 lg:p-4">
      <MailAvatar :address="address" size="lg" class="max-lg:size-13.5 lg:size-13" />
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <p v-if="address.name" class="truncate text-[17px] font-bold text-highlighted lg:text-base">{{ address.name }}</p>
        <p class="flex min-w-0 items-center gap-1.5 text-[13px] text-muted lg:text-[12.5px]">
          <span class="truncate">{{ address.email }}</span>
          <UButton
            icon="i-lucide-copy"
            color="neutral"
            variant="ghost"
            size="xs"
            class="size-6.5 shrink-0 justify-center rounded-[7px] text-muted max-lg:hidden"
            :aria-label="t('mail.address.copy')"
            @click="copy"
          />
        </p>
      </div>
      <UButton
        icon="i-lucide-copy"
        color="neutral"
        variant="soft"
        class="size-11 shrink-0 justify-center rounded-xl text-muted lg:hidden"
        :aria-label="t('mail.address.copy')"
        @click="copy"
      />
    </div>

    <div class="grid grid-cols-2 gap-2 max-lg:mt-3.5 lg:gap-1.5 lg:px-3 lg:pb-3">
      <UButton
        :label="t('mail.address.write')"
        icon="i-lucide-pen-line"
        variant="soft"
        class="h-12 rounded-xl px-3 font-semibold lg:h-10 lg:rounded-[10px] lg:px-2.5 lg:text-[13px]"
        @click="write"
      />
      <UButton
        :label="t('mail.address.messages')"
        icon="i-lucide-search"
        color="neutral"
        variant="soft"
        class="h-12 rounded-xl px-3 font-semibold lg:h-10 lg:rounded-[10px] lg:px-2.5 lg:text-[13px]"
        :ui="{ leadingIcon: 'text-muted' }"
        @click="go(`/mail/unified?q=${encodeURIComponent(search)}`)"
      />
      <UButton
        v-if="sender && !mine"
        :label="t('mail.address.rule')"
        icon="i-lucide-workflow"
        color="neutral"
        variant="soft"
        class="h-12 rounded-xl px-3 font-semibold lg:h-10 lg:rounded-[10px] lg:px-2.5 lg:text-[13px]"
        :ui="{ leadingIcon: 'text-muted' }"
        @click="go(`/rules?from=${encodeURIComponent(address.email)}&account=${accountId}`)"
      />
      <UButton
        v-if="sender && !mine"
        :label="t('mail.address.block')"
        icon="i-lucide-ban"
        color="error"
        variant="soft"
        class="h-12 rounded-xl px-3 font-semibold lg:h-10 lg:rounded-[10px] lg:px-2.5 lg:text-[13px]"
        @click="block"
      />
    </div>

    <div v-if="sender && !mine" class="flex flex-col gap-2 max-lg:mt-3.5 lg:border-t lg:border-default lg:bg-(--surface-side) lg:p-3">
      <p class="text-[12.5px] text-muted lg:text-xs">{{ t('mail.address.classify') }}</p>
      <div role="radiogroup" :aria-label="t('mail.address.classify')" class="grid grid-cols-3 gap-0.5 rounded-xl bg-elevated p-[3px] lg:rounded-[11px]">
        <button
          v-for="c in categories"
          :key="c.value"
          type="button"
          role="radio"
          :aria-checked="category === c.value"
          class="inline-flex h-10.5 min-w-0 items-center justify-center gap-1.25 rounded-[9px] px-1 text-[13px] transition-colors focus-visible:outline-2 focus-visible:outline-primary lg:h-9 lg:rounded-lg lg:text-[12.5px]"
          :class="category === c.value
            ? 'bg-default font-semibold text-highlighted shadow-xs dark:bg-accented dark:shadow-none'
            : 'font-medium text-muted hover:text-default'"
          @click="classify(c.value)"
        >
          <UIcon :name="c.icon" class="size-3.5 shrink-0" />
          <span class="truncate">{{ t(`mail.page.category.${c.value}`) }}</span>
        </button>
      </div>
    </div>
  </DefineCard>

  <DefineHeader>
    <button
      type="button"
      class="inline-flex h-7 max-w-full items-center gap-1.5 rounded-lg px-2 text-[15.5px] font-bold text-highlighted transition-colors hover:bg-elevated"
      :class="open && 'bg-elevated'"
      :title="address.email"
      v-bind="$attrs"
      aria-haspopup="dialog"
      :aria-expanded="open"
      @click.stop="sheet && (open = true)"
    >
      <span class="truncate">{{ addressLabel(address) }}</span>
      <UIcon name="i-lucide-chevron-down" class="size-3.5 shrink-0 text-dimmed transition-transform" :class="open && 'rotate-180'" />
    </button>
  </DefineHeader>

  <template v-if="sheet">
    <HeaderButton />
    <UDrawer v-model:open="open" :title="addressLabel(address)" :ui="{ header: 'sr-only', container: 'pb-[calc(1.375rem+env(safe-area-inset-bottom))]', content: 'rounded-t-[22px]' }">
      <template #body>
        <div @click.stop>
          <Card />
        </div>
      </template>
    </UDrawer>
  </template>

  <UPopover v-else v-model:open="open" :content="{ align: 'start', sideOffset: 6 }">
    <HeaderButton v-if="header" />
    <button
      v-else
      type="button"
      class="inline-flex max-w-full items-center gap-1.5 rounded-full py-0.5 ps-0.5 pe-2 align-middle hover:bg-elevated"
      :class="strong ? 'font-medium text-highlighted' : 'text-toned'"
      :title="address.email"
      v-bind="$attrs"
      @click.stop
    >
      <MailAvatar :address="address" size="xs" />
      <span class="truncate">{{ addressLabel(address) }}</span>
    </button>

    <template #content>
      <div class="w-96 overflow-hidden rounded-2xl text-sm" @click.stop>
        <Card />
      </div>
    </template>
  </UPopover>
</template>
