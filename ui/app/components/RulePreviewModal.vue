<script setup lang="ts">
import type { RulePreview } from '~/utils/types'

const props = defineProps<{
  open: boolean
  ruleName: string
  summary: string
  data: RulePreview | null
  applying: boolean
}>()
const emit = defineEmits<{ close: [], apply: [] }>()
const { t } = useI18n()

const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
const title = computed(() => {
  const name = props.ruleName.trim()
  return name ? t('settings.rules.preview.titleNamed', { name }) : t('settings.rules.preview.title')
})
const tiles = computed(() => {
  const d = props.data
  if (!d) return []
  return [
    { label: t('settings.rules.preview.unread'), value: fmt.value.format(d.unread), icon: 'i-lucide-mail' },
    { label: t('settings.rules.preview.attachments'), value: fmt.value.format(d.with_attachments), icon: 'i-lucide-paperclip' },
    { label: t('settings.rules.preview.bulk'), value: fmt.value.format(d.bulk), icon: 'i-lucide-newspaper' },
    {
      label: t('settings.rules.preview.period'),
      value: d.oldest !== null && d.newest !== null ? t('settings.rules.preview.periodValue', { from: shortDate(d.oldest), to: shortDate(d.newest) }) : '—',
      icon: 'i-lucide-calendar-range'
    }
  ]
})
</script>

<template>
  <UModal
    :open="open"
    :title="title"
    :description="summary"
    :ui="{ content: 'max-w-5xl', body: 'flex min-h-0 flex-col gap-4' }"
    @update:open="(o) => { if (!o) emit('close') }"
  >
    <template #body>
      <div v-if="!data" class="flex items-center justify-center gap-2 py-16 text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" /> {{ t('settings.rules.preview.loading') }}
      </div>
      <template v-else>
        <section class="flex flex-wrap items-end gap-6">
          <div>
            <p class="text-5xl font-semibold leading-none text-highlighted">{{ fmt.format(data.count) }}</p>
            <p class="mt-1 text-sm text-muted">{{ t('settings.rules.preview.messages', data.count) }}</p>
          </div>
          <div class="grid flex-1 grid-cols-2 gap-2 sm:grid-cols-4">
            <div v-for="tile in tiles" :key="tile.label" class="rounded-lg border border-default px-3 py-2">
              <p class="flex items-center gap-1.5 text-xs text-muted">
                <UIcon :name="tile.icon" class="size-3.5" />
                {{ tile.label }}
              </p>
              <p class="mt-1 truncate font-semibold text-highlighted">{{ tile.value }}</p>
            </div>
          </div>
        </section>

        <p v-if="!data.count" class="rounded-lg border border-default p-6 text-center text-sm text-muted">{{ t('settings.rules.preview.none') }}</p>
        <div v-else class="grid min-h-0 gap-4 lg:grid-cols-[16rem_1fr]">
          <div>
            <h3 class="mb-2 text-sm font-semibold text-highlighted">{{ t('settings.rules.preview.senders') }}</h3>
            <ChartsRankBars :items="data.top_senders.map(a => ({ label: a.name || a.email, detail: a.name ? a.email : undefined, value: a.count }))" />
          </div>
          <section class="flex min-h-0 flex-col rounded-lg border border-default">
            <h3 class="border-b border-default px-3 py-2 text-sm font-semibold text-highlighted">{{ t('settings.rules.preview.list') }}</h3>
            <div class="max-h-[45vh] overflow-y-auto">
              <div
                v-for="m in data.messages"
                :key="m.id"
                class="grid grid-cols-[4.5rem_minmax(0,11rem)_minmax(0,1fr)_1rem] items-center gap-2 border-b border-default px-3 py-0.5 text-xs leading-5 last:border-b-0"
              >
                <span class="tabular-nums text-muted">{{ shortDate(m.date) }}</span>
                <span class="truncate" :class="m.flags.seen ? 'text-toned' : 'font-semibold text-highlighted'">{{ addressLabel(m.from) }}</span>
                <span class="truncate">{{ m.subject || t('common.noSubject') }}</span>
                <UIcon v-if="m.has_attachments" name="i-lucide-paperclip" class="size-3 text-dimmed" />
              </div>
              <p v-if="data.count > data.messages.length" class="px-3 py-1.5 text-xs text-muted">
                {{ t('settings.rules.preview.more', { n: fmt.format(data.count - data.messages.length) }, data.count - data.messages.length) }}
              </p>
            </div>
          </section>
        </div>
      </template>
    </template>
    <template #footer>
      <div class="flex w-full items-center justify-end gap-2">
        <UButton :label="t('common.cancel')" color="neutral" variant="ghost" @click="emit('close')" />
        <UButton
          :label="data && data.count ? t('settings.rules.preview.apply', { n: fmt.format(data.count) }, data.count) : t('settings.rules.preview.applyNone')"
          icon="i-lucide-play"
          :loading="applying"
          :disabled="!data"
          @click="emit('apply')"
        />
      </div>
    </template>
  </UModal>
</template>
