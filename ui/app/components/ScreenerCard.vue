<script setup lang="ts">
import type { DropdownMenuItem } from '@nuxt/ui'
import type { HeldSender, Screen } from '~/utils/types'

/** On touch screens, a swipe right accepts and a swipe left refuses. */
const props = defineProps<{
  sender: HeldSender
  active: boolean
  domain: boolean
  /** The domain choice is offered (not for public webmail domains). */
  domainable: boolean
  showAccount: boolean
}>()
const emit = defineEmits<{
  screen: [verdict: Screen]
  open: []
  focus: []
  'update:domain': [value: boolean]
}>()
const { t } = useI18n()
const { accountById, accountColor } = useMail()

const address = computed(() => ({ name: props.sender.name, email: props.sender.email }))
const who = computed(() => props.sender.name?.trim() || props.sender.email)
const host = computed(() => props.sender.email.split('@').pop() ?? '')
const account = computed(() => accountById(props.sender.account_id))

const acceptItems = computed<DropdownMenuItem[]>(() => [
  { label: t('mail.screener.asNewsletter'), icon: 'i-lucide-newspaper', onSelect: () => emit('screen', 'newsletter') },
  { label: t('mail.screener.asNotification'), icon: 'i-lucide-bell', onSelect: () => emit('screen', 'notification') }
])

/** Past this distance (px), letting go decides. */
const THRESHOLD = 96
const dx = ref(0)
const dragging = ref(false)
let start: { x: number, y: number, id: number } | null = null
/** The gesture turned out horizontal (a swipe) rather than a scroll. */
let horizontal: boolean | null = null
/** A swipe just ended: the click that may follow is not an "open". */
let swiped = false

function down(e: PointerEvent) {
  if (e.pointerType === 'mouse') return
  start = { x: e.clientX, y: e.clientY, id: e.pointerId }
  horizontal = null
}
function move(e: PointerEvent) {
  if (!start || e.pointerId !== start.id) return
  const x = e.clientX - start.x
  const y = e.clientY - start.y
  if (horizontal === null) {
    if (Math.abs(x) < 8 && Math.abs(y) < 8) return
    horizontal = Math.abs(x) > Math.abs(y)
    if (!horizontal) {
      start = null
      return
    }
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
    dragging.value = true
  }
  dx.value = x
}
function up() {
  if (!start) return
  const x = dx.value
  start = null
  dragging.value = false
  if (horizontal) {
    swiped = true
    setTimeout(() => { swiped = false }, 50)
  }
  horizontal = null
  dx.value = 0
  if (x > THRESHOLD) emit('screen', 'inbox')
  else if (x < -THRESHOLD) emit('screen', 'block')
}
function cancel() {
  start = null
  horizontal = null
  dragging.value = false
  dx.value = 0
}
function open() {
  if (!swiped) emit('open')
}

const root = useTemplateRef<HTMLElement>('root')
watch(() => props.active, (on) => {
  if (on) root.value?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
})
</script>

<template>
  <article ref="root" class="relative" :aria-label="who" @click="emit('focus')">
    <div
      v-if="dx !== 0"
      class="absolute inset-0 flex items-center gap-2 rounded-2xl px-5 text-[15px] font-semibold text-white"
      :class="dx > 0 ? 'justify-start bg-success' : 'justify-end bg-error'"
      aria-hidden="true"
    >
      <UIcon :name="dx > 0 ? 'i-lucide-user-round-check' : 'i-lucide-user-round-x'" class="size-5" :class="Math.abs(dx) > THRESHOLD ? '' : 'opacity-60'" />
      {{ dx > 0 ? $t('mail.screener.accept') : $t('mail.screener.refuse') }}
    </div>

    <div
      class="relative flex touch-pan-y flex-col gap-3 rounded-2xl border bg-(--surface-card) p-4 shadow-xs dark:shadow-none lg:p-5"
      :class="[active ? 'border-primary/60 ring-1 ring-primary/40' : 'border-default', dragging ? '' : 'transition-transform duration-150']"
      :style="dx ? { transform: `translateX(${dx}px)` } : undefined"
      @pointerdown="down"
      @pointermove="move"
      @pointerup="up"
      @pointercancel="cancel"
    >
      <div class="flex items-start gap-3">
        <MailAvatar :address="address" :account-id="sender.account_id" size="lg" />
        <div class="flex min-w-0 flex-1 flex-col gap-0.5">
          <div class="flex min-w-0 items-center gap-2">
            <span class="truncate text-[15px] font-semibold text-highlighted">{{ who }}</span>
            <span
              v-if="showAccount && account"
              class="inline-flex h-5 max-w-40 shrink-0 items-center truncate rounded-full px-2 text-[11px] font-semibold"
              :class="accountColor(account.id).soft"
              :title="account.email"
            >{{ accountName(account) }}</span>
            <span class="ms-auto shrink-0 ps-1 text-xs font-medium text-muted" :title="longDate(sender.latest.date)">{{ shortDate(sender.latest.date) }}</span>
          </div>
          <p class="truncate text-[13px] text-muted">
            <template v-if="sender.name?.trim()">{{ sender.email }} · </template>{{ $t('mail.screener.messages', { n: sender.count }, sender.count) }}
          </p>
        </div>
      </div>

      <button
        type="button"
        class="flex flex-col gap-0.5 rounded-xl bg-(--surface-side) px-3.5 py-2.5 text-start transition-colors hover:bg-elevated focus-visible:outline-2 focus-visible:outline-primary"
        :title="$t('mail.screener.openLatest')"
        @click.stop="open"
      >
        <span class="truncate text-sm font-semibold text-highlighted">{{ sender.latest.subject || $t('common.noSubject') }}</span>
        <span v-if="sender.latest.preview" class="line-clamp-2 text-[13px] text-muted">{{ sender.latest.preview }}</span>
      </button>

      <p v-if="sender.trust === 'low'" class="flex items-start gap-2 rounded-xl bg-error/8 px-3.5 py-2 text-[12.5px] text-error">
        <UIcon name="i-lucide-shield-alert" class="mt-px size-4 shrink-0" />
        {{ $t('mail.screener.lowTrust') }}
      </p>

      <div class="flex flex-wrap items-center gap-x-3 gap-y-2.5">
        <UCheckbox
          v-if="domainable"
          :model-value="domain"
          :label="$t('mail.screener.wholeDomain', { domain: `@${host}` })"
          class="min-w-0"
          :ui="{ label: 'text-[13px] text-default truncate' }"
          @update:model-value="(v) => emit('update:domain', v === true)"
          @click.stop
        />
        <div class="ms-auto flex items-center gap-2 max-sm:w-full">
          <UButton
            :label="$t('mail.screener.refuse')"
            icon="i-lucide-user-round-x"
            color="neutral"
            variant="outline"
            class="justify-center rounded-[10px] max-sm:min-h-11 max-sm:flex-1"
            @click.stop="emit('screen', 'block')"
          />
          <UFieldGroup class="max-sm:flex-1">
            <UButton
              :label="$t('mail.screener.accept')"
              icon="i-lucide-user-round-check"
              class="flex-1 justify-center rounded-s-[10px] max-sm:min-h-11"
              @click.stop="emit('screen', 'inbox')"
            />
            <UDropdownMenu :items="acceptItems" :content="{ align: 'end' }">
              <UButton
                icon="i-lucide-chevron-down"
                class="rounded-e-[10px] border-s border-white/25 max-sm:min-h-11"
                :aria-label="$t('mail.screener.acceptAs')"
                @click.stop
              />
            </UDropdownMenu>
          </UFieldGroup>
        </div>
      </div>
    </div>
  </article>
</template>
