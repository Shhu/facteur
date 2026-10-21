<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import type { Outgoing } from '~/utils/types'

const { items, refresh } = useOutbox()
const { accounts, accountById } = useMail()
const { editOutgoing } = useComposer()
const showError = useErrorToast()
const { t } = useI18n()
const back = useScreenBack()

onMounted(refresh)

const scheduled = computed(() => items.value.filter(o => o.state !== 'failed'))
const failed = computed(() => items.value.filter(o => o.state === 'failed'))

function status(o: Outgoing) {
  if (o.state === 'sending') return t('settings.outbox.sending')
  if (o.state === 'failed') return t('settings.outbox.failedStatus')
  return o.scheduled ? t('settings.outbox.scheduledAt', { when: untilLabel(o.send_at) }) : t('settings.outbox.soon')
}

async function sendNow(o: Outgoing) {
  await api.sendNow(o.id).catch(showError)
}

async function remove(o: Outgoing) {
  const ok = await confirm(t('settings.outbox.removeConfirm', { subject: o.subject || t('common.noSubject') }), { title: t('settings.outbox.removeTitle'), kind: 'warning' })
  if (!ok) return
  try {
    const draft = await api.cancelSend(o.id)
    if (draft.draft_id) await api.discardDraft(draft.account_id, draft.draft_id)
  } catch (e) {
    showError(e)
  }
}

const reminderLabel = (seconds: number) => {
  const days = Math.round(seconds / 86_400)
  return days >= 7 ? t('settings.outbox.remindWeek') : t('settings.outbox.remindDays', { n: days }, days)
}
</script>

<template>
  <UDashboardPanel id="outbox">
    <template #header>
      <UDashboardNavbar :title="$t('settings.outbox.title')">
        <template #leading>
          <!-- Below lg the sidebar is a drawer: offer a way back instead. -->
          <UButton
            icon="i-lucide-arrow-left"
            color="neutral"
            variant="ghost"
            class="size-11 justify-center lg:hidden"
            :aria-label="$t('tools.shared.back')"
            @click="back"
          />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="mx-auto flex w-full max-w-3xl flex-col gap-6 py-2">
        <div v-if="!items.length" class="flex flex-col items-center gap-2 py-16 text-sm text-muted">
          <UIcon name="i-lucide-send-horizontal" class="size-8" />
          {{ $t('settings.outbox.empty') }}
        </div>

        <section v-for="part in [{ title: $t('settings.outbox.failed'), list: failed }, { title: $t('settings.outbox.scheduled'), list: scheduled }]" v-show="part.list.length" :key="part.title">
          <h2 class="mb-2 text-sm font-medium text-muted">{{ part.title }}</h2>
          <div class="divide-y divide-default overflow-hidden rounded-lg border border-default">
            <div v-for="o in part.list" :key="o.id" class="flex flex-wrap items-center gap-3 px-4 py-3">
              <UIcon
                :name="o.state === 'failed' ? 'i-lucide-circle-alert' : 'i-lucide-calendar-clock'"
                class="size-5 shrink-0"
                :class="o.state === 'failed' ? 'text-error' : 'text-primary'"
              />
              <div class="min-w-0 flex-1">
                <p class="truncate text-sm font-medium text-highlighted">{{ o.subject || $t('common.noSubject') }}</p>
                <p class="truncate text-xs text-muted">
                  {{ $t('settings.outbox.to', { to: o.to.join(', ') }) }}<template v-if="accounts.length > 1 && accountById(o.account_id)"> · {{ $t('settings.outbox.from', { account: accountName(accountById(o.account_id)!) }) }}</template>
                </p>
                <p class="text-xs" :class="o.state === 'failed' ? 'text-error' : 'text-toned'">
                  {{ status(o) }}<template v-if="o.remind_after"> · {{ reminderLabel(o.remind_after) }}</template>
                </p>
                <p v-if="o.error" class="mt-1 text-xs text-muted">{{ o.error }}</p>
              </div>
              <div v-if="o.state !== 'sending'" class="flex items-center gap-1">
                <UButton
                  :label="o.state === 'failed' ? $t('settings.outbox.retry') : $t('settings.outbox.sendNow')"
                  icon="i-lucide-send"
                  size="sm"
                  color="neutral"
                  variant="outline"
                  @click="sendNow(o)"
                />
                <UButton :label="$t('settings.outbox.edit')" icon="i-lucide-pencil" size="sm" color="neutral" variant="ghost" @click="editOutgoing(o.id)" />
                <UButton icon="i-lucide-trash-2" size="sm" color="neutral" variant="ghost" :aria-label="$t('settings.outbox.remove')" @click="remove(o)" />
              </div>
            </div>
          </div>
        </section>

        <p v-if="scheduled.length" class="text-xs text-dimmed">
          {{ $t('settings.outbox.note') }}
        </p>
      </div>
    </template>
  </UDashboardPanel>
</template>
