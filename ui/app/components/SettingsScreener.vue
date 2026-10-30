<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import type { Account, Screening } from '~/utils/types'

const props = withDefaults(defineProps<{
  account: Account
  touch?: boolean
}>(), { touch: false })
const { tick, mailboxByRole, loadAccounts } = useMail()
const { done } = useUndo()
const showError = useErrorToast()
const { t } = useI18n()

const on = computed(() => props.account.screener_since !== null)
const switching = ref(false)

async function setOn(value: boolean) {
  const a = props.account
  if (!value) {
    // Turned off, the messages still waiting go back to the inbox: said first.
    const waiting = mailboxByRole('screener', a.id)?.total ?? 0
    if (waiting > 0) {
      const ok = await confirm(t('settings.accounts.screener.offConfirm', { n: waiting }, waiting), {
        title: t('settings.accounts.screener.offTitle'),
        kind: 'warning'
      })
      if (!ok) return
    }
  }
  switching.value = true
  try {
    await api.setScreener(a.id, value)
    await loadAccounts()
  } catch (e) {
    showError(e)
  } finally {
    switching.value = false
  }
}

const decisions = ref<Screening[]>([])
let request = 0
async function load() {
  const mine = ++request
  const list = await api.screening(props.account.id).catch(() => null)
  if (list && mine === request) decisions.value = list
}
const reloader = useReloader(load)
watch(() => props.account.id, () => reloader.now(), { immediate: true })
watch(tick, reloader.schedule)

const query = ref('')
const shown = computed(() => {
  const q = query.value.trim().toLowerCase()
  return q ? decisions.value.filter(d => d.pattern.includes(q)) : decisions.value
})
const isDomain = (d: Screening) => d.pattern.startsWith('@')

/** Accepted ↔ refused, through the screener itself: messages move as for a first decision. */
async function flip(d: Screening) {
  const block = d.decision === 'accept'
  try {
    const token = await api.screenSender(props.account.id, d.pattern, block ? 'block' : 'inbox', isDomain(d))
    done(token, t(block ? 'settings.accounts.screener.nowBlocked' : 'settings.accounts.screener.nowAccepted', { pattern: d.pattern }), block ? 'i-lucide-user-round-x' : 'i-lucide-user-round-check')
  } catch (e) {
    showError(e)
  }
  await load()
}

/** Refused → refused and deleted: what lies in "Screened out" goes to the trash, the next ones too. */
async function alsoDelete(d: Screening) {
  try {
    const token = await api.screenSender(props.account.id, d.pattern, 'delete', isDomain(d))
    done(token, t('settings.accounts.screener.nowDeleted', { pattern: d.pattern }), 'i-lucide-trash-2')
  } catch (e) {
    showError(e)
  }
  await load()
}

const badgeKey = (d: Screening) => d.decision === 'accept' ? 'accepted' : d.decision === 'delete' ? 'deleted' : 'blocked'

async function forget(d: Screening) {
  try {
    await api.forgetScreening(props.account.id, d.pattern)
    decisions.value = decisions.value.filter(x => x.pattern !== d.pattern)
  } catch (e) {
    showError(e)
  }
}
</script>

<template>
  <div class="flex flex-col gap-3">
    <div class="flex items-center gap-3">
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="font-semibold text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ $t('settings.accounts.screener.label') }}</span>
        <span class="text-[13px] text-muted">{{ $t('settings.accounts.screener.help') }}</span>
      </div>
      <USwitch
        :model-value="on"
        size="lg"
        :loading="switching"
        :disabled="switching"
        :aria-label="$t('settings.accounts.screener.label')"
        @update:model-value="setOn"
      />
    </div>

    <UButton
      v-if="on"
      :label="$t('settings.accounts.screener.review')"
      icon="i-lucide-user-round-search"
      trailing-icon="i-lucide-chevron-right"
      color="neutral"
      variant="outline"
      :size="touch ? 'lg' : 'sm'"
      :to="{ path: '/screener', query: { account: String(account.id) } }"
      :class="touch ? 'min-h-11 justify-center rounded-xl' : 'self-start rounded-[9px]'"
      :ui="{ trailingIcon: 'size-3.5 text-dimmed' }"
    />

    <template v-if="decisions.length">
      <div class="flex items-center gap-2.5">
        <span class="flex-1 text-xs font-bold uppercase tracking-wider text-muted">
          {{ $t('settings.accounts.screener.decisions', { n: decisions.length }, decisions.length) }}
        </span>
      </div>
      <UInput
        v-if="decisions.length > 5"
        v-model="query"
        icon="i-lucide-search"
        :placeholder="$t('settings.accounts.screener.search')"
        :aria-label="$t('settings.accounts.screener.search')"
        :size="touch ? 'lg' : 'md'"
        :ui="{ base: touch ? 'h-11 rounded-xl' : 'h-9.5 rounded-[10px]' }"
      />
      <ul v-if="shown.length" class="max-h-80 divide-y divide-default overflow-y-auto rounded-xl border border-default">
        <li
          v-for="d in shown"
          :key="d.pattern"
          class="flex items-center gap-2.5 ps-3.5 pe-1.5"
          :class="touch ? 'min-h-14 py-1.5' : 'min-h-11 py-1'"
        >
          <UIcon
            :name="isDomain(d) ? 'i-lucide-globe' : 'i-lucide-at-sign'"
            class="size-4 shrink-0"
            :class="isDomain(d) ? 'text-blue-600 dark:text-blue-400' : 'text-pink-700 dark:text-pink-300'"
          />
          <div class="flex min-w-0 flex-1 flex-col">
            <span class="truncate text-highlighted" :class="touch ? 'text-[15px]' : 'text-sm'">{{ d.pattern }}</span>
            <span class="truncate text-xs text-muted">
              {{ isDomain(d) ? $t('settings.accounts.screener.wholeDomain') : $t('settings.accounts.screener.address') }} · {{ shortDate(d.created_at) }}
            </span>
          </div>
          <UBadge
            :label="$t(`settings.accounts.screener.${badgeKey(d)}`)"
            :color="d.decision === 'accept' ? 'success' : 'error'"
            variant="subtle"
            class="shrink-0"
          />
          <UTooltip v-if="d.decision === 'block'" :text="$t('settings.accounts.screener.makeDelete')">
            <UButton
              icon="i-lucide-trash-2"
              color="neutral"
              variant="ghost"
              :size="touch ? 'lg' : 'sm'"
              :class="touch ? 'size-11 justify-center' : ''"
              :aria-label="$t('settings.accounts.screener.makeDelete')"
              @click="alsoDelete(d)"
            />
          </UTooltip>
          <UTooltip :text="d.decision === 'accept' ? $t('settings.accounts.screener.makeBlock') : $t('settings.accounts.screener.makeAccept')">
            <UButton
              :icon="d.decision === 'accept' ? 'i-lucide-user-round-x' : 'i-lucide-user-round-check'"
              color="neutral"
              variant="ghost"
              :size="touch ? 'lg' : 'sm'"
              :class="touch ? 'size-11 justify-center' : ''"
              :aria-label="d.decision === 'accept' ? $t('settings.accounts.screener.makeBlock') : $t('settings.accounts.screener.makeAccept')"
              @click="flip(d)"
            />
          </UTooltip>
          <UTooltip :text="$t('settings.accounts.screener.forget')">
            <UButton
              icon="i-lucide-x"
              color="neutral"
              variant="ghost"
              :size="touch ? 'lg' : 'sm'"
              :class="touch ? 'size-11 justify-center' : ''"
              :aria-label="$t('settings.accounts.screener.forget')"
              @click="forget(d)"
            />
          </UTooltip>
        </li>
      </ul>
      <p v-else class="text-[13px] text-muted">{{ $t('settings.accounts.screener.noMatch') }}</p>
    </template>
    <p v-else-if="on" class="text-xs text-dimmed">{{ $t('settings.accounts.screener.empty') }}</p>
  </div>
</template>
