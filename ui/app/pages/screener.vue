<script setup lang="ts">
import type { HeldSender, Screen } from '~/utils/types'

const route = useRoute()
const { accounts, accountById, tick } = useMail()
const { open: composing } = useComposer()
const { done } = useUndo()
const toast = useToast()
const showError = useErrorToast()
const { t } = useI18n()
const back = useScreenBack()

const scope = ref<number | null>(null)
watch(() => route.query.account, (id) => {
  scope.value = typeof id === 'string' && id ? Number(id) : null
}, { immediate: true })
watch(() => accounts.value.map(a => a.id).join(), () => {
  if (scope.value !== null && !accountById(scope.value)) scope.value = null
})
const enabled = computed(() => accounts.value.some(a => a.screener_since !== null && (scope.value === null || a.id === scope.value)))

const senders = ref<HeldSender[] | null>(null)
/** Hidden at once, before the engine has moved their messages. */
const pending = ref(new Set<string>())
const keyOf = (s: HeldSender) => `${s.account_id}:${s.email}`
let request = 0

async function load() {
  const mine = ++request
  try {
    const list = await api.heldSenders(scope.value)
    if (mine === request) senders.value = list
  } catch (e) {
    if (mine === request) {
      senders.value ??= []
      showError(e)
    }
  }
}
const reloader = useReloader(load, 1000)
watch(scope, () => reloader.now(), { immediate: true })
watch(tick, reloader.schedule)

const list = computed(() => (senders.value ?? []).filter(s => !pending.value.has(keyOf(s))))

const hostOf = (s: HeldSender) => s.email.split('@').pop()?.toLowerCase() ?? ''
/** False for public mail providers, whose domain would also cover strangers. */
const domainable = (s: HeldSender) => s.domainable
const domains = ref(new Set<string>())
function setDomain(s: HeldSender, on: boolean) {
  const next = new Set(domains.value)
  if (on) next.add(keyOf(s))
  else next.delete(keyOf(s))
  domains.value = next
}

const DONE: Record<Screen, { key: string, icon: string }> = {
  inbox: { key: 'mail.screener.accepted', icon: 'i-lucide-user-round-check' },
  newsletter: { key: 'mail.screener.acceptedNewsletter', icon: 'i-lucide-newspaper' },
  notification: { key: 'mail.screener.acceptedNotification', icon: 'i-lucide-bell' },
  block: { key: 'mail.screener.refused', icon: 'i-lucide-user-round-x' }
}

async function screen(s: HeldSender, verdict: Screen) {
  const key = keyOf(s)
  const domain = domainable(s) && domains.value.has(key)
  const covered = (senders.value ?? []).filter(o => o.account_id === s.account_id && (domain ? hostOf(o) === hostOf(s) : keyOf(o) === key))
  pending.value = new Set([...pending.value, ...covered.map(keyOf)])
  try {
    const token = await api.screenSender(s.account_id, s.email, verdict, domain)
    const name = domain ? `@${hostOf(s)}` : (s.name?.trim() || s.email)
    // No undo token when nothing moved; the decision is still recorded.
    if (token === null) toast.add({ title: t(DONE[verdict].key, { name }), icon: DONE[verdict].icon })
    else done(token, t(DONE[verdict].key, { name }), DONE[verdict].icon)
  } catch (e) {
    showError(e)
  } finally {
    await load()
    const next = new Set(pending.value)
    for (const o of covered) next.delete(keyOf(o))
    pending.value = next
  }
}

function openLatest(s: HeldSender) {
  navigateTo(threadRoute(s.latest))
}

const cursor = ref(0)
watch(() => list.value.length, (n) => {
  if (cursor.value > n - 1) cursor.value = Math.max(0, n - 1)
})
const current = computed(() => list.value[cursor.value] ?? null)
const free = () => !composing.value && !overlayOpen()
const step = (d: number) => () => {
  if (free() && list.value.length) cursor.value = Math.min(list.value.length - 1, Math.max(0, cursor.value + d))
}
const onCurrent = (fn: (s: HeldSender) => void) => () => {
  if (free() && current.value) fn(current.value)
}
defineShortcuts({
  j: step(1),
  k: step(-1),
  arrowdown: step(1),
  arrowup: step(-1),
  y: onCurrent(s => screen(s, 'inbox')),
  n: onCurrent(s => screen(s, 'block')),
  o: onCurrent(openLatest)
})
</script>

<template>
  <UDashboardPanel id="screener" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <UDashboardNavbar :title="t('mail.screener.title')" class="max-lg:hidden">
        <template v-if="accounts.length > 1" #right>
          <AccountSelect v-model="scope" allow-all />
        </template>
      </UDashboardNavbar>
      <div class="flex shrink-0 items-center gap-1 border-b border-default bg-default px-2 py-2.5 lg:hidden dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-arrow-left"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl text-highlighted"
          :aria-label="t('tools.shared.back')"
          @click="back"
        />
        <h1 class="text-[17px] font-semibold text-highlighted">{{ t('mail.screener.title') }}</h1>
      </div>
    </template>

    <template #body>
      <div class="mx-auto flex w-full max-w-3xl flex-col gap-3 px-4 pt-3.5 pb-8 lg:gap-4 lg:px-8 lg:pt-8 lg:pb-12">
        <div v-if="accounts.length > 1" class="lg:hidden">
          <AccountSelect v-model="scope" allow-all size="lg" />
        </div>

        <template v-if="senders === null" />
        <UEmpty
          v-else-if="!list.length"
          :icon="enabled ? 'i-lucide-user-round-check' : 'i-lucide-user-round-search'"
          :title="t('mail.screener.empty')"
          :description="enabled ? t('mail.screener.emptyHint') : t('mail.screener.offHint')"
          :actions="enabled ? undefined : [{ label: t('mail.screener.openSettings'), icon: 'i-lucide-settings', color: 'neutral', variant: 'subtle', to: '/settings' }]"
          variant="naked"
          class="py-16"
        />
        <template v-else>
          <p class="text-[13px] text-muted">
            {{ t('mail.screener.intro') }}
            <span class="max-lg:hidden">
              · <UKbd value="j" /> <UKbd value="k" /> {{ t('mail.screener.hint.move') }}
              · <UKbd value="y" /> {{ t('mail.screener.hint.accept') }}
              · <UKbd value="n" /> {{ t('mail.screener.hint.refuse') }}
              · <UKbd value="o" /> {{ t('mail.screener.hint.open') }}
            </span>
            <span class="lg:hidden">{{ t('mail.screener.swipeHint') }}</span>
          </p>
          <TransitionGroup tag="div" class="flex flex-col gap-3" move-class="transition-transform duration-200" leave-active-class="transition-opacity duration-150" leave-to-class="opacity-0">
            <ScreenerCard
              v-for="(s, i) in list"
              :key="keyOf(s)"
              :sender="s"
              :active="i === cursor"
              :domain="domains.has(keyOf(s))"
              :domainable="domainable(s)"
              :show-account="accounts.length > 1"
              @update:domain="(v) => setDomain(s, v)"
              @screen="(v) => screen(s, v)"
              @open="openLatest(s)"
              @focus="cursor = i"
            />
          </TransitionGroup>
        </template>
      </div>
    </template>
  </UDashboardPanel>
</template>
