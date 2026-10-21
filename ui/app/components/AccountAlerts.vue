<script setup lang="ts">
import { QUOTA_WARNING, type Account } from '~/utils/types'

const { accounts } = useMail()
const { authProblems } = useAccountHealth()
const { reconnect, reconnecting } = useCalendar()

const refused = computed(() => accounts.value.filter(a => a.id in authProblems.value))
const full = computed(() => accounts.value.filter(a => a.quota && a.quota.used / a.quota.limit >= QUOTA_WARNING))
const percent = (a: Account) =>
  new Intl.NumberFormat(intlLocale(), { style: 'percent', maximumFractionDigits: 0 }).format(Math.min(1, a.quota!.used / a.quota!.limit))

const passwordFor = ref<Account | null>(null)
const passwordOpen = computed({
  get: () => passwordFor.value !== null,
  set: (open: boolean) => {
    if (!open) passwordFor.value = null
  }
})

function fix(a: Account) {
  if (a.auth.kind === 'oauth2') reconnect(a)
  else passwordFor.value = a
}
</script>

<template>
  <div v-if="refused.length || full.length" class="flex flex-col">
    <div
      v-for="a in full"
      :key="`quota-${a.id}`"
      class="flex items-center gap-2.5 border-b border-default bg-warning/8 px-4 py-2 text-sm sm:px-5"
      role="status"
    >
      <UIcon name="i-lucide-hard-drive" class="size-4 shrink-0 text-warning" />
      <span class="min-w-0 flex-1 font-medium text-highlighted">{{ $t('app.ops.health.quotaFull', { name: accountName(a), pct: percent(a) }) }}</span>
    </div>
    <div
      v-for="a in refused"
      :key="a.id"
      class="flex items-center gap-2.5 border-b border-default bg-error/8 px-4 py-2 text-sm sm:px-5"
      role="alert"
    >
      <UIcon name="i-lucide-key-round" class="size-4 shrink-0 text-error" />
      <span class="min-w-0 flex-1">
        <span class="font-medium text-highlighted">
          {{ $t(a.auth.kind === 'oauth2' ? 'app.ops.health.refusedOAuth' : 'app.ops.health.refusedPassword', { name: accountName(a) }) }}
        </span>
        <span class="block truncate text-xs text-muted" :title="authProblems[a.id]">{{ authProblems[a.id] }}</span>
      </span>
      <UButton
        :label="a.auth.kind === 'oauth2' ? $t('app.ops.health.reconnect') : $t('app.ops.health.enterPassword')"
        :icon="a.auth.kind === 'oauth2' ? 'i-lucide-refresh-ccw' : 'i-lucide-key-round'"
        size="xs"
        color="error"
        variant="soft"
        class="shrink-0"
        :loading="reconnecting === a.id"
        @click="fix(a)"
      />
    </div>
    <AccountPasswordModal v-if="passwordFor" v-model:open="passwordOpen" :account="passwordFor" />
  </div>
</template>
