<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import type { MessageDetail, MessageSummary } from '~/utils/types'

/** `latest`: last message of the thread; on mobile its reply buttons sit in the thread's bottom bar.
 *  `draft`: saved in Drafts, never sent; it is resumed instead of answered. */
const props = defineProps<{ message: MessageSummary, detail?: MessageDetail, expanded: boolean, loading: boolean, latest?: boolean, draft?: boolean }>()
defineEmits<{ toggle: [id: number] }>()

const { reply, forward, composeMailto, resume, discardSaved } = useComposer()
const { quick } = useTemplates()
const { accountById, accountColor, isMine } = useMail()
const showError = useErrorToast()
const { t } = useI18n()
const toast = useToast()
const { done } = useUndo()
const allowRemote = ref(false)
const unsubscribing = ref(false)
const openLink = useOpenLink()

/** Remote content shown only because the sender is approved, not by a click: withdrawn if the
 *  engine takes that back (the trust weighed again after its DKIM check, now low). */
let autoAllowed = false
// Approved sender: images shown on open.
watch(() => props.detail?.images_allowed, (allowed) => {
  if (allowed) {
    if (!allowRemote.value) autoAllowed = true
    allowRemote.value = true
  } else if (autoAllowed) {
    autoAllowed = false
    allowRemote.value = false
  }
}, { immediate: true })

async function alwaysShowRemote(key: string) {
  try {
    await api.addTrustedImages(key)
    autoAllowed = false
    allowRemote.value = true
    toast.add({ title: t('mail.messageCard.imagesAlways'), description: t('mail.messageCard.imagesAlwaysDesc', { key }), icon: 'i-lucide-image' })
  } catch (e) {
    showError(e)
  }
}

/** The sender's approval applies: signed by its domain, not one's own copy. */
const senderApprovable = computed(() => !!props.detail?.sender_key && props.detail.sender_proven && !props.detail.own_copy && !lowTrust.value)
const remoteActions = computed(() => {
  const d = props.detail
  // Never a lasting approval from a suspicious message.
  if (!d || lowTrust.value) return [{ label: t('mail.messageCard.show'), color: 'neutral' as const, variant: 'outline' as const, onClick: showRemote }]
  const sender = senderApprovable.value ? d.sender_key! : null
  return [
    { label: t('mail.messageCard.show'), color: 'neutral' as const, variant: 'outline' as const, onClick: showRemote },
    ...(sender ? [{ label: t('mail.messageCard.alwaysFor', { key: sender }), color: 'neutral' as const, variant: 'ghost' as const, onClick: () => alwaysShowRemote(sender) }] : []),
    // The sites hosting the images: approved, they load whoever sends them.
    ...d.blocked_hosts
      .filter(h => h !== sender)
      .slice(0, 2)
      .map(h => ({ label: t('mail.messageCard.alwaysFrom', { host: h }), color: 'neutral' as const, variant: 'ghost' as const, onClick: () => alwaysShowRemote(h) }))
  ]
})
const remoteNotice = computed(() => {
  const d = props.detail
  if (!d) return ''
  if (d.sender_key && !d.sender_proven && !d.own_copy && !lowTrust.value) return t('mail.messageCard.remoteUnproven', { key: d.sender_key })
  return d.image_hosts.length ? t('mail.messageCard.remotePartly') : t('mail.messageCard.remoteBlocked')
})

const lowTrust = computed(() => props.detail?.trust.level === 'low')
const trustTooltip = computed(() => props.detail?.trust.reasons.filter(r => r.positive).map(r => r.text).join(' · ') ?? '')

async function showRemote() {
  if (lowTrust.value && !await confirm(
    t('mail.messageCard.remoteConfirm'),
    { title: t('mail.messageCard.remoteConfirmTitle'), kind: 'warning' }
  )) return
  autoAllowed = false
  allowRemote.value = true
  // Shown again at the next opening (never for a message found untrustworthy).
  api.rememberImages(props.message.id).catch(showError)
}

async function reportSpam() {
  try {
    done(await api.reportSpam([props.message.id]), t('mail.messageCard.reportedJunk'), 'i-lucide-shield-alert')
    // Its row may lie past the list's first page, which the usual refresh does not reread.
    useListStale().bump()
  } catch (e) {
    showError(e)
  }
}

async function unsubscribe() {
  const d = props.detail
  if (!d?.unsubscribe) return
  const who = addressLabel(d.summary.from)
  if (!await confirm(t('mail.messageCard.unsubConfirm', { who }), { title: t('mail.messageCard.unsubscribe'), kind: 'warning' })) return
  unsubscribing.value = true
  try {
    let r = await api.unsubscribe(d.summary.id)
    if (r.method === 'confirm') {
      // The sender picks the address and subject: show them before sending anything.
      const ok = await confirm(t('mail.messageCard.unsubMailConfirm', { address: r.address, subject: r.subject }), {
        title: t('mail.messageCard.unsubMailTitle'),
        kind: 'warning'
      })
      if (!ok) return
      r = await api.unsubscribe(d.summary.id, true)
    }
    if (r.method === 'web') {
      // The sender chose this page: same checks as a link of the message.
      if (!await openLink(r.url, { cautious: lowTrust.value && 'message' })) return
      toast.add({ title: t('mail.messageCard.unsubPageOpened'), description: t('mail.messageCard.unsubPageDesc'), icon: 'i-lucide-external-link' })
    } else {
      toast.add({
        title: t('mail.messageCard.unsubscribed'),
        description: r.method === 'mailto' ? t('mail.messageCard.unsubSent', { address: r.address }) : t('mail.messageCard.unsubDone', { who }),
        icon: 'i-lucide-mail-x'
      })
    }
  } catch (e) {
    showError(e, t('mail.messageCard.unsubFailed'))
  } finally {
    unsubscribing.value = false
  }
}

const discarding = ref(false)
async function discardDraft() {
  if (!await confirm(t('mail.messageCard.discardDraftConfirm'), { title: t('mail.messageCard.discardDraft'), kind: 'warning', okLabel: t('mail.messageCard.discardDraft') })) return
  discarding.value = true
  try {
    if (await discardSaved(props.message.id)) useListStale().bump()
  } finally {
    discarding.value = false
  }
}

const sourceOpen = ref(false)

/** Prints the message (or saves it as PDF), headers included. Isolated frame: no scripts, remote
 *  images only if they are shown. */
function printMessage() {
  const d = props.detail
  if (!d) return
  const remote = remoteSources(allowRemote.value, d.image_hosts)
  const row = (label: string, value: string) => (value ? `<tr><th>${label}</th><td>${escapeHtml(value)}</td></tr>` : '')
  // No DNS prefetch of the mail's links (see MailFrame): first in the head, before any link.
  const doc = `<!doctype html><html><head><meta http-equiv="x-dns-prefetch-control" content="off"><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:${remote}; style-src 'unsafe-inline'${remote}; font-src data:${remote}">
<title>${escapeHtml(d.summary.subject || t('mail.messageCard.print.message'))}</title>
<style>
  body { font: 13px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; color: #18181b; margin: 24px; }
  h1 { font-size: 18px; margin: 0 0 8px; }
  table.facteur-head { border-collapse: collapse; margin-bottom: 16px; padding-bottom: 12px; border-bottom: 1px solid #d4d4d8; width: 100%; }
  table.facteur-head th { text-align: left; color: #71717a; font-weight: 500; padding: 1px 12px 1px 0; width: 1%; white-space: nowrap; vertical-align: top; }
  img { max-width: 100%; height: auto; }
  blockquote { margin: 0 0 0 .5em; padding-left: .75em; border-left: 3px solid #d4d4d8; color: #71717a; }
</style></head><body>
<h1>${escapeHtml(d.summary.subject || t('common.noSubject'))}</h1>
<table class="facteur-head">
${row(t('mail.messageCard.print.from'), d.summary.from ? addressFull(d.summary.from) : '')}
${row(t('mail.messageCard.print.to'), d.summary.to.map(addressFull).join(', '))}
${row(t('mail.messageCard.print.cc'), d.cc.map(addressFull).join(', '))}
${row(t('mail.messageCard.print.date'), longDate(d.summary.date))}
${row(t('mail.messageCard.print.attachments'), d.attachments.map(a => a.filename).join(', '))}
</table>
${d.html}
</body></html>`
  const frame = document.createElement('iframe')
  frame.setAttribute('sandbox', 'allow-same-origin allow-modals')
  frame.style.cssText = 'position:fixed;right:0;bottom:0;width:0;height:0;border:0'
  frame.srcdoc = doc
  frame.onload = () => {
    frame.contentWindow?.focus()
    frame.contentWindow?.print()
    setTimeout(() => frame.remove(), 1000)
  }
  document.body.appendChild(frame)
}

const darkMail = useDarkMail()
/** This message in its original colours, despite dark mode. */
const original = ref(false)

const moreItems = computed(() => [[
  ...(darkMail.active.value && props.detail && !props.detail.is_plain
    ? [{
        label: original.value ? t('mail.messageCard.darken') : t('mail.messageCard.originalColors'),
        icon: original.value ? 'i-lucide-moon' : 'i-lucide-sun',
        onSelect: () => { original.value = !original.value }
      }]
    : []),
  { label: t('mail.messageCard.print.action'), icon: 'i-lucide-printer', onSelect: printMessage },
  { label: t('mail.messageCard.showSource'), icon: 'i-lucide-file-code', onSelect: () => { sourceOpen.value = true } }
]])

/** Recipients shown before "+N"; all of them (with Cc and Reply-To) once expanded. */
const FIRST_RECIPIENTS = 3
const showAll = ref(false)
const to = computed(() => props.detail?.summary.to ?? props.message.to)
const shownTo = computed(() => (showAll.value ? to.value : to.value.slice(0, FIRST_RECIPIENTS)))
/** Reply-To only when it differs from the sender. */
const replyTo = computed(() => (props.detail?.reply_to ?? []).filter(a => a.email.toLowerCase() !== props.message.from?.email.toLowerCase()))
/** Recipients only: a Reply-To is no recipient, it is shown apart. */
const hiddenCount = computed(() => Math.max(0, to.value.length - FIRST_RECIPIENTS) + (props.detail?.cc.length ?? 0))

const account = computed(() => accountById(props.message.account_id))
const color = computed(() => accountColor(props.message.account_id))
/** Sent by me: the recipients are shown, not my own account. */
const fromMe = computed(() => isMine(props.message.from?.email, props.message.account_id))
/** My address among the recipients (account address or alias); none when Bcc or a mailing list. */
const ownRecipient = computed(() => [...to.value, ...(props.detail?.cc ?? [])].find(a => isMine(a.email, props.message.account_id)) ?? null)
/** Which of my accounts or aliases received the message: an alias shows its own address. */
const receivedAs = computed(() => {
  const a = account.value
  if (!a || fromMe.value) return null
  const own = ownRecipient.value
  if (own && own.email.toLowerCase() !== a.email.toLowerCase()) return { label: own.email, email: own.email }
  return { label: accountName(a), email: own?.email ?? a.email }
})
const othersCount = computed(() => to.value.length + (props.detail?.cc.length ?? 0) - (ownRecipient.value ? 1 : 0))
/** Shown on "Reply all": how many people it writes to, against a reply-all by mistake. */
const replyAllCount = computed(() => {
  const d = props.detail
  if (!d) return 0
  const all = [...(d.reply_to?.length ? d.reply_to : [d.summary.from]), ...d.summary.to, ...d.cc].filter((a): a is NonNullable<typeof a> => !!a)
  return new Set(all.map(a => a.email.toLowerCase()).filter(e => !isMine(e, props.message.account_id))).size
})

const invite = computed(() => props.detail?.invite ?? null)

/** A day and time found in the message's words: "Mercredi 14 octobre · 18:30", made an event. */
const appointment = computed(() => props.detail?.appointment ?? null)
const appointmentWhen = computed(() => {
  const e = appointment.value?.event
  if (!e?.start) return ''
  const d = new Date(e.start * 1000)
  const day = dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(d)
  const label = `${day.charAt(0).toUpperCase()}${day.slice(1)}`
  return e.all_day ? label : `${label} · ${dateFormat({ hour: '2-digit', minute: '2-digit' }).format(d)}`
})
const eventOpen = ref(false)
/** Under an invitation card, the calendar service's own e-mail (hard to read in dark mode,
 *  with its own answer buttons) stays folded until asked for. */
const originalOpen = ref(false)
const originalFrom = computed(() => {
  const provider = invite.value ? inviteProvider(invite.value) : null
  if (provider === 'google') return t('mail.invite.provider.google')
  if (provider === 'outlook') return 'Outlook'
  return addressLabel(props.message.from)
})
const originalExcerpt = computed(() => {
  const d = props.detail
  if (!d?.invite) return ''
  const text = (d.is_plain ? d.html.replace(/<[^>]*>/g, ' ') : htmlLines(d.html)).replace(/\s+/g, ' ').trim()
  return text.length > 48 ? `${text.slice(0, 47).trimEnd()}…` : text
})

</script>

<template>
  <div
    v-if="!expanded"
    role="button"
    tabindex="0"
    class="flex cursor-pointer items-start gap-3 rounded-2xl border border-default bg-(--surface-card) px-3.5 py-3 text-left shadow-xs transition-colors hover:bg-elevated/40 lg:items-center lg:px-4 dark:shadow-none"
    @click="$emit('toggle', message.id)"
    @keydown.enter.self="$emit('toggle', message.id)"
  >
    <MailAvatar :address="message.from" />
    <div class="flex min-w-0 flex-1 flex-col gap-0.5">
      <div class="flex min-w-0 items-baseline gap-2">
        <span class="truncate text-sm font-semibold text-highlighted">{{ addressLabel(message.from) }}</span>
        <UBadge v-if="draft" :label="$t('mail.messageCard.draft')" color="error" variant="subtle" size="sm" class="shrink-0" />
        <span class="flex-1" />
        <span class="shrink-0 text-xs text-muted lg:hidden">{{ shortDate(message.date) }}</span>
      </div>
      <p class="line-clamp-2 text-[13px] leading-snug text-muted lg:hidden">{{ message.preview }}</p>
      <p class="hidden truncate text-[13px] text-muted lg:block">{{ message.preview }}</p>
    </div>
    <div class="hidden shrink-0 items-center gap-2 lg:flex">
      <UIcon v-if="message.has_attachments" name="i-lucide-paperclip" class="size-4 text-dimmed" />
      <UIcon v-if="message.flags.flagged" name="i-lucide-pin" class="size-4 text-amber-600 dark:text-amber-400" />
      <UTooltip :text="longDate(message.date)">
        <span class="text-xs text-muted">{{ shortDate(message.date) }}</span>
      </UTooltip>
    </div>
  </div>

  <article
    v-else
    class="overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none"
    :class="invite && 'max-lg:overflow-visible max-lg:rounded-none max-lg:border-0 max-lg:bg-transparent max-lg:shadow-none'"
  >
    <div
      v-if="invite"
      role="button"
      tabindex="0"
      class="flex items-center gap-2.5 px-1 text-left lg:hidden"
      @click="$emit('toggle', message.id)"
      @keydown.enter.self="$emit('toggle', message.id)"
    >
      <MailAvatar :address="message.from" />
      <div class="flex min-w-0 flex-1 flex-col gap-px">
        <span class="truncate text-[14.5px] font-semibold text-highlighted">{{ addressLabel(message.from) }}</span>
        <span class="flex min-w-0 items-center gap-1.5 text-xs text-muted">
          <template v-if="receivedAs">
            <span class="shrink-0">{{ $t('mail.invite.to') }}</span>
            <span class="inline-flex min-w-0 items-center gap-1.25 font-semibold text-toned" :title="receivedAs.email">
              <span class="size-1.75 shrink-0 rounded-full" :class="color.dot" />
              <span class="truncate">{{ receivedAs.label }}</span>
            </span>
            <span class="shrink-0">·</span>
          </template>
          <span class="shrink-0">{{ shortDate(message.date) }}</span>
        </span>
      </div>
      <UDropdownMenu :items="moreItems" :content="{ align: 'end' }" size="lg">
        <UButton
          icon="i-lucide-ellipsis"
          color="neutral"
          variant="ghost"
          class="-me-1 size-11 justify-center rounded-xl"
          :aria-label="$t('mail.threadView.more')"
          @click.stop
        />
      </UDropdownMenu>
    </div>
    <div
      role="button"
      tabindex="0"
      class="flex w-full items-start gap-3 border-b border-default/60 px-3.5 pt-3.5 pb-3 text-left lg:px-4.5 lg:py-4"
      :class="invite && 'max-lg:hidden'"
      @click="$emit('toggle', message.id)"
      @keydown.enter.self="$emit('toggle', message.id)"
    >
      <MailAvatar :address="message.from" />
      <div class="flex min-w-0 flex-1 flex-col gap-1">
        <div class="flex min-w-0 items-center gap-2">
          <div class="flex min-w-0 items-center gap-x-2 lg:flex-wrap" @click.stop>
            <AddressChip v-if="message.from" :address="message.from" :account-id="message.account_id" sender header class="-ms-2 min-w-0" />
            <span v-else class="truncate text-[15px] font-bold text-highlighted">{{ addressLabel(message.from) }}</span>
            <span v-if="message.from?.name" class="hidden truncate text-[12.5px] text-muted lg:inline">{{ message.from.email }}</span>
            <!-- One's own copy: its trust only says one writes to these people, nothing to show. -->
            <UPopover v-if="detail?.trust.level === 'high' && !detail.own_copy" :content="{ align: 'start' }">
              <button
                type="button"
                class="relative inline-flex shrink-0 items-center gap-1 rounded-full py-px ps-[5px] pe-2 text-[11px] font-semibold text-green-700 before:absolute before:-inset-3 lg:bg-green-100 lg:before:hidden dark:text-green-300 lg:dark:bg-green-500/15"
                :aria-label="$t('mail.reader.verified')"
              >
                <UIcon name="i-lucide-shield-check" class="size-3.5" />
                <span class="hidden lg:inline">{{ $t('mail.reader.verified') }}</span>
              </button>
              <template #content>
                <p class="max-w-80 p-3 text-sm text-toned">{{ $t('mail.messageCard.verified', { reasons: trustTooltip }) }}</p>
              </template>
            </UPopover>
            <UPopover v-else-if="lowTrust" :content="{ align: 'start' }">
              <button
                type="button"
                class="relative inline-flex shrink-0 items-center gap-1 rounded-full py-px ps-[5px] pe-2 text-[11px] font-semibold text-red-700 before:absolute before:-inset-3 lg:bg-red-100 lg:before:hidden dark:text-red-300 lg:dark:bg-red-500/15"
                :aria-label="$t('mail.messageCard.untrusted')"
              >
                <UIcon name="i-lucide-shield-alert" class="size-3.5" />
                <span class="hidden lg:inline">{{ $t('mail.messageCard.untrusted') }}</span>
              </button>
              <template #content>
                <p class="max-w-80 p-3 text-sm text-toned">{{ $t('mail.messageCard.fraudTitle') }}</p>
              </template>
            </UPopover>
            <UBadge v-if="draft" :label="$t('mail.messageCard.draft')" color="error" variant="subtle" size="sm" class="shrink-0" />
          </div>
          <span class="flex-1" />
          <UIcon v-if="message.has_attachments" name="i-lucide-paperclip" class="size-4 shrink-0 text-dimmed" />
          <UIcon v-if="message.flags.flagged" name="i-lucide-pin" class="size-4 shrink-0 text-amber-600 dark:text-amber-400" />
          <UTooltip :text="longDate(message.date)">
            <span class="shrink-0 text-xs text-muted">{{ shortDate(message.date) }}</span>
          </UTooltip>
          <!-- Mobile: the message menu lives here, its reply row being in the bottom bar. -->
          <UDropdownMenu v-if="detail" :items="moreItems" :content="{ align: 'end' }" size="lg">
            <UButton
              icon="i-lucide-ellipsis"
              color="neutral"
              variant="ghost"
              class="-my-2.5 -me-2.5 size-11 justify-center rounded-xl lg:hidden"
              :aria-label="$t('mail.threadView.more')"
              @click.stop
            />
          </UDropdownMenu>
        </div>
        <span v-if="message.from?.name" class="truncate text-[12.5px] text-muted lg:hidden">{{ message.from.email }}</span>

        <div class="space-y-0.5 text-sm" @click.stop>
          <div class="flex min-w-0 flex-wrap items-center gap-1.5 text-[12.5px] text-muted">
            <template v-if="receivedAs && account">
              <span class="hidden lg:inline">{{ $t('mail.messageCard.to') }}</span>
              <span
                class="mt-0.5 inline-flex min-w-0 items-center gap-1.5 rounded-full py-0.5 ps-0.5 pe-2 text-xs font-semibold lg:mt-0 lg:text-[12.5px]"
                :class="color.soft"
                :title="receivedAs.email"
              >
                <img v-if="account.avatar" :src="account.avatar" alt="" class="size-4.5 shrink-0 rounded-full object-cover">
                <span v-else class="flex size-4.5 shrink-0 items-center justify-center rounded-full text-[8px] font-bold" :class="color.solid">{{ initials(accountName(account)) }}</span>
                <span class="truncate"><span class="lg:hidden">{{ `${$t('mail.messageCard.to')}\u00a0` }}</span>{{ receivedAs.label }}</span>
              </span>
              <UButton
                v-if="!showAll && othersCount > 0"
                :label="$t('mail.messageCard.others', othersCount)"
                size="xs"
                color="neutral"
                variant="outline"
                class="relative h-6 rounded-full px-2.25 text-[12.5px] font-semibold before:absolute before:-inset-3 lg:before:hidden"
                @click="showAll = true"
              />
            </template>
            <template v-else>
              <span class="w-14 shrink-0 text-xs text-muted">{{ $t('mail.messageCard.to') }}</span>
              <AddressChip v-for="a in shownTo" :key="`to:${a.email}`" :address="a" :account-id="message.account_id" class="text-sm" />
              <UButton
                v-if="!showAll && hiddenCount > 0"
                :label="$t('mail.messageCard.others', hiddenCount)"
                size="xs"
                color="neutral"
                variant="outline"
                class="relative h-6 rounded-full px-2.25 text-[12.5px] font-semibold before:absolute before:-inset-3 lg:before:hidden"
                @click="showAll = true"
              />
            </template>
            <!-- Answers go elsewhere than the sender: said where the reply would go. -->
            <UButton
              v-if="!showAll && replyTo.length"
              :label="$t('mail.messageCard.repliesTo', { who: replyTo[0]!.email })"
              icon="i-lucide-reply"
              size="xs"
              color="neutral"
              variant="ghost"
              class="relative h-6 min-w-0 rounded-full px-2 text-[12.5px] text-muted before:absolute before:-inset-3 lg:before:hidden"
              :ui="{ label: 'truncate' }"
              @click="showAll = true"
            />
          </div>
          <template v-if="showAll">
            <div v-if="receivedAs && to.length" class="flex flex-wrap items-center gap-0.5">
              <span class="w-14 shrink-0 text-xs text-muted">{{ $t('mail.messageCard.to') }}</span>
              <AddressChip v-for="a in to" :key="`to:${a.email}`" :address="a" :account-id="message.account_id" />
            </div>
            <div v-if="detail?.cc.length" class="flex flex-wrap items-center gap-0.5">
              <span class="w-14 shrink-0 text-xs text-muted">{{ $t('mail.messageCard.cc') }}</span>
              <AddressChip v-for="a in detail.cc" :key="`cc:${a.email}`" :address="a" :account-id="message.account_id" />
            </div>
            <div v-if="replyTo.length" class="flex flex-wrap items-center gap-0.5">
              <span class="w-14 shrink-0 text-xs text-muted">{{ $t('mail.messageCard.replyTo') }}</span>
              <AddressChip v-for="a in replyTo" :key="`rt:${a.email}`" :address="a" :account-id="message.account_id" />
            </div>
            <UButton
              :label="$t('mail.messageCard.less')"
              size="xs"
              color="neutral"
              variant="link"
              class="relative px-0 before:absolute before:-inset-3 lg:before:hidden"
              @click="showAll = false"
            />
          </template>
        </div>
      </div>
    </div>

    <div class="px-4 pt-4 pb-4.5 lg:ps-16.5 lg:pe-5.5 lg:pt-5" :class="invite && 'max-lg:px-0 max-lg:pt-3.5 max-lg:pb-1'">
      <div v-if="loading && !detail" class="flex items-center gap-2 py-6 text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" /> {{ $t('mail.messageCard.downloading') }}
      </div>
      <template v-else-if="detail">
        <div class="max-w-180">
          <UAlert
            v-if="lowTrust"
            class="mb-3"
            color="error"
            variant="subtle"
            icon="i-lucide-shield-alert"
            :title="$t('mail.messageCard.fraudTitle')"
            :actions="[{ label: $t('mail.messageCard.reportJunk'), color: 'error', variant: 'outline', onClick: reportSpam }]"
          >
            <template #description>
              <ul class="mt-1 list-disc ps-4">
                <li v-for="r in detail.trust.reasons.filter(r => !r.positive)" :key="r.text">{{ r.text }}</li>
              </ul>
              <p class="mt-1">{{ $t('mail.messageCard.fraudAdvice') }}</p>
            </template>
          </UAlert>
          <div v-if="detail.unsubscribe && !lowTrust" class="mb-3 flex items-center gap-2 rounded-lg bg-elevated/60 px-3 py-2 text-sm text-muted">
            <UIcon name="i-lucide-newspaper" class="size-4 shrink-0" />
            <span class="flex-1">{{ $t('mail.messageCard.bulk') }}</span>
            <UButton
              :label="$t('mail.messageCard.unsubscribe')"
              icon="i-lucide-mail-x"
              size="xs"
              color="neutral"
              variant="outline"
              class="max-lg:min-h-11"
              :loading="unsubscribing"
              @click="unsubscribe"
            />
          </div>
          <UAlert
            v-if="detail.has_remote_content && !allowRemote && !detail.images_covered && (!detail.invite || originalOpen)"
            class="mb-3"
            color="neutral"
            variant="subtle"
            icon="i-lucide-image-off"
            :description="remoteNotice"
            :actions="remoteActions"
          />
          <template v-if="detail.invite">
            <InviteCard :message-id="detail.summary.id" :invite="detail.invite" :account-id="message.account_id" :html="detail.html" />
            <InviteOriginal :open="originalOpen" :from="originalFrom" :excerpt="originalExcerpt" class="mt-3.5 lg:mt-3" @toggle="originalOpen = !originalOpen" />
          </template>
          <MailFrame
            v-if="!detail.invite || originalOpen"
            :class="detail.invite && 'mt-3'"
            :message-id="detail.summary.id"
            :html="detail.html"
            :plain="detail.is_plain"
            :allow-remote="allowRemote"
            :hosts="detail.image_hosts"
            :cautious="lowTrust"
            :darken="darkMail.active.value && !original"
            :highlight="appointment?.phrase"
            @mailto="composeMailto"
          />

          <div
            v-if="appointment && appointmentWhen"
            class="mt-3.5 flex items-center gap-3 rounded-[14px] border border-sky-200 bg-sky-50 px-3 py-2.5 lg:rounded-xl dark:border-sky-500/30 dark:bg-sky-500/8"
          >
            <span class="flex size-8.5 shrink-0 items-center justify-center rounded-[10px] bg-(--surface-card) text-sky-700 lg:size-8 lg:rounded-[9px] dark:text-sky-300">
              <UIcon name="i-lucide-calendar" class="size-4.25" />
            </span>
            <div class="flex min-w-0 flex-1 flex-col">
              <span class="truncate text-sm font-semibold text-highlighted lg:text-[13.5px]">{{ appointmentWhen }}</span>
              <span v-if="appointment.event.place" class="truncate text-xs text-muted lg:text-[12.5px]">{{ appointment.event.place }}</span>
            </div>
            <UButton
              icon="i-lucide-plus"
              color="neutral"
              variant="ghost"
              class="h-10 shrink-0 rounded-[10px] bg-(--surface-card) px-3 font-semibold text-sky-700 hover:bg-(--surface-card)/70 lg:h-8.5 lg:rounded-[9px] dark:text-sky-300"
              @click="eventOpen = true"
            >
              <span class="lg:hidden">{{ $t('mail.messageCard.addToCalendarShort') }}</span>
              <span class="max-lg:hidden">{{ $t('mail.messageCard.addToCalendar') }}</span>
            </UButton>
          </div>

          <div v-if="detail.attachments.length" class="mt-4 flex flex-wrap gap-2" :class="invite && 'max-lg:mt-3.5'">
            <AttachmentChip v-for="a in detail.attachments" :key="a.index" :message-id="message.id" :account-id="message.account_id" :attachment="a" />
          </div>
        </div>

        <div v-if="draft" class="mt-4.5 flex flex-wrap gap-2">
          <UButton
            icon="i-lucide-file-pen"
            :label="$t('mail.messageCard.resumeDraft')"
            size="lg"
            class="rounded-[10px] font-semibold max-lg:min-h-11"
            @click="resume(message.id)"
          />
          <UButton
            icon="i-lucide-trash-2"
            :label="$t('mail.messageCard.discardDraft')"
            size="lg"
            color="error"
            variant="outline"
            class="rounded-[10px] max-lg:min-h-11"
            :loading="discarding"
            @click="discardDraft"
          />
          <UDropdownMenu :items="moreItems">
            <UButton icon="i-lucide-ellipsis" size="lg" color="neutral" variant="ghost" class="hidden rounded-[10px] lg:inline-flex" :aria-label="$t('mail.threadView.more')" />
          </UDropdownMenu>
        </div>

        <div v-else-if="latest && !fromMe && quick.length" class="mt-4.5 flex flex-wrap items-center gap-2">
          <UButton
            v-for="tpl in quick"
            :key="tpl.id"
            :label="tpl.name || $t('app.composer.templates.untitled')"
            icon="i-lucide-zap"
            size="sm"
            color="neutral"
            variant="soft"
            class="max-w-64 rounded-full max-lg:min-h-10"
            :ui="{ label: 'truncate' }"
            @click="reply(detail, false, tpl.html)"
          />
        </div>

        <div v-if="!draft" class="mt-4.5 flex-wrap gap-2" :class="latest ? 'hidden lg:flex' : 'flex'">
          <UButton
            icon="i-lucide-reply"
            :label="$t('mail.messageCard.reply')"
            size="lg"
            :color="latest ? 'primary' : 'neutral'"
            :variant="latest ? 'solid' : 'outline'"
            class="rounded-[10px] font-semibold max-lg:min-h-11"
            @click="reply(detail)"
          />
          <UButton icon="i-lucide-reply-all" :label="replyAllCount > 1 ? `${$t('mail.messageCard.replyAll')} · ${replyAllCount}` : $t('mail.messageCard.replyAll')" size="lg" color="neutral" variant="outline" class="rounded-[10px] max-lg:min-h-11" @click="reply(detail, true)" />
          <UButton icon="i-lucide-forward" :label="$t('mail.messageCard.forward')" size="lg" color="neutral" variant="outline" class="rounded-[10px] max-lg:min-h-11" @click="forward(detail)" />
          <UDropdownMenu :items="moreItems">
            <UButton icon="i-lucide-ellipsis" size="lg" color="neutral" variant="ghost" class="hidden rounded-[10px] lg:inline-flex" :aria-label="$t('mail.threadView.more')" />
          </UDropdownMenu>
        </div>
      </template>
    </div>
  </article>
  <CalendarEventForm v-if="appointment" v-model:open="eventOpen" :suggestion="appointment.event" />
  <MessageSource v-if="sourceOpen" v-model:open="sourceOpen" :message-id="message.id" :subject="message.subject" />
</template>
