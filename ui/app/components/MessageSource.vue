<script setup lang="ts">
const props = defineProps<{ messageId: number, subject: string }>()
const open = defineModel<boolean>('open', { required: true })
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()

const source = ref<string | null>(null)
const tab = ref<'headers' | 'full'>('headers')
watch(open, async (o) => {
  if (!o || source.value !== null) return
  try {
    source.value = await api.messageSource(props.messageId)
  } catch (e) {
    showError(e, t('mail.source.unavailable'))
    open.value = false
  }
}, { immediate: true })

const headers = computed(() => {
  const s = source.value ?? ''
  const end = s.search(/\r?\n\r?\n/)
  return end === -1 ? s : s.slice(0, end)
})
const shown = computed(() => (tab.value === 'headers' ? headers.value : source.value ?? ''))

async function copy() {
  try {
    await navigator.clipboard.writeText(shown.value)
    toast.add({ title: t('mail.source.copied'), icon: 'i-lucide-copy' })
  } catch {
    toast.add({ title: t('mail.source.copyFailed'), color: 'error' })
  }
}

async function saveEml() {
  const name = (props.subject || 'message').slice(0, 60)
  try {
    const path = await api.saveMessageSource(props.messageId, `${name}.eml`)
    if (path) toast.add({ title: t('mail.source.saved'), description: path, icon: 'i-lucide-check' })
  } catch (e) {
    showError(e)
  }
}
</script>

<template>
  <UModal v-model:open="open" :title="$t('mail.source.title')" :description="subject || $t('common.noSubject')" :ui="{ content: 'max-w-5xl', body: 'flex min-h-0 flex-col gap-3' }">
    <template #body>
      <div class="flex flex-wrap items-center gap-2">
        <UTabs
          v-model="tab"
          :items="[{ label: $t('mail.source.headers'), value: 'headers' }, { label: $t('mail.source.full'), value: 'full' }]"
          :content="false"
          size="xs"
          variant="pill"
        />
        <UButton :label="$t('mail.source.copy')" icon="i-lucide-copy" size="xs" color="neutral" variant="ghost" class="ms-auto" :disabled="source === null" @click="copy" />
        <UButton :label="$t('mail.source.saveEml')" icon="i-lucide-download" size="xs" color="neutral" variant="outline" @click="saveEml" />
      </div>
      <div v-if="source === null" class="flex items-center gap-2 py-10 text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" /> {{ $t('mail.source.loading') }}
      </div>
      <pre v-else class="max-h-[65vh] overflow-auto whitespace-pre-wrap break-all rounded-md bg-elevated/50 p-3 font-mono text-xs text-default">{{ shown }}</pre>
    </template>
  </UModal>
</template>
