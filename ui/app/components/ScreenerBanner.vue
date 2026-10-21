<script setup lang="ts">
import type { HeldSender } from '~/utils/types'

const props = defineProps<{
  /** `null`: all accounts (unified inbox). */
  accountId: number | null
}>()
const { mailboxes, tick } = useMail()

const waiting = computed(() => mailboxes.value.some(m => m.role === 'screener' && m.total > 0 && (props.accountId === null || m.account_id === props.accountId)))

const senders = ref<HeldSender[]>([])
let request = 0
async function load() {
  const mine = ++request
  if (!waiting.value) {
    senders.value = []
    return
  }
  const list = await api.heldSenders(props.accountId).catch(() => null)
  if (list && mine === request) senders.value = list
}
const reloader = useReloader(load)
watch([waiting, () => props.accountId], () => reloader.now(), { immediate: true })
watch(tick, reloader.schedule)

const faces = computed(() => senders.value.slice(0, 3).map(s => ({ key: `${s.account_id}:${s.email}`, address: { name: s.name, email: s.email }, accountId: s.account_id })))
const detail = computed(() => {
  const list = senders.value
  const who = (s: HeldSender) => s.name || s.email
  if (list.length === 1) {
    const s = list[0]!
    return s.latest.subject ? `${who(s)} · ${s.latest.subject}` : who(s)
  }
  return list.map(who).join(', ')
})
const to = computed(() => (props.accountId === null ? '/screener' : { path: '/screener', query: { account: String(props.accountId) } }))
</script>

<template>
  <NuxtLink
    v-if="waiting && senders.length"
    :to="to"
    class="flex items-center gap-3 border-b border-default bg-primary/8 px-4 py-2.5 text-sm transition-colors hover:bg-primary/12 sm:px-5"
  >
    <span class="flex shrink-0 -space-x-1.5">
      <MailAvatar
        v-for="f in faces"
        :key="f.key"
        :address="f.address"
        :account-id="f.accountId"
        size="xs"
        class="ring-2 ring-(--list-bg)"
      />
    </span>
    <span class="min-w-0 flex-1">
      <span class="block truncate font-semibold text-highlighted">{{ $t('mail.screener.banner', { n: senders.length }, senders.length) }}</span>
      <span class="block truncate text-xs text-muted">{{ detail }}</span>
    </span>
    <span class="flex shrink-0 items-center gap-1 rounded-md bg-primary px-2.5 py-1 text-xs font-semibold text-inverted">
      {{ $t('mail.screener.review') }}
      <UIcon name="i-lucide-chevron-right" class="size-3.5" />
    </span>
  </NuxtLink>
</template>
