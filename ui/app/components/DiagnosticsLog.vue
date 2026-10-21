<script setup lang="ts">
import { useVirtualizer } from '@tanstack/vue-virtual'
import { refDebounced } from '@vueuse/core'
import type { AccountDiagnostics, LogLine } from '~/composables/usePerf'
import { clock, num } from '~/utils/diagnostics'

const props = defineProps<{ accounts: AccountDiagnostics[] }>()
const { t } = useI18n()
const toast = useToast()
const { accountColor } = useMail()

const LEVELS = ['error', 'warn', 'info', 'debug'] as const
const levels = ref<Set<string>>(new Set())
function toggle(level: string) {
  const next = new Set(levels.value)
  if (next.has(level)) next.delete(level)
  else next.add(level)
  levels.value = next
}
const query = ref('')
const debouncedQuery = refDebounced(query, 300)
const limit = ref(1000)
const limits = [300, 1000, 5000].map(n => ({ label: num(n), value: n }))
const follow = ref(true)
const wrap = ref(true)

const lines = shallowRef<LogLine[]>([])
const error = ref<string | null>(null)
const loading = ref(false)
async function load() {
  if (loading.value) return
  loading.value = true
  try {
    const next = await api.readLog(limit.value, levels.value.size ? [...levels.value].join(',') : null, debouncedQuery.value.trim() || null)
    // Nothing new: the list (and its scroll) stays as it is.
    const [a, b] = [lines.value.at(-1), next.at(-1)]
    if (next.length !== lines.value.length || a?.ts !== b?.ts || a?.message !== b?.message || next[0]?.ts !== lines.value[0]?.ts) lines.value = next
    error.value = null
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
}
watch([levels, debouncedQuery, limit], load, { immediate: true })
const timer = setInterval(() => follow.value && load(), 2000)
onBeforeUnmount(() => clearInterval(timer))

const scroller = useTemplateRef<HTMLElement>('scroller')
const virtualizer = useVirtualizer(computed(() => ({
  count: lines.value.length,
  getScrollElement: () => scroller.value,
  estimateSize: () => 22,
  overscan: 20
})))
const visible = computed(() => virtualizer.value.getVirtualItems())
function measure(el: unknown) {
  if (el) virtualizer.value.measureElement(el as Element)
}
// Wrapping changes every height.
watch(wrap, () => nextTick(() => virtualizer.value.measure()))

/** Following: the newest line stays in view; scrolling up stops it, back to the bottom resumes. */
watch(lines, () => {
  if (follow.value) nextTick(() => virtualizer.value.scrollToIndex(lines.value.length - 1, { align: 'end' }))
})
let lastTop = 0
function onScroll() {
  const el = scroller.value
  if (!el) return
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40
  if (el.scrollTop < lastTop - 4 && !atBottom) follow.value = false
  else if (atBottom && !follow.value && el.scrollTop > lastTop) follow.value = true
  lastTop = el.scrollTop
}

const levelClass: Record<string, string> = {
  error: 'bg-error/15 text-error',
  warn: 'bg-warning/15 text-warning',
  info: 'bg-info/10 text-info',
  debug: 'bg-elevated text-muted',
  trace: 'bg-elevated text-dimmed'
}
const accountOf = (id: number) => props.accounts.find(a => a.id === id)
const shortTarget = (target: string) => target.replace(/^facteur_(core|app_lib)::?/, '') || target

async function copy(line: LogLine) {
  const text = `${new Date(line.ts).toISOString()} ${line.level.toUpperCase()} ${line.target}: ${line.message}`
  try {
    await navigator.clipboard.writeText(text)
    toast.add({ title: t('app.diagnostics.log.copied'), icon: 'i-lucide-clipboard-check', color: 'success', duration: 1500 })
  } catch {}
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div class="flex flex-wrap items-center gap-2 border-b border-default px-4 py-2.5 sm:px-5">
      <UButton
        v-for="l in LEVELS"
        :key="l"
        :label="t(`app.diagnostics.log.level.${l}`)"
        size="xs"
        :color="levels.has(l) ? 'primary' : 'neutral'"
        :variant="levels.has(l) ? 'soft' : 'outline'"
        @click="toggle(l)"
      />
      <UInput v-model="query" icon="i-lucide-search" size="sm" :placeholder="t('app.diagnostics.log.search')" :aria-label="t('app.diagnostics.log.search')" class="w-64 max-w-full" />
      <USelect v-model="limit" :items="limits" size="sm" class="w-24" :aria-label="t('app.diagnostics.log.lines')" />
      <div class="ms-auto flex items-center gap-4">
        <USwitch v-model="wrap" size="sm" :label="t('app.diagnostics.log.wrap')" />
        <USwitch v-model="follow" size="sm" :label="t('app.diagnostics.log.follow')" />
      </div>
    </div>
    <UAlert v-if="error" color="error" variant="soft" icon="i-lucide-circle-alert" :description="error" class="m-3 w-auto" />
    <p v-else-if="!lines.length && !loading" class="p-5 text-sm text-muted">{{ levels.size || query ? t('app.diagnostics.nothingMatches') : t('app.diagnostics.log.empty') }}</p>
    <div ref="scroller" class="min-h-0 flex-1 overflow-auto font-mono text-xs" @scroll.passive="onScroll">
      <div class="relative w-full" :style="{ height: `${virtualizer.getTotalSize()}px` }">
        <div
          v-for="v in visible"
          :key="String(v.key)"
          :ref="measure"
          :data-index="v.index"
          class="absolute top-0 left-0 w-full"
          :style="{ transform: `translateY(${v.start}px)` }"
        >
          <button
            type="button"
            class="flex w-full flex-wrap gap-x-2 px-4 py-[3px] text-left hover:bg-elevated/60 sm:flex-nowrap sm:px-5"
            :class="lines[v.index]!.level === 'error' && 'bg-error/5'"
            :title="t('app.diagnostics.log.clickToCopy')"
            @click="copy(lines[v.index]!)"
          >
            <span class="shrink-0 tabular-nums text-dimmed">{{ clock(lines[v.index]!.ts, true) }}</span>
            <span class="w-11 shrink-0 rounded px-1 text-center text-[10.5px] font-semibold uppercase" :class="levelClass[lines[v.index]!.level]">{{ lines[v.index]!.level }}</span>
            <span class="w-10 shrink-0">
              <UTooltip v-if="lines[v.index]!.account !== null" :text="accountOf(lines[v.index]!.account!)?.email ?? t('app.diagnostics.log.unknownAccount')">
                <span class="inline-flex items-center gap-1 rounded bg-elevated px-1 text-default">
                  <span class="size-1.5 rounded-full" :class="accountColor(lines[v.index]!.account!).dot" />#{{ lines[v.index]!.account }}
                </span>
              </UTooltip>
            </span>
            <span class="shrink-0 truncate text-muted sm:w-36" :title="lines[v.index]!.target">{{ shortTarget(lines[v.index]!.target) }}</span>
            <span class="min-w-0 flex-1 text-default max-sm:basis-full" :class="wrap ? 'break-words whitespace-pre-wrap' : 'truncate whitespace-pre'">{{ lines[v.index]!.message }}</span>
          </button>
        </div>
      </div>
    </div>
    <p class="border-t border-default px-4 py-1.5 text-xs text-muted sm:px-5">{{ t('app.diagnostics.log.count', { n: num(lines.length) }, lines.length) }}</p>
  </div>
</template>
