<script setup lang="ts">
import type { Account } from '~/utils/types'

const open = defineModel<boolean>('open', { required: true })
const props = defineProps<{ account: Account }>()
const { t } = useI18n()
const toast = useToast()
const { authProblems } = useAccountHealth()

const password = ref('')
const saving = ref(false)
const error = ref<string | null>(null)
const refused = computed(() => authProblems.value[props.account.id] ?? null)

watch(open, (isOpen) => {
  if (isOpen) {
    password.value = ''
    error.value = null
  }
})

async function save() {
  if (!password.value || saving.value) return
  saving.value = true
  error.value = null
  try {
    await api.updatePassword(props.account.id, password.value)
    password.value = ''
    open.value = false
    toast.add({ title: t('settings.accounts.passwordSaved'), description: props.account.email, icon: 'i-lucide-check' })
  } catch (e) {
    error.value = String(e)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <UModal v-model:open="open" :title="$t('settings.accounts.passwordTitle')" :description="account.email" :ui="{ content: 'max-w-md' }">
    <template #body>
      <form class="flex flex-col gap-4" @submit.prevent="save">
        <UAlert
          v-if="refused"
          :title="$t('settings.accounts.passwordRefused')"
          :description="refused"
          color="error"
          variant="subtle"
          icon="i-lucide-key-round"
        />
        <p class="text-sm text-muted">{{ $t('settings.accounts.passwordHelp', { user: account.username }) }}</p>
        <UFormField :label="$t('settings.account.password')" :error="error ?? undefined">
          <UInput v-model="password" type="password" autocomplete="current-password" autofocus class="w-full" />
        </UFormField>
        <div class="flex justify-end gap-2">
          <UButton :label="$t('common.cancel')" color="neutral" variant="ghost" @click="open = false" />
          <UButton type="submit" :label="$t('settings.accounts.passwordSave')" :loading="saving" :disabled="!password" />
        </div>
      </form>
    </template>
  </UModal>
</template>
