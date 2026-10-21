<script setup lang="ts">
import type { Account } from '~/utils/types'

const open = defineModel<boolean>('open', { required: true })
const props = defineProps<{ account: Account }>()
const { t } = useI18n()
const { accountColor, loadAccounts } = useMail()
const showError = useErrorToast()
const toast = useToast()

const name = computed(() => accountName(props.account))
const color = computed(() => accountColor(props.account.id))
const google = computed(() => props.account.auth.kind === 'oauth2' && props.account.auth.provider === 'google')
const domain = computed(() => props.account.email.split('@').pop() ?? '')

const busy = ref<'refresh' | 'upload' | 'initials' | null>(null)
const fileInput = useTemplateRef<HTMLInputElement>('fileInput')
const cropOpen = ref(false)
const cropSrc = ref('')

async function run(kind: NonNullable<typeof busy.value>, action: () => Promise<string>) {
  busy.value = kind
  try {
    const title = await action()
    await loadAccounts()
    toast.add({ title, icon: 'i-lucide-check' })
  } catch (e) {
    showError(e, t('app.avatar.failed'))
  } finally {
    busy.value = null
  }
}

const refresh = () => run('refresh', async () =>
  (await api.refreshAccountAvatar(props.account.id)) ? t('app.avatar.found') : t('app.avatar.notFound'))

const useInitials = () => run('initials', async () => {
  await api.setAccountAvatar(props.account.id, null)
  return t('app.avatar.saved')
})

function pick(e: Event) {
  const input = e.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file) return
  const reader = new FileReader()
  reader.onload = () => {
    cropSrc.value = String(reader.result)
    cropOpen.value = true
  }
  reader.onerror = () => showError(reader.error, t('app.avatar.failed'))
  reader.readAsDataURL(file)
}

const cropped = (src: string) => run('upload', async () => {
  await api.setAccountAvatar(props.account.id, src)
  return t('app.avatar.saved')
})

watch(cropOpen, (v) => {
  if (!v) cropSrc.value = ''
})
</script>

<template>
  <UModal v-model:open="open" :title="$t('app.avatar.title')" :description="account.email" :ui="{ content: 'max-w-md' }">
    <template #body>
      <div class="flex flex-col items-center gap-5">
        <UAvatar
          :src="account.avatar ?? undefined"
          :alt="name"
          :text="initials(name)"
          class="size-24 text-3xl"
          :class="color.solid"
          :ui="{ fallback: 'text-inherit font-bold' }"
        />
        <p v-if="account.avatar_custom && account.avatar" class="text-center text-xs text-muted">
          {{ $t('app.avatar.custom') }}
        </p>

        <div class="flex w-full flex-col gap-2">
          <UButton
            icon="i-lucide-refresh-cw"
            color="neutral"
            variant="soft"
            block
            :label="$t('app.avatar.refresh')"
            :loading="busy === 'refresh'"
            :disabled="!!busy"
            @click="refresh"
          />
          <p class="px-1 text-xs text-muted">
            {{ google ? $t('app.avatar.refreshGoogle') : $t('app.avatar.refreshLogo', { domain }) }}
          </p>

          <UButton
            icon="i-lucide-upload"
            color="neutral"
            variant="soft"
            block
            class="mt-2"
            :label="$t('app.avatar.upload')"
            :loading="busy === 'upload'"
            :disabled="!!busy"
            @click="fileInput?.click()"
          />
          <p class="px-1 text-xs text-muted">{{ $t('app.avatar.uploadHint') }}</p>

          <UButton
            v-if="account.avatar"
            icon="i-lucide-circle-user-round"
            color="neutral"
            variant="ghost"
            block
            class="mt-2"
            :label="$t('app.avatar.initials')"
            :loading="busy === 'initials'"
            :disabled="!!busy"
            @click="useInitials"
          />
        </div>
      </div>
      <input ref="fileInput" type="file" accept="image/png,image/jpeg,image/gif,image/webp,image/bmp" class="hidden" @change="pick">
      <ImageCropper v-if="cropSrc" v-model:open="cropOpen" :src="cropSrc" initial-shape="square" @done="cropped" />
    </template>
  </UModal>
</template>
