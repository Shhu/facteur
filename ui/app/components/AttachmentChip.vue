<script setup lang="ts">
import type { Attachment, AttachmentReport } from '~/utils/types'

const props = defineProps<{ messageId: number, attachment: Attachment, accountId?: number }>()
const { download, view, opening } = useAttachments()
const { accountColor } = useMail()
const { t } = useI18n()

const open = ref(false)
const report = ref<AttachmentReport | null>(null)
const error = ref<string | null>(null)
let request: Promise<AttachmentReport> | null = null

function inspect(): Promise<AttachmentReport> {
  request ??= api.inspectAttachment(props.messageId, props.attachment.index).then(
    (r) => {
      report.value = r
      return r
    },
    (e) => {
      request = null
      error.value = String(e)
      throw e
    }
  )
  return request
}

watch(open, (o) => {
  if (o) inspect().then(loadThumbnail).catch(() => {})
})

const thumbnail = ref<string | null>(null)
let thumbnailLoading = false
/** Once unmounted, a late thumbnail must not create an object URL that would never be revoked. */
let unmounted = false
async function loadThumbnail(r: AttachmentReport) {
  if (thumbnail.value || thumbnailLoading || r.kind !== 'image' || !r.view_mime || r.trust.level === 'low') return
  thumbnailLoading = true
  try {
    const { mime, data } = await api.attachmentView(props.messageId, props.attachment.index)
    if (!unmounted && mime.startsWith('image/')) thumbnail.value = URL.createObjectURL(new Blob([data], { type: mime }))
  } catch {
    // No thumbnail: the card works without it.
  } finally {
    thumbnailLoading = false
  }
}
onBeforeUnmount(() => {
  unmounted = true
  if (thumbnail.value) URL.revokeObjectURL(thumbnail.value)
})

async function show() {
  open.value = false
  const r = report.value ?? await inspect().catch(() => null)
  if (r?.view_mime) await view(props.messageId, props.attachment.index, props.attachment.filename, r.view_mime)
}

async function save() {
  open.value = false
  await download(props.messageId, props.attachment.index, props.attachment.filename, report.value ?? await inspect().catch(() => null))
}

async function primary() {
  const r = report.value ?? await inspect().catch(() => null)
  if (r?.viewable) await show()
  else await save()
}

const fallbackIcon = computed(() => {
  const m = props.attachment.mime
  if (m.startsWith('image/')) return 'i-lucide-image'
  if (m === 'application/pdf') return 'i-lucide-file-text'
  return 'i-lucide-paperclip'
})
const kindIcons: Record<AttachmentReport['kind'], string> = {
  image: 'i-lucide-image',
  pdf: 'i-lucide-file-text',
  text: 'i-lucide-file-type',
  archive: 'i-lucide-file-archive',
  document: 'i-lucide-file-spreadsheet',
  executable: 'i-lucide-file-cog',
  other: 'i-lucide-file'
}
const look = computed(() => ({
  high: { label: t('mail.attachment.trusted'), icon: 'i-lucide-shield-check', text: 'text-success' },
  medium: { label: t('mail.attachment.check'), icon: 'i-lucide-shield', text: 'text-warning' },
  low: { label: t('mail.attachment.suspicious'), icon: 'i-lucide-shield-alert', text: 'text-error' }
})[report.value?.trust.level ?? 'medium'])
const isEvent = computed(() => props.attachment.mime.toLowerCase() === 'text/calendar' || /\.ics$/i.test(props.attachment.filename))
const eventTone = computed(() => (props.accountId !== undefined ? accountColor(props.accountId).soft : 'bg-elevated text-toned'))
const busy = computed(() => opening.value === `${props.messageId}:${props.attachment.index}`)
</script>

<template>
  <UPopover v-model:open="open" mode="hover" :open-delay="300" :close-delay="200" :content="{ side: 'top', align: 'start' }">
    <UButton
      v-if="isEvent && report?.trust.level !== 'low'"
      color="neutral"
      variant="outline"
      :loading="busy"
      class="gap-2 ps-1.5 max-lg:min-h-11"
      @click="primary"
    >
      <template #leading>
        <span class="flex size-5.5 shrink-0 items-center justify-center rounded-md" :class="eventTone">
          <UIcon :name="busy ? 'i-lucide-loader-circle' : 'i-lucide-calendar'" class="size-3.25" :class="{ 'animate-spin': busy }" />
        </span>
      </template>
      <span class="truncate">{{ attachment.filename }}</span>
      <span class="shrink-0 text-dimmed">{{ fileSize(attachment.size) }}</span>
    </UButton>
    <UButton
      v-else
      color="neutral"
      variant="outline"
      :icon="report?.trust.level === 'low' ? 'i-lucide-shield-alert' : report ? kindIcons[report.kind] : fallbackIcon"
      :label="`${attachment.filename} · ${fileSize(attachment.size)}`"
      :loading="busy"
      :ui="{ leadingIcon: report?.trust.level === 'low' ? 'text-error' : undefined }"
      @click="primary"
    />

    <template #content>
      <div class="w-[22rem] max-w-[calc(100vw-1rem)] p-3 text-sm">
        <div class="flex items-center gap-2">
          <template v-if="report">
            <UIcon :name="look.icon" class="size-4 shrink-0" :class="look.text" />
            <span class="font-medium" :class="look.text">{{ look.label }}</span>
            <span class="ms-auto text-xs tabular-nums text-muted">{{ report.trust.score }}/100</span>
          </template>
          <template v-else-if="error">
            <UIcon name="i-lucide-circle-help" class="size-4 text-muted" />
            <span class="text-muted">{{ $t('mail.attachment.analysisFailed') }}</span>
          </template>
          <template v-else>
            <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin text-muted" />
            <span class="text-muted">{{ $t('mail.attachment.analyzing') }}</span>
          </template>
        </div>

        <p class="mt-2 break-all font-semibold leading-snug text-highlighted">{{ attachment.filename }}</p>
        <p class="mt-0.5 text-xs text-muted">
          {{ report?.detected ?? attachment.mime }} · {{ fileSize(report?.size ?? attachment.size) }}
        </p>

        <img v-if="thumbnail" :src="thumbnail" alt="" draggable="false" class="mt-2 max-h-40 rounded-md border border-default object-contain">

        <div v-if="report?.archive_entries.length" class="mt-2 rounded-md bg-elevated/50 px-2 py-1.5 text-xs">
          <p class="mb-1 text-muted">{{ $t('mail.attachment.contains', report.archive_count) }}</p>
          <p v-for="name in report.archive_entries" :key="name" class="truncate font-mono text-toned">{{ name }}</p>
          <p v-if="report.archive_count > report.archive_entries.length" class="text-dimmed">…</p>
        </div>

        <ul v-if="report?.trust.reasons.length" class="mt-2 space-y-1 text-xs">
          <li v-for="r in report.trust.reasons" :key="r.text" class="flex gap-1.5">
            <UIcon :name="r.positive ? 'i-lucide-check' : 'i-lucide-triangle-alert'" class="mt-px size-3.5 shrink-0" :class="r.positive ? 'text-success' : 'text-warning'" />
            <span :class="r.positive ? 'text-toned' : 'text-highlighted'">{{ r.text }}</span>
          </li>
        </ul>

        <div class="mt-3 flex flex-wrap items-center gap-1.5">
          <UButton
            v-if="report?.viewable"
            :label="$t('mail.attachment.openInApp')"
            icon="i-lucide-eye"
            size="xs"
            :loading="busy"
            @click="show"
          />
          <UButton
            :label="report?.trust.level === 'low' ? $t('mail.attachment.saveAnyway') : $t('mail.attachment.download')"
            icon="i-lucide-download"
            size="xs"
            :color="report?.trust.level === 'low' ? 'error' : 'neutral'"
            :variant="report?.trust.level === 'low' ? 'soft' : 'outline'"
            @click="save"
          />
        </div>
      </div>
    </template>
  </UPopover>
</template>
