<script setup lang="ts">
import { useMediaQuery } from '@vueuse/core'
import type { Coverage } from '~/utils/types'

const route = useRoute()
const desktop = useMediaQuery('(min-width: 1024px)')
const { accountById, tick } = useMail()

const account = computed(() => accountById(Number(route.params.id)))

const coverage = ref<Coverage>()
const busy = ref(false)
async function readCoverage() {
  const id = Number(route.params.id)
  coverage.value = (await api.coverage().catch(() => null))?.find(c => c.account_id === id) ?? coverage.value
  busy.value = (await api.historyBusy().catch(() => [] as number[])).includes(id)
}
// Coverage changes several times a second during a sync: throttle reloads.
const coverageReloader = useReloader(readCoverage)
const loadCoverage = () => coverageReloader.now()
loadCoverage()
watch(tick, coverageReloader.schedule)

function back() {
  if (window.history.state?.back === '/settings') useRouter().back()
  else navigateTo('/settings', { replace: true })
}
</script>

<template>
  <UDashboardPanel id="settings-account" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <div class="flex shrink-0 items-center gap-1 border-b border-default bg-default px-2 py-2.5 dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-arrow-left"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl text-highlighted"
          :aria-label="$t('settings.accounts.back')"
          @click="back"
        />
        <h1 class="text-[17px] font-semibold text-highlighted">{{ $t('settings.accounts.detailTitle') }}</h1>
      </div>
    </template>

    <template #body>
      <div v-if="desktop && account" class="mx-auto w-full max-w-3xl px-8 py-10 xl:px-0">
        <UCard :ui="{ root: 'overflow-hidden rounded-2xl bg-(--surface-card) shadow-xs dark:shadow-none', body: 'p-0 sm:p-0' }">
          <SettingsAccount
            :account="account"
            layout="card"
            :coverage="coverage"
            :busy="busy"
            @refresh="loadCoverage"
          />
        </UCard>
      </div>
      <div v-else class="mx-auto w-full max-w-xl px-4 pt-4 pb-[calc(2rem+env(safe-area-inset-bottom))]">
        <SettingsAccount
          v-if="account"
          :account="account"
          layout="page"
          :coverage="coverage"
          :busy="busy"
          @refresh="loadCoverage"
        />
        <div v-else class="flex flex-col items-center gap-3 py-16 text-sm text-muted">
          <UIcon name="i-lucide-user-x" class="size-7" />
          {{ $t('settings.accounts.notFound') }}
          <UButton :label="$t('settings.accounts.back')" color="neutral" variant="outline" to="/settings" />
        </div>
      </div>
    </template>
  </UDashboardPanel>
</template>
