<script setup lang="ts">
import { answerConfirm, confirmRequest } from '~/composables/useConfirm'

const { t } = useI18n()
/** The question rendered: kept once answered, so the dialog doesn't change (or empty) while it
 *  fades out. */
const request = shallowRef(confirmRequest.value)
watch(confirmRequest, (r) => {
  if (r) request.value = r
})
const open = computed({
  get: () => confirmRequest.value !== null,
  set: (v) => {
    if (!v) answerConfirm(false)
  }
})
const look = computed(() => {
  switch (request.value?.kind) {
    case 'warning': return { icon: 'i-lucide-triangle-alert', tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400', ok: 'error' as const }
    case 'error': return { icon: 'i-lucide-circle-alert', tone: 'bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-400', ok: 'error' as const }
    default: return { icon: 'i-lucide-circle-help', tone: 'bg-primary/10 text-primary', ok: 'primary' as const }
  }
})
</script>

<template>
  <!-- Above any other dialog: a question asked from one (pending actions) must not open under it. -->
  <UModal
    v-model:open="open"
    :title="request?.title || t('common.confirm.title')"
    :description="request?.message"
    :ui="{ overlay: 'z-[70]', content: 'z-[71] max-w-md', header: 'hidden', body: 'p-5 sm:p-6', footer: 'justify-end gap-2 px-5 pb-5 sm:px-6 border-0 pt-0' }"
  >
    <template #body>
      <div class="flex gap-4">
        <span class="flex size-11 shrink-0 items-center justify-center rounded-xl" :class="look.tone">
          <UIcon :name="look.icon" class="size-5.5" />
        </span>
        <div class="min-w-0 flex-1 pt-0.5">
          <h2 class="text-base font-semibold text-highlighted">{{ request?.title || t('common.confirm.title') }}</h2>
          <p class="mt-1.5 text-sm whitespace-pre-line break-words text-muted">{{ request?.message }}</p>
        </div>
      </div>
    </template>
    <template #footer>
      <UButton
        :label="request?.cancelLabel || t('common.cancel')"
        color="neutral"
        variant="outline"
        class="rounded-[10px] px-4"
        @click="answerConfirm(false)"
      />
      <UButton
        :label="request?.okLabel || t('common.confirm.ok')"
        :color="look.ok"
        class="rounded-[10px] px-4"
        :class="look.ok === 'error' ? 'btn-danger' : ''"
        autofocus
        @click="answerConfirm(true)"
      />
    </template>
  </UModal>
</template>
