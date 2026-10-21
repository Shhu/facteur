<script setup lang="ts">
const { viewed, download, close } = useAttachments()

/** Saving goes through the same inspection as the attachment chip: a suspicious file asks first. */
const saving = ref(false)
async function save() {
  const v = viewed.value
  if (!v || saving.value) return
  saving.value = true
  try {
    await download(v.messageId, v.index, v.filename)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <UModal
    :open="viewed !== null"
    :title="viewed?.filename"
    fullscreen
    :ui="{ body: 'flex min-h-0 flex-1 items-center justify-center p-0 sm:p-0 bg-elevated/40', title: 'truncate' }"
    @update:open="(o) => { if (!o) close() }"
  >
    <template #actions>
      <UButton
        v-if="viewed"
        :label="$t('mail.attachment.download')"
        icon="i-lucide-download"
        size="sm"
        color="neutral"
        variant="outline"
        :loading="saving"
        @click="save"
      />
    </template>
    <template #body>
      <template v-if="viewed">
        <img v-if="viewed.mime.startsWith('image/')" :src="viewed.url" :alt="viewed.filename" draggable="false" class="max-h-full max-w-full object-contain p-4">
        <LazyPdfViewer v-else-if="viewed.data" :data="viewed.data" />
        <pre v-else class="size-full overflow-auto whitespace-pre-wrap p-6 font-mono text-sm text-default">{{ viewed.text }}</pre>
      </template>
    </template>
  </UModal>
</template>
