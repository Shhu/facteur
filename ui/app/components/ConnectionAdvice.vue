<script setup lang="ts">
const advice = useConnectionAdvice()
const { accountById } = useMail()
const showError = useErrorToast()
const { t } = useI18n()

onMounted(async () => {
  try {
    const ids = await api.connectionAdvice()
    advice.value = [...new Set([...advice.value, ...ids])]
  } catch {
    // Older engine: nothing to offer.
  }
})

/** One account at a time; kept while the dialog fades out. */
const shown = computed(() => advice.value.map(id => accountById(id)).find(a => !!a) ?? null)
const kept = shallowRef(shown.value)
watch(shown, (a) => {
  if (a) kept.value = a
})
const answering = ref(false)
const open = computed({
  get: () => !!shown.value,
  set: (v) => {
    if (!v) answer(null)
  }
})

async function answer(accept: boolean | null) {
  const a = shown.value
  if (!a || answering.value) return
  answering.value = true
  try {
    await api.answerConnectionAdvice(a.id, accept)
    advice.value = advice.value.filter(id => id !== a.id)
  } catch (e) {
    showError(e)
  } finally {
    answering.value = false
  }
}
</script>

<template>
  <UModal
    v-model:open="open"
    :title="t('settings.accounts.sync.adviceTitle')"
    :ui="{ content: 'max-w-md', header: 'hidden', body: 'p-5 sm:p-6', footer: 'justify-end gap-2 px-5 pb-5 sm:px-6 border-0 pt-0 flex-wrap' }"
  >
    <template #body>
      <div class="flex gap-4">
        <span class="flex size-11 shrink-0 items-center justify-center rounded-xl bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400">
          <UIcon name="i-lucide-unplug" class="size-5.5" />
        </span>
        <div class="min-w-0 flex-1 pt-0.5">
          <h2 class="text-base font-semibold text-highlighted">{{ t('settings.accounts.sync.adviceTitle') }}</h2>
          <p class="mt-1.5 text-sm text-muted">{{ t('settings.accounts.sync.adviceBody', { account: kept ? accountName(kept) : '' }) }}</p>
          <p class="mt-2 text-sm text-muted">{{ t('settings.accounts.sync.adviceEffect') }}</p>
        </div>
      </div>
    </template>
    <template #footer>
      <UButton :label="t('settings.accounts.sync.adviceNever')" color="neutral" variant="ghost" class="rounded-[10px] px-3" :disabled="answering" @click="answer(false)" />
      <span class="flex-1" />
      <UButton :label="t('settings.accounts.sync.adviceLater')" color="neutral" variant="outline" class="rounded-[10px] px-4" :disabled="answering" @click="answer(null)" />
      <UButton :label="t('settings.accounts.sync.adviceAccept')" class="rounded-[10px] px-4" autofocus :loading="answering" @click="answer(true)" />
    </template>
  </UModal>
</template>
