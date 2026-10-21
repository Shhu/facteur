<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import twColors from 'tailwindcss/colors'
import type { Account, Coverage } from '~/utils/types'

/** `row`: its line in the desktop list; `card`: the account screen on desktop; `page`: the
 *  account screen on mobile. Content only: the page draws the card around it. */
const props = withDefaults(defineProps<{
  account: Account
  layout: 'row' | 'card' | 'page'
  coverage?: Coverage
  busy?: boolean
}>(), { coverage: undefined, busy: false })
const emit = defineEmits<{
  refresh: []
}>()

const { accounts, syncing, accountColor, loadAccounts, loadMailboxes } = useMail()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()

const a = computed(() => props.account)
const color = computed(() => accountColor(a.value.id))
const name = computed(() => accountName(a.value))
const shown = useSettingMatch(() => [name.value, a.value.name ?? undefined, a.value.email, t('settings.accounts.keywords')])
const avatarOpen = ref(false)

/** Shades of the account colour as variables, for the tinted header and accents (the palette's
 *  classes only cover avatars and dots). */
const palette = twColors as unknown as Record<string, Record<number, string>>
const tint = computed(() => {
  const c = palette[accountColorName(a.value, accounts.value)] ?? {}
  return {
    '--acc-50': c[50], '--acc-200': c[200], '--acc-300': c[300], '--acc-400': c[400], '--acc-500': c[500], '--acc-700': c[700]
  } as Record<string, string>
})

const isSyncing = computed(() => syncing.value.includes(a.value.id))
const synced = useSyncedLabel()
const syncedLabel = computed(() => synced(a.value.last_sync))

/** A failure comes with the engine's `sync_finished` event (one toast, not two). */
async function syncNow() {
  await syncByUser([a.value.id])
}

const numberFormat = computed(() => new Intl.NumberFormat(intlLocale()))
const num = (n: number) => numberFormat.value.format(n)
/** `pct` (0-100) as the language writes a percentage ("42 %", "42%"). */
const percentFormat = computed(() => new Intl.NumberFormat(intlLocale(), { style: 'percent', maximumFractionDigits: 0 }))
const isComplete = computed(() => !!props.coverage && props.coverage.server !== null && props.coverage.local >= props.coverage.server)
const pct = computed(() => {
  const c = props.coverage
  if (!c || c.server === null) return 0
  return c.server ? Math.min(100, Math.round((c.local / c.server) * 100)) : 100
})

const fetching = ref(false)
const historyBusy = computed(() => props.busy || fetching.value)

async function fetchHistory() {
  fetching.value = true
  try {
    const added = await api.fetchAllHistory(a.value.id)
    toast.add({ title: t('settings.accounts.historyFetched'), description: t('settings.accounts.historyAdded', { n: num(added) }, added), icon: 'i-lucide-check' })
  } catch (e) {
    showError(e, t('settings.accounts.historyFailed'))
  } finally {
    fetching.value = false
    emit('refresh')
  }
}

const reconnecting = ref(false)
const passwordOpen = ref(false)
const { authProblems } = useAccountHealth()
const refused = computed(() => authProblems.value[a.value.id] ?? null)
/** Google account without its photo: reconnecting is how to get it. */
const getsPhoto = computed(() => a.value.auth.kind === 'oauth2' && a.value.auth.provider === 'google' && !a.value.avatar)

/** Through the provider's page again: renews the token and the permissions (photo). */
async function reconnect() {
  const account = a.value
  if (account.auth.kind !== 'oauth2') return
  reconnecting.value = true
  try {
    const identity = await api.oauthLogin(account.auth.provider, account.email)
    await api.reconnect(account.id, identity.ticket)
    await loadAccounts()
    toast.add({ title: t('settings.accounts.reconnected'), description: account.email, icon: 'i-lucide-check' })
  } catch (e) {
    showError(e, t('settings.accounts.reconnectFailed'))
  } finally {
    reconnecting.value = false
  }
}

async function remove() {
  const account = a.value
  const ok = await confirm(t('settings.accounts.removeConfirm', { email: account.email }), {
    title: t('settings.accounts.removeTitle'),
    kind: 'warning'
  })
  if (!ok) return
  try {
    await api.removeAccount(account.id)
    await loadAccounts()
    await loadMailboxes()
    if (!accounts.value.length) navigateTo('/setup')
    else navigateTo('/settings', { replace: true })
  } catch (e) {
    showError(e)
  }
}

const security = computed(() => ({ tls: 'TLS', starttls: 'STARTTLS', none: t('settings.accounts.unencrypted') }))
const imapLine = computed(() => `${a.value.imap.host}:${a.value.imap.port} · ${security.value[a.value.imap.security]}`)
const smtpLine = computed(() => `${a.value.smtp.host}:${a.value.smtp.port} · ${security.value[a.value.smtp.security]}`)
const serversSummary = computed(() => {
  const { imap, smtp } = a.value
  const same = imap.security === smtp.security
  return same
    ? `${imap.host}:${imap.port} · ${smtp.host}:${smtp.port} · ${security.value[imap.security]}`
    : `${imapLine.value} · ${smtpLine.value}`
})
const authLabel = computed(() => {
  const auth = a.value.auth
  if (auth.kind === 'password') return t('settings.accounts.authPassword')
  return t('settings.accounts.authOAuth', { provider: auth.provider === 'google' ? 'Google' : 'Microsoft' })
})

const sheet = ref<'identities' | 'servers' | null>(null)
const sheetOpen = computed({
  get: () => sheet.value !== null,
  set: (open: boolean) => {
    if (!open) sheet.value = null
  }
})
</script>

<template>
  <NuxtLink
    v-if="layout === 'row'"
    v-show="shown"
    :to="`/settings/account/${account.id}`"
    class="flex w-full items-center gap-3.5 px-5.5 py-3 text-start transition-colors hover:bg-elevated/40 focus-visible:bg-elevated/40 focus-visible:outline-none"
  >
    <UAvatar
      :src="account.avatar ?? undefined"
      :alt="name"
      :text="initials(name)"
      class="size-9.5 shrink-0 text-[13px]"
      :class="color.solid"
      :ui="{ fallback: 'text-inherit font-bold' }"
    />
    <span class="flex min-w-0 flex-1 flex-col">
      <span class="truncate text-[14.5px] font-semibold text-highlighted">{{ name }}</span>
      <span class="truncate text-[12.5px] text-muted">{{ account.name ? `${account.name} · ${account.email}` : account.email }}</span>
    </span>
    <span v-if="refused" class="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-error/10 px-2.5 py-1 text-xs font-semibold text-error">
      <UIcon name="i-lucide-key-round" class="size-3.5" />
      {{ $t('settings.accounts.refused') }}
    </span>
    <span v-else class="hidden shrink-0 items-center gap-1.5 text-xs text-muted xl:inline-flex">
      <UIcon v-if="isSyncing" name="i-lucide-loader-circle" class="size-3.5 animate-spin" />
      <span v-else class="size-1.75 rounded-full" :class="account.last_sync ? 'bg-green-600 dark:bg-green-500' : 'bg-(--ui-border-accented)'" />
      {{ isSyncing ? $t('common.syncing') : syncedLabel }}
    </span>
    <span
      v-if="coverage"
      class="inline-flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-semibold"
      :class="isComplete ? 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300' : 'bg-elevated text-muted'"
    >
      <UIcon :name="isComplete ? 'i-lucide-check' : 'i-lucide-cloud-download'" class="size-3.5" />
      {{ $t('settings.accounts.storedCount', { n: num(coverage.local) }, coverage.local) }}
    </span>
    <UIcon name="i-lucide-chevron-right" class="size-4.5 shrink-0 text-dimmed rtl:rotate-180" />
  </NuxtLink>

  <div v-else-if="layout === 'card'" class="relative" :style="tint">
    <button
      type="button"
      class="group/avatar absolute start-5.5 top-4.5 z-1 size-12 flex items-center justify-center rounded-full text-white transition-colors hover:bg-black/40 focus-visible:bg-black/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
      :aria-label="$t('app.avatar.edit')"
      :title="$t('app.avatar.edit')"
      @click="avatarOpen = true"
    >
      <UIcon name="i-lucide-pencil" class="size-4.5 opacity-0 transition-opacity group-hover/avatar:opacity-100 group-focus-visible/avatar:opacity-100" />
    </button>
    <AccountAvatarModal v-model:open="avatarOpen" :account="account" />
    <AccountPasswordModal v-if="account.auth.kind === 'password'" v-model:open="passwordOpen" :account="account" />
    <div class="flex w-full items-center gap-3.5 border-b border-default bg-(--acc-50) px-5.5 py-4.5 text-start dark:bg-(--acc-500)/7">
      <UAvatar
        :src="account.avatar ?? undefined"
        :alt="name"
        :text="initials(name)"
        class="size-12 shrink-0 text-base"
        :class="color.solid"
        :ui="{ fallback: 'text-inherit font-bold' }"
      />
      <span class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="truncate text-base font-bold text-highlighted">{{ name }}</span>
        <span class="truncate text-[13px] text-muted">{{ account.name ? `${account.name} · ${account.email}` : account.email }}</span>
      </span>
      <span
        class="inline-flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-semibold"
        :class="account.last_sync || isSyncing ? 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300' : 'bg-elevated text-muted'"
      >
        <UIcon v-if="isSyncing" name="i-lucide-loader-circle" class="size-3.5 animate-spin" />
        <span v-else class="size-1.75 rounded-full" :class="account.last_sync ? 'bg-green-600 dark:bg-green-500' : 'bg-(--ui-border-accented)'" />
        {{ isSyncing ? $t('common.syncing') : syncedLabel }}
      </span>
    </div>

    <div class="grid grid-cols-2 gap-5 border-b border-default px-5.5 py-4.5">
      <div class="flex flex-col gap-2.5">
        <span class="text-xs font-bold uppercase tracking-wider text-muted">{{ $t('settings.accounts.colorTitle') }}</span>
        <AccountColorPicker :account="account" />
      </div>
      <div class="flex min-w-0 flex-col gap-2.5">
        <span class="text-xs font-bold uppercase tracking-wider text-muted">{{ $t('settings.accounts.servers') }}</span>
        <div class="flex flex-col gap-1 text-[13px] text-default">
          <span class="truncate"><span class="inline-block w-11.5 font-semibold text-muted">IMAP</span>{{ imapLine }}</span>
          <span class="truncate"><span class="inline-block w-11.5 font-semibold text-muted">SMTP</span>{{ smtpLine }}</span>
        </div>
      </div>
    </div>

    <div v-if="coverage" class="flex flex-col gap-2.5 border-b border-default px-5.5 py-4.5">
      <div class="flex flex-wrap items-center gap-2.5">
        <UIcon
          :name="isComplete ? 'i-lucide-circle-check' : 'i-lucide-cloud-download'"
          class="size-4.5 shrink-0"
          :class="isComplete ? 'text-success' : 'text-(--acc-700) dark:text-(--acc-300)'"
        />
        <span class="min-w-0 flex-1 text-sm text-highlighted">
          <template v-if="coverage.server === null">{{ $t('settings.accounts.coverageUnknown', { local: num(coverage.local) }) }}</template>
          <i18n-t v-else keypath="settings.accounts.stored" :plural="coverage.server ?? 0" scope="global">
            <template #count>
              <b class="font-semibold">{{ num(coverage.local) }} / {{ num(coverage.server ?? 0) }}</b>
            </template>
          </i18n-t>
          · <span class="text-muted">{{ $t('settings.accounts.bodies', { n: num(coverage.bodies) }, coverage.bodies) }}</span>
        </span>
        <UBadge v-if="isComplete" :label="$t('settings.accounts.complete')" color="success" variant="subtle" />
        <UButton
          v-else-if="coverage.server !== null"
          :label="$t('settings.accounts.fetchAll')"
          icon="i-lucide-download"
          size="sm"
          color="neutral"
          variant="outline"
          class="rounded-[9px]"
          :loading="historyBusy"
          :disabled="historyBusy"
          @click="fetchHistory"
        />
      </div>
      <div v-if="coverage.server" class="h-2 overflow-hidden rounded-full bg-elevated">
        <div class="h-full rounded-full transition-[width]" :class="color.dot" :style="{ width: `${pct}%` }" />
      </div>
      <SyncProgress v-if="historyBusy" :account-id="account.id" />
    </div>

    <div v-if="account.quota" class="border-b border-default px-5.5 py-4.5">
      <AccountQuota :quota="account.quota" />
    </div>

    <div class="border-b border-default px-5.5 py-4.5">
      <SettingsSignature :account="account" />
    </div>

    <div class="border-b border-default px-5.5 py-4">
      <IdentitiesEditor :account="account" />
    </div>

    <div class="border-b border-default px-5.5 py-4">
      <SettingsScreener :account="account" />
    </div>

    <div class="border-b border-default px-5.5 py-4">
      <SettingsSyncRhythm :account="account" />
    </div>

    <div class="border-b border-default px-5.5 py-4">
      <SettingsCalendar :account="account" />
    </div>

    <div class="border-b border-default px-5.5 py-4">
      <SettingsContacts :account="account" />
    </div>

    <div class="flex flex-wrap items-center gap-2.5 bg-(--surface-side) px-5.5 py-3.5">
      <span class="min-w-0 flex-1 text-[12.5px] text-muted">
        {{ account.last_sync ? $t('settings.accounts.lastSync', { date: longDate(account.last_sync) }) : $t('common.neverSynced') }}
      </span>
      <UTooltip v-if="account.auth.kind === 'oauth2'" :text="getsPhoto ? $t('settings.accounts.photoTooltip') : $t('settings.accounts.reconnectTooltip')">
        <UButton
          :label="getsPhoto ? $t('settings.accounts.getPhoto') : $t('settings.accounts.reconnect')"
          icon="i-lucide-refresh-ccw"
          size="sm"
          color="neutral"
          variant="outline"
          class="rounded-[9px] bg-(--surface-card)"
          :loading="reconnecting"
          @click="reconnect"
        />
      </UTooltip>
      <UButton
        v-if="account.auth.kind === 'password'"
        :label="refused ? $t('app.ops.health.enterPassword') : $t('settings.accounts.passwordChange')"
        icon="i-lucide-key-round"
        size="sm"
        :color="refused ? 'error' : 'neutral'"
        :variant="refused ? 'soft' : 'outline'"
        class="rounded-[9px]"
        :class="refused ? '' : 'bg-(--surface-card)'"
        @click="passwordOpen = true"
      />
      <UButton
        v-if="account.auth.kind === 'password'"
        :label="$t('settings.accounts.syncNow')"
        icon="i-lucide-refresh-cw"
        size="sm"
        color="neutral"
        variant="outline"
        class="rounded-[9px] bg-(--surface-card)"
        :loading="isSyncing"
        :disabled="isSyncing"
        @click="syncNow"
      />
      <UButton
        :label="$t('settings.accounts.removeTitle')"
        icon="i-lucide-trash-2"
        size="sm"
        color="error"
        variant="soft"
        class="rounded-[9px]"
        @click="remove"
      />
    </div>
  </div>

  <div v-else class="flex flex-col gap-4" :style="tint">
    <div class="flex flex-col items-center gap-2.5 rounded-[20px] border border-(--acc-200) bg-(--acc-50) px-4.5 py-5.5 text-center dark:border-(--acc-400)/25 dark:bg-(--acc-500)/10">
      <span class="relative">
        <UAvatar
          :src="account.avatar ?? undefined"
          :alt="name"
          :text="initials(name)"
          class="size-18 text-2xl"
          :class="color.solid"
          :ui="{ fallback: 'text-inherit font-bold' }"
        />
        <button
          type="button"
          class="group/avatar absolute inset-0 flex items-center justify-center rounded-full text-white transition-colors hover:bg-black/40 focus-visible:bg-black/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
          :aria-label="$t('app.avatar.edit')"
          @click="avatarOpen = true"
        >
          <UIcon name="i-lucide-pencil" class="size-5 opacity-0 transition-opacity group-hover/avatar:opacity-100 group-focus-visible/avatar:opacity-100" />
        </button>
        <span class="pointer-events-none absolute -end-0.5 -bottom-0.5 flex size-6.5 items-center justify-center rounded-full bg-default text-muted shadow ring-1 ring-default">
          <UIcon name="i-lucide-pencil" class="size-3.5" />
        </span>
      </span>
      <AccountAvatarModal v-model:open="avatarOpen" :account="account" />
      <AccountPasswordModal v-if="account.auth.kind === 'password'" v-model:open="passwordOpen" :account="account" />
      <div class="flex min-w-0 max-w-full flex-col gap-0.5">
        <span class="truncate text-xl font-bold text-highlighted">{{ name }}</span>
        <span class="truncate text-[13px] text-muted">{{ account.email }}</span>
      </div>
      <span
        class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-semibold"
        :class="account.last_sync || isSyncing ? 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300' : 'bg-elevated text-muted'"
      >
        <UIcon v-if="isSyncing" name="i-lucide-loader-circle" class="size-3.5 animate-spin" />
        <span v-else class="size-1.75 rounded-full" :class="account.last_sync ? 'bg-green-600 dark:bg-green-500' : 'bg-(--ui-border-accented)'" />
        {{ isSyncing ? $t('common.syncing') : syncedLabel }}
      </span>
    </div>

    <section class="flex flex-col gap-3 rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <h2 class="text-[15px] font-semibold text-highlighted">{{ $t('settings.accounts.colorTitle') }}</h2>
      <AccountColorPicker :account="account" size="lg" />
    </section>

    <section v-if="coverage" class="flex flex-col gap-2.5 rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <div class="flex items-baseline gap-2">
        <h2 class="flex-1 text-[15px] font-semibold text-highlighted">{{ $t('settings.accounts.onDevice') }}</h2>
        <span v-if="coverage.server !== null" class="text-[13px] font-bold" :class="isComplete ? 'text-success' : 'text-(--acc-700) dark:text-(--acc-300)'">{{ percentFormat.format(pct / 100) }}</span>
      </div>
      <div v-if="coverage.server" class="h-2 overflow-hidden rounded-full bg-elevated">
        <div class="h-full rounded-full transition-[width]" :class="color.dot" :style="{ width: `${pct}%` }" />
      </div>
      <span class="text-[13px] text-muted">
        {{ coverage.server === null
          ? $t('settings.accounts.coverageUnknown', { local: num(coverage.local) })
          : $t('settings.accounts.storedShort', { local: num(coverage.local), server: num(coverage.server) }) }}
        · {{ $t('settings.accounts.bodies', { n: num(coverage.bodies) }, coverage.bodies) }}
      </span>
      <SyncProgress v-if="historyBusy" :account-id="account.id" />
      <UButton
        v-if="!isComplete && coverage.server !== null"
        :label="$t('settings.accounts.fetchAll')"
        icon="i-lucide-download"
        size="lg"
        color="neutral"
        variant="outline"
        block
        class="min-h-11 rounded-xl"
        :loading="historyBusy"
        :disabled="historyBusy"
        @click="fetchHistory"
      />
    </section>

    <section v-if="account.quota" class="rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <AccountQuota :quota="account.quota" />
    </section>

    <section class="rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <SettingsSignature :account="account" touch />
    </section>

    <section class="rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <SettingsScreener :account="account" touch />
    </section>

    <section class="rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <SettingsSyncRhythm :account="account" touch />
    </section>

    <section class="rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <SettingsCalendar :account="account" touch />
    </section>

    <section class="rounded-2xl border border-default bg-(--surface-card) px-4 pt-3.5 pb-4 shadow-xs dark:shadow-none">
      <SettingsContacts :account="account" touch />
    </section>

    <section class="divide-y divide-default overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none">
      <SettingsListRow
        :label="$t('app.identities.label')"
        :sublabel="$t('settings.accounts.aliases', { n: account.identities.length }, account.identities.length)"
        button
        size="lg"
        @click="sheet = 'identities'"
      />
      <SettingsListRow
        :label="$t('settings.accounts.servers')"
        :sublabel="serversSummary"
        button
        size="lg"
        @click="sheet = 'servers'"
      />
      <button
        v-if="account.auth.kind === 'oauth2'"
        type="button"
        class="flex min-h-13 w-full items-center gap-2.5 px-3.5 text-start text-[15px] font-medium text-primary active:bg-elevated/60 disabled:opacity-60"
        :disabled="reconnecting"
        @click="reconnect"
      >
        <UIcon :name="reconnecting ? 'i-lucide-loader-circle' : 'i-lucide-refresh-ccw'" class="size-4.5" :class="reconnecting ? 'animate-spin' : ''" />
        {{ getsPhoto ? $t('settings.accounts.getPhoto') : $t('settings.accounts.reconnect') }}
      </button>
      <button
        v-if="account.auth.kind === 'password'"
        type="button"
        class="flex min-h-13 w-full items-center gap-2.5 px-3.5 text-start text-[15px] font-medium active:bg-elevated/60"
        :class="refused ? 'text-error' : 'text-primary'"
        @click="passwordOpen = true"
      >
        <UIcon name="i-lucide-key-round" class="size-4.5" />
        {{ refused ? $t('app.ops.health.enterPassword') : $t('settings.accounts.passwordChange') }}
      </button>
      <button
        v-if="account.auth.kind === 'password'"
        type="button"
        class="flex min-h-13 w-full items-center gap-2.5 px-3.5 text-start text-[15px] font-medium text-primary active:bg-elevated/60 disabled:opacity-60"
        :disabled="isSyncing"
        @click="syncNow"
      >
        <UIcon :name="isSyncing ? 'i-lucide-loader-circle' : 'i-lucide-refresh-cw'" class="size-4.5" :class="isSyncing ? 'animate-spin' : ''" />
        {{ isSyncing ? $t('common.syncing') : $t('settings.accounts.syncNow') }}
      </button>
    </section>

    <UButton
      :label="$t('settings.accounts.removeTitle')"
      icon="i-lucide-trash-2"
      size="lg"
      color="error"
      variant="soft"
      block
      class="mt-2 min-h-12.5 rounded-[14px] text-[15px] font-semibold"
      @click="remove"
    />

    <UDrawer
      v-model:open="sheetOpen"
      :title="sheet === 'servers' ? $t('settings.accounts.servers') : $t('app.identities.label')"
      handle-only
      :ui="{ container: 'pb-[calc(1rem+env(safe-area-inset-bottom))]' }"
    >
      <template #body>
        <IdentitiesEditor v-if="sheet === 'identities'" :account="account" touch />
        <dl v-else-if="sheet === 'servers'" class="divide-y divide-default overflow-hidden rounded-2xl border border-default text-sm">
          <div v-for="row in [
            { label: $t('settings.account.imapServer'), value: imapLine },
            { label: $t('settings.account.smtpServer'), value: smtpLine },
            { label: $t('settings.account.username'), value: account.username },
            { label: $t('settings.accounts.auth'), value: authLabel }
          ]" :key="row.label" class="flex min-h-13 flex-col justify-center gap-0.5 px-4 py-2">
            <dt class="text-[12.5px] text-muted">{{ row.label }}</dt>
            <dd class="break-all text-[15px] text-highlighted">{{ row.value }}</dd>
          </div>
        </dl>
      </template>
    </UDrawer>
  </div>
</template>
