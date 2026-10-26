<script setup lang="ts">
import type { TabsItem } from '@nuxt/ui'
import { useMediaQuery } from '@vueuse/core'
import type { Diagnostics } from '~/composables/usePerf'
import { commandStats, spanLabel } from '~/utils/diagnostics'

/** Developer tools only; everything is polled only while the panel is open. */
const { open, commands, engine, freezes, samples, clear, pollEngine } = usePerf()
const { t } = useI18n()
const toast = useToast()
const showError = useErrorToast()
const wide = useMediaQuery('(min-width: 1024px)')

type Tab = 'overview' | 'commands' | 'engine' | 'log' | 'ui'
const tab = ref<Tab>('overview')
const tabs = computed<TabsItem[]>(() => [
  { label: t('app.diagnostics.tabs.overview'), value: 'overview', icon: 'i-lucide-layout-dashboard' },
  { label: t('app.diagnostics.tabs.commands'), value: 'commands', icon: 'i-lucide-terminal', badge: commands.value.length || undefined },
  { label: t('app.diagnostics.tabs.engine'), value: 'engine', icon: 'i-lucide-database', badge: engine.value.length || undefined },
  { label: t('app.diagnostics.tabs.log'), value: 'log', icon: 'i-lucide-scroll-text' },
  { label: t('app.diagnostics.tabs.ui'), value: 'ui', icon: 'i-lucide-monitor', badge: freezes.value.length || undefined }
])

const diag = ref<Diagnostics | null>(null)
const failure = ref<string | null>(null)
async function poll() {
  try {
    diag.value = await api.diagnostics()
    failure.value = null
  } catch (e) {
    failure.value = String(e)
  }
  await pollEngine()
}
let timer: ReturnType<typeof setInterval> | undefined
watch(open, (v) => {
  clearInterval(timer)
  if (v) {
    poll()
    timer = setInterval(poll, 2000)
  }
}, { immediate: true })
onBeforeUnmount(() => clearInterval(timer))

const copying = ref(false)
async function copySnapshot() {
  copying.value = true
  try {
    const log = await api.readLog(300, null, null).catch(e => [{ error: String(e) }])
    const snapshot = {
      taken_at: new Date().toISOString(),
      user_agent: navigator.userAgent,
      diagnostics: diag.value,
      commands: { stats: commandStats(commands.value).sort((a, b) => b.p95 - a.p95), recent: commands.value.slice(-100) },
      engine: engine.value,
      ui: { samples: samples.value, freezes: freezes.value },
      log
    }
    await navigator.clipboard.writeText(JSON.stringify(snapshot, null, 2))
    toast.add({ title: t('app.diagnostics.copied'), icon: 'i-lucide-clipboard-check', color: 'success' })
  } catch (e) {
    showError(e)
  } finally {
    copying.value = false
  }
}

async function openLogs() {
  try {
    await api.openLogs()
  } catch (e) {
    showError(e)
  }
}

const subtitle = computed(() => {
  const d = diag.value
  if (!d) return ''
  return [`facteur ${d.app_version}`, `${d.os}/${d.arch}`, t('app.diagnostics.uptime', { span: spanLabel(d.uptime_secs) })].join(' · ')
})
</script>

<template>
  <UModal
    v-model:open="open"
    :fullscreen="!wide"
    :ui="{ content: wide ? 'w-[calc(100vw-3rem)] max-w-[1400px] h-[calc(100dvh-3rem)] max-h-[980px] divide-y-0 overflow-hidden' : 'divide-y-0' }"
    :title="t('app.diagnostics.title')"
    :description="subtitle"
  >
    <template #content>
      <div class="flex min-h-0 flex-1 flex-col bg-(--surface-card)">
        <header class="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-default px-4 py-3 sm:px-5">
          <div class="flex min-w-0 items-center gap-2.5">
            <span class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
              <UIcon name="i-lucide-activity" class="size-4.5" />
            </span>
            <div class="min-w-0">
              <h2 class="font-semibold text-highlighted">{{ t('app.diagnostics.title') }}</h2>
              <p class="truncate text-xs text-muted">{{ subtitle || t('app.diagnostics.loading') }}</p>
            </div>
          </div>
          <div class="ms-auto flex flex-wrap items-center gap-1">
            <UButton icon="i-lucide-clipboard-copy" :label="wide ? t('app.diagnostics.copy') : undefined" :aria-label="t('app.diagnostics.copy')" color="neutral" variant="outline" size="sm" :loading="copying" @click="copySnapshot" />
            <UButton icon="i-lucide-folder-open" :label="wide ? t('app.diagnostics.openLogs') : undefined" :aria-label="t('app.diagnostics.openLogs')" color="neutral" variant="ghost" size="sm" @click="openLogs" />
            <UButton icon="i-lucide-eraser" :label="wide ? t('app.diagnostics.clear') : undefined" :aria-label="t('app.diagnostics.clear')" color="neutral" variant="ghost" size="sm" @click="clear" />
            <UButton icon="i-lucide-x" color="neutral" variant="ghost" size="sm" :aria-label="t('app.diagnostics.close')" @click="open = false" />
          </div>
        </header>

        <div class="border-b border-default px-2 sm:px-3">
          <UTabs v-model="tab" :items="tabs" :content="false" variant="link" size="sm" class="overflow-x-auto" :ui="{ list: 'border-b-0 w-max min-w-full', trigger: 'flex-none' }" />
        </div>

        <UAlert v-if="failure" color="error" variant="soft" icon="i-lucide-circle-alert" :title="t('app.diagnostics.failed')" :description="failure" class="m-3 w-auto" />

        <div class="min-h-0 flex-1">
          <DiagnosticsOverview v-if="tab === 'overview'" :diag="diag" />
          <DiagnosticsCommands v-else-if="tab === 'commands'" />
          <DiagnosticsEngine v-else-if="tab === 'engine'" />
          <DiagnosticsLog v-else-if="tab === 'log'" :accounts="diag?.accounts ?? []" />
          <DiagnosticsUi v-else />
        </div>
      </div>
    </template>
  </UModal>
</template>
