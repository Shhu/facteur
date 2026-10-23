<script setup lang="ts">
import { open as pickFile } from '@tauri-apps/plugin-dialog'
import { confirm } from '~/composables/useConfirm'
import { DICTATION_LANGUAGE } from '~/composables/useDictation'
import type { AiModel } from '~/utils/types'

/** The local AI, off unless turned on here: a model to download (or a file of one's own), its
 *  download, and turning it off again (its file deleted). */
withDefaults(defineProps<{ touch?: boolean }>(), { touch: false })
const { status, refresh } = useAi()
const showError = useErrorToast()
const { t } = useI18n()

onMounted(refresh)

const on = computed(() => !!status.value?.model || (!!status.value?.download && status.value.download.model !== status.value.dictation_model.id))
/** Turned on, no model yet: the models offered are shown. */
const choosing = ref(false)
const picked = ref<string | null>(null)
watch(status, (s) => {
  if (s && !picked.value) picked.value = s.fitting
}, { immediate: true })

const switchOn = computed({
  get: () => on.value || choosing.value,
  set: (v) => {
    if (v) choosing.value = true
    else turnOff()
  }
})

const current = computed(() => status.value?.models.find(m => m.id === status.value?.model) ?? null)
const download = computed(() => status.value?.download ?? null)
const downloading = computed(() => [...status.value?.models ?? [], ...(status.value ? [status.value.dictation_model] : [])].find(m => m.id === download.value?.model) ?? null)
const textDownloading = computed(() => downloading.value?.tier !== 'dictation')
const percent = computed(() => (download.value && download.value.total ? Math.round(download.value.done / download.value.total * 100) : 0))
const memoryGb = computed(() => (status.value?.memory ? Math.round(status.value.memory / 1e9) : null))

const tierLabel = (m: AiModel) => t(`settings.ai.tier.${m.tier}`)
const tooBig = (m: AiModel) => memoryGb.value !== null && memoryGb.value < m.memory

async function install() {
  if (!picked.value) return
  try {
    await api.aiInstall(picked.value)
    choosing.value = false
    await refresh()
  } catch (e) {
    showError(e)
  }
}

async function useFile() {
  const path = await pickFile({ multiple: false, directory: false, filters: [{ name: 'GGUF', extensions: ['gguf'] }] }).catch(() => null)
  if (typeof path !== 'string') return
  try {
    await api.aiUseFile(path)
    choosing.value = false
    await refresh()
  } catch (e) {
    showError(e)
  }
}

async function cancel() {
  await api.aiCancel().catch(showError)
}

/** Dictation apart: its model downloaded when turned on, deleted when turned off. */
const dictationOn = computed({
  get: () => !!status.value?.dictation || download.value?.model === status.value?.dictation_model.id,
  set: (v) => {
    if (v) installDictation()
    else dictationOff()
  }
})
async function installDictation() {
  try {
    await api.aiInstall(status.value!.dictation_model.id)
    await refresh()
  } catch (e) {
    showError(e)
  }
}
/** The language spoken to the microphone: found by the model unless one is chosen. */
const spoken = ref('auto')
const spokenItems = computed(() => [
  { label: t('settings.ai.dictation.languageAuto'), value: 'auto' },
  { label: 'Français', value: 'fr' },
  { label: 'English', value: 'en' }
])
onMounted(async () => {
  const v = await api.getSetting(DICTATION_LANGUAGE).catch(() => null)
  spoken.value = v === 'fr' || v === 'en' ? v : 'auto'
})
async function setSpoken(v: string) {
  spoken.value = v
  await api.setSetting(DICTATION_LANGUAGE, v).catch(showError)
}

async function dictationOff() {
  if (download.value?.model === status.value?.dictation_model.id) {
    await cancel()
    return
  }
  const size = fileSize(status.value!.dictation_model.bytes)
  if (!await confirm(t('settings.ai.dictation.offConfirm', { size }), { title: t('settings.ai.dictation.offTitle'), kind: 'warning', okLabel: t('settings.ai.off') })) return
  try {
    await api.dictationOff()
    await refresh()
  } catch (e) {
    showError(e)
  }
}

/** The graphics card, or the processor only (a slow integrated card, an unstable driver). */
const graphicsOn = computed({
  get: () => status.value?.graphics ?? true,
  set: async (v: boolean) => {
    try {
      await api.aiGraphics(v)
      await refresh()
    } catch (e) {
      showError(e)
    }
  }
})

async function turnOff() {
  if (!on.value) {
    choosing.value = false
    return
  }
  const size = current.value ? fileSize(current.value.bytes) : null
  const ok = await confirm(size ? t('settings.ai.offConfirm', { size }) : t('settings.ai.offConfirmFile'), { title: t('settings.ai.offTitle'), kind: 'warning', okLabel: t('settings.ai.off') })
  if (!ok) return
  try {
    await api.aiTurnOff()
    choosing.value = false
    await refresh()
  } catch (e) {
    showError(e)
  }
}
</script>

<template>
  <div class="flex flex-col gap-4" :class="touch ? 'px-4 py-3.5' : 'px-5.5 py-4.5'">
    <div class="flex items-center gap-4">
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="text-sm font-semibold text-highlighted">{{ t('settings.ai.title') }}</span>
        <span class="text-[13px] text-muted">{{ status?.available === false ? t(status.processor_unsupported ? 'settings.ai.oldProcessor' : 'settings.ai.unavailable') : t('settings.ai.help') }}</span>
      </div>
      <USwitch v-model="switchOn" size="lg" :disabled="!status?.available" :aria-label="t('settings.ai.title')" />
    </div>

    <p v-if="switchOn" class="flex items-start gap-2 rounded-xl bg-elevated/50 px-3 py-2.5 text-[13px] text-toned">
      <UIcon name="i-lucide-shield-check" class="mt-0.5 size-4 shrink-0 text-success" />
      {{ t('settings.ai.private') }}
    </p>

    <UAlert v-if="status?.error && !download" :description="status.error" color="error" variant="subtle" icon="i-lucide-triangle-alert" />

    <div v-if="download && downloading && textDownloading" class="flex flex-col gap-2">
      <div class="flex items-center gap-3 text-sm">
        <span class="min-w-0 flex-1 truncate">{{ t('settings.ai.downloading', { name: downloading.name }) }}</span>
        <span class="shrink-0 text-xs text-muted tabular-nums">{{ fileSize(download.done) }} / {{ fileSize(download.total) }}</span>
        <UButton :label="t('common.cancel')" color="neutral" variant="ghost" size="xs" @click="cancel" />
      </div>
      <UProgress :model-value="percent" size="sm" />
    </div>

    <div v-else-if="status?.model && !choosing && !(download && textDownloading)" class="flex flex-wrap items-center gap-3">
      <UIcon name="i-lucide-cpu" class="size-4 shrink-0 text-muted" />
      <span class="min-w-0 flex-1 text-sm">
        <template v-if="current">{{ current.name }} · <span class="text-muted">{{ tierLabel(current) }}, {{ fileSize(current.bytes) }}</span></template>
        <template v-else>{{ t('settings.ai.ownFile') }} · <span class="break-all text-muted">{{ status.file }}</span></template>
        <UBadge v-if="!status.ready" :label="t('settings.ai.missing')" color="error" variant="subtle" size="sm" class="ms-2" />
      </span>
      <UButton :label="t('settings.ai.change')" color="neutral" variant="outline" size="sm" @click="choosing = true" />
    </div>

    <div v-if="choosing && !(download && textDownloading)" class="flex flex-col gap-3">
      <div class="grid gap-2" :class="touch ? '' : 'sm:grid-cols-3'">
        <button
          v-for="m in status?.models ?? []"
          :key="m.id"
          type="button"
          class="flex flex-col items-start gap-1 rounded-xl border px-3.5 py-3 text-start transition-colors"
          :class="picked === m.id ? 'border-primary bg-primary/5' : 'border-default hover:bg-elevated/50'"
          :aria-pressed="picked === m.id"
          @click="picked = m.id"
        >
          <span class="flex w-full items-center gap-2">
            <span class="text-sm font-semibold text-highlighted">{{ tierLabel(m) }}</span>
            <UBadge v-if="m.id === status?.fitting" :label="t('settings.ai.fitting')" color="primary" variant="subtle" size="sm" />
          </span>
          <span class="text-[13px] text-toned">{{ m.name }} · {{ fileSize(m.bytes) }}</span>
          <span class="text-xs" :class="tooBig(m) ? 'text-warning' : 'text-muted'">
            {{ tooBig(m) ? t('settings.ai.slowHere', { n: m.memory }) : t('settings.ai.memory', { n: m.memory }) }}
          </span>
        </button>
      </div>
      <div class="flex flex-wrap items-center gap-2">
        <UButton
          :label="t('settings.ai.download', { size: fileSize(status?.models.find(m => m.id === picked)?.bytes ?? 0) })"
          icon="i-lucide-download"
          :disabled="!picked"
          @click="install"
        />
        <UButton :label="t('settings.ai.useFile')" color="neutral" variant="ghost" icon="i-lucide-file-box" @click="useFile" />
        <span class="flex-1" />
        <UButton v-if="status?.model" :label="t('common.cancel')" color="neutral" variant="ghost" @click="choosing = false" />
      </div>
      <p class="text-xs text-muted">{{ t('settings.ai.sourceHelp') }}</p>
    </div>

    <div v-if="status?.available" class="flex flex-col gap-3 border-t border-default pt-4">
      <div class="flex items-center gap-4">
        <div class="flex min-w-0 flex-1 flex-col gap-0.5">
          <span class="flex items-center gap-2 text-sm font-semibold text-highlighted">
            <UIcon name="i-lucide-mic" class="size-4 text-muted" />
            {{ t('settings.ai.dictation.title') }}
            <UBadge v-if="status.dictation_ready" :label="t('settings.ai.dictation.ready')" color="success" variant="subtle" size="sm" />
          </span>
          <span class="text-[13px] text-muted">{{ t('settings.ai.dictation.help', { size: fileSize(status.dictation_model.bytes) }) }}</span>
        </div>
        <USwitch v-model="dictationOn" size="lg" :disabled="!!download && textDownloading" :aria-label="t('settings.ai.dictation.title')" />
      </div>
      <div v-if="status.dictation" class="flex flex-wrap items-center gap-x-4 gap-y-1.5">
        <div class="flex min-w-48 flex-1 flex-col">
          <span class="text-[13px] font-medium text-default">{{ t('settings.ai.dictation.language') }}</span>
          <span class="text-xs text-muted">{{ t('settings.ai.dictation.languageHelp') }}</span>
        </div>
        <USelect
          :model-value="spoken"
          :items="spokenItems"
          size="sm"
          class="w-48"
          :aria-label="t('settings.ai.dictation.language')"
          @update:model-value="(v: string) => setSpoken(v)"
        />
      </div>
      <div v-if="download && downloading && !textDownloading" class="flex flex-col gap-2">
        <div class="flex items-center gap-3 text-sm">
          <span class="min-w-0 flex-1 truncate">{{ t('settings.ai.downloading', { name: downloading.name }) }}</span>
          <span class="shrink-0 text-xs text-muted tabular-nums">{{ fileSize(download.done) }} / {{ fileSize(download.total) }}</span>
          <UButton :label="t('common.cancel')" color="neutral" variant="ghost" size="xs" @click="cancel" />
        </div>
        <UProgress :model-value="percent" size="sm" />
      </div>
    </div>

    <div v-if="status?.available && (on || status.dictation)" class="flex items-center gap-4 border-t border-default pt-4">
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span class="flex items-center gap-2 text-sm font-semibold text-highlighted">
          <UIcon name="i-lucide-cpu" class="size-4 text-muted" />
          {{ t('settings.ai.graphics.title') }}
        </span>
        <span v-if="graphicsOn && status.graphics_failed" class="text-[13px] text-muted">
          {{ t('settings.ai.graphics.failed') }}
        </span>
        <span v-else class="text-[13px] text-muted">
          {{ graphicsOn && status.card ? t('settings.ai.graphics.using', { card: status.card }) : t(graphicsOn ? 'settings.ai.graphics.help' : 'settings.ai.graphics.off') }}
        </span>
      </div>
      <UButton
        v-if="graphicsOn && status.graphics_failed"
        :label="t('settings.ai.graphics.retry')"
        color="neutral"
        variant="outline"
        size="sm"
        @click="graphicsOn = true"
      />
      <USwitch v-model="graphicsOn" size="lg" :aria-label="t('settings.ai.graphics.title')" />
    </div>
  </div>
</template>
