<script setup lang="ts">
import type { Account } from '~/utils/types'

const { accounts, accountId, loadAccounts } = useMail()

async function added(account: Account) {
  await loadAccounts()
  accountId.value = account.id
  navigateTo('/')
}
</script>

<template>
  <UDashboardPanel :ui="{ body: 'bg-(--surface-read) p-4 sm:px-8 sm:py-7' }">
    <template #header>
      <UDashboardNavbar :title="accounts.length ? $t('settings.setup.add') : $t('settings.setup.welcome')">
        <template v-if="accounts.length" #leading>
          <UButton icon="i-lucide-arrow-left" color="neutral" variant="ghost" to="/settings" :aria-label="$t('settings.setup.back')" />
        </template>
      </UDashboardNavbar>
    </template>
    <template #body>
      <div class="mx-auto w-full max-w-[600px]">
        <p v-if="!accounts.length" class="mb-5 text-muted">
          {{ $t('settings.setup.intro') }}
        </p>
        <AccountForm :cancellable="accounts.length > 0" @added="added" @cancel="navigateTo('/settings')" />
      </div>
    </template>
  </UDashboardPanel>
</template>
