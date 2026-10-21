<script setup lang="ts">
import { openUrl } from '@tauri-apps/plugin-opener'
import type { LinkReport } from '~/utils/types'

const props = defineProps<{
  href: string
  /** Engine analysis; null until it arrives. */
  report: LinkReport | null
  error?: string | null
  /** Window coordinates; `above`: the card is placed above the point. */
  x: number
  y: number
  above: boolean
}>()
const emit = defineEmits<{ enter: [], leave: [], close: [] }>()
const toast = useToast()
const { t } = useI18n()

/** Parsed locally, shown at once. */
const quickHost = computed(() => {
  try {
    return new URL(props.href).hostname.replace(/^www\./, '')
  } catch {
    return props.href
  }
})

const level = computed(() => props.report?.trust.level)
const look = computed(() => ({
  high: { label: t('mail.linkPreview.trusted'), icon: 'i-lucide-shield-check', color: 'success' as const, text: 'text-success' },
  medium: { label: t('mail.linkPreview.check'), icon: 'i-lucide-shield', color: 'warning' as const, text: 'text-warning' },
  low: { label: t('mail.linkPreview.suspicious'), icon: 'i-lucide-shield-alert', color: 'error' as const, text: 'text-error' }
})[level.value ?? 'medium'])

/** Subdomain (muted) and registrable domain (bold): the part that matters. */
const hostParts = computed(() => {
  const r = props.report
  if (!r) return { sub: '', domain: quickHost.value }
  const sub = r.host.endsWith(r.domain) ? r.host.slice(0, r.host.length - r.domain.length) : ''
  return { sub, domain: r.host.endsWith(r.domain) ? r.domain : r.host }
})

const facts = computed(() => {
  const r = props.report
  if (!r) return []
  const out: { icon: string, text: string, warn?: boolean }[] = []
  const monthYear = (at: number) => dateFormat({ month: 'long', year: 'numeric' }).format(new Date(at * 1000))
  if (r.registered) out.push({ icon: 'i-lucide-calendar', text: t('mail.linkPreview.created', { date: monthYear(r.registered) }) })
  out.push({
    icon: 'i-lucide-mails',
    text: r.seen_messages ? t('mail.linkPreview.seenIn', r.seen_messages) : t('mail.linkPreview.neverSeen')
  })
  // The domain the text announces (the link goes elsewhere): where the user expects to land.
  const a = r.announced
  if (a) {
    const domain = a.domain
    if (a.registered) {
      // Less than 180 days old: as the engine counts it, a warning.
      const young = Date.now() / 1000 - a.registered < 180 * 86_400
      out.push({ icon: 'i-lucide-calendar', text: t('mail.linkPreview.announced.created', { domain, date: monthYear(a.registered) }), warn: young })
    }
    out.push({
      icon: 'i-lucide-mails',
      text: a.seen_messages ? t('mail.linkPreview.announced.seenIn', { domain, n: a.seen_messages }, a.seen_messages) : t('mail.linkPreview.announced.neverSeen', { domain })
    })
    if (a.sender_domain) out.push({ icon: 'i-lucide-user-check', text: t('mail.linkPreview.announced.sender', { domain }) })
  }
  if (r.tracking.length) {
    out.push({ icon: 'i-lucide-radar', text: t('mail.linkPreview.tracking', { list: r.tracking.slice(0, 3).join(', ') + (r.tracking.length > 3 ? '…' : '') }) })
  }
  return out
})
/** What the sender's domain lacks to prove its mail: worth telling the sender too. */
const missing = computed(() => {
  const a = props.report?.sender_auth
  if (!a) return []
  const out: string[] = []
  if (a.dmarc === false) out.push(t('mail.linkPreview.sender.noDmarc'))
  if (a.spf === false) out.push(t('mail.linkPreview.sender.noSpf'))
  if (a.dkim.state === 'missing') out.push(t('mail.linkPreview.sender.noDkim'))
  else if (a.dkim.state === 'invalid') out.push(t('mail.linkPreview.sender.badDkim'))
  else if (a.dkim.state === 'other') out.push(t('mail.linkPreview.sender.otherDkim', { domain: a.dkim.domain }))
  return out
})
/** Reasons minus those repeating facts shown separately (engine texts, in both languages). */
const reasons = computed(() => (props.report?.trust.reasons ?? []).filter(r => !/^(Domaine créé|Domaine récent|Domaine existant|Présent dans|Domaine jamais vu|Domaine annoncé|Domain created|Recent domain|Domain in use|Found in|Domain never seen|Announced domain)/.test(r.text)))

async function open(url: string) {
  emit('close')
  try {
    await openUrl(url)
  } catch (e) {
    toast.add({ title: t('mail.linkPreview.openFailed'), description: String(e), color: 'error', icon: 'i-lucide-circle-alert' })
  }
}

async function copy() {
  try {
    await navigator.clipboard.writeText(props.report?.url ?? props.href)
    toast.add({ title: t('mail.linkPreview.copied'), icon: 'i-lucide-copy' })
  } catch {
    toast.add({ title: t('mail.linkPreview.copyFailed'), color: 'error' })
  }
}
</script>

<template>
  <Teleport to="body">
    <div
      class="fixed z-50 w-[22rem] max-w-[calc(100vw-1rem)] rounded-lg border border-default bg-default p-3 text-sm shadow-lg"
      :style="{ left: `${x}px`, top: `${y}px`, transform: above ? 'translateY(-100%)' : undefined }"
      role="dialog"
      :aria-label="$t('mail.linkPreview.label')"
      @mouseenter="emit('enter')"
      @mouseleave="emit('leave')"
    >
      <div class="flex items-center gap-2">
        <template v-if="report">
          <UIcon :name="look.icon" class="size-4 shrink-0" :class="look.text" />
          <span class="font-medium" :class="look.text">{{ look.label }}</span>
          <span class="ms-auto text-xs tabular-nums text-muted">{{ report.trust.score }}/100</span>
        </template>
        <template v-else-if="error">
          <UIcon name="i-lucide-circle-help" class="size-4 text-muted" />
          <span class="text-muted">{{ $t('mail.linkPreview.analysisFailed') }}</span>
        </template>
        <template v-else>
          <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin text-muted" />
          <span class="text-muted">{{ $t('mail.linkPreview.analyzing') }}</span>
        </template>
      </div>

      <p class="mt-2 break-all text-base leading-snug">
        <span class="text-muted">{{ hostParts.sub }}</span><span class="font-semibold text-highlighted">{{ hostParts.domain }}</span>
        <UIcon v-if="report && !report.secure" name="i-lucide-lock-open" class="ms-1 size-3.5 align-[-2px] text-warning" />
      </p>
      <p class="mt-0.5 line-clamp-2 break-all font-mono text-xs text-muted" :title="report?.url ?? href">{{ report?.url ?? href }}</p>

      <p v-if="report?.redirect_to" class="mt-2 break-all text-xs">
        <span class="text-muted">{{ $t('mail.linkPreview.redirectsTo') }} </span>
        <span class="text-highlighted">{{ report.redirect_to }}</span>
      </p>

      <ul v-if="reasons.length" class="mt-2 space-y-1 text-xs">
        <li v-for="r in reasons" :key="r.text" class="flex gap-1.5">
          <UIcon :name="r.positive ? 'i-lucide-check' : 'i-lucide-triangle-alert'" class="mt-px size-3.5 shrink-0" :class="r.positive ? 'text-success' : 'text-warning'" />
          <span :class="r.positive ? 'text-toned' : 'text-highlighted'">{{ r.text }}</span>
        </li>
      </ul>
      <ul v-if="facts.length" class="mt-2 space-y-1 border-t border-default pt-2 text-xs text-muted">
        <li v-for="f in facts" :key="f.text" class="flex gap-1.5" :class="f.warn ? 'text-warning' : undefined">
          <UIcon :name="f.icon" class="mt-px size-3.5 shrink-0" />
          <span>{{ f.text }}</span>
        </li>
      </ul>

      <div v-if="report?.sender_auth && missing.length" class="mt-2 border-t border-default pt-2 text-xs">
        <p class="flex items-center gap-1.5 text-muted">
          <UIcon name="i-lucide-shield-question" class="size-3.5 shrink-0" />
          {{ $t('mail.linkPreview.sender.title', { domain: report.sender_auth.domain }) }}
        </p>
        <ul class="mt-1 space-y-0.5 ps-5 text-toned">
          <li v-for="m in missing" :key="m" class="list-disc">{{ m }}</li>
        </ul>
        <p class="mt-1 ps-5 text-dimmed">{{ $t('mail.linkPreview.sender.advice') }}</p>
      </div>

      <div class="mt-3 flex flex-wrap items-center gap-1.5">
        <UButton
          :label="level === 'low' ? $t('mail.linkPreview.openAnyway') : $t('mail.linkPreview.open')"
          icon="i-lucide-external-link"
          size="xs"
          :color="level === 'low' ? 'error' : 'primary'"
          :variant="level === 'low' ? 'soft' : 'solid'"
          @click="open(report?.url ?? href)"
        />
        <UButton
          v-if="report?.clean_url"
          :label="$t('mail.linkPreview.noTracking')"
          icon="i-lucide-eraser"
          size="xs"
          color="neutral"
          variant="outline"
          @click="open(report.clean_url)"
        />
        <UButton :label="$t('mail.linkPreview.copy')" icon="i-lucide-copy" size="xs" color="neutral" variant="ghost" class="ms-auto" @click="copy" />
      </div>
    </div>
  </Teleport>
</template>
