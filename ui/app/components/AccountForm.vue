<script setup lang="ts">
import type { FormError } from '@nuxt/ui'
import twColors from 'tailwindcss/colors'
import type { AccountColorName } from '~/utils/accountColors'
import type { Account, Discovered, DiscoverySource, NewAccount, OAuthProvider, Security } from '~/utils/types'

withDefaults(defineProps<{ cancellable?: boolean }>(), { cancellable: false })
const emit = defineEmits<{ added: [account: Account], cancel: [] }>()
const { accounts } = useMail()
const showError = useErrorToast()
const { t } = useI18n()

// Known providers, configured without any network lookup.
// appPassword: the provider requires an app-specific password.
type Preset = { imap: [string, number, Security], smtp: [string, number, Security], appPassword?: true, oauth?: OAuthProvider }
const presets: Record<string, Preset> = {
  'gmail.com': { imap: ['imap.gmail.com', 993, 'tls'], smtp: ['smtp.gmail.com', 465, 'tls'], oauth: 'google' },
  'icloud.com': { imap: ['imap.mail.me.com', 993, 'tls'], smtp: ['smtp.mail.me.com', 587, 'starttls'], appPassword: true },
  'yahoo.com': { imap: ['imap.mail.yahoo.com', 993, 'tls'], smtp: ['smtp.mail.yahoo.com', 465, 'tls'], appPassword: true },
  'fastmail.com': { imap: ['imap.fastmail.com', 993, 'tls'], smtp: ['smtp.fastmail.com', 465, 'tls'], appPassword: true },
  'outlook.com': {
    imap: ['outlook.office365.com', 993, 'tls'],
    smtp: ['smtp.office365.com', 587, 'starttls'],
    oauth: 'microsoft'
  },
  'free.fr': { imap: ['imap.free.fr', 993, 'tls'], smtp: ['smtp.free.fr', 465, 'tls'] },
  'orange.fr': { imap: ['imap.orange.fr', 993, 'tls'], smtp: ['smtp.orange.fr', 465, 'tls'] },
  'sfr.fr': { imap: ['imap.sfr.fr', 993, 'tls'], smtp: ['smtp.sfr.fr', 465, 'tls'] },
  'laposte.net': { imap: ['imap.laposte.net', 993, 'tls'], smtp: ['smtp.laposte.net', 465, 'tls'] }
}
const aliases: Record<string, string> = {
  'googlemail.com': 'gmail.com', 'me.com': 'icloud.com', 'mac.com': 'icloud.com', 'yahoo.fr': 'yahoo.com',
  'hotmail.com': 'outlook.com', 'hotmail.fr': 'outlook.com', 'live.com': 'outlook.com', 'live.fr': 'outlook.com',
  'outlook.fr': 'outlook.com', 'msn.com': 'outlook.com', 'wanadoo.fr': 'orange.fr', 'neuf.fr': 'sfr.fr'
}

const state = reactive<NewAccount>({
  name: '',
  email: '',
  username: '',
  password: '',
  imap: { host: '', port: 993, security: 'tls' },
  smtp: { host: '', port: 465, security: 'tls' },
  auth: { kind: 'password' }
})
const advanced = ref(false)
const loading = ref(false)
const showPassword = ref(false)
/** The username follows the address until unticked. */
const sameUsername = ref(true)

/** Preselected: the colour the account would get by its rank. Left automatic unless changed. */
const autoColor = ACCOUNT_COLOR_NAMES[accounts.value.length % ACCOUNT_COLOR_NAMES.length]!
const color = ref<AccountColorName>(autoColor)
const colorName = (c: string) => t(`settings.appearance.colors.${c}`)
const palette = (twColors as unknown as Record<string, Record<number, string>>)
const swatch = (c: AccountColorName) => ({ '--sw': palette[c]?.[600], '--sw-dark': palette[c]?.[400] })
// Until the user edits the server settings, they are derived from the address.
const touched = ref(false)

const domain = computed(() => state.email.split('@')[1]?.toLowerCase().trim() ?? '')
const emailValid = computed(() => /^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(state.email.trim()))
/** A Microsoft account may sign in with any address, a Gmail one included: chosen by hand,
 *  Microsoft's servers and sign-in replace what the address suggests. */
const microsoftChosen = ref(false)
const preset = computed(() => microsoftChosen.value ? presets['outlook.com'] : presets[aliases[domain.value] ?? domain.value])

// Google and Microsoft no longer accept passwords: OAuth2 in the browser, whether the domain is
// known or discovery led to their servers (custom domain). Servers set by hand win: Microsoft's
// servers for a Gmail address mean a Microsoft account signed in with it.
const oauthProvider = computed<OAuthProvider | null>(() => {
  const host = state.imap.host.toLowerCase().trim().replace(/\.$/, '')
  // Whole labels only: imap.xgmail.com is not Google's.
  const under = (d: string) => host === d || host.endsWith('.' + d)
  if (under('gmail.com') || under('googlemail.com')) return 'google'
  if (under('office365.com') || under('outlook.com')) return 'microsoft'
  return host ? null : preset.value?.oauth ?? null
})
const providerName = computed(() => oauthProvider.value === 'google' ? 'Google' : 'Microsoft')

// Discovery (provider autoconfig, Mozilla ISPDB, MX, SRV, probes) for domains without a preset.
type Discovery =
  | { status: 'idle' }
  | { status: 'searching' }
  | { status: 'found', result: Discovered }
  | { status: 'failed' }
const discovery = ref<Discovery>({ status: 'idle' })
const sourceLabel = (source: DiscoverySource) => t(`settings.account.source.${source}`)
const discoveryLabel = computed(() => {
  if (discovery.value.status !== 'found') return ''
  const { provider, source } = discovery.value.result
  return provider ? `${provider} (${sourceLabel(source)})` : sourceLabel(source)
})

function applyServers(imap: NewAccount['imap'], smtp: NewAccount['smtp']) {
  Object.assign(state.imap, imap)
  Object.assign(state.smtp, smtp)
}

const emptyServers = () => [{ host: '', port: 993, security: 'tls' as Security }, { host: '', port: 465, security: 'tls' as Security }] as const

let discoverTimer: ReturnType<typeof setTimeout> | undefined
let discoverSeq = 0

async function discover(email: string) {
  const seq = ++discoverSeq
  discovery.value = { status: 'searching' }
  try {
    const result = await api.discover(email)
    if (seq !== discoverSeq || touched.value) return
    applyServers(result.imap, result.smtp)
    state.username = result.username
    sameUsername.value = result.username === state.email
    discovery.value = { status: 'found', result }
  } catch {
    if (seq !== discoverSeq || touched.value) return
    // Nothing answered: leave the fields empty and open the settings rather than guess a host.
    applyServers(...emptyServers())
    discovery.value = { status: 'failed' }
    advanced.value = true
  }
}

function applyPreset(p: Preset) {
  const [ih, ip, is] = p.imap
  const [sh, sp, ss] = p.smtp
  applyServers({ host: ih, port: ip, security: is }, { host: sh, port: sp, security: ss })
}

watch(microsoftChosen, () => {
  if (touched.value) return
  discoverSeq++
  discovery.value = { status: 'idle' }
  if (preset.value) applyPreset(preset.value)
  else applyServers(...emptyServers())
})

watch(() => state.email, (email, previous) => {
  if (sameUsername.value || !state.username || state.username === previous) state.username = email
  clearTimeout(discoverTimer)
  discoverSeq++ // invalidates a lookup in flight
  if (touched.value) return
  discovery.value = { status: 'idle' }
  if (!domain.value) return
  const p = preset.value
  if (p) {
    applyPreset(p)
    return
  }
  applyServers(...emptyServers())
  if (emailValid.value) discoverTimer = setTimeout(() => discover(state.email.trim()), 700)
})

watch(sameUsername, (same) => {
  if (same) state.username = state.email
})

const serversKnown = computed(() => !!state.imap.host && !!state.smtp.host)
const securityLabel = (s: Security) => securityItems.value.find(i => i.value === s)!.label
const serversSummary = computed(() => t('settings.account.serversSummary', {
  host: state.imap.host,
  imap: state.imap.port,
  smtp: state.smtp.port,
  security: state.imap.security === state.smtp.security ? securityLabel(state.imap.security) : `${securityLabel(state.imap.security)} / ${securityLabel(state.smtp.security)}`
}))
const serversShort = computed(() => [state.imap.host, state.imap.port, state.smtp.port, securityLabel(state.imap.security)].join(' · '))
const servers = [
  { kind: 'imap', title: 'settings.account.incoming', proto: 'IMAP', icon: 'i-lucide-inbox', badge: 'bg-teal-100 text-teal-700 dark:bg-teal-400/14 dark:text-teal-400' },
  { kind: 'smtp', title: 'settings.account.outgoing', proto: 'SMTP', icon: 'i-lucide-send', badge: 'bg-sky-100 text-sky-700 dark:bg-sky-500/16 dark:text-sky-300' }
] as const

const field = { root: 'w-full', base: 'h-11 rounded-xl max-sm:h-12.5 max-sm:text-base' }
const small = { root: 'w-full', base: 'h-10 rounded-[10px] max-sm:h-11.5 max-sm:text-base' }

const securityItems = computed(() => [
  { label: 'TLS', value: 'tls' },
  { label: 'STARTTLS', value: 'starttls' },
  { label: t('settings.account.securityNone'), value: 'none' }
])

function validate(s: NewAccount): FormError[] {
  const errors: FormError[] = []
  if (!/^[^@\s]+@[^@\s]+$/.test(s.email)) errors.push({ name: 'email', message: t('settings.account.invalidEmail') })
  if (!s.username) errors.push({ name: 'username', message: t('settings.account.required') })
  if (!s.imap.host) errors.push({ name: 'imap.host', message: t('settings.account.required') })
  if (!s.smtp.host) errors.push({ name: 'smtp.host', message: t('settings.account.required') })
  return errors
}

async function submit() {
  loading.value = true
  try {
    const name = state.name || state.email.split('@')[0]!
    let auth: NewAccount['auth'] = { kind: 'password' }
    if (oauthProvider.value) {
      // Opens the provider's page in the browser; the engine waits for the redirect on a local port.
      const identity = await api.oauthLogin(oauthProvider.value, state.email || null)
      // The servers know the account by its sign-in name. A Microsoft account opened with a
      // Gmail address has a mailbox of its own (@outlook.com) that the sign-in does not give:
      // the address typed stays. Google's sign-in name is the mailbox's address.
      if (identity.email) {
        state.username = identity.email
        if (oauthProvider.value === 'google' || !state.email) state.email = identity.email
      }
      auth = { kind: 'oauth2', provider: oauthProvider.value, ticket: identity.ticket }
    }
    const account = await api.addAccount({ ...state, name, auth })
    if (color.value !== autoColor) await api.setAccountColor(account.id, color.value).catch(() => {})
    emit('added', account)
  } catch (e) {
    showError(e, oauthProvider.value ? t('settings.account.oauthFailed', { provider: providerName.value }) : t('settings.account.failed'))
    advanced.value = true
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <UForm
    :state="state"
    :validate="validate"
    class="flex flex-col gap-4 sm:gap-0 sm:overflow-hidden sm:rounded-[20px] sm:border sm:border-default sm:bg-(--surface-card) sm:shadow-xs dark:shadow-none"
    @submit="submit"
    @error="advanced = true"
  >
    <div class="flex items-center gap-3 rounded-2xl border border-default bg-(--surface-card) p-3.5 sm:gap-3.5 sm:rounded-none sm:border-0 sm:border-b sm:bg-(--surface-side) sm:px-6 sm:py-5">
      <span
        class="flex size-11.5 shrink-0 items-center justify-center rounded-full text-[15px] font-bold shadow-[0_0_0_4px_color-mix(in_oklab,var(--sw)_20%,transparent)] sm:size-13 sm:text-[17px] dark:shadow-[0_0_0_4px_color-mix(in_oklab,var(--sw-dark)_20%,transparent)]"
        :class="ACCOUNT_COLORS[color].solid"
        :style="swatch(color)"
        aria-hidden="true"
      >{{ initials(state.name.trim() || state.email || '?') }}</span>
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="truncate text-base font-bold text-highlighted sm:text-[17px]" :class="{ 'text-dimmed': !state.name && !state.email }">
          {{ state.name.trim() || state.email.split('@')[0] || $t('settings.account.namePlaceholder') }}
        </span>
        <span class="truncate text-[12.5px] text-muted sm:text-[13px]">
          {{ state.email || $t('settings.account.emailPlaceholder') }}<span class="max-sm:hidden"> · {{ $t('settings.account.preview') }}</span>
        </span>
      </div>
    </div>

    <div class="flex flex-col gap-4 sm:px-6 sm:pt-5.5 sm:pb-1.5">
      <UFormField :label="$t('settings.account.name')" name="name" :hint="$t('settings.account.nameHint')">
        <UInput v-model="state.name" icon="i-lucide-user" size="lg" :placeholder="$t('settings.account.namePlaceholder')" :ui="field" />
      </UFormField>
      <UFormField :label="$t('settings.account.email')" name="email">
        <UInput v-model="state.email" type="email" icon="i-lucide-at-sign" size="lg" autocomplete="email" :placeholder="$t('settings.account.emailPlaceholder')" :ui="field" />
      </UFormField>
      <UFormField v-if="!oauthProvider" :label="$t('settings.account.password')" name="password">
        <UInput
          v-model="state.password"
          :type="showPassword ? 'text' : 'password'"
          icon="i-lucide-lock"
          size="lg"
          autocomplete="current-password"
          :ui="{ ...field, trailing: 'pe-1' }"
        >
          <template #trailing>
            <UButton
              :icon="showPassword ? 'i-lucide-eye-off' : 'i-lucide-eye'"
              color="neutral"
              variant="link"
              class="size-9 justify-center rounded-[9px] max-sm:size-11"
              :aria-label="showPassword ? $t('settings.account.hidePassword') : $t('settings.account.showPassword')"
              :aria-pressed="showPassword"
              @click="showPassword = !showPassword"
            />
          </template>
        </UInput>
        <template #help>
          <span :class="preset?.appPassword ? 'text-info' : ''">{{ $t('settings.account.passwordHint') }}</span>
        </template>
      </UFormField>

      <UAlert
        v-if="oauthProvider"
        :description="$t('settings.account.oauthNote', { provider: providerName })"
        icon="i-lucide-shield-check"
        color="info"
        variant="subtle"
      />
      <UButton
        v-if="emailValid && (microsoftChosen || oauthProvider !== 'microsoft')"
        :label="microsoftChosen ? $t('settings.account.notMicrosoft') : $t('settings.account.isMicrosoft')"
        icon="i-lucide-repeat"
        color="neutral"
        variant="link"
        size="sm"
        class="self-start px-0"
        @click="microsoftChosen = !microsoftChosen"
      />

      <div
        v-if="!advanced && !touched && discovery.status === 'searching'"
        class="flex items-center gap-3 rounded-[14px] border border-default bg-(--surface-side) px-3.5 py-3 text-[13.5px] text-muted"
      >
        <UIcon name="i-lucide-loader-circle" class="size-5 animate-spin" />
        {{ $t('settings.account.searching') }}
      </div>
      <div
        v-else-if="!advanced && domain && serversKnown"
        class="flex items-center gap-3 rounded-[14px] border border-success/30 bg-success/8 px-3.5 py-3"
        :title="discoveryLabel || undefined"
      >
        <span class="flex size-7.5 shrink-0 items-center justify-center rounded-full bg-success text-white sm:size-8">
          <UIcon name="i-lucide-check" class="size-4" />
        </span>
        <div class="flex min-w-0 flex-1 flex-col gap-px">
          <span class="truncate text-[13.5px] font-semibold text-highlighted max-sm:hidden">
            {{ touched ? $t('settings.account.serversSet', { domain }) : $t('settings.account.serversFound', { domain }) }}
          </span>
          <span class="truncate text-[13.5px] font-semibold text-highlighted sm:hidden">
            {{ touched ? $t('settings.account.servers') : $t('settings.account.serversFoundShort') }}
          </span>
          <span class="truncate text-[12.5px] text-muted max-sm:hidden">{{ serversSummary }}</span>
          <span class="truncate text-xs text-muted sm:hidden">{{ serversShort }}</span>
        </div>
        <UButton :label="$t('settings.account.edit')" color="neutral" variant="outline" class="h-8.5 rounded-[9px] max-sm:h-10" @click="advanced = true" />
      </div>

      <div v-if="advanced" class="flex flex-col gap-3 rounded-[14px] border border-default bg-(--surface-side) p-3 sm:p-3.5" @input="touched = true">
        <div class="flex items-center">
          <span class="flex-1 text-sm font-semibold text-highlighted">{{ $t('settings.account.servers') }}</span>
          <UButton
            v-if="serversKnown"
            :label="$t('settings.account.hide')"
            trailing-icon="i-lucide-chevron-up"
            variant="ghost"
            size="sm"
            @click="advanced = false"
          />
        </div>
        <p v-if="!touched && discovery.status === 'failed'" class="flex items-start gap-2 text-[13px] text-muted">
          <UIcon name="i-lucide-circle-help" class="mt-0.5 size-4 shrink-0" />
          {{ $t('settings.account.notFound') }}
        </p>
        <UFormField :label="$t('settings.account.username')" name="username" :ui="{ label: 'text-[13px]' }">
          <UInput v-model="state.username" :disabled="sameUsername" autocomplete="username" :ui="{ ...small, base: `${small.base} bg-(--surface-card)` }" />
          <template #help>
            <UCheckbox v-model="sameUsername" :label="$t('settings.account.sameAsEmail')" size="sm" :ui="{ label: 'text-[12.5px] font-normal text-muted' }" />
          </template>
        </UFormField>
        <div v-for="s in servers" :key="s.kind" class="flex flex-col gap-2 rounded-xl border border-default bg-(--surface-card) p-3">
          <span class="inline-flex items-center gap-2 text-[13px] font-semibold text-highlighted">
            <span class="flex size-6 items-center justify-center rounded-[7px]" :class="s.badge">
              <UIcon :name="s.icon" class="size-3.5" />
            </span>
            {{ $t(s.title) }} <span class="font-normal text-dimmed">{{ s.proto }}</span>
          </span>
          <div class="flex flex-col gap-2 sm:flex-row">
            <UFormField :name="`${s.kind}.host`" class="min-w-0 flex-1">
              <UInput v-model="state[s.kind].host" :aria-label="`${$t(s.title)} : ${$t('settings.account.host')}`" :ui="small" />
            </UFormField>
            <div class="grid grid-cols-2 gap-2 sm:flex">
              <UInputNumber
                v-model="state[s.kind].port"
                :min="1"
                :max="65535"
                :format-options="{ useGrouping: false }"
                :aria-label="`${$t(s.title)} : ${$t('settings.account.port')}`"
                class="sm:w-27"
                :ui="{ base: 'h-10 rounded-[10px] text-center font-semibold max-sm:h-11.5 max-sm:px-10 max-sm:text-base' }"
              />
              <USelect
                v-model="state[s.kind].security"
                :items="securityItems"
                :icon="state[s.kind].security === 'none' ? 'i-lucide-lock-open' : 'i-lucide-lock'"
                :aria-label="`${$t(s.title)} : ${$t('settings.account.security')}`"
                class="sm:w-30"
                :ui="{ base: 'h-10 rounded-[10px] max-sm:h-11.5 max-sm:text-base', leadingIcon: state[s.kind].security === 'none' ? 'text-error' : 'text-success' }"
                @update:model-value="touched = true"
              />
            </div>
          </div>
        </div>
      </div>

      <!-- Shown even when collapsed: a discovered setting may have picked it. -->
      <UAlert
        v-if="state.imap.security === 'none' || state.smtp.security === 'none'"
        color="error"
        variant="subtle"
        icon="i-lucide-lock-open"
        :title="$t('settings.account.unencryptedTitle')"
        :description="$t('settings.account.unencryptedHelp')"
      />

      <div class="flex flex-col gap-2">
        <span class="text-sm font-semibold text-highlighted">{{ $t('settings.account.color') }}</span>
        <div role="radiogroup" :aria-label="$t('settings.account.color')" class="grid grid-cols-5 justify-items-center gap-2.5 sm:flex sm:flex-wrap">
          <UTooltip v-for="c in ACCOUNT_COLOR_NAMES" :key="c" :text="colorName(c)">
            <button
              type="button"
              role="radio"
              class="flex size-10 items-center justify-center rounded-full bg-(--sw) text-white ring-offset-(--surface-card) transition-transform hover:scale-110 sm:size-7.5 dark:bg-(--sw-dark) dark:text-(--ui-color-neutral-900)"
              :class="color === c ? 'ring-2 ring-(--sw) ring-offset-3 dark:ring-(--sw-dark)' : ''"
              :style="swatch(c)"
              :aria-label="colorName(c)"
              :aria-checked="color === c"
              @click="color = c"
            />
          </UTooltip>
        </div>
      </div>
    </div>

    <div class="flex items-center gap-2.5 pt-1 sm:px-6 sm:pt-4.5 sm:pb-5.5">
      <UButton
        v-if="cancellable"
        :label="$t('common.cancel')"
        color="neutral"
        variant="outline"
        class="h-12 rounded-xl px-4.5 max-sm:hidden"
        @click="emit('cancel')"
      />
      <!-- Waits for discovery: validation would reject a still-empty host. -->
      <UButton
        type="submit"
        :loading="loading"
        :disabled="discovery.status === 'searching'"
        :label="oauthProvider ? $t('settings.account.signInWith', { provider: providerName }) : $t('settings.account.signIn')"
        :trailing-icon="oauthProvider ? 'i-lucide-external-link' : 'i-lucide-arrow-right'"
        class="h-13 flex-1 justify-center rounded-[14px] text-base font-semibold sm:h-12 sm:rounded-xl sm:text-[15px]"
      />
    </div>
  </UForm>
</template>
